//! Samples documents to learn a collection's field paths for autocomplete.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::TryStreamExt as _;
use mongodb::bson::{Bson, Document, doc};

use crate::{Backend, BackendError, Namespace};

const SAMPLE_SIZE: i32 = 100;
const MAX_PATHS: usize = 1000;
const MAX_DEPTH: usize = 10;

/// A field path and the BSON types seen at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub path: String,
    pub types: Vec<&'static str>,
}

impl Backend {
    pub(crate) async fn sample_fields(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<FieldInfo>, BackendError> {
        let client = self.session(namespace.connection)?.client;
        let documents: Vec<Document> = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .aggregate([doc! { "$sample": { "size": SAMPLE_SIZE } }])
            .max_time(Duration::from_secs(5))
            .await?
            .try_collect()
            .await?;
        Ok(field_paths(&documents))
    }
}

/// Paths in first-seen order. Array elements share their array's path, so
/// `items.sku` covers `items: [{ sku }]`.
pub fn field_paths(documents: &[Document]) -> Vec<FieldInfo> {
    let mut fields: Vec<FieldInfo> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for document in documents {
        walk(document, "", 0, &mut fields, &mut index);
    }
    fields
}

fn walk(
    document: &Document,
    prefix: &str,
    depth: usize,
    fields: &mut Vec<FieldInfo>,
    index: &mut HashMap<String, usize>,
) {
    for (key, value) in document {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        record(&path, value, depth, fields, index);
    }
}

fn record(
    path: &str,
    value: &Bson,
    depth: usize,
    fields: &mut Vec<FieldInfo>,
    index: &mut HashMap<String, usize>,
) {
    let type_name = nordpil_query::type_name(value);
    match index.get(path) {
        Some(&i) => {
            if !fields[i].types.contains(&type_name) {
                fields[i].types.push(type_name);
            }
        }
        None if fields.len() < MAX_PATHS => {
            index.insert(path.to_owned(), fields.len());
            fields.push(FieldInfo {
                path: path.to_owned(),
                types: vec![type_name],
            });
        }
        None => return,
    }
    if depth >= MAX_DEPTH {
        return;
    }
    match value {
        Bson::Document(child) => walk(child, path, depth + 1, fields, index),
        Bson::Array(items) => {
            for item in items {
                if let Bson::Document(child) = item {
                    walk(child, path, depth + 1, fields, index);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_nested_paths_and_types() {
        let documents = [
            doc! { "name": "a", "address": { "city": "x" }, "items": [{ "sku": "s" }] },
            doc! { "name": 5, "items": [] },
        ];

        let fields: Vec<_> = field_paths(&documents)
            .into_iter()
            .map(|field| (field.path, field.types))
            .collect();

        assert_eq!(
            fields,
            [
                ("name".to_owned(), vec!["String", "Int32"]),
                ("address".to_owned(), vec!["Object"]),
                ("address.city".to_owned(), vec!["String"]),
                ("items".to_owned(), vec!["Array"]),
                ("items.sku".to_owned(), vec!["String"]),
            ]
        );
    }
}
