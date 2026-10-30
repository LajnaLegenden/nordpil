//! A text editor for queries and documents: syntax highlighting, a
//! keyboard-driven completion popup, and brackets and quotes that close
//! themselves.

use std::ops::Range;

use eframe::egui::text::{CCursor, CCursorRange, LayoutJob};
use eframe::egui::text_edit::TextEditState;
use eframe::egui::{
    self, Align2, CompletionPopup, CornerRadius, Event, FontId, Frame, Key, Margin, Modifiers,
    RectAlign, Stroke, Suggestion, TextFormat, TextStyle,
};
use nordpil_query::Highlight;

use crate::theme;

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
    line_numbers: bool,
    /// Drawn without its own frame, inside one the caller draws.
    frameless: bool,
    popup_open: bool,
    /// Set by Ctrl+Space; cleared when the popup closes.
    explicit: bool,
    /// The word a completion just inserted, so the popup doesn't reopen for it.
    accepted_word: Option<String>,
    highlight_cache: Option<(String, Vec<Span>)>,
}

impl CodeEditor {
    pub fn new(id: egui::Id, multiline: bool) -> Self {
        Self {
            id,
            multiline,
            hint: String::new(),
            rows: 4,
            line_numbers: false,
            frameless: false,
            popup_open: false,
            explicit: false,
            accepted_word: None,
            highlight_cache: None,
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

    pub fn line_numbers(mut self) -> Self {
        self.line_numbers = true;
        self
    }

    pub fn frameless(mut self) -> Self {
        self.frameless = true;
        self
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

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut String,
        language: &dyn Language,
    ) -> CodeEditorOutput {
        let focused = ui.memory(|m| m.has_focus(self.id));
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
                self.type_with_pairs(ui, text);
            }
        }

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
        let palette = theme::palette(ui);
        let frame = if self.frameless {
            Frame::new().inner_margin(Margin {
                left: gutter as i8,
                ..Margin::symmetric(0, 4)
            })
        } else {
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
                .inner_margin(Margin {
                    left: 11 + gutter as i8,
                    right: 11,
                    top: 7,
                    bottom: 7,
                })
        };
        let edit = edit
            .font(TextStyle::Monospace)
            .hint_text(self.hint.clone())
            .desired_width(f32::INFINITY)
            .frame(frame)
            .layouter(&mut layouter);

        let explicit = self.explicit;
        let accepted_word = &mut self.accepted_word;
        let mut offered = Vec::new();
        let output = CompletionPopup::new(self.id)
            .word_boundary(|c| !language.is_word_char(c))
            .align(RectAlign::BOTTOM_START)
            .max_height(280.0)
            .show(ui, edit, |query| {
                if accepted_word.as_deref() == Some(query.word) {
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

        if let Some(accepted) = &output.accepted
            && let Some(completion) = offered.iter().find(|c| c.insert == accepted.insert)
        {
            self.finish_completion(ui.ctx(), text, completion, language);
        }

        self.popup_open = output.is_open;
        if !output.is_open {
            self.explicit = false;
        }
        let response = output.text_edit.response.response;
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
            ui.painter().rect_stroke(
                response.rect.expand(1.0),
                CornerRadius::same(10),
                Stroke::new(3.0, palette.focus),
                egui::StrokeKind::Outside,
            );
        }
        CodeEditorOutput {
            response,
            submitted,
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
    let visuals = ui.visuals();
    let mut job = LayoutJob::default();
    let mut push = |range: Range<usize>, kind: Option<Highlight>| {
        let color = kind.map_or(visuals.text_color(), |kind| color(kind, visuals));
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

pub fn color(kind: Highlight, visuals: &egui::Visuals) -> egui::Color32 {
    theme::syntax(kind, visuals)
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
        let state = State {
            text: String::new(),
            editor: CodeEditor::new(egui::Id::unique("editor"), multiline),
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
}
