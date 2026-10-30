//! Listing, creating, dropping and hiding indexes.

use std::collections::HashMap;

use futures_util::TryStreamExt as _;
use mongodb::bson::{Bson, DateTime, Document, doc};

use crate::{Backend, BackendError, Namespace};

#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    /// The index as the server describes it: `key`, `unique`, `expireAfterSeconds`, ...
    pub spec: Document,
    /// Bytes on disk, when the server reports it.
    pub size: Option<u64>,
    /// Uses since `since`, when the server reports it.
    pub ops: Option<i64>,
    pub since: Option<DateTime>,
}

impl IndexInfo {
    pub fn keys(&self) -> Document {
        self.spec.get_document("key").cloned().unwrap_or_default()
    }

    pub fn is_id_index(&self) -> bool {
        self.name == "_id_"
    }
}

impl Backend {
    pub(crate) async fn list_indexes(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<IndexInfo>, BackendError> {
        let client = self.session(namespace.connection)?.client;
        let database = client.database(&namespace.database);
        let specs: Vec<Document> = database
            .run_cursor_command(doc! { "listIndexes": &namespace.collection })
            .await?
            .try_collect()
            .await?;

        // Usage and sizes are extras: views and limited users may not get them.
        let collection = database.collection::<Document>(&namespace.collection);
        let mut usage: HashMap<String, (i64, Option<DateTime>)> = HashMap::new();
        if let Ok(cursor) = collection.aggregate([doc! { "$indexStats": {} }]).await
            && let Ok(stats) = cursor.try_collect::<Vec<_>>().await
        {
            for stat in stats {
                let (Ok(name), Ok(accesses)) =
                    (stat.get_str("name"), stat.get_document("accesses"))
                else {
                    continue;
                };
                let ops = match accesses.get("ops") {
                    Some(Bson::Int64(n)) => *n,
                    Some(Bson::Int32(n)) => i64::from(*n),
                    _ => 0,
                };
                usage.insert(
                    name.to_owned(),
                    (ops, accesses.get_datetime("since").ok().copied()),
                );
            }
        }
        let mut sizes: HashMap<String, u64> = HashMap::new();
        let stats_stage = doc! { "$collStats": { "storageStats": {} } };
        if let Ok(cursor) = collection.aggregate([stats_stage]).await
            && let Ok(stats) = cursor.try_collect::<Vec<_>>().await
            && let Some(index_sizes) = stats
                .first()
                .and_then(|s| s.get_document("storageStats").ok())
                .and_then(|s| s.get_document("indexSizes").ok())
        {
            for (name, size) in index_sizes {
                let size = match size {
                    Bson::Int32(n) => u64::try_from(*n).ok(),
                    Bson::Int64(n) => u64::try_from(*n).ok(),
                    Bson::Double(n) => Some(*n as u64),
                    _ => None,
                };
                if let Some(size) = size {
                    sizes.insert(name.clone(), size);
                }
            }
        }

        Ok(specs
            .into_iter()
            .map(|spec| {
                let name = spec.get_str("name").unwrap_or_default().to_owned();
                let (ops, since) = usage
                    .get(&name)
                    .map_or((None, None), |&(ops, since)| (Some(ops), since));
                IndexInfo {
                    size: sizes.get(&name).copied(),
                    ops,
                    since,
                    name,
                    spec,
                }
            })
            .collect())
    }

    /// `options` are index spec fields such as `unique`, `sparse`,
    /// `expireAfterSeconds`, `partialFilterExpression`, `collation`, `hidden`
    /// and `name`. Without a name, one is made from the keys, like mongosh.
    pub(crate) async fn create_index(
        &self,
        namespace: &Namespace,
        keys: Document,
        mut options: Document,
    ) -> Result<(), BackendError> {
        if keys.is_empty() {
            return Err(BackendError::new(
                "Enter at least one key, e.g. { field: 1 }.",
            ));
        }
        let client = self.writable_session(namespace.connection)?.client;
        if !matches!(options.get("name"), Some(Bson::String(name)) if !name.is_empty()) {
            options.insert("name", default_name(&keys));
        }
        let mut index = doc! { "key": keys };
        index.extend(options);
        client
            .database(&namespace.database)
            .run_command(doc! { "createIndexes": &namespace.collection, "indexes": [index] })
            .await?;
        Ok(())
    }

    pub(crate) async fn drop_index(
        &self,
        namespace: &Namespace,
        name: &str,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .drop_index(name)
            .await?;
        Ok(())
    }

    /// Hidden indexes are kept up to date but not used by queries, so you can
    /// test dropping one without rebuilding it afterwards.
    pub(crate) async fn set_index_hidden(
        &self,
        namespace: &Namespace,
        name: &str,
        hidden: bool,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        client
            .database(&namespace.database)
            .run_command(doc! {
                "collMod": &namespace.collection,
                "index": { "name": name, "hidden": hidden },
            })
            .await?;
        Ok(())
    }
}

/// `{ a: 1, b: -1 }` becomes `a_1_b_-1`, like the shell and drivers do.
fn default_name(keys: &Document) -> String {
    keys.iter()
        .map(|(key, value)| {
            let value = match value {
                Bson::Int32(n) => n.to_string(),
                Bson::Int64(n) => n.to_string(),
                Bson::Double(n) if n.fract() == 0.0 => format!("{n:.0}"),
                Bson::Double(n) => n.to_string(),
                Bson::String(text) => text.clone(),
                other => other.to_string(),
            };
            format!("{key}_{value}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_indexes_from_keys() {
        assert_eq!(default_name(&doc! { "a": 1, "b": -1.0 }), "a_1_b_-1");
        assert_eq!(default_name(&doc! { "body": "text" }), "body_text");
    }
}
