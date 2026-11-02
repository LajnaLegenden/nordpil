//! Following an id field to the document it refers to: `userId` in one
//! collection to the document with that `_id` in `users`, or wherever the
//! user linked the field to.

use bson::oid::ObjectId;
use bson::{Bson, Document};
use nordpil_backend::references::Mapping;

/// What id fields in one database can be looked up in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    /// The database's collections.
    pub collections: Vec<String>,
    /// The fields the user linked to collections.
    pub mappings: Vec<Mapping>,
}

/// A collection to look a value up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target<'a> {
    pub collection: &'a str,
    /// The field there that holds the value; `None` for `_id`.
    pub field: Option<&'a str>,
}

impl References {
    /// The user's link for fields named `key`.
    pub fn mapping(&self, key: &str) -> Option<&Mapping> {
        self.mappings.iter().find(|mapping| mapping.key == key)
    }

    /// Where a field named `key` may point: the user's link first, then the
    /// collections [`guess`] finds. Collections that don't exist are left out.
    pub fn targets(&self, key: &str) -> Vec<Target<'_>> {
        let mut targets: Vec<Target<'_>> = self
            .mapping(key)
            .filter(|mapping| self.collections.contains(&mapping.collection))
            .map(|mapping| Target {
                collection: &mapping.collection,
                field: mapping.field.as_deref(),
            })
            .into_iter()
            .collect();
        for collection in guess(key, &self.collections) {
            if !targets.iter().any(|target| target.collection == collection) {
                targets.push(Target {
                    collection,
                    field: None,
                });
            }
        }
        targets
    }
}

/// The collections a field named `key` may refer to: the key without its
/// `id` or `ids` ending, as is or plural, compared without case, `_` or `-`.
/// `userId` and `user_ids` both match `user` and `users`.
fn guess<'a>(key: &str, collections: &'a [String]) -> Vec<&'a str> {
    let lower = key.to_lowercase();
    let Some(stem) = lower
        .strip_suffix("ids")
        .or_else(|| lower.strip_suffix("id"))
        .map(normalize)
        .filter(|stem| !stem.is_empty())
    else {
        return Vec::new();
    };
    let mut names = vec![format!("{stem}s"), format!("{stem}es")];
    if let Some(base) = stem.strip_suffix('y') {
        names.push(format!("{base}ies"));
    }
    names.push(stem);
    collections
        .iter()
        .map(String::as_str)
        .filter(|collection| names.contains(&normalize(collection)))
        .collect()
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The filter that finds what `value` refers to: `{ _id: value }`, or `$in`
/// for an array. `field` names another field than `_id` to match. For `_id`,
/// a 24-digit hex string is read as an ObjectId. `None` for values that
/// can't be ids: objects, null and empty arrays.
pub fn filter(value: &Bson, field: Option<&str>) -> Option<Document> {
    let (field, convert): (&str, fn(&Bson) -> Bson) = match field {
        None | Some("_id") => ("_id", id),
        Some(field) => (field, Bson::clone),
    };
    let condition = match value {
        Bson::Document(_) | Bson::Null | Bson::Undefined => return None,
        Bson::Array(items) if items.is_empty() => return None,
        Bson::Array(items) => {
            let values: Vec<Bson> = items.iter().map(convert).collect();
            Bson::Document(bson::doc! { "$in": values })
        }
        _ => convert(value),
    };
    let mut filter = Document::new();
    filter.insert(field, condition);
    Some(filter)
}

fn id(value: &Bson) -> Bson {
    match value {
        Bson::String(text) => {
            ObjectId::parse_str(text).map_or_else(|_| value.clone(), Bson::ObjectId)
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn matches_id_keys_to_collections_and_their_plurals() {
        let collections: Vec<String> = [
            "user",
            "users",
            "categories",
            "order_items",
            "Companies",
            "boxes",
            "orders",
        ]
        .map(str::to_owned)
        .into();

        assert_eq!(guess("userId", &collections), ["user", "users"]);
        assert_eq!(guess("user_ids", &collections), ["user", "users"]);
        assert_eq!(guess("categoryID", &collections), ["categories"]);
        assert_eq!(guess("orderItemId", &collections), ["order_items"]);
        assert_eq!(guess("company-id", &collections), ["Companies"]);
        assert_eq!(guess("boxIds", &collections), ["boxes"]);
        assert!(guess("_id", &collections).is_empty());
        assert!(guess("orders", &collections).is_empty());
        assert!(guess("parentId", &collections).is_empty());
    }

    #[test]
    fn hex_strings_become_object_ids_and_arrays_use_in() {
        let oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();

        assert_eq!(
            filter(&Bson::String(oid.to_hex()), None),
            Some(doc! { "_id": oid })
        );
        assert_eq!(
            filter(&Bson::String("abc".into()), None),
            Some(doc! { "_id": "abc" })
        );
        assert_eq!(filter(&Bson::Int32(7), None), Some(doc! { "_id": 7 }));
        assert_eq!(
            filter(
                &Bson::Array(vec![Bson::String(oid.to_hex()), Bson::Int32(7)]),
                None
            ),
            Some(doc! { "_id": { "$in": [oid, 7] } })
        );
        assert_eq!(filter(&Bson::Array(Vec::new()), None), None);
        assert_eq!(filter(&Bson::Document(doc! {}), None), None);
    }

    fn mapping(key: &str, collection: &str, field: Option<&str>) -> Mapping {
        Mapping {
            key: key.to_owned(),
            collection: collection.to_owned(),
            field: field.map(str::to_owned),
        }
    }

    #[test]
    fn linked_collections_come_before_guesses() {
        let references = References {
            collections: ["users", "accounts", "products"].map(str::to_owned).into(),
            mappings: vec![
                mapping("userId", "accounts", None),
                mapping("ownerRef", "users", None),
                mapping("sku", "products", Some("code")),
                mapping("gone", "dropped", None),
            ],
        };
        let target = |collection, field| Target { collection, field };

        assert_eq!(
            references.targets("userId"),
            [target("accounts", None), target("users", None)]
        );
        assert_eq!(references.targets("ownerRef"), [target("users", None)]);
        assert_eq!(
            references.targets("sku"),
            [target("products", Some("code"))]
        );
        assert!(references.targets("gone").is_empty());
    }

    #[test]
    fn other_fields_than_id_match_values_as_they_are() {
        let hex = "65a1b2c3d4e5f60718293a4b";

        assert_eq!(
            filter(&Bson::String(hex.to_owned()), Some("code")),
            Some(doc! { "code": hex })
        );
        assert_eq!(
            filter(&Bson::Array(vec![Bson::String("A-1".into())]), Some("code")),
            Some(doc! { "code": { "$in": ["A-1"] } })
        );
        assert_eq!(
            filter(&Bson::String(hex.to_owned()), Some("_id")),
            Some(doc! { "_id": ObjectId::parse_str(hex).unwrap() })
        );
    }
}
