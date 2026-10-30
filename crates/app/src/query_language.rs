//! Placeholder query language for the editor spike. The `nordpil-query`
//! lexer and autocomplete replace it in phases 2 and 4.

use crate::widgets::code_editor::{Completion, Highlight, Language, Span};

pub struct SpikeLanguage;

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

impl Language for SpikeLanguage {
    fn highlight(&self, text: &str) -> Vec<Span> {
        let bytes = text.as_bytes();
        let mut spans = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let start = i;
            let kind = match bytes[i] {
                b'"' | b'\'' => {
                    i = skip_quoted(bytes, i);
                    Highlight::String
                }
                b'/' => {
                    i = skip_quoted(bytes, i);
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    Highlight::Regex
                }
                b'0'..=b'9' => {
                    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                        i += 1;
                    }
                    Highlight::Number
                }
                b'{' | b'}' | b'[' | b']' | b':' | b',' | b'(' | b')' => {
                    i += 1;
                    Highlight::Punctuation
                }
                c if c == b'$' || c == b'_' || c.is_ascii_alphabetic() => {
                    while i < bytes.len() && is_word_byte(bytes[i]) {
                        i += 1;
                    }
                    let word = &text[start..i];
                    let next = text[i..].trim_start().chars().next();
                    if word.starts_with('$') {
                        Highlight::Operator
                    } else if next == Some('(') {
                        Highlight::Function
                    } else if next == Some(':') {
                        Highlight::Key
                    } else if matches!(word, "true" | "false" | "null" | "new") {
                        Highlight::Keyword
                    } else {
                        Highlight::Plain
                    }
                }
                _ => {
                    // Step over one whole char so ranges stay on char boundaries.
                    i += text[i..].chars().next().map_or(1, char::len_utf8);
                    continue;
                }
            };
            spans.push((start..i, kind));
        }
        spans
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

/// Skips a string or regex literal that starts at `start`; returns the index
/// after the closing delimiter, or the text length when it is unterminated.
fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}
