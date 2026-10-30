//! Turns a syntax tree into BSON. Only literals and an allowlist of mongosh
//! constructors (`ObjectId`, `ISODate`, `NumberLong`, ...) are accepted;
//! nothing is executed.

use std::ops::Range;
use std::str::FromStr;

use base64::Engine as _;
use bson::raw::CString;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp};

use crate::parser::{Member, Node, NodeKind, ParseError};

pub fn eval(node: &Node) -> Result<Bson, ParseError> {
    match &node.kind {
        NodeKind::Object(members) => object(members).map(Bson::Document),
        NodeKind::Array(items) => items
            .iter()
            .map(eval)
            .collect::<Result<Vec<_>, _>>()
            .map(Bson::Array),
        NodeKind::String(value) => Ok(Bson::String(value.clone())),
        NodeKind::Number(text) => number(text, &node.span),
        NodeKind::Negative(operand) => match &operand.kind {
            NodeKind::Number(text) => number(&format!("-{text}"), &node.span),
            NodeKind::Ident(name) if name == "Infinity" => Ok(Bson::Double(f64::NEG_INFINITY)),
            _ => Err(error("Only numbers can be negative.", &node.span)),
        },
        NodeKind::Regex { pattern, flags } => regex(pattern, flags, &node.span),
        NodeKind::Ident(name) => ident(name, &node.span),
        NodeKind::Call { callee, args, .. } => call(callee, args, &node.span),
        NodeKind::Missing => Err(error("Expected a value.", &node.span)),
    }
}

fn object(members: &[Member]) -> Result<Document, ParseError> {
    let mut document = Document::new();
    for member in members {
        // Like JavaScript, a repeated key keeps its first position and last value.
        document.insert(member.key.clone(), eval(&member.value)?);
    }
    Ok(document)
}

fn error(message: impl Into<String>, span: &Range<usize>) -> ParseError {
    ParseError {
        message: message.into(),
        span: span.clone(),
    }
}

/// Integers become Int32 when they fit, then Int64; anything else is a Double.
fn number(text: &str, span: &Range<usize>) -> Result<Bson, ParseError> {
    let invalid = || error(format!("{text} is not a valid number."), span);
    let clean = text.replace('_', "");
    let (negative, digits) = match clean.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, clean.as_str()),
    };
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        let value = i64::from_str_radix(hex, 16).map_err(|_| invalid())?;
        return Ok(integer(if negative { -value } else { value }));
    }
    if digits.is_empty() || !digits.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return Err(invalid());
    }
    if digits.chars().all(|c| c.is_ascii_digit())
        && let Ok(value) = clean.parse::<i64>()
    {
        return Ok(integer(value));
    }
    clean
        .parse::<f64>()
        .map(Bson::Double)
        .map_err(|_| invalid())
}

fn integer(value: i64) -> Bson {
    i32::try_from(value).map_or(Bson::Int64(value), Bson::Int32)
}

fn regex(pattern: &str, flags: &str, span: &Range<usize>) -> Result<Bson, ParseError> {
    if let Some(flag) = flags.chars().find(|c| !"imsxlu".contains(*c)) {
        return Err(error(
            format!("The regex flag {flag} isn't supported by MongoDB. Use i, m, s, x, l or u."),
            span,
        ));
    }
    let mut sorted: Vec<char> = flags.chars().collect();
    sorted.sort_unstable();
    sorted.dedup();
    let options: String = sorted.into_iter().collect();
    let cstring = |text: String| {
        CString::try_from(text).map_err(|_| error("A regex can't contain a null character.", span))
    };
    Ok(Bson::RegularExpression(Regex {
        pattern: cstring(pattern.to_owned())?,
        options: cstring(options)?,
    }))
}

fn ident(name: &str, span: &Range<usize>) -> Result<Bson, ParseError> {
    match name {
        "true" => Ok(Bson::Boolean(true)),
        "false" => Ok(Bson::Boolean(false)),
        "null" => Ok(Bson::Null),
        "NaN" => Ok(Bson::Double(f64::NAN)),
        "Infinity" => Ok(Bson::Double(f64::INFINITY)),
        "MinKey" => Ok(Bson::MinKey),
        "MaxKey" => Ok(Bson::MaxKey),
        "undefined" => Err(error("undefined isn't supported. Use null.", span)),
        _ => Err(error(
            format!("Unknown name {name}. Put text in quotes, e.g. '{name}'."),
            span,
        )),
    }
}

fn call(callee: &str, args: &[Node], span: &Range<usize>) -> Result<Bson, ParseError> {
    let args = Args { callee, args, span };
    match callee {
        "ObjectId" | "ObjectID" => match args.optional_string(0)? {
            None => Ok(Bson::ObjectId(bson::oid::ObjectId::new())),
            Some(hex) => bson::oid::ObjectId::parse_str(&hex)
                .map(Bson::ObjectId)
                .map_err(|_| args.fail("needs 24 hex characters")),
        },
        "ISODate" | "Date" => date(&args),
        "NumberInt" | "Int32" => {
            let text = args.number_text(0)?;
            text.parse::<i32>()
                .map(Bson::Int32)
                .map_err(|_| args.fail("needs a 32-bit integer"))
        }
        "NumberLong" | "Long" => {
            let text = args.number_text(0)?;
            text.parse::<i64>()
                .map(Bson::Int64)
                .map_err(|_| args.fail("needs a 64-bit integer"))
        }
        "NumberDecimal" | "Decimal128" => {
            let text = args.number_text(0)?;
            Decimal128::from_str(&text)
                .map(Bson::Decimal128)
                .map_err(|_| args.fail("needs a decimal number"))
        }
        "Double" => {
            let text = args.number_text(0)?;
            text.parse::<f64>()
                .map(Bson::Double)
                .map_err(|_| args.fail("needs a number"))
        }
        "UUID" => {
            let text = args.string(0)?;
            bson::uuid::Uuid::parse_str(&text)
                .map(|uuid| Bson::Binary(Binary::from_uuid(uuid)))
                .map_err(|_| args.fail("needs a UUID like 'xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx'"))
        }
        "BinData" => {
            let subtype = args.number_text(0)?;
            let subtype = subtype
                .parse::<u8>()
                .map_err(|_| args.fail("needs a subtype from 0 to 255"))?;
            let data = args.string(1)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| args.fail("needs base64 data"))?;
            Ok(Bson::Binary(Binary {
                subtype: BinarySubtype::from(subtype),
                bytes,
            }))
        }
        "Timestamp" => timestamp(&args),
        "MinKey" => Ok(Bson::MinKey),
        "MaxKey" => Ok(Bson::MaxKey),
        "RegExp" => {
            let pattern = args.string(0)?;
            let flags = args.optional_string(1)?.unwrap_or_default();
            regex(&pattern, &flags, span)
        }
        "Code" => Ok(Bson::JavaScriptCode(args.string(0)?)),
        _ => Err(error(
            format!(
                "Unknown function {callee}. Supported: ObjectId, ISODate, NumberInt, NumberLong, \
                 NumberDecimal, UUID, BinData, Timestamp, RegExp, MinKey, MaxKey."
            ),
            span,
        )),
    }
}

fn date(args: &Args<'_>) -> Result<Bson, ParseError> {
    let Some(arg) = args.args.first() else {
        return Ok(Bson::DateTime(DateTime::now()));
    };
    match eval(arg)? {
        Bson::String(text) => parse_date(&text)
            .map(Bson::DateTime)
            .ok_or_else(|| args.fail("needs a date like '2024-01-31' or '2024-01-31T12:00:00Z'")),
        Bson::Int32(ms) => Ok(Bson::DateTime(DateTime::from_millis(ms.into()))),
        Bson::Int64(ms) => Ok(Bson::DateTime(DateTime::from_millis(ms))),
        _ => Err(args.fail("needs a date string or milliseconds since 1970")),
    }
}

/// Accepts what mongosh's ISODate accepts: a date, or a date and time with
/// or without a time zone (UTC is assumed).
fn parse_date(text: &str) -> Option<DateTime> {
    let text = text.trim();
    let candidate = if text.len() == 10 {
        format!("{text}T00:00:00Z")
    } else if !text.ends_with(['Z', 'z']) && !has_offset(text) {
        format!("{text}Z")
    } else {
        text.to_owned()
    };
    DateTime::parse_rfc3339_str(candidate.replacen(' ', "T", 1)).ok()
}

fn has_offset(text: &str) -> bool {
    let time = text.get(10..).unwrap_or("");
    time.rfind(['+', '-']).is_some_and(|index| index > 0)
}

fn timestamp(args: &Args<'_>) -> Result<Bson, ParseError> {
    let (time, increment) = match args.args {
        [object] => match eval(object)? {
            Bson::Document(document) => (
                document.get("t").cloned().unwrap_or(Bson::Null),
                document.get("i").cloned().unwrap_or(Bson::Null),
            ),
            _ => return Err(args.fail("needs { t: seconds, i: increment }")),
        },
        [time, increment] => (eval(time)?, eval(increment)?),
        _ => return Err(args.fail("needs { t: seconds, i: increment }")),
    };
    let as_u32 = |value: Bson| match value {
        Bson::Int32(n) => u32::try_from(n).ok(),
        Bson::Int64(n) => u32::try_from(n).ok(),
        _ => None,
    };
    match (as_u32(time), as_u32(increment)) {
        (Some(time), Some(increment)) => Ok(Bson::Timestamp(Timestamp { time, increment })),
        _ => Err(args.fail("needs whole numbers from 0 to 4294967295")),
    }
}

struct Args<'a> {
    callee: &'a str,
    args: &'a [Node],
    span: &'a Range<usize>,
}

impl Args<'_> {
    fn fail(&self, what: &str) -> ParseError {
        error(format!("{}() {what}.", self.callee), self.span)
    }

    fn optional_string(&self, index: usize) -> Result<Option<String>, ParseError> {
        match self.args.get(index) {
            None => Ok(None),
            Some(node) => match &node.kind {
                NodeKind::String(text) => Ok(Some(text.clone())),
                _ => Err(self.fail("needs a string")),
            },
        }
    }

    fn string(&self, index: usize) -> Result<String, ParseError> {
        self.optional_string(index)?
            .ok_or_else(|| self.fail("needs a string"))
    }

    /// A number written as a literal or as a string, e.g. `NumberLong('9007199254740993')`.
    /// Using the text avoids losing precision through a double.
    fn number_text(&self, index: usize) -> Result<String, ParseError> {
        let Some(node) = self.args.get(index) else {
            return Err(self.fail("needs a number"));
        };
        match &node.kind {
            NodeKind::String(text) | NodeKind::Number(text) => Ok(text.trim().replace('_', "")),
            NodeKind::Negative(operand) => match &operand.kind {
                NodeKind::Number(text) => Ok(format!("-{}", text.replace('_', ""))),
                _ => Err(self.fail("needs a number")),
            },
            _ => Err(self.fail("needs a number")),
        }
    }
}
