//! Inserting, editing and deleting documents.

use mongodb::bson::{Bson, Document, doc};
use mongodb::options::UpdateModifications;

use crate::{Backend, BackendError, ErrorKind, Namespace};

/// What an edit changes, as an update plus a filter that only matches the
/// document as it was loaded. If someone else changed those fields in the
/// meantime, nothing matches and the edit is reported as a conflict.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Edit {
    pub filter: Document,
    pub update: Document,
}

impl Backend {
    pub(crate) async fn insert_documents(
        &self,
        namespace: &Namespace,
        documents: Vec<Document>,
    ) -> Result<u64, BackendError> {
        if documents.is_empty() {
            return Err(BackendError::new("There is nothing to insert."));
        }
        let client = self.writable_session(namespace.connection)?.client;
        let result = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .insert_many(documents)
            .await?;
        Ok(result.inserted_ids.len() as u64)
    }

    pub(crate) async fn update_document(
        &self,
        namespace: &Namespace,
        original: &Document,
        updated: &Document,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        let collection = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection);
        let id = original
            .get("_id")
            .cloned()
            .ok_or_else(|| BackendError::new("Documents without an _id can't be edited."))?;
        let matched = match edit(original, updated)? {
            None => return Ok(()),
            Some(Plan::Update(edit)) => {
                collection
                    .update_one(edit.filter, edit.update)
                    .await?
                    .matched_count
            }
            Some(Plan::Replace(replacement)) => {
                collection
                    .replace_one(doc! { "_id": id.clone() }, replacement)
                    .await?
                    .matched_count
            }
        };
        if matched > 0 {
            return Ok(());
        }
        let still_exists = collection.find_one(doc! { "_id": id }).await?.is_some();
        Err(if still_exists {
            BackendError::new(
                "This document changed since it was loaded, so the edit was not saved. \
                 Refresh and edit it again.",
            )
            .with_kind(ErrorKind::Conflict)
        } else {
            BackendError::new("This document was deleted since it was loaded.")
                .with_kind(ErrorKind::Conflict)
        })
    }

    pub(crate) async fn delete_document(
        &self,
        namespace: &Namespace,
        id: Bson,
    ) -> Result<(), BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        let result = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .delete_one(doc! { "_id": id })
            .await?;
        if result.deleted_count == 0 {
            return Err(BackendError::new("This document was already deleted."));
        }
        Ok(())
    }

    /// Returns how many documents changed.
    pub(crate) async fn update_many(
        &self,
        namespace: &Namespace,
        filter: Document,
        update: Bson,
    ) -> Result<u64, BackendError> {
        let update = match update {
            Bson::Document(update) if update.keys().all(|key| key.starts_with('$')) => {
                UpdateModifications::Document(update)
            }
            Bson::Array(stages) => UpdateModifications::Pipeline(
                stages
                    .into_iter()
                    .map(|stage| match stage {
                        Bson::Document(stage) => Ok(stage),
                        _ => Err(BackendError::new("Each pipeline stage must be a document.")),
                    })
                    .collect::<Result<_, _>>()?,
            ),
            _ => {
                return Err(BackendError::new(
                    "Use update operators, e.g. { $set: { field: 'value' } }, or a pipeline.",
                ));
            }
        };
        let client = self.writable_session(namespace.connection)?.client;
        let result = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .update_many(filter, update)
            .await?;
        Ok(result.modified_count)
    }

    /// Returns how many documents were deleted.
    pub(crate) async fn delete_many(
        &self,
        namespace: &Namespace,
        filter: Document,
    ) -> Result<u64, BackendError> {
        let client = self.writable_session(namespace.connection)?.client;
        let result = client
            .database(&namespace.database)
            .collection::<Document>(&namespace.collection)
            .delete_many(filter)
            .await?;
        Ok(result.deleted_count)
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Plan {
    Update(Edit),
    /// Field names with `.` or a leading `$` can't be addressed by path, so
    /// the whole document is replaced, guarded by `_id` only.
    Replace(Document),
}

/// Works out how to save `updated` over `original`. `None` when nothing changed.
pub(crate) fn edit(original: &Document, updated: &Document) -> Result<Option<Plan>, BackendError> {
    if original.get("_id") != updated.get("_id") {
        return Err(BackendError::new(
            "The _id can't be changed. Clone the document to give it a new _id.",
        ));
    }
    if original == updated {
        return Ok(None);
    }
    let unsafe_key = |key: &String| key.contains('.') || key.starts_with('$');
    if original.keys().chain(updated.keys()).any(unsafe_key) {
        let mut replacement = updated.clone();
        replacement.remove("_id");
        return Ok(Some(Plan::Replace(replacement)));
    }

    let mut set = Document::new();
    let mut unset = Document::new();
    let mut filter = doc! { "_id": original.get("_id").cloned().unwrap_or(Bson::Null) };
    diff("", original, updated, &mut set, &mut unset, &mut filter);

    let mut update = Document::new();
    if !set.is_empty() {
        update.insert("$set", set);
    }
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }
    Ok(Some(Plan::Update(Edit { filter, update })))
}

/// Records changed paths. Sub-documents are compared field by field so an
/// edit only touches what changed; arrays and other values are set whole.
fn diff(
    prefix: &str,
    original: &Document,
    updated: &Document,
    set: &mut Document,
    unset: &mut Document,
    filter: &mut Document,
) {
    for (key, new) in updated {
        let path = format!("{prefix}{key}");
        match (original.get(key), new) {
            (Some(old), new) if old == new => {}
            (Some(Bson::Document(old)), Bson::Document(new))
                if is_addressable(old) && is_addressable(new) =>
            {
                diff(&format!("{path}."), old, new, set, unset, filter);
            }
            (Some(old), new) => {
                filter.insert(path.clone(), old.clone());
                set.insert(path, new.clone());
            }
            (None, new) => {
                filter.insert(path.clone(), doc! { "$exists": false });
                set.insert(path, new.clone());
            }
        }
    }
    for (key, old) in original {
        if !updated.contains_key(key) {
            let path = format!("{prefix}{key}");
            filter.insert(path.clone(), old.clone());
            unset.insert(path, "");
        }
    }
}

/// Whether every key can be part of a dotted path.
fn is_addressable(document: &Document) -> bool {
    document
        .keys()
        .all(|key| !key.contains('.') && !key.starts_with('$') && !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(original: Document, updated: Document) -> Option<Plan> {
        edit(&original, &updated).unwrap()
    }

    #[test]
    fn unchanged_document_needs_nothing() {
        let document = doc! { "_id": 1, "a": 1 };
        assert_eq!(plan(document.clone(), document), None);
    }

    #[test]
    fn sets_and_unsets_only_changed_paths_guarded_by_old_values() {
        let original =
            doc! { "_id": 1, "name": "a", "address": { "city": "x", "zip": "1" }, "gone": true };
        let updated =
            doc! { "_id": 1, "name": "a", "address": { "city": "y", "zip": "1" }, "new": [1] };

        assert_eq!(
            plan(original, updated),
            Some(Plan::Update(Edit {
                filter: doc! {
                    "_id": 1,
                    "address.city": "x",
                    "new": { "$exists": false },
                    "gone": true,
                },
                update: doc! {
                    "$set": { "address.city": "y", "new": [1] },
                    "$unset": { "gone": "" },
                },
            }))
        );
    }

    #[test]
    fn keeps_number_types_apart() {
        let original = doc! { "_id": 1, "n": 1_i32 };
        let updated = doc! { "_id": 1, "n": 1_i64 };
        assert!(matches!(plan(original, updated), Some(Plan::Update(_))));
    }

    #[test]
    fn refuses_to_change_the_id() {
        assert!(edit(&doc! { "_id": 1 }, &doc! { "_id": 2 }).is_err());
    }

    #[test]
    fn replaces_documents_with_unaddressable_keys() {
        let original = doc! { "_id": 1, "a.b": 1 };
        let updated = doc! { "_id": 1, "a.b": 2 };
        assert_eq!(
            plan(original, updated),
            Some(Plan::Replace(doc! { "a.b": 2 }))
        );
    }
}
