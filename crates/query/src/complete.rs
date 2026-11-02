//! Context-aware completion. Reads the tokens before the cursor to find out
//! whether a key or a value is being typed, and in which object, then
//! suggests sampled field paths, operators, constructors or values seen in
//! the current results.

use crate::lexer::{TokenKind, lex};
use crate::values::ValueHint;

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
    /// Where the cursor ends up, as a byte offset into `insert`, e.g. inside
    /// the brackets of `$in: []`.
    pub cursor: usize,
    /// Bytes after the cursor to remove, e.g. an automatically closed quote
    /// that `insert` already ends with.
    pub delete_after: usize,
    pub detail: String,
}

impl Suggestion {
    /// The cursor goes to the end of `insert`.
    pub(crate) fn new(label: &str, insert: &str, detail: &str) -> Self {
        Self {
            label: label.to_owned(),
            insert: insert.to_owned(),
            cursor: insert.len(),
            delete_after: 0,
            detail: detail.to_owned(),
        }
    }

    /// `insert` marks the cursor position with `|`.
    pub(crate) fn snippet(label: &str, insert: &str, detail: &str) -> Self {
        let (before, after) = insert.split_once('|').unwrap_or((insert, ""));
        Self {
            cursor: before.len(),
            ..Self::new(label, &format!("{before}{after}"), detail)
        }
    }
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

/// Inserts mark the cursor with `|`.
pub(crate) const CONSTRUCTORS: &[(&str, &str, &str)] = &[
    ("ObjectId", "ObjectId('|')", "ObjectId"),
    ("ISODate", "ISODate('|')", "date"),
    ("NumberInt", "NumberInt(|)", "32-bit integer"),
    ("NumberLong", "NumberLong('|')", "64-bit integer"),
    ("NumberDecimal", "NumberDecimal('|')", "128-bit decimal"),
    ("UUID", "UUID('|')", "UUID"),
    ("Timestamp", "Timestamp({ t: |, i: 1 })", "timestamp"),
    ("BinData", "BinData(0, '|')", "binary"),
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
    /// The array of `$in`, `$nin` or `$all` on a field: its values.
    Values(String),
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

/// Suggestions for the word that ends at byte offset `cursor`. `values` are
/// values seen in the current results, suggested for their field.
pub fn complete(
    text: &str,
    cursor: usize,
    mode: Mode,
    fields: &[FieldHint],
    values: &[ValueHint],
    explicit: bool,
) -> Vec<Suggestion> {
    let (Some(before), Some(after)) = (text.get(..cursor), text.get(cursor..)) else {
        return Vec::new();
    };
    let word_start = before
        .char_indices()
        .rev()
        .find(|&(_, c)| !is_word_char(c))
        .map_or(0, |(index, c)| index + c.len_utf8());
    let word = &before[word_start..];
    let quote = opening_quote(&before[..word_start]);
    // Right after an opening quote, suggest even before anything is typed.
    if word.is_empty() && !explicit && quote.is_none() {
        return Vec::new();
    }
    let context_end = word_start - quote.map_or(0, char::len_utf8);
    let stack = frames(&before[..context_end]);
    let context = Context { quote, after };

    let mut candidates = match stack.last() {
        Some(Frame::Object {
            owner,
            in_value: false,
            ..
        }) => key_suggestions(mode, owner, fields, &context),
        Some(Frame::Object {
            owner,
            key: Some(key),
            in_value: true,
        }) => {
            let path = value_path(owner, key);
            let mut list = field_values(path.as_deref(), values, &context);
            list.extend(value_suggestions(key, &context));
            list
        }
        Some(Frame::Array {
            owner: Owner::Values(path),
        }) => {
            let mut list = field_values(Some(path), values, &context);
            list.extend(value_suggestions("", &context));
            list
        }
        Some(Frame::Array { owner }) if *owner != Owner::Logical => value_suggestions("", &context),
        Some(Frame::Call) => Vec::new(),
        _ => Vec::new(),
    };
    rank(&mut candidates, word);
    candidates.truncate(MAX_SUGGESTIONS);
    candidates
}

/// What the text around the word means for the inserts.
struct Context<'a> {
    /// The quote that opens the string the word is in.
    quote: Option<char>,
    /// The text after the cursor.
    after: &'a str,
}

impl Context<'_> {
    /// Closes the open quote, reusing a closing quote that is already there.
    fn close_quote(&self, suggestion: &mut Suggestion) {
        if let Some(quote) = self.quote {
            suggestion.insert.push(quote);
            suggestion.cursor = suggestion.insert.len();
            if self.after.starts_with(quote) {
                suggestion.delete_after = quote.len_utf8();
            }
        }
    }
}

/// The quote at the end of `text`, if it opens a string rather than closing one.
pub(crate) fn opening_quote(text: &str) -> Option<char> {
    let quote = text
        .chars()
        .next_back()
        .filter(|c| matches!(c, '\'' | '"'))?;
    let opens = matches!(
        lex(text).last().map(|token| &token.kind),
        Some(TokenKind::String {
            terminated: false,
            ..
        })
    );
    opens.then_some(quote)
}

/// The field whose values fit at `key`'s value, e.g. `age` in
/// `{ age: { $gt: | } }`.
fn value_path(owner: &Owner, key: &str) -> Option<String> {
    let comparison = matches!(key, "$eq" | "$ne" | "$gt" | "$gte" | "$lt" | "$lte");
    match owner {
        Owner::Filter if !key.starts_with('$') => Some(key.to_owned()),
        Owner::ElemMatch(path) if !key.starts_with('$') => Some(format!("{path}.{key}")),
        Owner::Field(path) if comparison => Some(path.clone()),
        _ => None,
    }
}

/// Whether a value, rather than a key, is being typed at the end of `text`.
/// Outside any brackets counts as a value.
pub(crate) fn at_value(text: &str) -> bool {
    !matches!(
        frames(text).last(),
        Some(Frame::Object {
            in_value: false,
            ..
        })
    )
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
            "$in" | "$nin" | "$all" => match owner {
                Owner::Field(path) => Owner::Values(path.clone()),
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
    context: &Context<'_>,
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
                Some(key(path, &field.detail, context))
            })
            .collect::<Vec<_>>()
    };
    let operators = |list: &[(&str, &str)]| {
        list.iter()
            .map(|&(name, detail)| key(name, detail, context))
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

/// A key suggestion, quoted when needed, followed by `: ` and, for operators
/// that take an array or object, the brackets with the cursor inside.
fn key(name: &str, detail: &str, context: &Context<'_>) -> Suggestion {
    let mut suggestion = match context.quote {
        Some(_) => Suggestion::new(name, name, detail),
        None if needs_quotes(name) => Suggestion::new(name, &format!("'{name}'"), detail),
        None => Suggestion::new(name, name, detail),
    };
    context.close_quote(&mut suggestion);
    // When editing a key that already has its value, keep that value.
    let rest = &context.after[suggestion.delete_after..];
    if rest.trim_start().starts_with(':') {
        return suggestion;
    }
    let (snippet, cursor_from_end) = match name {
        "$in" | "$nin" | "$all" => (": []", 1),
        "$and" | "$or" | "$nor" => (": [{  }]", 3),
        "$elemMatch" | "$not" | "$expr" => (": {  }", 2),
        _ => (": ", 0),
    };
    suggestion.insert.push_str(snippet);
    suggestion.cursor = suggestion.insert.len() - cursor_from_end;
    suggestion
}

fn needs_quotes(name: &str) -> bool {
    let mut chars = name.chars();
    !(chars.next().is_some_and(crate::lexer::is_ident_start)
        && chars.all(crate::lexer::is_ident_continue))
}

fn value_suggestions(key: &str, context: &Context<'_>) -> Vec<Suggestion> {
    match key {
        "$type" => TYPE_ALIASES
            .iter()
            .map(|alias| match context.quote {
                Some(_) => {
                    let mut suggestion = Suggestion::new(alias, alias, "BSON type");
                    context.close_quote(&mut suggestion);
                    suggestion
                }
                None => Suggestion::new(alias, &format!("'{alias}'"), "BSON type"),
            })
            .collect(),
        _ if context.quote.is_some() => Vec::new(),
        "$exists" => ["true", "false"]
            .map(|value| Suggestion::new(value, value, "boolean"))
            .into(),
        _ => CONSTRUCTORS
            .iter()
            .map(|&(label, insert, detail)| Suggestion::snippet(label, insert, detail))
            .collect(),
    }
}

/// Values seen at `path` in the current results. Inside quotes, only strings.
fn field_values(
    path: Option<&str>,
    values: &[ValueHint],
    context: &Context<'_>,
) -> Vec<Suggestion> {
    let Some(path) = path else {
        return Vec::new();
    };
    values
        .iter()
        .filter(|value| value.path == path)
        .filter_map(|value| match (context.quote, &value.string) {
            (Some(quote), Some(string)) => {
                let mut suggestion =
                    Suggestion::new(&value.literal, &escape(string, quote), &value.detail);
                context.close_quote(&mut suggestion);
                Some(suggestion)
            }
            (Some(_), None) => None,
            (None, _) => Some(Suggestion::new(
                &value.literal,
                &value.literal,
                &value.detail,
            )),
        })
        .collect()
}

/// `text` as the inside of a string literal quoted with `quote`.
pub(crate) fn escape(text: &str, quote: char) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\\' || c == quote {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Keeps matches of `word`: prefix matches first, then matches at the start
/// of a path segment (`ci` finds `address.city`), then, for words of three
/// or more characters, substrings. Ties keep their original order.
pub(crate) fn rank(candidates: &mut Vec<Suggestion>, word: &str) {
    let word = word.to_lowercase();
    let score = |label: &str| {
        // String values match on their text, not the quote.
        let label = label.trim_start_matches(['\'', '"']).to_lowercase();
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

    fn values() -> Vec<ValueHint> {
        crate::value_hints(&[
            bson::doc! { "status": "shipped", "age": 30, "tags": ["it's"] },
            bson::doc! { "status": "pending", "age": 41 },
        ])
    }

    fn suggestions(text: &str, explicit: bool) -> Vec<Suggestion> {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        complete(&text, cursor, Mode::Filter, &fields(), &values(), explicit)
    }

    /// Completes at `|` in `text`.
    fn labels(text: &str, mode: Mode) -> Vec<String> {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        complete(&text, cursor, mode, &fields(), &values(), true)
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    fn inserts(text: &str) -> Vec<String> {
        suggestions(text, false)
            .into_iter()
            .map(|s| s.insert)
            .collect()
    }

    /// The first suggestion's insert with `|` at its cursor, and how many
    /// bytes after the cursor it removes.
    fn first(text: &str) -> (String, usize) {
        let suggestion = suggestions(text, false).remove(0);
        let mut insert = suggestion.insert;
        insert.insert(suggestion.cursor, '|');
        (insert, suggestion.delete_after)
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
        assert!(suggestions("{ |", false).is_empty());
        assert!(!suggestions("{ |", true).is_empty());
    }

    #[test]
    fn operators_insert_their_brackets_with_the_cursor_inside() {
        assert_eq!(first("{ age: { $in| } }"), ("$in: [|]".to_owned(), 0));
        assert_eq!(first("{ $or| }"), ("$or: [{ | }]".to_owned(), 0));
        assert_eq!(
            first("{ items: { $elemM| } }"),
            ("$elemMatch: { | }".to_owned(), 0)
        );
        assert_eq!(first("{ age: { $gt| } }"), ("$gt: |".to_owned(), 0));
        assert_eq!(first("{ nam| }"), ("name: |".to_owned(), 0));
        assert_eq!(first("{ _id: Obj| }"), ("ObjectId('|')".to_owned(), 0));
    }

    #[test]
    fn editing_a_key_keeps_its_value() {
        assert_eq!(first("{ nam|: 1 }"), ("name|".to_owned(), 0));
        assert_eq!(first("{ age: { $i|: [1] } }"), ("$in|".to_owned(), 0));
    }

    #[test]
    fn reuses_a_closing_quote_after_the_cursor() {
        assert_eq!(
            first("{ 'address.c|' }"),
            ("address.city': |".to_owned(), 1)
        );
        assert_eq!(
            first("{ name: { $type: 'str|' } }"),
            ("string'|".to_owned(), 1)
        );
    }

    #[test]
    fn suggests_values_from_the_results() {
        assert_eq!(
            labels("{ status: '|' }", Mode::Filter),
            ["'shipped'", "'pending'"]
        );
        assert_eq!(first("{ status: '|' }"), ("shipped'|".to_owned(), 1));
        assert_eq!(first("{ status: sh| }"), ("'shipped'|".to_owned(), 0));
        assert_eq!(first("{ age: { $gt: 4| } }"), ("41|".to_owned(), 0));
        assert_eq!(
            first("{ status: { $in: ['p|'] } }"),
            ("pending'|".to_owned(), 1)
        );
        assert_eq!(first("{ tags: \"|\" }"), ("it's\"|".to_owned(), 1));
        assert_eq!(first("{ tags: '|' }"), ("it\\'s'|".to_owned(), 1));
    }

    #[test]
    fn nothing_after_a_closing_quote() {
        assert!(suggestions("{ status: 'shipped'| }", false).is_empty());
    }
}
