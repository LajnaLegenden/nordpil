//! Connects the editor to the `nordpil-query` crate. Completion is still a
//! fixed list of operators and constructors; phase 4 makes it context-aware.

use crate::widgets::code_editor::{Completion, Language, Span};

pub struct QueryLanguage;

const OPERATORS: &[(&str, &str)] = &[
    ("$eq", "equal to"),
    ("$ne", "not equal to"),
    ("$gt", "greater than"),
    ("$gte", "greater than or equal to"),
    ("$lt", "less than"),
    ("$lte", "less than or equal to"),
    ("$in", "in array"),
    ("$nin", "not in array"),
    ("$and", "all clauses match"),
    ("$or", "any clause matches"),
    ("$nor", "no clause matches"),
    ("$not", "negates an expression"),
    ("$exists", "field exists"),
    ("$type", "BSON type is"),
    ("$regex", "matches a regex"),
    ("$elemMatch", "array element matches"),
    ("$size", "array length is"),
    ("$all", "array contains all"),
];

const CONSTRUCTORS: &[(&str, &str)] = &[
    ("ObjectId", "ObjectId"),
    ("ISODate", "date"),
    ("NumberInt", "32-bit integer"),
    ("NumberLong", "64-bit integer"),
    ("NumberDecimal", "128-bit decimal"),
    ("UUID", "UUID binary"),
];

impl Language for QueryLanguage {
    fn highlight(&self, text: &str) -> Vec<Span> {
        nordpil_query::highlight(text)
    }

    fn is_word_char(&self, c: char) -> bool {
        c.is_ascii() && is_word_byte(c as u8)
    }

    fn complete(&self, text: &str, cursor: usize, explicit: bool) -> Vec<Completion> {
        let word_start = text[..cursor]
            .char_indices()
            .rev()
            .find(|&(_, c)| !self.is_word_char(c))
            .map_or(0, |(offset, c)| offset + c.len_utf8());
        let word = &text[word_start..cursor];
        if word.is_empty() && !explicit {
            return Vec::new();
        }

        let operators = OPERATORS.iter().map(|&(name, detail)| Completion {
            label: name.to_owned(),
            insert: format!("{name}: "),
            detail: detail.to_owned(),
        });
        let constructors = CONSTRUCTORS.iter().map(|&(name, detail)| Completion {
            label: name.to_owned(),
            insert: format!("{name}("),
            detail: detail.to_owned(),
        });
        operators
            .chain(constructors)
            .filter(|item| item.label.to_lowercase().starts_with(&word.to_lowercase()))
            .collect()
    }
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'.')
}
