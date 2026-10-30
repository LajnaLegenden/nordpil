//! Values seen in a page of results, for value autocomplete. Reads only the
//! documents already loaded, so it never queries the server.

use std::collections::HashMap;

use bson::{Bson, Document};

use crate::format::{Style, format_value};

/// Values kept per field, most common first.
const VALUES_PER_FIELD: usize = 20;
/// Distinct values tracked per field before new ones are ignored.
const TRACKED_PER_FIELD: usize = 200;
/// Values looked at in total, so huge documents stay quick.
const MAX_VISITED: usize = 50_000;
const MAX_DEPTH: usize = 8;
/// Longer strings are not useful to suggest.
const MAX_STRING_LEN: usize = 120;

/// A value seen at a field path, e.g. `'shipped'` at `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueHint {
    pub path: String,
    /// The value in shell syntax, e.g. `'shipped'` or `ObjectId('…')`.
    pub literal: String,
    /// The text of a string value, for completing inside quotes.
    pub string: Option<String>,
    /// How many of the documents have it, e.g. `8 of 20`.
    pub detail: String,
}

struct Seen {
    literal: String,
    string: Option<String>,
    documents: usize,
    /// Index of the last document counted, so each counts once.
    last_document: usize,
}

#[derive(Default)]
struct Collector {
    /// Paths in first-seen order, each with its values in first-seen order.
    paths: Vec<(String, Vec<Seen>)>,
    index: HashMap<String, usize>,
    visited: usize,
}

/// Values per field path in `documents`, most common first. Array elements
/// count for the array's path, so `tags: ['a']` suggests `'a'` for `tags`.
pub fn value_hints(documents: &[Document]) -> Vec<ValueHint> {
    let mut collector = Collector::default();
    for (number, document) in documents.iter().enumerate() {
        collector.walk(document, "", 0, number);
    }
    let total = documents.len();
    collector
        .paths
        .into_iter()
        .flat_map(|(path, mut seen)| {
            // Stable, so equally common values keep their first-seen order.
            seen.sort_by_key(|seen| std::cmp::Reverse(seen.documents));
            seen.truncate(VALUES_PER_FIELD);
            seen.into_iter().map(move |value| ValueHint {
                path: path.clone(),
                literal: value.literal,
                string: value.string,
                detail: format!("{} of {total}", value.documents),
            })
        })
        .collect()
}

impl Collector {
    fn walk(&mut self, document: &Document, prefix: &str, depth: usize, number: usize) {
        for (key, value) in document {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            self.value(&path, value, depth, number);
        }
    }

    fn value(&mut self, path: &str, value: &Bson, depth: usize, number: usize) {
        if self.visited >= MAX_VISITED || depth > MAX_DEPTH {
            return;
        }
        self.visited += 1;
        match value {
            Bson::Document(document) => self.walk(document, path, depth + 1, number),
            Bson::Array(items) => {
                for item in items {
                    self.value(path, item, depth + 1, number);
                }
            }
            Bson::String(text) if text.len() > MAX_STRING_LEN => {}
            Bson::String(_)
            | Bson::Int32(_)
            | Bson::Int64(_)
            | Bson::Double(_)
            | Bson::Decimal128(_)
            | Bson::Boolean(_)
            | Bson::Null
            | Bson::ObjectId(_)
            | Bson::DateTime(_) => self.record(path, value, number),
            _ => {}
        }
    }

    fn record(&mut self, path: &str, value: &Bson, number: usize) {
        let index = match self.index.get(path) {
            Some(&index) => index,
            None => {
                self.index.insert(path.to_owned(), self.paths.len());
                self.paths.push((path.to_owned(), Vec::new()));
                self.paths.len() - 1
            }
        };
        let seen = &mut self.paths[index].1;
        let literal = format_value(value, Style::Compact);
        if let Some(existing) = seen.iter_mut().find(|seen| seen.literal == literal) {
            if existing.last_document != number {
                existing.documents += 1;
                existing.last_document = number;
            }
        } else if seen.len() < TRACKED_PER_FIELD {
            seen.push(Seen {
                literal,
                string: match value {
                    Bson::String(text) => Some(text.clone()),
                    _ => None,
                },
                documents: 1,
                last_document: number,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn counts_documents_per_value_most_common_first() {
        let documents = [
            doc! { "status": "shipped", "tags": ["a", "a", "b"] },
            doc! { "status": "pending", "tags": ["b"] },
            doc! { "status": "shipped", "total": 1.5 },
        ];

        let hints = value_hints(&documents);
        let of = |path: &str| {
            hints
                .iter()
                .filter(|hint| hint.path == path)
                .map(|hint| (hint.literal.as_str(), hint.detail.as_str()))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            of("status"),
            [("'shipped'", "2 of 3"), ("'pending'", "1 of 3")]
        );
        assert_eq!(of("tags"), [("'b'", "2 of 3"), ("'a'", "1 of 3")]);
        assert_eq!(of("total"), [("1.5", "1 of 3")]);
    }

    #[test]
    fn walks_into_subdocuments_and_arrays_of_documents() {
        let documents = [doc! {
            "customer": { "name": "Ada" },
            "items": [{ "sku": "A-1" }],
            "blob": bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: vec![1] },
        }];

        let hints = value_hints(&documents);

        let paths: Vec<_> = hints.iter().map(|hint| hint.path.as_str()).collect();
        assert_eq!(paths, ["customer.name", "items.sku"]);
        assert_eq!(hints[0].string.as_deref(), Some("Ada"));
    }
}
