//! Context-aware completion. Reads the tokens before the cursor to find out
//! whether a key or a value is being typed, and in which object, then
//! suggests sampled field paths, operators or constructors.

use crate::lexer::{TokenKind, lex};

/// Which query input is being completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Filter,
    /// Projection, sort and index key patterns: keys are field paths.
    Fields,
    Collation,
    /// A whole document being written: only values are completed.
    Document,
}

/// A field path seen in the collection, e.g. `address.city`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldHint {
    pub path: String,
    /// Shown next to the path, e.g. the BSON types seen.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    /// Replaces the word before the cursor (see [`is_word_char`]).
    pub insert: String,
    pub detail: String,
}

/// Characters of a completable word: identifiers, `$` and dots.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '.')
}

/// Most suggestions shown at once.
const MAX_SUGGESTIONS: usize = 50;

const ROOT_OPERATORS: &[(&str, &str)] = &[
    ("$and", "all clauses match"),
    ("$or", "any clause matches"),
    ("$nor", "no clause matches"),
    ("$expr", "aggregation expression"),
    ("$text", "text search"),
    ("$comment", "comment for logs"),
];

const FIELD_OPERATORS: &[(&str, &str)] = &[
    ("$eq", "equal to"),
    ("$ne", "not equal to"),
    ("$gt", "greater than"),
    ("$gte", "greater than or equal"),
    ("$lt", "less than"),
    ("$lte", "less than or equal"),
    ("$in", "in array"),
    ("$nin", "not in array"),
    ("$exists", "field exists"),
    ("$type", "BSON type is"),
    ("$regex", "matches regex"),
    ("$options", "regex options"),
    ("$not", "negates operators"),
    ("$elemMatch", "an array element matches"),
    ("$size", "array length is"),
    ("$all", "array contains all"),
    ("$mod", "divisor and remainder"),
];

const CONSTRUCTORS: &[(&str, &str, &str)] = &[
    ("ObjectId", "ObjectId('", "ObjectId"),
    ("ISODate", "ISODate('", "date"),
    ("NumberInt", "NumberInt(", "32-bit integer"),
    ("NumberLong", "NumberLong('", "64-bit integer"),
    ("NumberDecimal", "NumberDecimal('", "128-bit decimal"),
    ("UUID", "UUID('", "UUID"),
    ("Timestamp", "Timestamp({ t: ", "timestamp"),
    ("BinData", "BinData(0, '", "binary"),
    ("MinKey", "MinKey()", "lowest value"),
    ("MaxKey", "MaxKey()", "highest value"),
    ("true", "true", "boolean"),
    ("false", "false", "boolean"),
    ("null", "null", "null"),
];

const TYPE_ALIASES: &[&str] = &[
    "string",
    "int",
    "long",
    "double",
    "decimal",
    "bool",
    "date",
    "objectId",
    "array",
    "object",
    "null",
    "regex",
    "binData",
    "timestamp",
    "number",
];

const COLLATION_KEYS: &[(&str, &str)] = &[
    ("locale", "e.g. 'en', 'sv', 'simple'"),
    ("strength", "1 base, 2 accents, 3 case"),
    ("caseLevel", "true or false"),
    ("caseFirst", "'upper', 'lower' or 'off'"),
    ("numericOrdering", "compare digits as numbers"),
    ("alternate", "'non-ignorable' or 'shifted'"),
    ("maxVariable", "'punct' or 'space'"),
    ("backwards", "French accent order"),
];

/// What the object holding the cursor is the value of.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// A filter document: the root, or an element of `$and`/`$or`/`$nor`.
    Filter,
    /// A logical operator's array of filters.
    Logical,
    /// Operators applied to a field, e.g. `{ age: { | } }`.
    Field(String),
    /// `$elemMatch` on an array field: sub-fields and operators.
    ElemMatch(String),
    /// Anything else, e.g. `$expr` or an array of values.
    Other,
}

#[derive(Debug)]
enum Frame {
    Object {
        owner: Owner,
        key: Option<String>,
        in_value: bool,
    },
    Array {
        owner: Owner,
    },
    Call,
}

/// Suggestions for the word that ends at byte offset `cursor`.
pub fn complete(
    text: &str,
    cursor: usize,
    mode: Mode,
    fields: &[FieldHint],
    explicit: bool,
) -> Vec<Suggestion> {
    let Some(before) = text.get(..cursor) else {
        return Vec::new();
    };
    let word_start = before
        .char_indices()
        .rev()
        .find(|&(_, c)| !is_word_char(c))
        .map_or(0, |(index, c)| index + c.len_utf8());
    let word = &before[word_start..];
    let quote = before[..word_start]
        .chars()
        .next_back()
        .filter(|c| matches!(c, '\'' | '"'));
    if word.is_empty() && !explicit {
        return Vec::new();
    }
    let context_end = word_start - quote.map_or(0, char::len_utf8);
    let stack = frames(&before[..context_end]);

    let mut candidates = match stack.last() {
        Some(Frame::Object {
            owner,
            in_value: false,
            ..
        }) => key_suggestions(mode, owner, fields, quote),
        Some(Frame::Object {
            key: Some(key),
            in_value: true,
            ..
        }) => value_suggestions(key, quote),
        Some(Frame::Array { owner }) if *owner != Owner::Logical => value_suggestions("", quote),
        Some(Frame::Call) => Vec::new(),
        _ => Vec::new(),
    };
    rank(&mut candidates, word);
    candidates.truncate(MAX_SUGGESTIONS);
    candidates
}

fn frames(text: &str) -> Vec<Frame> {
    let mut stack: Vec<Frame> = Vec::new();
    for token in lex(text) {
        match token.kind {
            TokenKind::LBrace => {
                let owner = owner_of_next_value(&stack);
                stack.push(Frame::Object {
                    owner,
                    key: None,
                    in_value: false,
                });
            }
            TokenKind::LBracket => {
                let owner = owner_of_next_value(&stack);
                stack.push(Frame::Array { owner });
            }
            TokenKind::LParen => stack.push(Frame::Call),
            TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen => {
                stack.pop();
            }
            TokenKind::Colon => {
                if let Some(Frame::Object { in_value, .. }) = stack.last_mut() {
                    *in_value = true;
                }
            }
            TokenKind::Comma => {
                if let Some(Frame::Object { key, in_value, .. }) = stack.last_mut() {
                    *key = None;
                    *in_value = false;
                }
            }
            TokenKind::Ident(name)
            | TokenKind::Number(name)
            | TokenKind::String { value: name, .. } => {
                if let Some(Frame::Object {
                    key,
                    in_value: false,
                    ..
                }) = stack.last_mut()
                {
                    *key = Some(name);
                }
            }
            _ => {}
        }
    }
    stack
}

fn owner_of_next_value(stack: &[Frame]) -> Owner {
    match stack.last() {
        None => Owner::Filter,
        Some(Frame::Array {
            owner: Owner::Logical,
        }) => Owner::Filter,
        Some(Frame::Object {
            owner,
            key: Some(key),
            in_value: true,
        }) => match key.as_str() {
            "$and" | "$or" | "$nor" => Owner::Logical,
            "$elemMatch" => match owner {
                Owner::Field(path) => Owner::ElemMatch(path.clone()),
                _ => Owner::Other,
            },
            "$not" => match owner {
                Owner::Field(path) => Owner::Field(path.clone()),
                _ => Owner::Other,
            },
            key if key.starts_with('$') => Owner::Other,
            key => match owner {
                Owner::Filter => Owner::Field(key.to_owned()),
                Owner::ElemMatch(path) => Owner::Field(format!("{path}.{key}")),
                _ => Owner::Other,
            },
        },
        Some(_) => Owner::Other,
    }
}

fn key_suggestions(
    mode: Mode,
    owner: &Owner,
    fields: &[FieldHint],
    quote: Option<char>,
) -> Vec<Suggestion> {
    let field_keys = |prefix: &str| {
        fields
            .iter()
            .filter_map(|field| {
                let path = match prefix {
                    "" => Some(field.path.as_str()),
                    prefix => field
                        .path
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_prefix('.')),
                }?;
                Some(key(path, &field.detail, quote))
            })
            .collect::<Vec<_>>()
    };
    let operators = |list: &[(&str, &str)]| {
        list.iter()
            .map(|&(name, detail)| key(name, detail, quote))
            .collect::<Vec<_>>()
    };
    match (mode, owner) {
        (Mode::Filter, Owner::Filter) => {
            let mut list = field_keys("");
            list.extend(operators(ROOT_OPERATORS));
            list
        }
        (Mode::Filter, Owner::Field(_)) => operators(FIELD_OPERATORS),
        (Mode::Filter, Owner::ElemMatch(path)) => {
            let mut list = field_keys(path);
            list.extend(operators(FIELD_OPERATORS));
            list
        }
        (Mode::Fields, Owner::Filter) => field_keys(""),
        (Mode::Collation, Owner::Filter) => operators(COLLATION_KEYS),
        _ => Vec::new(),
    }
}

/// A key suggestion, quoted when needed, ending in `: `.
fn key(name: &str, detail: &str, quote: Option<char>) -> Suggestion {
    let insert = match quote {
        Some(quote) => format!("{name}{quote}: "),
        None if needs_quotes(name) => format!("'{name}': "),
        None => format!("{name}: "),
    };
    Suggestion {
        label: name.to_owned(),
        insert,
        detail: detail.to_owned(),
    }
}

fn needs_quotes(name: &str) -> bool {
    let mut chars = name.chars();
    !(chars.next().is_some_and(crate::lexer::is_ident_start)
        && chars.all(crate::lexer::is_ident_continue))
}

fn value_suggestions(key: &str, quote: Option<char>) -> Vec<Suggestion> {
    match key {
        "$type" => TYPE_ALIASES
            .iter()
            .map(|alias| Suggestion {
                label: (*alias).to_owned(),
                insert: match quote {
                    Some(quote) => format!("{alias}{quote}"),
                    None => format!("'{alias}'"),
                },
                detail: "BSON type".to_owned(),
            })
            .collect(),
        _ if quote.is_some() => Vec::new(),
        "$exists" => ["true", "false"]
            .map(|value| Suggestion {
                label: value.to_owned(),
                insert: value.to_owned(),
                detail: "boolean".to_owned(),
            })
            .into(),
        _ => CONSTRUCTORS
            .iter()
            .map(|&(label, insert, detail)| Suggestion {
                label: label.to_owned(),
                insert: insert.to_owned(),
                detail: detail.to_owned(),
            })
            .collect(),
    }
}

/// Keeps matches of `word`: prefix matches first, then matches at the start
/// of a path segment (`ci` finds `address.city`), then, for words of three
/// or more characters, substrings. Ties keep their original order.
fn rank(candidates: &mut Vec<Suggestion>, word: &str) {
    let word = word.to_lowercase();
    let score = |label: &str| {
        let label = label.to_lowercase();
        if label.starts_with(&word) {
            Some(0)
        } else if label
            .split('.')
            .skip(1)
            .any(|segment| segment.starts_with(&word))
        {
            Some(1)
        } else if word.trim_start_matches('$').chars().count() >= 3 && label.contains(&word) {
            Some(2)
        } else {
            None
        }
    };
    let mut scored: Vec<(u8, Suggestion)> = candidates
        .drain(..)
        .filter_map(|candidate| score(&candidate.label).map(|score| (score, candidate)))
        .collect();
    scored.sort_by_key(|(score, _)| *score);
    candidates.extend(scored.into_iter().map(|(_, candidate)| candidate));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<FieldHint> {
        [
            "_id",
            "name",
            "age",
            "address.city",
            "address.zip",
            "items.sku",
            "items.qty",
        ]
        .map(|path| FieldHint {
            path: path.to_owned(),
            detail: String::new(),
        })
        .into()
    }

    /// Completes at `|` in `text`.
    fn labels(text: &str, mode: Mode) -> Vec<String> {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        complete(&text, cursor, mode, &fields(), true)
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    fn inserts(text: &str) -> Vec<String> {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        complete(&text, cursor, Mode::Filter, &fields(), false)
            .into_iter()
            .map(|s| s.insert)
            .collect()
    }

    #[test]
    fn suggests_fields_and_root_operators_at_top_level_keys() {
        assert_eq!(
            labels("{ add|", Mode::Filter),
            ["address.city", "address.zip"]
        );
        assert_eq!(labels("{ name: 1, $o|", Mode::Filter), ["$or"]);
    }

    #[test]
    fn suggests_operators_inside_a_field() {
        assert_eq!(labels("{ age: { $g|", Mode::Filter), ["$gt", "$gte"]);
        assert_eq!(
            labels("{ age: { $not: { $l|", Mode::Filter),
            ["$lt", "$lte"]
        );
    }

    #[test]
    fn logical_arrays_hold_filters() {
        assert_eq!(labels("{ $or: [ { na|", Mode::Filter), ["name"]);
        assert_eq!(
            labels("{ $and: [{ a: 1 }, { age: { $e|", Mode::Filter)[0],
            "$eq"
        );
    }

    #[test]
    fn elem_match_suggests_sub_fields() {
        assert_eq!(labels("{ items: { $elemMatch: { q|", Mode::Filter), ["qty"]);
        assert_eq!(
            labels("{ items: { $elemMatch: { $e|", Mode::Filter),
            ["$eq", "$exists", "$elemMatch"]
        );
    }

    #[test]
    fn matches_path_segments_and_longer_substrings() {
        assert_eq!(labels("{ ci|", Mode::Filter), ["address.city"]);
        assert_eq!(
            labels("{ dress|", Mode::Filter),
            ["address.city", "address.zip"]
        );
    }

    #[test]
    fn suggests_constructors_and_type_names_for_values() {
        assert_eq!(labels("{ _id: Obj|", Mode::Filter), ["ObjectId"]);
        assert_eq!(labels("{ name: { $type: 'str|", Mode::Filter), ["string"]);
        assert_eq!(labels("{ name: { $exists: t|", Mode::Filter), ["true"]);
    }

    #[test]
    fn quotes_dotted_paths_and_closes_open_quotes() {
        assert_eq!(inserts("{ address.c|"), ["'address.city': "]);
        assert_eq!(inserts("{ 'address.c|"), ["address.city': "]);
        assert_eq!(inserts("{ nam|"), ["name: "]);
    }

    #[test]
    fn other_modes() {
        assert_eq!(labels("{ i|", Mode::Fields), ["items.sku", "items.qty"]);
        assert_eq!(labels("{ loc|", Mode::Collation), ["locale"]);
    }

    #[test]
    fn nothing_for_an_empty_word_unless_asked() {
        let text = "{ ";
        assert!(complete(text, 2, Mode::Filter, &fields(), false).is_empty());
        assert!(!complete(text, 2, Mode::Filter, &fields(), true).is_empty());
    }
}
