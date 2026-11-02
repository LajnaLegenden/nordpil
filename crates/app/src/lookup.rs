//! Following an id field to the document it refers to: `userId` in one
//! collection to the document with that `_id` in `users`.

use bson::oid::ObjectId;
use bson::{Bson, Document, doc};

/// The collections a field named `key` may refer to: the key without its
/// `id` or `ids` ending, as is or plural, compared without case, `_` or `-`.
/// `userId` and `user_ids` both match `user` and `users`.
pub fn targets<'a>(key: &str, collections: &'a [String]) -> Vec<&'a str> {
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
/// for an array. A 24-digit hex string is read as an ObjectId. `None` for
/// values that can't be ids: objects, null and empty arrays.
pub fn filter(value: &Bson) -> Option<Document> {
    match value {
        Bson::Document(_) | Bson::Null | Bson::Undefined => None,
        Bson::Array(items) if items.is_empty() => None,
        Bson::Array(items) => {
            let ids: Vec<Bson> = items.iter().map(id).collect();
            Some(doc! { "_id": { "$in": ids } })
        }
        _ => Some(doc! { "_id": id(value) }),
    }
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

        assert_eq!(targets("userId", &collections), ["user", "users"]);
        assert_eq!(targets("user_ids", &collections), ["user", "users"]);
        assert_eq!(targets("categoryID", &collections), ["categories"]);
        assert_eq!(targets("orderItemId", &collections), ["order_items"]);
        assert_eq!(targets("company-id", &collections), ["Companies"]);
        assert_eq!(targets("boxIds", &collections), ["boxes"]);
        assert!(targets("_id", &collections).is_empty());
        assert!(targets("orders", &collections).is_empty());
        assert!(targets("parentId", &collections).is_empty());
    }

    #[test]
    fn hex_strings_become_object_ids_and_arrays_use_in() {
        let oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();

        assert_eq!(
            filter(&Bson::String(oid.to_hex())),
            Some(doc! { "_id": oid })
        );
        assert_eq!(
            filter(&Bson::String("abc".into())),
            Some(doc! { "_id": "abc" })
        );
        assert_eq!(filter(&Bson::Int32(7)), Some(doc! { "_id": 7 }));
        assert_eq!(
            filter(&Bson::Array(vec![
                Bson::String(oid.to_hex()),
                Bson::Int32(7)
            ])),
            Some(doc! { "_id": { "$in": [oid, 7] } })
        );
        assert_eq!(filter(&Bson::Array(Vec::new())), None);
        assert_eq!(filter(&Bson::Document(doc! {})), None);
    }
}
