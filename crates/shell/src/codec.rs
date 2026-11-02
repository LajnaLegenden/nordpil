//! Values between Rust and JavaScript travel as JSON. Plain values are plain
//! JSON; other BSON values are canonical extended JSON wrapped in an object
//! whose only key is [`TAG`]. BSON keys can't contain a NUL, so no real
//! document is mistaken for a wrapped value.

use bson::{Bson, Document};
use serde_json::{Map, Value};

pub const TAG: &str = "\0";

/// A value for JavaScript. Int32 and finite doubles become numbers.
pub fn to_json(value: Bson) -> Value {
    match value {
        Bson::Document(document) => Value::Object(document_to_json(document)),
        Bson::Array(items) => Value::Array(items.into_iter().map(to_json).collect()),
        Bson::String(text) => Value::String(text),
        Bson::Boolean(flag) => Value::Bool(flag),
        Bson::Null => Value::Null,
        Bson::Int32(n) => Value::from(n),
        Bson::Double(n) if n.is_finite() => Value::from(n),
        other => {
            let mut wrapper = Map::new();
            wrapper.insert(TAG.to_owned(), other.into_canonical_extjson());
            Value::Object(wrapper)
        }
    }
}

pub fn document_to_json(document: Document) -> Map<String, Value> {
    document
        .into_iter()
        .map(|(key, value)| (key, to_json(value)))
        .collect()
}

/// A value from JavaScript. Integers that fit 32 bits become Int32, other
/// numbers Double, like mongosh.
pub fn from_json(value: Value) -> Result<Bson, String> {
    Ok(match value {
        Value::Null => Bson::Null,
        Value::Bool(flag) => Bson::Boolean(flag),
        Value::String(text) => Bson::String(text),
        Value::Number(number) => match number.as_i64().and_then(|n| i32::try_from(n).ok()) {
            Some(n) => Bson::Int32(n),
            None => Bson::Double(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::Array(items) => {
            Bson::Array(items.into_iter().map(from_json).collect::<Result<_, _>>()?)
        }
        Value::Object(mut map) => {
            if map.len() == 1
                && let Some(inner) = map.remove(TAG)
            {
                return Bson::try_from(inner).map_err(|error| error.to_string());
            }
            Bson::Document(document_from_json(map)?)
        }
    })
}

pub fn document_from_json(map: Map<String, Value>) -> Result<Document, String> {
    map.into_iter()
        .map(|(key, value)| Ok((key, from_json(value)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use bson::oid::ObjectId;
    use bson::{DateTime, Decimal128, doc};

    use super::*;

    #[test]
    fn round_trips_through_json() {
        let document = doc! {
            "_id": ObjectId::new(),
            "n": 5,
            "big": 5_000_000_000_i64,
            "pi": 3.5,
            "nan": f64::NAN,
            "when": DateTime::from_millis(1_700_000_000_000),
            "price": "1.10".parse::<Decimal128>().unwrap(),
            "tags": ["a", { "$date": "not a date" }],
        };

        let json = to_json(Bson::Document(document.clone()));
        let back = from_json(json).unwrap();

        assert_eq!(
            format!("{back:?}").replace("NaN", "x"),
            format!("{:?}", Bson::Document(document)).replace("NaN", "x")
        );
    }

    #[test]
    fn plain_numbers_follow_mongosh() {
        let parse = |text: &str| from_json(serde_json::from_str(text).unwrap()).unwrap();
        assert_eq!(parse("7"), Bson::Int32(7));
        assert_eq!(parse("3000000000"), Bson::Double(3e9));
        assert_eq!(parse("1.5"), Bson::Double(1.5));
    }
}
