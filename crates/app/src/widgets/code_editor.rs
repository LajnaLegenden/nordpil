//! A text editor for queries and documents: syntax highlighting plus a
//! keyboard-driven completion popup.

use std::ops::Range;

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CompletionPopup, Key, Modifiers, RectAlign, Suggestion, TextFormat, TextStyle,
};

/// Highlight classes the editor knows how to color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    Plain,
    Key,
    String,
    Number,
    Operator,
    Function,
    Keyword,
    Regex,
    Punctuation,
}

/// Highlighted byte range of the editor text.
pub type Span = (Range<usize>, Highlight);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    /// Replaces the word before the cursor. End it with a non-word character
    /// (e.g. `"$gt: "`), or the popup reopens for the inserted word.
    pub insert: String,
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
    popup_open: bool,
    /// Set by Ctrl+Space; cleared when the popup closes.
    explicit: bool,
    highlight_cache: Option<(String, Vec<Span>)>,
}

impl CodeEditor {
    pub fn new(id: egui::Id, multiline: bool) -> Self {
        Self {
            id,
            multiline,
            hint: String::new(),
            popup_open: false,
            explicit: false,
            highlight_cache: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut String,
        language: &dyn Language,
    ) -> CodeEditorOutput {
        let mut submitted = false;
        if ui.memory(|m| m.has_focus(self.id)) {
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
        }

        let cache = &mut self.highlight_cache;
        let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
            let text = buffer.as_str();
            if cache.as_ref().is_none_or(|(cached, _)| cached != text) {
                *cache = Some((text.to_owned(), language.highlight(text)));
            }
            let spans = cache.as_ref().map_or(&[][..], |(_, spans)| spans);
            let mut job = layout_job(ui, text, spans);
            job.wrap.max_width = wrap_width;
            ui.fonts_mut(|fonts| fonts.layout_job(job))
        };
        let edit = if self.multiline {
            egui::TextEdit::multiline(text).lock_focus(true)
        } else {
            egui::TextEdit::singleline(text)
        };
        let edit = edit
            .font(TextStyle::Monospace)
            .hint_text(self.hint.clone())
            .desired_width(f32::INFINITY)
            .layouter(&mut layouter);

        let explicit = self.explicit;
        let output = CompletionPopup::new(self.id)
            .word_boundary(|c| !language.is_word_char(c))
            .align(RectAlign::BOTTOM_START)
            .show(ui, edit, |query| {
                let cursor = byte_offset(query.text, query.word_range.end.0);
                language
                    .complete(query.text, cursor, explicit)
                    .into_iter()
                    .map(|item| {
                        Suggestion::new(item.insert)
                            .content(item.label)
                            .description(item.detail)
                    })
                    .collect()
            });

        self.popup_open = output.is_open;
        if !output.is_open {
            self.explicit = false;
        }
        CodeEditorOutput {
            response: output.text_edit.response.response,
            submitted,
        }
    }
}

fn byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(offset, _)| offset)
}

fn layout_job(ui: &egui::Ui, text: &str, spans: &[Span]) -> LayoutJob {
    let font_id = TextStyle::Monospace.resolve(ui.style());
    let visuals = ui.visuals();
    let mut job = LayoutJob::default();
    let mut push = |range: Range<usize>, kind: Highlight| {
        let format = TextFormat::simple(font_id.clone(), color(kind, visuals));
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
            push(pos..range.start, Highlight::Plain);
        }
        push(range.clone(), *kind);
        pos = range.end;
    }
    // Always push, even when empty, so an empty editor still has a font and row height.
    push(pos..text.len(), Highlight::Plain);
    job
}

/// VS Code Dark+ / Light+ inspired colors.
fn color(kind: Highlight, visuals: &egui::Visuals) -> Color32 {
    let pick = |dark: [u8; 3], light: [u8; 3]| {
        let [r, g, b] = if visuals.dark_mode { dark } else { light };
        Color32::from_rgb(r, g, b)
    };
    match kind {
        Highlight::Plain | Highlight::Punctuation => visuals.text_color(),
        Highlight::Key => pick([156, 220, 254], [0, 16, 128]),
        Highlight::String => pick([206, 145, 120], [163, 21, 21]),
        Highlight::Number => pick([181, 206, 168], [9, 134, 88]),
        Highlight::Operator => pick([197, 134, 192], [175, 0, 219]),
        Highlight::Function => pick([220, 220, 170], [121, 94, 38]),
        Highlight::Keyword => pick([86, 156, 214], [0, 0, 255]),
        Highlight::Regex => pick([209, 105, 105], [129, 31, 63]),
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::spike_language::SpikeLanguage;

    struct State {
        text: String,
        editor: CodeEditor,
        submitted: bool,
    }

    fn harness() -> Harness<'static, State> {
        let state = State {
            text: String::new(),
            editor: CodeEditor::new(egui::Id::unique("editor"), false),
            submitted: false,
        };
        Harness::new_ui_state(
            |ui, state: &mut State| {
                let label = ui.label("Query");
                let output = state.editor.show(ui, &mut state.text, &SpikeLanguage);
                output.response.labelled_by(label.id);
                state.submitted |= output.submitted;
            },
            state,
        )
    }

    fn type_text(harness: &mut Harness<'_, State>, text: &str) {
        harness.get_by_role(Role::TextInput).focus();
        harness.run();
        harness.get_by_role(Role::TextInput).type_text(text);
        harness.run();
    }

    #[test]
    fn arrow_down_and_enter_accept_second_completion() {
        let mut harness = harness();
        type_text(&mut harness, "$gt");
        assert!(harness.state().editor.popup_open);

        harness.key_press(Key::ArrowDown);
        harness.run();
        harness.key_press(Key::Enter);
        harness.run();

        assert_eq!(harness.state().text, "$gte: ");
        assert!(!harness.state().editor.popup_open);
        assert!(!harness.state().submitted);
    }

    #[test]
    fn enter_submits_when_popup_is_closed() {
        let mut harness = harness();
        type_text(&mut harness, "{ a: 1 }");
        assert!(!harness.state().editor.popup_open);

        harness.key_press(Key::Enter);
        harness.run();

        assert!(harness.state().submitted);
    }
}
