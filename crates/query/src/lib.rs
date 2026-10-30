//! Query language for nordpil: mongosh shell syntax to BSON and back,
//! plus highlighting.
//!
//! ```
//! let filter = nordpil_query::parse_document("{ age: { $gt: 30 }, name: /^an/i }").unwrap();
//! assert_eq!(filter.keys().collect::<Vec<_>>(), ["age", "name"]);
//! ```

mod complete;
mod eval;
mod format;
mod highlight;
pub mod lexer;
pub mod parser;

use bson::{Bson, Document};

pub use complete::{FieldHint, Mode, Suggestion, complete, is_word_char};
pub use format::{Style, format_document, format_value};
pub use highlight::{Highlight, highlight};
pub use parser::ParseError;

/// Parses one value. Empty input is an error.
pub fn parse_value(text: &str) -> Result<Bson, Vec<ParseError>> {
    let parsed = parser::parse(text);
    if !parsed.errors.is_empty() {
        return Err(parsed.errors);
    }
    let Some(root) = parsed.root else {
        return Err(vec![ParseError {
            message: "Enter a value.".to_owned(),
            span: 0..text.len(),
        }]);
    };
    eval::eval(&root).map_err(|error| vec![error])
}

/// Parses a document, e.g. a filter. Empty input gives an empty document.
pub fn parse_document(text: &str) -> Result<Document, Vec<ParseError>> {
    let parsed = parser::parse(text);
    if !parsed.errors.is_empty() {
        return Err(parsed.errors);
    }
    let Some(root) = parsed.root else {
        return Ok(Document::new());
    };
    match eval::eval(&root).map_err(|error| vec![error])? {
        Bson::Document(document) => Ok(document),
        _ => Err(vec![ParseError {
            message: "Expected a document in braces, e.g. { field: 'value' }.".to_owned(),
            span: root.span,
        }]),
    }
}

/// The BSON type's name as Compass and the MongoDB docs write it.
pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "Double",
        Bson::String(_) => "String",
        Bson::Array(_) => "Array",
        Bson::Document(_) => "Object",
        Bson::Boolean(_) => "Boolean",
        Bson::Null => "Null",
        Bson::RegularExpression(_) => "Regular expression",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "JavaScript code",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Timestamp(_) => "Timestamp",
        Bson::Binary(_) => "Binary",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Symbol(_) => "Symbol",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Undefined => "Undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

#[cfg(test)]
mod tests;
