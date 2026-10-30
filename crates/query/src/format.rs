//! Writes BSON as mongosh shell syntax. For every type the parser reads,
//! parsing the output gives back the same value.

use std::fmt::Write as _;

use base64::Engine as _;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document};

/// 0000-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z.
const MIN_ISO_MILLIS: i64 = -62_167_219_200_000;
const MAX_ISO_MILLIS: i64 = 253_402_300_799_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Everything on one line: `{ a: 1, b: [ 1, 2 ] }`.
    Compact,
    /// One field or item per line, indented by this many spaces.
    Pretty(usize),
}

pub fn format_value(value: &Bson, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    out
}

pub fn format_document(document: &Document, style: Style) -> String {
    let mut out = String::new();
    write_document(&mut out, document, style, 0);
    out
}

fn write_value(out: &mut String, value: &Bson, style: Style, depth: usize) {
    match value {
        Bson::Document(document) => write_document(out, document, style, depth),
        Bson::Array(items) => {
            write_list(out, ('[', ']'), items.iter(), style, depth, |out, item| {
                write_value(out, item, style, depth + 1);
            })
        }
        Bson::String(text) => write_string(out, text),
        Bson::Int32(n) => write!(out, "{n}").unwrap(),
        Bson::Int64(n) => write!(out, "Long('{n}')").unwrap(),
        Bson::Double(n) => write_double(out, *n),
        Bson::Decimal128(n) => write!(out, "Decimal128('{n}')").unwrap(),
        Bson::Boolean(b) => write!(out, "{b}").unwrap(),
        Bson::Null => out.push_str("null"),
        Bson::ObjectId(id) => write!(out, "ObjectId('{}')", id.to_hex()).unwrap(),
        Bson::DateTime(date) => {
            let ms = date.timestamp_millis();
            // Outside years 0000-9999 the RFC 3339 text would be clamped, so use milliseconds.
            match date.try_to_rfc3339_string() {
                Ok(text) if (MIN_ISO_MILLIS..=MAX_ISO_MILLIS).contains(&ms) => {
                    write!(out, "ISODate('{text}')").unwrap();
                }
                _ => write!(out, "new Date({ms})").unwrap(),
            }
        }
        Bson::Binary(binary) => write_binary(out, binary),
        Bson::Timestamp(ts) => {
            write!(out, "Timestamp({{ t: {}, i: {} }})", ts.time, ts.increment).unwrap()
        }
        Bson::RegularExpression(regex) => {
            let pattern = regex.pattern.as_str();
            // A `/.../` literal can't hold these, so fall back to RegExp('...').
            let needs_constructor = pattern.is_empty()
                || pattern.contains(['\n', '\r'])
                || pattern.contains("\\/")
                || pattern.ends_with('\\')
                || class_left_open(pattern);
            if needs_constructor {
                out.push_str("RegExp(");
                write_string(out, pattern);
                out.push_str(", ");
                write_string(out, regex.options.as_str());
                out.push(')');
            } else {
                write!(
                    out,
                    "/{}/{}",
                    escape_slashes(pattern),
                    regex.options.as_str()
                )
                .unwrap();
            }
        }
        Bson::JavaScriptCode(code) => {
            out.push_str("Code(");
            write_string(out, code);
            out.push(')');
        }
        Bson::JavaScriptCodeWithScope(code) => {
            out.push_str("Code(");
            write_string(out, &code.code);
            out.push_str(", ");
            write_document(out, &code.scope, Style::Compact, depth);
            out.push(')');
        }
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        // Deprecated types: shown for reading, not accepted by the parser.
        Bson::Symbol(symbol) => {
            out.push_str("Symbol(");
            write_string(out, symbol);
            out.push(')');
        }
        Bson::Undefined => out.push_str("undefined"),
        Bson::DbPointer(_) => out.push_str("DBPointer()"),
    }
}

fn write_document(out: &mut String, document: &Document, style: Style, depth: usize) {
    write_list(
        out,
        ('{', '}'),
        document.iter(),
        style,
        depth,
        |out, (key, value)| {
            write_key(out, key);
            out.push_str(": ");
            write_value(out, value, style, depth + 1);
        },
    );
}

fn write_list<T>(
    out: &mut String,
    (open, close): (char, char),
    items: impl Iterator<Item = T>,
    style: Style,
    depth: usize,
    mut write_item: impl FnMut(&mut String, T),
) {
    let mut items = items.peekable();
    out.push(open);
    if items.peek().is_none() {
        out.push(close);
        return;
    }
    match style {
        Style::Compact => {
            for (index, item) in items.enumerate() {
                out.push_str(if index == 0 { " " } else { ", " });
                write_item(out, item);
            }
            out.push(' ');
        }
        Style::Pretty(indent) => {
            for (index, item) in items.enumerate() {
                out.push_str(if index == 0 { "\n" } else { ",\n" });
                out.push_str(&" ".repeat(indent * (depth + 1)));
                write_item(out, item);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * depth));
        }
    }
    out.push(close);
}

/// Bare when the key is a plain identifier (`name`, `$gt`), quoted otherwise.
fn write_key(out: &mut String, key: &str) {
    let mut chars = key.chars();
    let bare = chars.next().is_some_and(crate::lexer::is_ident_start)
        && chars.all(crate::lexer::is_ident_continue)
        && !matches!(key, "true" | "false" | "null" | "new");
    if bare {
        out.push_str(key);
    } else {
        write_string(out, key);
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('\'');
    for c in text.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{{{:x}}}", u32::from(c)).unwrap(),
            c => out.push(c),
        }
    }
    out.push('\'');
}

/// Always shows a decimal point or exponent, so a Double never reads back as an integer.
fn write_double(out: &mut String, n: f64) {
    if n.is_nan() {
        out.push_str("NaN");
    } else if n.is_infinite() {
        out.push_str(if n > 0.0 { "Infinity" } else { "-Infinity" });
    } else {
        // `{:?}` prints the shortest text that parses back exactly, with `.0` for whole numbers.
        write!(out, "{n:?}").unwrap();
    }
}

fn write_binary(out: &mut String, binary: &Binary) {
    if binary.subtype == BinarySubtype::Uuid
        && let Ok(uuid) = binary.to_uuid()
    {
        write!(out, "UUID('{uuid}')").unwrap();
        return;
    }
    let data = base64::engine::general_purpose::STANDARD.encode(&binary.bytes);
    write!(out, "BinData({}, '{data}')", u8::from(binary.subtype)).unwrap();
}

/// Escapes unescaped `/` so the pattern fits in a `/.../` literal.
fn escape_slashes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut escaped = false;
    for c in pattern.chars() {
        if c == '/' && !escaped {
            out.push('\\');
        }
        escaped = c == '\\' && !escaped;
        out.push(c);
    }
    out
}

/// Whether a `[` character class is still open at the end. The lexer would
/// then read the closing `/` as part of the class.
fn class_left_open(pattern: &str) -> bool {
    let mut in_class = false;
    let mut escaped = false;
    for c in pattern.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' => in_class = true,
            ']' => in_class = false,
            _ => {}
        }
    }
    in_class
}
