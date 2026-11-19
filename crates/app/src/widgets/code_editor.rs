//! A text editor for queries and documents: syntax highlighting, a
//! keyboard-driven completion popup, brackets and quotes that close
//! themselves, and opt-in vim keys.

use std::ops::Range;

use eframe::egui::text::{CCursor, CCursorRange, LayoutJob};
use eframe::egui::text_edit::{TextEditOutput, TextEditState};
use eframe::egui::{
    self, Align2, Color32, CompletionPopup, CornerRadius, Event, EventFilter, FontId, Frame, Key,
    Margin, Modifiers, RectAlign, Stroke, Suggestion, TextFormat, TextStyle,
};
use nordpil_query::Highlight;

use crate::theme;
use crate::widgets::vim::{self, Edit, Register, Vim};

/// Highlighted byte range of the editor text.
pub type Span = (Range<usize>, Highlight);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    /// Replaces the word before the cursor.
    pub insert: String,
    /// Where the cursor ends up, as a byte offset into `insert`.
    pub cursor: usize,
    /// Bytes after the cursor to remove, e.g. a closing quote that `insert`
    /// already ends with.
    pub delete_after: usize,
    pub detail: String,
}

/// Language support for the editor. All offsets are byte offsets into `text`.
pub trait Language {
    /// Spans sorted by start and not overlapping. Gaps render as plain text.
    fn highlight(&self, text: &str) -> Vec<Span>;

    /// Characters that belong to a completable word. The word that ends at
    /// the cursor is what an accepted completion replaces.
    fn is_word_char(&self, c: char) -> bool;

    /// Completions for the word that ends at `cursor`. `explicit` is true
    /// when the user asked for completions with Ctrl+Space.
    fn complete(&self, text: &str, cursor: usize, explicit: bool) -> Vec<Completion>;
}

#[derive(Debug)]
pub struct CodeEditorOutput {
    /// The text field. Give it an accessible name, e.g. with
    /// [`egui::Response::labelled_by`].
    pub response: egui::Response,
    /// Enter (single line) or Ctrl/Cmd+Enter (multiline) while no popup is open.
    pub submitted: bool,
}

pub struct CodeEditor {
    id: egui::Id,
    multiline: bool,
    hint: String,
    /// Visible rows of a multiline editor.
    rows: usize,
    /// The editor grows to at least this height, e.g. to fill a scroll area.
    min_height: f32,
    line_numbers: bool,
    /// Drawn without its own frame, inside one the caller draws.
    frameless: bool,
    /// Only an object is valid, so focusing it empty puts in `{  }`.
    object: bool,
    popup_open: bool,
    /// Set by Ctrl+Space; cleared when the popup closes.
    explicit: bool,
    /// The word a completion just inserted, so the popup doesn't reopen for it.
    accepted_word: Option<String>,
    highlight_cache: Option<(String, Vec<Span>)>,
    /// Set by [`Self::vim`]; used while vim keys are on.
    vim: Option<VimState>,
}

/// The mode an editor with vim keys takes when it gains focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimFocus {
    /// Insert, for a field you go to in order to type, like the filter bar.
    Insert,
    /// The mode it had when it lost focus.
    Keep,
}

/// An editor's vim keys, kept across frames.
struct VimState {
    focus: VimFocus,
    vim: Vim,
    /// The moving end of a visual selection, on a char. Outside visual mode
    /// the text edit's cursor is vim's.
    cursor: usize,
    /// The selection last put on the text edit, so a mouse change shows.
    shown: Option<Range<usize>>,
    /// The text after vim's keys last ran, before the text edit, so a
    /// change made around vim, like a paste, shows next frame.
    last_text: Option<String>,
    /// Keys after a mode change, held for the next frame so each frame's
    /// keys run in one mode.
    deferred: Vec<Event>,
    had_focus: bool,
}

impl CodeEditor {
    pub fn new(id: egui::Id, multiline: bool) -> Self {
        Self {
            id,
            multiline,
            hint: String::new(),
            rows: 4,
            min_height: 0.0,
            line_numbers: false,
            frameless: false,
            object: false,
            popup_open: false,
            explicit: false,
            accepted_word: None,
            highlight_cache: None,
            vim: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn rows(mut self, rows: usize) -> Self {
        self.rows = rows;
        self
    }

    pub fn set_min_height(&mut self, height: f32) {
        self.min_height = height;
    }

    pub fn line_numbers(mut self) -> Self {
        self.line_numbers = true;
        self
    }

    pub fn frameless(mut self) -> Self {
        self.frameless = true;
        self
    }

    /// Only an object is valid here: focusing the editor while it is empty
    /// puts in `{  }` with the cursor inside, and leaving it with nothing
    /// else in it empties it again.
    pub fn object(mut self) -> Self {
        self.object = true;
        self
    }

    /// Vim keys while they are turned on with [`set_vim`].
    pub fn vim(mut self, focus: VimFocus) -> Self {
        self.vim = Some(VimState {
            focus,
            vim: Vim::new(self.multiline),
            cursor: 0,
            shown: None,
            last_text: None,
            deferred: Vec::new(),
            had_focus: false,
        });
        self
    }

    /// The vim mode, when this editor has vim keys and they are on.
    pub fn vim_mode(&self, ctx: &egui::Context) -> Option<vim::Mode> {
        let state = self.vim.as_ref().filter(|_| vim_enabled(ctx))?;
        Some(state.vim.mode())
    }

    pub fn id(&self) -> egui::Id {
        self.id
    }

    /// Focuses the editor and selects all of `text`, like Ctrl+L in a browser.
    pub fn focus_and_select_all(&self, ctx: &egui::Context, text: &str) {
        ctx.memory_mut(|memory| memory.request_focus(self.id));
        let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(0),
            CCursor::new(text.chars().count()),
        )));
        state.store(ctx, self.id);
    }

    /// Whether the completion popup was open last frame.
    pub fn popup_open(&self) -> bool {
        self.popup_open
    }

    /// Closes the completion popup, as Escape would, before the editor is
    /// shown this frame.
    pub fn dismiss_popup(&self, ui: &egui::Ui) {
        if self.popup_open {
            ui.input_mut(|input| {
                input.events.push(Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                });
            });
        }
    }

    /// Focuses the editor where its cursor was; the first time, at the end.
    pub fn focus(&self, ctx: &egui::Context, text: &str) {
        let has_cursor = TextEditState::load(ctx, self.id)
            .is_some_and(|state| state.cursor.char_range().is_some());
        if has_cursor {
            ctx.memory_mut(|memory| memory.request_focus(self.id));
        } else {
            self.focus_at(ctx, text.chars().count());
        }
    }

    /// The selection as byte offsets into `text`; empty at the cursor.
    /// `None` until the editor has been shown.
    pub fn selection(&self, ctx: &egui::Context, text: &str) -> Option<Range<usize>> {
        let range = TextEditState::load(ctx, self.id)?.cursor.char_range()?;
        let (a, b) = (range.primary.index.0, range.secondary.index.0);
        Some(byte_offset(text, a.min(b))..byte_offset(text, a.max(b)))
    }

    /// Focuses the editor with the cursor after `chars` characters.
    pub fn focus_at(&self, ctx: &egui::Context, chars: usize) {
        ctx.memory_mut(|memory| memory.request_focus(self.id));
        let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(chars))));
        state.store(ctx, self.id);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut String,
        language: &dyn Language,
    ) -> CodeEditorOutput {
        let focused = ui.memory(|m| m.has_focus(self.id));
        let vim_on = self.vim.is_some() && vim_enabled(ui.ctx());
        if vim_on {
            self.vim_focus(ui, text, focused);
        }
        let mut submitted = false;
        if focused {
            ui.input_mut(|input| {
                if input.consume_key(Modifiers::CTRL, Key::Space) {
                    self.explicit = true;
                }
                // While the popup is open, Enter accepts a completion instead.
                if !self.popup_open {
                    let modifiers = if self.multiline {
                        Modifiers::COMMAND
                    } else {
                        Modifiers::NONE
                    };
                    submitted = input.consume_key(modifiers, Key::Enter);
                }
            });
            if ui.is_enabled() {
                if vim_on {
                    self.vim_keys(ui, text);
                }
                if !self.in_vim_command_mode(ui.ctx()) {
                    self.type_with_pairs(ui, text);
                }
            }
        }
        let command_mode = self.in_vim_command_mode(ui.ctx());

        let cache = &mut self.highlight_cache;
        let multiline = self.multiline;
        let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
            let text = buffer.as_str();
            if cache.as_ref().is_none_or(|(cached, _)| cached != text) {
                *cache = Some((text.to_owned(), language.highlight(text)));
            }
            let spans = cache.as_ref().map_or(&[][..], |(_, spans)| spans);
            let mut job = layout_job(ui, text, spans);
            // A single line scrolls sideways instead of wrapping.
            job.wrap.max_width = if multiline { wrap_width } else { f32::INFINITY };
            ui.fonts_mut(|fonts| fonts.layout_job(job))
        };
        let gutter = if self.line_numbers {
            gutter_width(ui, text)
        } else {
            0.0
        };
        let edit = if self.multiline {
            egui::TextEdit::multiline(text)
                .lock_focus(true)
                .desired_rows(self.rows)
        } else {
            egui::TextEdit::singleline(text)
        };
        let margin = Margin {
            left: 11 + gutter as i8,
            right: 11,
            top: 7,
            bottom: 7,
        };
        let frame = match (self.frameless, self.line_numbers) {
            // Lines up with the gutter, which starts at the caller's frame.
            (true, true) => Frame::new().inner_margin(margin),
            (true, false) => Frame::new().inner_margin(Margin::symmetric(0, 4)),
            (false, _) => frame(ui, focused).inner_margin(margin),
        };
        let mut edit = edit
            .min_size(egui::vec2(0.0, self.min_height))
            .font(TextStyle::Monospace)
            .hint_text(self.hint.clone())
            .desired_width(f32::INFINITY)
            .frame(frame)
            .layouter(&mut layouter);
        if vim_on {
            // Escape switches modes, so it mustn't take the focus away.
            let filter = edit.get_event_filter();
            edit = edit.event_filter(EventFilter {
                escape: true,
                ..filter
            });
        }
        // Normal and visual mode draw a block cursor instead.
        let caret = ui.visuals().text_cursor.stroke;
        if command_mode {
            ui.visuals_mut().text_cursor.stroke.color = Color32::TRANSPARENT;
        }

        let explicit = self.explicit;
        let accepted_word = &mut self.accepted_word;
        let mut offered = Vec::new();
        let output = CompletionPopup::new(self.id)
            .word_boundary(|c| !language.is_word_char(c))
            .align(RectAlign::BOTTOM_START)
            .max_height(280.0)
            .show(ui, edit, |query| {
                if command_mode || accepted_word.as_deref() == Some(query.word) {
                    return Vec::new();
                }
                *accepted_word = None;
                let cursor = byte_offset(query.text, query.word_range.end.0);
                offered = language.complete(query.text, cursor, explicit);
                offered
                    .iter()
                    .map(|item| {
                        Suggestion::new(item.insert.clone())
                            .content(item.label.clone())
                            .description(item.detail.clone())
                    })
                    .collect()
            });
        ui.visuals_mut().text_cursor.stroke = caret;

        if let Some(accepted) = &output.accepted
            && let Some(completion) = offered.iter().find(|c| c.insert == accepted.insert)
        {
            self.finish_completion(ui.ctx(), text, completion, language);
        }

        self.popup_open = output.is_open;
        if !output.is_open {
            self.explicit = false;
        }
        let response = output.text_edit.response.response.clone();
        if self.object {
            self.fill_empty_object(ui.ctx(), &response, text);
        }
        if vim_on && focused {
            self.vim_after(ui, text, &output.text_edit);
        }
        if self.line_numbers {
            paint_line_numbers(
                ui,
                &output.text_edit.galley,
                output.text_edit.galley_pos,
                &response,
                gutter,
            );
        }
        if focused && !self.frameless {
            paint_focus_ring(ui, response.rect);
        }
        CodeEditorOutput {
            response,
            submitted,
        }
    }

    fn fill_empty_object(&self, ctx: &egui::Context, response: &egui::Response, text: &mut String) {
        if response.gained_focus() && text.trim().is_empty() {
            *text = "{  }".to_owned();
            let mut state = TextEditState::load(ctx, self.id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(2))));
            state.store(ctx, self.id);
            ctx.request_repaint();
        } else if response.lost_focus() && is_empty_object(text) {
            text.clear();
        }
    }

    /// Moves the cursor into a snippet such as `$in: []` and removes text the
    /// completion replaces after the cursor, such as a closing quote.
    fn finish_completion(
        &mut self,
        ctx: &egui::Context,
        text: &mut String,
        completion: &Completion,
        language: &dyn Language,
    ) {
        let Some(mut state) = TextEditState::load(ctx, self.id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let end = range.primary.index.0;
        let end_byte = byte_offset(text, end);
        let delete_end = (end_byte + completion.delete_after).min(text.len());
        if text.is_char_boundary(delete_end) {
            text.replace_range(end_byte..delete_end, "");
        }
        let insert_chars = completion.insert.chars().count();
        let cursor_chars = completion
            .insert
            .get(..completion.cursor)
            .map_or(insert_chars, |before| before.chars().count());
        let cursor = end - insert_chars + cursor_chars;
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
        state.store(ctx, self.id);

        let before = &text[..byte_offset(text, cursor)];
        let word_start = before
            .char_indices()
            .rev()
            .find(|&(_, c)| !language.is_word_char(c))
            .map_or(0, |(index, c)| index + c.len_utf8());
        let word = &before[word_start..];
        self.accepted_word = (!word.is_empty()).then(|| word.to_owned());
    }

    /// In vim's normal or visual mode, where keys are commands.
    fn in_vim_command_mode(&self, ctx: &egui::Context) -> bool {
        self.vim_mode(ctx)
            .is_some_and(|mode| mode != vim::Mode::Insert)
    }

    /// Starts insert mode when an [`VimFocus::Insert`] editor gains focus,
    /// and hands back the keys held from the last frame.
    fn vim_focus(&mut self, ui: &egui::Ui, text: &str, focused: bool) {
        let id = self.id;
        let Some(state) = &mut self.vim else {
            return;
        };
        let gained = focused && !state.had_focus;
        state.had_focus = focused;
        if !focused {
            state.deferred.clear();
            return;
        }
        if gained && state.focus == VimFocus::Insert {
            state.vim.start_insert(text, cursor_of(ui.ctx(), id, text));
        }
        if !state.deferred.is_empty() {
            let deferred = std::mem::take(&mut state.deferred);
            ui.input_mut(|input| {
                input.events.splice(0..0, deferred);
            });
        }
    }

    /// Runs this frame's keys through vim. In insert mode only Escape and
    /// undo are vim's; everything else is typed as usual.
    fn vim_keys(&mut self, ui: &egui::Ui, text: &mut String) {
        let (id, multiline, popup_open) = (self.id, self.multiline, self.popup_open);
        let Some(state) = &mut self.vim else {
            return;
        };
        let ctx = ui.ctx();
        let mut text_state = TextEditState::load(ctx, id).unwrap_or_default();
        let range = text_state.cursor.char_range();
        let primary = range.map_or(text.chars().count(), |range| range.primary.index.0);
        let was_inserting = state.vim.mode() == vim::Mode::Insert;
        if !was_inserting
            && let Some(last) = &state.last_text
            && last != text
        {
            state.vim.changed_outside(last, primary);
        }
        let shown = range.map(|range| sorted(&range));
        let mut cursor = if state.vim.mode().is_visual() && shown == state.shown {
            state.cursor
        } else {
            primary
        };
        let mut register = ctx
            .data(|data| data.get_temp::<Register>(register_id()))
            .unwrap_or_default();
        let mut handled = false;
        let mut press = |vim: &mut Vim, key: vim::Key| {
            let mut edit = Edit {
                text: &mut *text,
                cursor,
                register: &mut register,
            };
            vim.key(key, &mut edit);
            cursor = edit.cursor;
            handled = true;
        };
        ui.input_mut(|input| {
            let mut kept = Vec::new();
            for event in std::mem::take(&mut input.events) {
                let switched = |vim: &Vim| (vim.mode() == vim::Mode::Insert) != was_inserting;
                if switched(&state.vim) && is_keyboard(&event) {
                    state.deferred.push(event);
                    continue;
                }
                if let Event::Text(typed) = &event
                    && !was_inserting
                {
                    for (index, c) in typed.char_indices() {
                        if switched(&state.vim) {
                            state.deferred.push(Event::Text(typed[index..].to_owned()));
                            break;
                        }
                        press(&mut state.vim, vim::Key::Char(c));
                    }
                    continue;
                }
                match vim_event(&event, state.vim.mode(), multiline, popup_open) {
                    Some(VimEvent::Key(key)) => press(&mut state.vim, key),
                    Some(VimEvent::Drop) => {}
                    None => kept.push(event),
                }
            }
            input.events = kept;
        });
        state.last_text = Some(text.clone());
        if !handled {
            return;
        }
        ctx.data_mut(|data| data.insert_temp(register_id(), register));
        let selection = state.vim.selection(text, cursor);
        let new_range = match &selection {
            Some(selection) => {
                CCursorRange::two(CCursor::new(selection.start), CCursor::new(selection.end))
            }
            None => CCursorRange::one(CCursor::new(cursor)),
        };
        state.cursor = cursor;
        state.shown = selection;
        text_state.cursor.set_char_range(Some(new_range));
        text_state.store(ctx, id);
        if !state.deferred.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Follows the mouse: a drag or double click starts visual mode, a click
    /// ends it. Then draws the block cursor of normal and visual mode.
    fn vim_after(&mut self, ui: &egui::Ui, text: &str, output: &TextEditOutput) {
        let id = self.id;
        let Some(state) = &mut self.vim else {
            return;
        };
        let mode = state.vim.mode();
        if mode == vim::Mode::Insert {
            return;
        }
        let ctx = ui.ctx();
        let Some(mut text_state) = TextEditState::load(ctx, id) else {
            return;
        };
        let Some(range) = text_state.cursor.char_range() else {
            return;
        };
        let (primary, secondary) = (range.primary.index.0, range.secondary.index.0);
        let shown = sorted(&range);
        let cursor = if mode.is_visual() && state.shown.as_ref() == Some(&shown) {
            state.cursor
        } else if primary != secondary {
            let (anchor, cursor) = if primary > secondary {
                (secondary, primary - 1)
            } else {
                (secondary - 1, primary)
            };
            state.vim.select(anchor);
            state.cursor = cursor;
            state.shown = Some(shown);
            cursor
        } else {
            if mode.is_visual() {
                state.vim.normal();
            }
            state.shown = None;
            let cursor = state.vim.clamp(text, primary);
            if cursor != primary {
                text_state
                    .cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
                text_state.store(ctx, id);
                ctx.request_repaint();
            }
            cursor
        };
        paint_block_cursor(ui, output, text, cursor);
    }

    /// Types the text events into `text` itself, so brackets and quotes can
    /// close themselves. Also deletes empty pairs with Backspace and indents
    /// new lines in multiline editors.
    fn type_with_pairs(&self, ui: &egui::Ui, text: &mut String) {
        let Some(mut state) = TextEditState::load(ui.ctx(), self.id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (start, end) = (range.primary.index.0, range.secondary.index.0);
        let mut typing = Typing {
            text,
            start: start.min(end),
            end: start.max(end),
        };
        let multiline = self.multiline;
        let popup_open = self.popup_open;
        let mut changed = false;
        ui.input_mut(|input| {
            input.events.retain(|event| {
                let handled = match event {
                    Event::Text(typed) => {
                        for c in typed.chars().filter(|&c| multiline || c != '\n') {
                            typing.type_char(c);
                        }
                        true
                    }
                    Event::Key {
                        key: Key::Backspace,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.is_none() => typing.delete_pair(),
                    Event::Key {
                        key: Key::Enter,
                        pressed: true,
                        modifiers,
                        ..
                    } if multiline && !popup_open && modifiers.is_none() => {
                        typing.new_line();
                        true
                    }
                    _ => false,
                };
                changed |= handled;
                !handled
            });
        });
        if changed {
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(typing.start))));
            state.store(ui.ctx(), self.id);
        }
    }
}

/// Edits text at a cursor, both as char indices.
struct Typing<'a> {
    text: &'a mut String,
    /// Selection start; the cursor once nothing is selected.
    start: usize,
    end: usize,
}

const OPENERS: [(char, char); 5] = [('{', '}'), ('[', ']'), ('(', ')'), ('\'', '\''), ('"', '"')];

impl Typing<'_> {
    fn char_at(&self, index: usize) -> Option<char> {
        self.text.chars().nth(index)
    }

    fn next(&self) -> Option<char> {
        self.char_at(self.end)
    }

    fn previous(&self) -> Option<char> {
        self.start
            .checked_sub(1)
            .and_then(|index| self.char_at(index))
    }

    fn delete_selection(&mut self) {
        if self.start != self.end {
            let range = byte_offset(self.text, self.start)..byte_offset(self.text, self.end);
            self.text.replace_range(range, "");
            self.end = self.start;
        }
    }

    fn insert(&mut self, insert: &str) {
        self.delete_selection();
        self.text
            .insert_str(byte_offset(self.text, self.start), insert);
        self.start += insert.chars().count();
        self.end = self.start;
    }

    fn type_char(&mut self, c: char) {
        self.delete_selection();
        let closes = matches!(c, '}' | ']' | ')' | '\'' | '"');
        if closes && self.next() == Some(c) {
            // Step over the closer that was typed automatically.
            self.start += 1;
            self.end = self.start;
            return;
        }
        let closer = OPENERS
            .iter()
            .find(|(open, _)| *open == c)
            .map(|&(_, close)| close);
        match closer {
            Some(closer) if self.should_pair(c) => {
                self.insert(&format!("{c}{closer}"));
                self.start -= 1;
                self.end = self.start;
            }
            _ => self.insert(&c.to_string()),
        }
    }

    /// Pairs only before whitespace, a closer or a separator, and quotes
    /// only after a non-word character, so `don't` stays as typed.
    fn should_pair(&self, c: char) -> bool {
        let next_ok = self
            .next()
            .is_none_or(|next| next.is_whitespace() || matches!(next, '}' | ']' | ')' | ',' | ':'));
        let after_word = self.previous().is_some_and(|previous| {
            previous.is_alphanumeric() || matches!(previous, '_' | '$' | '.' | '\\')
        });
        match c {
            '\'' | '"' => next_ok && !after_word,
            _ => next_ok,
        }
    }

    /// Backspace between an empty pair such as `{}` deletes both.
    fn delete_pair(&mut self) -> bool {
        if self.start != self.end {
            return false;
        }
        let (Some(previous), Some(next)) = (self.previous(), self.next()) else {
            return false;
        };
        if !OPENERS.contains(&(previous, next)) {
            return false;
        }
        let range = byte_offset(self.text, self.start - 1)..byte_offset(self.text, self.start + 1);
        self.text.replace_range(range, "");
        self.start -= 1;
        self.end = self.start;
        true
    }

    /// Keeps the line's indentation, and between `{}` or `[]` opens an
    /// indented line with the closer on its own line.
    fn new_line(&mut self) {
        self.delete_selection();
        let cursor = byte_offset(self.text, self.start);
        let line_start = self.text[..cursor].rfind('\n').map_or(0, |index| index + 1);
        let indent: String = self.text[line_start..cursor]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let between = matches!(
            (self.previous(), self.next()),
            (Some('{'), Some('}')) | (Some('['), Some(']'))
        );
        if between {
            self.insert(&format!("\n{indent}  "));
            let cursor = self.start;
            self.insert(&format!("\n{indent}"));
            self.start = cursor;
            self.end = cursor;
        } else {
            self.insert(&format!("\n{indent}"));
        }
    }
}

/// `{ }` and nothing else, which is what an object editor holds after it is
/// focused empty.
/// The framed editor's look, without its inner margin: the input fill and
/// a border that turns accent while `focused`.
pub fn frame(ui: &egui::Ui, focused: bool) -> Frame {
    let palette = theme::palette(ui);
    Frame::new()
        .fill(ui.visuals().text_edit_bg_color())
        .corner_radius(CornerRadius::same(9))
        .stroke(Stroke::new(
            1.0,
            if focused {
                palette.accent
            } else {
                palette.line_strong
            },
        ))
}

/// Turns vim keys on or off for the editors built with [`CodeEditor::vim`].
pub fn set_vim(ctx: &egui::Context, enabled: bool) {
    ctx.data_mut(|data| data.insert_temp(vim_id(), enabled));
}

fn vim_enabled(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(vim_id()))
        .unwrap_or(false)
}

fn vim_id() -> egui::Id {
    egui::Id::unique("vim_keys")
}

/// One register for every editor, so a yank in the shell puts in the filter.
fn register_id() -> egui::Id {
    egui::Id::unique("vim_register")
}

fn cursor_of(ctx: &egui::Context, id: egui::Id, text: &str) -> usize {
    TextEditState::load(ctx, id)
        .and_then(|state| state.cursor.char_range())
        .map_or(text.chars().count(), |range| range.primary.index.0)
}

fn sorted(range: &CCursorRange) -> Range<usize> {
    let range = range.as_sorted_char_range();
    range.start.0..range.end.0
}

fn is_keyboard(event: &Event) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Key { .. }
            | Event::Paste(_)
            | Event::Copy
            | Event::Cut
            | Event::Ime(_)
    )
}

enum VimEvent {
    Key(vim::Key),
    /// Swallowed, e.g. Tab in normal mode.
    Drop,
}

/// What vim does with an event other than typed text; `None` leaves it to
/// the text edit.
fn vim_event(
    event: &Event,
    mode: vim::Mode,
    multiline: bool,
    popup_open: bool,
) -> Option<VimEvent> {
    let Event::Key {
        key,
        pressed,
        modifiers,
        ..
    } = event
    else {
        let ime = matches!(event, Event::Ime(_));
        return (ime && mode != vim::Mode::Insert).then_some(VimEvent::Drop);
    };
    if !pressed {
        return None;
    }
    if let Some(key) = undo_key(*key, *modifiers) {
        return Some(VimEvent::Key(key));
    }
    if mode == vim::Mode::Insert {
        let escape = *key == Key::Escape && modifiers.is_none() && !popup_open;
        return escape.then_some(VimEvent::Key(vim::Key::Escape));
    }
    if modifiers.ctrl && !modifiers.shift && *key == Key::R {
        return Some(VimEvent::Key(vim::Key::Redo));
    }
    // Ctrl+A, Shift+arrows and the like select, and a selection starts visual
    // mode. The text edit's editing combos stay out of normal mode.
    if modifiers.command || modifiers.ctrl || modifiers.alt {
        let edits = matches!(
            key,
            Key::Backspace | Key::Delete | Key::H | Key::K | Key::U | Key::W
        );
        return edits.then_some(VimEvent::Drop);
    }
    if modifiers.shift && is_move(*key) {
        return None;
    }
    let vim_key = match key {
        Key::Escape => vim::Key::Escape,
        Key::Enter => vim::Key::Enter,
        Key::Backspace => vim::Key::Backspace,
        Key::Delete => vim::Key::Delete,
        Key::ArrowLeft => vim::Key::Left,
        Key::ArrowRight => vim::Key::Right,
        Key::ArrowUp => vim::Key::Up,
        Key::ArrowDown => vim::Key::Down,
        Key::Home => vim::Key::Home,
        Key::End => vim::Key::End,
        Key::Tab if !multiline => return None,
        _ => return Some(VimEvent::Drop),
    };
    Some(VimEvent::Key(vim_key))
}

/// Ctrl+Z, and Ctrl+Shift+Z or Ctrl+Y; Cmd on a Mac.
fn undo_key(key: Key, modifiers: Modifiers) -> Option<vim::Key> {
    if !modifiers.command || modifiers.alt {
        return None;
    }
    match key {
        Key::Z if modifiers.shift => Some(vim::Key::Redo),
        Key::Z => Some(vim::Key::Undo),
        Key::Y => Some(vim::Key::Redo),
        _ => None,
    }
}

fn is_move(key: Key) -> bool {
    matches!(
        key,
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End
    )
}

/// A block over the char at `at`, with the char drawn on it.
fn paint_block_cursor(ui: &egui::Ui, output: &TextEditOutput, text: &str, at: usize) {
    let galley = &output.galley;
    let origin = output.galley_pos - egui::vec2(galley.rect.left(), 0.0);
    let spot = galley
        .pos_from_cursor(CCursor::new(at))
        .translate(origin.to_vec2());
    let under = text.chars().nth(at).filter(|&c| c != '\n');
    let font = TextStyle::Monospace.resolve(ui.style());
    let width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, under.unwrap_or(' ')));
    let rect = egui::Rect::from_min_size(spot.min, egui::vec2(width.max(2.0), spot.height()));
    let painter = ui.painter_at(output.text_clip_rect);
    painter.rect_filled(rect, 1.0, ui.visuals().text_cursor.stroke.color);
    if let Some(c) = under {
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            c,
            font,
            ui.visuals().text_edit_bg_color(),
        );
    }
}

/// The focus ring around a framed editor at `rect`.
pub fn paint_focus_ring(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().rect_stroke(
        rect.expand(1.0),
        CornerRadius::same(10),
        Stroke::new(3.0, theme::palette(ui).focus),
        egui::StrokeKind::Outside,
    );
}

pub fn is_empty_object(text: &str) -> bool {
    text.chars().filter(|c| !c.is_whitespace()).eq("{}".chars())
}

fn byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(offset, _)| offset)
}

fn gutter_width(ui: &egui::Ui, text: &str) -> f32 {
    let lines = text.lines().count().max(1) + usize::from(text.ends_with('\n'));
    let digits = lines.to_string().len().max(2);
    let font = TextStyle::Monospace.resolve(ui.style());
    let digit = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
    (digits as f32 * digit + 22.0).round()
}

fn paint_line_numbers(
    ui: &egui::Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    response: &egui::Response,
    gutter: f32,
) {
    let palette = theme::palette(ui);
    let rect = response.rect;
    let gutter_right = rect.left() + gutter;
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(gutter_right, rect.bottom())),
        CornerRadius {
            nw: 9,
            sw: 9,
            ..CornerRadius::ZERO
        },
        palette.surface,
    );
    painter.vline(gutter_right, rect.y_range(), Stroke::new(1.0, palette.line));
    let font = FontId::monospace(TextStyle::Monospace.resolve(ui.style()).size);
    let mut line = 1;
    let mut starts_line = true;
    for row in &galley.rows {
        if starts_line {
            let y = galley_pos.y + row.pos.y + row.row.size.y / 2.0;
            painter.text(
                egui::pos2(gutter_right - 10.0, y),
                Align2::RIGHT_CENTER,
                line.to_string(),
                font.clone(),
                palette.subtle,
            );
            line += 1;
        }
        starts_line = row.ends_with_newline;
    }
}

/// Monospace text colored by `spans`.
pub fn layout_job(ui: &egui::Ui, text: &str, spans: &[Span]) -> LayoutJob {
    let font_id = TextStyle::Monospace.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let palette = theme::palette(ui);
    let mut job = LayoutJob::default();
    let mut push = |range: Range<usize>, kind: Option<Highlight>| {
        let color = kind.map_or(text_color, |kind| theme::syntax(kind, &palette));
        let format = TextFormat::simple(font_id.clone(), color);
        job.append(&text[range], 0.0, format);
    };

    let mut pos = 0;
    for (range, kind) in spans {
        let valid = range.start >= pos
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        if !valid {
            continue;
        }
        if pos < range.start {
            push(pos..range.start, None);
        }
        push(range.clone(), Some(*kind));
        pos = range.end;
    }
    // Always push, even when empty, so an empty editor still has a font and row height.
    push(pos..text.len(), None);
    job
}

pub fn color(kind: Highlight, ui: &egui::Ui) -> egui::Color32 {
    theme::syntax(kind, &theme::palette(ui))
}

#[cfg(test)]
mod tests {
    use eframe::egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use nordpil_query::{Mode, ValueHint};

    use crate::query_language::QueryLanguage;

    struct State {
        text: String,
        editor: CodeEditor,
        submitted: bool,
        values: Vec<ValueHint>,
    }

    fn harness(multiline: bool) -> Harness<'static, State> {
        harness_with(CodeEditor::new(egui::Id::unique("editor"), multiline))
    }

    fn harness_with(editor: CodeEditor) -> Harness<'static, State> {
        let state = State {
            text: String::new(),
            editor,
            submitted: false,
            values: nordpil_query::value_hints(&[
                bson::doc! { "status": "shipped" },
                bson::doc! { "status": "pending" },
            ]),
        };
        Harness::new_ui_state(
            |ui, state: &mut State| {
                let label = ui.label("Query");
                let language = QueryLanguage::new(Mode::Filter, &[], &state.values);
                let output = state.editor.show(ui, &mut state.text, &language);
                output.response.labelled_by(label.id);
                state.submitted |= output.submitted;
            },
            state,
        )
    }

    fn type_text(harness: &mut Harness<'_, State>, text: &str) {
        let role = if harness.state().editor.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        harness.get_by_role(role).focus();
        harness.run();
        harness.get_by_role(role).type_text(text);
        harness.run();
    }

    /// The text with `|` at the cursor.
    fn with_cursor(harness: &Harness<'_, State>) -> String {
        let state = TextEditState::load(&harness.ctx, harness.state().editor.id).unwrap();
        let cursor = state.cursor.char_range().unwrap().primary.index.0;
        let mut text = harness.state().text.clone();
        text.insert(byte_offset(&text, cursor), '|');
        text
    }

    #[test]
    fn arrow_down_and_enter_accept_second_completion() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ age: { $gt");
        assert!(harness.state().editor.popup_open);

        harness.key_press(Key::ArrowDown);
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();

        assert_eq!(with_cursor(&harness), "{ age: { $gte: |}}");
        assert!(!harness.state().editor.popup_open);
        assert!(!harness.state().submitted);
    }

    #[test]
    fn object_editor_puts_in_braces_on_focus_and_removes_them_on_leave() {
        let mut harness = harness_with(CodeEditor::new(egui::Id::unique("editor"), false).object());
        harness.get_by_role(Role::TextInput).focus();
        harness.run();
        assert_eq!(with_cursor(&harness), "{ | }");

        harness.key_press(Key::Escape);
        harness.run();
        assert_eq!(harness.state().text, "");

        type_text(&mut harness, "a: 1");
        harness.key_press(Key::Escape);
        harness.run();
        assert_eq!(harness.state().text, "{ a: 1 }");
    }

    #[test]
    fn enter_submits_when_popup_is_closed() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ a: 1 }");
        assert_eq!(harness.state().text, "{ a: 1 }");
        assert!(!harness.state().editor.popup_open);

        harness.key_press(Key::Enter);
        harness.run();

        assert!(harness.state().submitted);
    }

    #[test]
    fn closes_brackets_and_quotes_and_steps_over_closers() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ a: [");
        assert_eq!(with_cursor(&harness), "{ a: [|]}");

        type_text(&mut harness, "'x'] }");
        assert_eq!(with_cursor(&harness), "{ a: ['x'] }|");
    }

    #[test]
    fn backspace_deletes_an_empty_pair() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ a: (");
        harness.key_press(Key::Backspace);
        harness.run();

        assert_eq!(with_cursor(&harness), "{ a: |}");
    }

    #[test]
    fn accepting_an_operator_puts_the_cursor_inside_its_brackets() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ age: { $in");
        harness.key_press(Key::Enter);
        harness.run();

        assert_eq!(with_cursor(&harness), "{ age: { $in: [|]}}");
    }

    #[test]
    fn accepting_a_value_inside_quotes_reuses_the_closing_quote() {
        let mut harness = harness(false);
        type_text(&mut harness, "{ status: '");
        assert!(harness.state().editor.popup_open);
        harness.key_press(Key::Enter);
        harness.run();

        assert_eq!(with_cursor(&harness), "{ status: 'shipped'|}");
        assert!(!harness.state().editor.popup_open);
    }

    #[test]
    fn enter_between_braces_opens_an_indented_line() {
        let mut harness = harness(true);
        type_text(&mut harness, "{");
        harness.key_press(Key::Enter);
        harness.run();

        assert_eq!(with_cursor(&harness), "{\n  |\n}");
    }

    // Vim keys -----------------------------------------------------------

    fn keep(multiline: bool) -> CodeEditor {
        CodeEditor::new(egui::Id::unique("editor"), multiline).vim(VimFocus::Keep)
    }

    fn insert(multiline: bool) -> CodeEditor {
        CodeEditor::new(egui::Id::unique("editor"), multiline).vim(VimFocus::Insert)
    }

    /// An editor holding `text`, with vim keys on, focused.
    fn vim_harness(text: &str, editor: CodeEditor) -> Harness<'static, State> {
        let mut harness = harness_with(editor);
        set_vim(&harness.ctx, true);
        harness.state_mut().text = text.to_owned();
        harness.run();
        focus(&mut harness);
        harness
    }

    fn focus(harness: &mut Harness<'_, State>) {
        let role = if harness.state().editor.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        harness.get_by_role(role).focus();
        harness.run();
    }

    /// Types each char as its own frame, like a person would.
    fn press(harness: &mut Harness<'_, State>, keys: &str) {
        for c in keys.chars() {
            harness.event(Event::Text(c.to_string()));
            harness.run();
        }
    }

    fn key(harness: &mut Harness<'_, State>, key: Key) {
        harness.key_press(key);
        harness.run();
    }

    fn mode(harness: &Harness<'_, State>) -> Option<vim::Mode> {
        harness.state().editor.vim_mode(&harness.ctx)
    }

    fn has_focus(harness: &Harness<'_, State>) -> bool {
        let id = harness.state().editor.id;
        harness.ctx.memory(|memory| memory.has_focus(id))
    }

    #[test]
    fn vim_keys_off_types_as_before() {
        let mut harness = harness_with(keep(false));
        type_text(&mut harness, "w");
        assert_eq!(harness.state().text, "w");
        assert_eq!(mode(&harness), None);
    }

    #[test]
    fn normal_mode_keys_are_commands() {
        let mut harness = vim_harness("foo bar", keep(false));
        press(&mut harness, "0wx");
        assert_eq!(with_cursor(&harness), "foo |ar");
    }

    #[test]
    fn an_operator_waits_for_its_motion_across_frames() {
        let mut harness = vim_harness("foo bar", keep(false));
        press(&mut harness, "0dw");
        assert_eq!(with_cursor(&harness), "|bar");
    }

    #[test]
    fn normal_mode_keeps_the_cursor_on_a_char() {
        let harness = vim_harness("ab", keep(false));
        assert_eq!(with_cursor(&harness), "a|b");
    }

    #[test]
    fn i_types_and_escape_goes_back_to_normal() {
        let mut harness = vim_harness("ab", keep(false));
        press(&mut harness, "0ix");
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
        key(&mut harness, Key::Escape);
        assert_eq!(with_cursor(&harness), "|xab");
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
    }

    #[test]
    fn insert_mode_keeps_self_closing_brackets() {
        let mut harness = vim_harness("", keep(false));
        press(&mut harness, "i{");
        assert_eq!(with_cursor(&harness), "{|}");
    }

    #[test]
    fn a_filter_style_editor_starts_in_insert_mode() {
        let mut harness = vim_harness("", insert(false));
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
        press(&mut harness, "x");
        assert_eq!(harness.state().text, "x");
    }

    #[test]
    fn a_filter_style_editor_is_back_in_insert_mode_on_focus() {
        let mut harness = vim_harness("ab", insert(false));
        key(&mut harness, Key::Escape);
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
        let id = harness.state().editor.id;
        harness.ctx.memory_mut(|memory| memory.surrender_focus(id));
        harness.run();
        focus(&mut harness);
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
    }

    #[test]
    fn a_shell_style_editor_keeps_its_mode_across_focus() {
        let mut harness = vim_harness("ab", keep(true));
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
        press(&mut harness, "i");
        let id = harness.state().editor.id;
        harness.ctx.memory_mut(|memory| memory.surrender_focus(id));
        harness.run();
        focus(&mut harness);
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
    }

    #[test]
    fn escape_in_normal_mode_keeps_the_focus() {
        let mut harness = vim_harness("ab", keep(false));
        key(&mut harness, Key::Escape);
        assert!(has_focus(&harness));
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
    }

    #[test]
    fn escape_closes_the_popup_before_leaving_insert_mode() {
        let mut harness = vim_harness("", insert(false));
        press(&mut harness, "{ status: '");
        assert!(harness.state().editor.popup_open);

        key(&mut harness, Key::Escape);
        assert!(!harness.state().editor.popup_open);
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));

        key(&mut harness, Key::Escape);
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
        assert!(has_focus(&harness));
    }

    #[test]
    fn the_popup_stays_closed_in_normal_mode() {
        let mut harness = vim_harness("", insert(false));
        press(&mut harness, "{ status: '");
        key(&mut harness, Key::Escape);
        key(&mut harness, Key::Escape);
        press(&mut harness, "hl");
        assert!(!harness.state().editor.popup_open);
    }

    #[test]
    fn enter_submits_a_single_line_in_normal_mode() {
        let mut harness = vim_harness("{ a: 1 }", keep(false));
        key(&mut harness, Key::Enter);
        assert!(harness.state().submitted);
        assert_eq!(harness.state().text, "{ a: 1 }");
    }

    #[test]
    fn enter_moves_down_in_multiline_normal_mode() {
        let mut harness = vim_harness("a\n  b", keep(true));
        press(&mut harness, "gg");
        key(&mut harness, Key::Enter);
        assert_eq!(with_cursor(&harness), "a\n  |b");
        assert!(!harness.state().submitted);
    }

    #[test]
    fn editing_combos_edit_nothing_in_normal_mode() {
        let mut harness = vim_harness("foo bar", keep(false));
        harness.key_press_modifiers(Modifiers::CTRL, Key::U);
        harness.key_press_modifiers(Modifiers::ALT, Key::Backspace);
        harness.run();
        assert_eq!(harness.state().text, "foo bar");
    }

    #[test]
    fn tab_in_normal_mode_types_nothing() {
        let mut harness = vim_harness("ab", keep(true));
        key(&mut harness, Key::Tab);
        assert_eq!(harness.state().text, "ab");
    }

    #[test]
    fn keys_after_a_mode_switch_wait_for_the_next_frame() {
        let mut harness = vim_harness("ab", keep(false));
        harness.event(Event::Text("0ix".to_owned()));
        harness.run();
        assert_eq!(harness.state().text, "xab");
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
    }

    #[test]
    fn ctrl_z_undoes_and_ctrl_r_redoes() {
        let mut harness = vim_harness("ab", keep(false));
        press(&mut harness, "x");
        assert_eq!(harness.state().text, "a");
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(harness.state().text, "ab");
        harness.key_press_modifiers(Modifiers::CTRL, Key::R);
        harness.run();
        assert_eq!(harness.state().text, "a");
    }

    #[test]
    fn ctrl_z_in_insert_mode_undoes_the_typing() {
        let mut harness = vim_harness("ab", keep(false));
        press(&mut harness, "Axy");
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run();
        assert_eq!(harness.state().text, "ab");
        assert_eq!(mode(&harness), Some(vim::Mode::Insert));
    }

    #[test]
    fn a_paste_in_normal_mode_undoes_as_one_step() {
        let mut harness = vim_harness("ab", keep(false));
        harness.event(Event::Paste("xy".to_owned()));
        harness.run();
        assert_eq!(harness.state().text, "axyb");
        press(&mut harness, "u");
        assert_eq!(harness.state().text, "ab");
    }

    #[test]
    fn a_mouse_selection_starts_visual_mode() {
        let mut harness = vim_harness("abc", keep(false));
        let id = harness.state().editor.id;
        let mut state = TextEditState::load(&harness.ctx, id).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(2))));
        state.store(&harness.ctx, id);
        harness.run();
        assert_eq!(mode(&harness), Some(vim::Mode::Visual));

        press(&mut harness, "d");
        assert_eq!(harness.state().text, "c");
    }

    #[test]
    fn a_click_ends_visual_mode() {
        let mut harness = vim_harness("abc", keep(false));
        press(&mut harness, "0vl");
        assert_eq!(mode(&harness), Some(vim::Mode::Visual));
        let id = harness.state().editor.id;
        let mut state = TextEditState::load(&harness.ctx, id).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(2))));
        state.store(&harness.ctx, id);
        harness.run();
        assert_eq!(mode(&harness), Some(vim::Mode::Normal));
        assert_eq!(with_cursor(&harness), "ab|c");
    }

    #[test]
    fn visual_mode_shows_as_the_selection() {
        let mut harness = vim_harness("abcd", keep(false));
        press(&mut harness, "0lvl");
        let range = harness
            .state()
            .editor
            .selection(&harness.ctx, &harness.state().text);
        assert_eq!(range, Some(1..3));
    }

    #[test]
    fn the_register_is_shared_between_editors() {
        struct Two {
            shell: (String, CodeEditor),
            filter: (String, CodeEditor),
        }
        let mut harness = Harness::new_ui_state(
            |ui, two: &mut Two| {
                let language = QueryLanguage::new(Mode::Filter, &[], &[]);
                for (text, editor) in [&mut two.shell, &mut two.filter] {
                    let label = ui.label("Editor");
                    editor
                        .show(ui, text, &language)
                        .response
                        .labelled_by(label.id);
                }
            },
            Two {
                shell: (
                    "db.users".to_owned(),
                    CodeEditor::new(egui::Id::unique("shell"), true).vim(VimFocus::Keep),
                ),
                filter: (
                    "{ }".to_owned(),
                    CodeEditor::new(egui::Id::unique("filter"), false).vim(VimFocus::Keep),
                ),
            },
        );
        set_vim(&harness.ctx, true);
        harness.get_by_role(Role::MultilineTextInput).focus();
        harness.run();
        for c in "0yiw".chars() {
            harness.event(Event::Text(c.to_string()));
            harness.run();
        }
        harness.get_by_role(Role::TextInput).focus();
        harness.run();
        harness.event(Event::Text("P".to_owned()));
        harness.run();
        assert_eq!(harness.state().filter.0, "{ db}");
    }
}
