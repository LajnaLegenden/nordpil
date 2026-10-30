//! Connects the editor to the `nordpil-query` crate: highlighting plus
//! completion from the cursor context and the collection's sampled fields.

use nordpil_query::{FieldHint, Mode};

use crate::widgets::code_editor::{Completion, Language, Span};

pub struct QueryLanguage<'a> {
    pub mode: Mode,
    pub fields: &'a [FieldHint],
}

impl<'a> QueryLanguage<'a> {
    pub fn new(mode: Mode, fields: &'a [FieldHint]) -> Self {
        Self { mode, fields }
    }
}

impl Language for QueryLanguage<'_> {
    fn highlight(&self, text: &str) -> Vec<Span> {
        nordpil_query::highlight(text)
    }

    fn is_word_char(&self, c: char) -> bool {
        nordpil_query::is_word_char(c)
    }

    fn complete(&self, text: &str, cursor: usize, explicit: bool) -> Vec<Completion> {
        nordpil_query::complete(text, cursor, self.mode, self.fields, explicit)
            .into_iter()
            .map(|suggestion| Completion {
                label: suggestion.label,
                insert: suggestion.insert,
                detail: suggestion.detail,
            })
            .collect()
    }
}
