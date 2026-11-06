//! Hides every value in query text before it goes to an AI agent, and puts
//! the values back in the agent's answer.
//!
//! Each literal (string, number, boolean, null, regex, `ObjectId(…)`,
//! `ISODate(…)` and other calls) becomes a placeholder string such as
//! `"__v3_Date__"`. Keys stay, except segments that look like values
//! themselves (ids, dates, emails, numbers), which become `"__k4__"`.
//! Strings like `'$price'` stay when every segment of the path is already a
//! known name, so the agent gets nothing it didn't have from the schema.
//!
//! ```
//! use nordpil_query::mask::Masks;
//!
//! let mut masks = Masks::new(["name"]);
//! let masked = masks.mask(&["{ name: 'Ada', age: { $gt: 30 } }"]);
//! assert_eq!(masked[0], "{ name: '__v1_String__', age: { $gt: '__v2_Int32__' } }");
//! assert_eq!(
//!     masks.restore(&masked[0]).unwrap(),
//!     "{ name: 'Ada', age: { $gt: 30 } }",
//! );
//! ```

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use bson::Bson;

use crate::lexer::{Token, TokenKind, is_ident_continue, is_ident_start, lex};
use crate::{Style, format_value, parse_value, type_name};

/// Stands for a key segment that holds a value, e.g. an id used as a key.
pub const DYNAMIC_KEY: &str = "<key>";

/// A parent with more distinct child keys than this is a map: its keys are
/// values.
const MAX_CHILD_KEYS: usize = 50;

/// Whether a key segment looks like a value rather than a name: an
/// ObjectId, UUID or other long hex string, a number, a date or an email.
pub fn is_dynamic_segment(segment: &str) -> bool {
    let hex = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit());
    let digits = |text: &[u8]| text.iter().all(u8::is_ascii_digit);
    let bytes = segment.as_bytes();
    (matches!(segment.len(), 24 | 32) && hex(segment))
        || is_uuid(segment)
        || (!segment.is_empty() && digits(bytes))
        || (bytes.len() >= 7 && digits(&bytes[..4]) && bytes[4] == b'-' && digits(&bytes[5..7]))
        || segment.contains('@')
}

fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Merges paths whose dynamic segments (see [`is_dynamic_segment`]) or map
/// keys (a parent with more than 50 child keys) become [`DYNAMIC_KEY`].
/// First-seen order; the types of merged paths are joined.
pub fn collapse_paths<'a, T: Clone + PartialEq + 'a>(
    paths: impl IntoIterator<Item = (&'a str, &'a [T])>,
) -> Vec<(String, Vec<T>)> {
    let paths: Vec<(&str, &[T])> = paths.into_iter().collect();
    let mut children: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (path, _) in &paths {
        let mut end = 0;
        for segment in path.split('.') {
            children
                .entry(&path[..end.max(1) - 1])
                .or_default()
                .insert(segment);
            end += segment.len() + 1;
        }
    }
    let mut out: Vec<(String, Vec<T>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (path, types) in paths {
        let mut collapsed = Vec::new();
        let mut end = 0;
        for segment in path.split('.') {
            let parent = &path[..end.max(1) - 1];
            let crowded = children
                .get(parent)
                .is_some_and(|keys| keys.len() > MAX_CHILD_KEYS);
            collapsed.push(if crowded || is_dynamic_segment(segment) {
                DYNAMIC_KEY
            } else {
                segment
            });
            end += segment.len() + 1;
        }
        let collapsed = collapsed.join(".");
        match index.get(&collapsed) {
            Some(&at) => {
                let merged = &mut out[at].1;
                for kind in types {
                    if !merged.contains(kind) {
                        merged.push(kind.clone());
                    }
                }
            }
            None => {
                index.insert(collapsed.clone(), out.len());
                out.push((collapsed, types.to_vec()));
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
struct Hidden {
    /// Without quotes, e.g. `__v3_Date__` or `__k4__`.
    placeholder: String,
    /// For a value, its text as written; for a key segment, the segment.
    original: String,
}

/// The values hidden so far. Keep one per conversation, so the same value
/// keeps its placeholder across turns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Masks {
    /// Names the agent already knows: schema path segments and keys.
    names: HashSet<String>,
    hidden: Vec<Hidden>,
}

impl Masks {
    /// `schema` holds the field paths sent to the agent; their segments
    /// count as known names for `'$path'` strings.
    pub fn new<S: AsRef<str>>(schema: impl IntoIterator<Item = S>) -> Self {
        let mut masks = Self::default();
        for path in schema {
            masks.learn(path.as_ref());
        }
        masks
    }

    fn learn(&mut self, path: &str) {
        for segment in path.split('.') {
            if !segment.is_empty() && segment != DYNAMIC_KEY && !is_dynamic_segment(segment) {
                self.names.insert(segment.to_owned());
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }

    /// Masks each text. Keys from all of them count as known names first, so
    /// `'$total'` in one text may refer to a key made in another.
    pub fn mask(&mut self, texts: &[&str]) -> Vec<String> {
        let cleaned: Vec<(String, Vec<Token>)> = texts
            .iter()
            .map(|text| {
                let text = without_comments(text);
                let tokens = lex(&text);
                (text, tokens)
            })
            .collect();
        for (_, tokens) in &cleaned {
            for at in 0..tokens.len() {
                if let Some(key) = key_at(tokens, at) {
                    self.learn(key);
                }
            }
        }
        cleaned
            .iter()
            .map(|(text, tokens)| self.mask_one(text, tokens))
            .collect()
    }

    fn mask_one(&mut self, text: &str, tokens: &[Token]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        let mut at = 0;
        while at < tokens.len() {
            let token = &tokens[at];
            out.push_str(&text[cursor..token.span.start]);
            let mut end = at;
            if let Some(key) = key_at(tokens, at) {
                if key.starts_with('$') || !key.split('.').any(is_dynamic_segment) {
                    out.push_str(&text[token.span.clone()]);
                } else {
                    let masked: Vec<String> = key
                        .split('.')
                        .map(|segment| {
                            if is_dynamic_segment(segment) {
                                self.hide(segment, None)
                            } else {
                                segment.to_owned()
                            }
                        })
                        .collect();
                    out.push_str(&quote(&masked.join(".")));
                }
                cursor = token.span.end;
                at += 1;
                continue;
            }
            match &token.kind {
                TokenKind::LBrace
                | TokenKind::RBrace
                | TokenKind::LBracket
                | TokenKind::RBracket
                | TokenKind::Colon
                | TokenKind::Comma => out.push_str(&text[token.span.clone()]),
                TokenKind::String { value, .. } if self.is_known_ref(value) => {
                    out.push_str(&text[token.span.clone()]);
                }
                TokenKind::Ident(word) if self.is_known_ref(word) => {
                    out.push_str(&text[token.span.clone()]);
                }
                _ => {
                    end = value_end(tokens, at);
                    let span = token.span.start..tokens[end].span.end;
                    out.push_str(&self.hide_value(&text[span]));
                }
            }
            cursor = tokens[end].span.end;
            at = end + 1;
        }
        out.push_str(&text[cursor..]);
        out
    }

    /// `$path` whose segments are all known names, or a `$$variable`.
    fn is_known_ref(&self, text: &str) -> bool {
        if let Some(variable) = text.strip_prefix("$$") {
            return variable.chars().next().is_some_and(is_ident_start);
        }
        let Some(path) = text.strip_prefix('$') else {
            return false;
        };
        !path.is_empty() && path.split('.').all(|segment| self.names.contains(segment))
    }

    fn hide_value(&mut self, original: &str) -> String {
        let kind = parse_value(original).map_or("Value", |value| type_name(&value));
        let kind: String = kind.chars().filter(char::is_ascii_alphanumeric).collect();
        quote(&self.hide(original, Some(&kind)))
    }

    /// The placeholder for a value of type `kind`, or for a key segment when
    /// `kind` is `None`; the same one again for the same original.
    fn hide(&mut self, original: &str, kind: Option<&str>) -> String {
        if let Some(hidden) = self.hidden.iter().find(|hidden| {
            hidden.original == original && is_key_kind(&hidden.placeholder) == kind.is_none()
        }) {
            return hidden.placeholder.clone();
        }
        let n = self.hidden.len() + 1;
        let placeholder = match kind {
            Some(kind) => format!("__v{n}_{kind}__"),
            None => format!("__k{n}__"),
        };
        self.hidden.push(Hidden {
            placeholder: placeholder.clone(),
            original: original.to_owned(),
        });
        placeholder
    }

    /// Puts the hidden values back into the agent's text. Fails on a
    /// placeholder this conversation never made, or one used inside other
    /// text.
    pub fn restore(&self, text: &str) -> Result<String, String> {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for token in lex(text) {
            let word = match &token.kind {
                TokenKind::String { value, .. } => value,
                TokenKind::Ident(word) => word,
                _ => continue,
            };
            if !word.contains("__v") && !word.contains("__k") {
                continue;
            }
            let replacement = self.restore_word(word, &token)?;
            if let Some(replacement) = replacement {
                out.push_str(&text[cursor..token.span.start]);
                out.push_str(&replacement);
                cursor = token.span.end;
            }
        }
        out.push_str(&text[cursor..]);
        Ok(out)
    }

    fn restore_word(&self, word: &str, token: &Token) -> Result<Option<String>, String> {
        let placeholders = placeholders_in(word);
        if placeholders.is_empty() {
            return Ok(None);
        }
        let find = |placeholder: &str| {
            self.hidden
                .iter()
                .find(|hidden| hidden.placeholder == placeholder)
                .ok_or_else(|| format!("Unknown placeholder {placeholder}."))
        };
        if let [(range, placeholder)] = placeholders.as_slice()
            && !is_key_kind(placeholder)
        {
            if *range != (0..word.len()) {
                return Err(format!(
                    "Placeholder {placeholder} must be used alone, not inside other text."
                ));
            }
            return Ok(Some(find(placeholder)?.original.clone()));
        }
        let mut restored = String::new();
        let mut last = 0;
        for (range, placeholder) in &placeholders {
            if !is_key_kind(placeholder) {
                return Err(format!(
                    "Placeholder {placeholder} must be used alone, not inside other text."
                ));
            }
            restored.push_str(&word[last..range.start]);
            restored.push_str(&find(placeholder)?.original);
            last = range.end;
        }
        restored.push_str(&word[last..]);
        let is_ident = matches!(token.kind, TokenKind::Ident(_))
            && restored.chars().next().is_some_and(is_ident_start)
            && restored.chars().all(|c| is_ident_continue(c) || c == '.');
        Ok(Some(if is_ident { restored } else { quote(&restored) }))
    }
}

fn is_key_kind(placeholder: &str) -> bool {
    placeholder.starts_with("__k")
}

/// Placeholders (`__v12_Date__`, `__k3__`) in `word`, with their byte ranges.
fn placeholders_in(word: &str) -> Vec<(Range<usize>, String)> {
    let bytes = word.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = word[at..].find("__") {
        let start = at + offset;
        let mut pos = start + 2;
        let kind = bytes.get(pos).copied();
        if !matches!(kind, Some(b'v' | b'k')) {
            at = start + 1;
            continue;
        }
        pos += 1;
        let digits = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == digits {
            at = start + 1;
            continue;
        }
        if kind == Some(b'v') {
            if bytes.get(pos) != Some(&b'_') {
                at = start + 1;
                continue;
            }
            pos += 1;
            let name = pos;
            while bytes.get(pos).is_some_and(u8::is_ascii_alphanumeric) {
                pos += 1;
            }
            if pos == name {
                at = start + 1;
                continue;
            }
        }
        if !word[pos..].starts_with("__") {
            at = start + 1;
            continue;
        }
        let end = pos + 2;
        found.push((start..end, word[start..end].to_owned()));
        at = end;
    }
    found
}

/// The key when the token at `at` is one (a string or word before `:`).
fn key_at(tokens: &[Token], at: usize) -> Option<&str> {
    if tokens.get(at + 1).map(|token| &token.kind) != Some(&TokenKind::Colon) {
        return None;
    }
    match &tokens[at].kind {
        TokenKind::String { value, .. } => Some(value),
        TokenKind::Ident(word) => Some(word),
        _ => None,
    }
}

/// Index of the last token of the value starting at `at`: a call's closing
/// parenthesis (`ObjectId(…)`, `new Date(…)`), the number after a minus, or
/// the token itself.
fn value_end(tokens: &[Token], at: usize) -> usize {
    let kind = |offset: usize| tokens.get(at + offset).map(|token| &token.kind);
    match kind(0) {
        Some(TokenKind::Ident(word))
            if word == "new" && matches!(kind(1), Some(TokenKind::Ident(_))) =>
        {
            if kind(2) == Some(&TokenKind::LParen) {
                closing_paren(tokens, at + 2)
            } else {
                at + 1
            }
        }
        Some(TokenKind::Ident(_)) if kind(1) == Some(&TokenKind::LParen) => {
            closing_paren(tokens, at + 1)
        }
        Some(TokenKind::Minus)
            if matches!(kind(1), Some(TokenKind::Number(_) | TokenKind::Ident(_))) =>
        {
            at + 1
        }
        _ => at,
    }
}

/// The matching `)` for the `(` at `open`, or the last token when it's
/// missing.
fn closing_paren(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            _ => {}
        }
    }
    tokens.len() - 1
}

/// Comments can hold values too; blank them, keeping byte offsets.
fn without_comments(text: &str) -> String {
    let mut out = text.to_owned();
    for token in lex(text) {
        if token.kind == TokenKind::Comment {
            out.replace_range(token.span.clone(), &" ".repeat(token.span.len()));
        }
    }
    out
}

fn quote(text: &str) -> String {
    format_value(&Bson::String(text.to_owned()), Style::Compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_value;
    use proptest::prelude::*;

    fn masked(schema: &[&str], text: &str) -> (String, Masks) {
        let mut masks = Masks::new(schema.iter().copied());
        let out = masks.mask(&[text]).remove(0);
        (out, masks)
    }

    #[test]
    fn every_literal_kind_is_hidden_and_restored() {
        let text = "{ a: 'x', b: -1.5, c: true, d: null, e: /^ab/i, \
                    f: ObjectId('507f1f77bcf86cd799439011'), g: ISODate('2024-01-02T00:00:00Z'), \
                    h: new Date('2024-01-02'), i: NumberLong(5), j: [1, 'two'], k: Infinity }";
        let (out, masks) = masked(&[], text);
        for leak in [
            "'x'",
            "1.5",
            "true",
            "null",
            "^ab",
            "507f",
            "2024",
            "NumberLong",
            "two",
            "Infinity",
        ] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        assert!(out.contains("'__v6_ObjectId__'"), "{out}");
        assert!(out.contains("'__v7_Date__'"), "{out}");
        assert_eq!(masks.restore(&out).unwrap(), text);
    }

    #[test]
    fn same_value_keeps_its_placeholder_across_texts_and_turns() {
        let mut masks = Masks::new(Vec::<&str>::new());
        let out = masks.mask(&["{ a: 'x', b: 'x' }", "{ c: 'x' }"]);
        assert_eq!(out[0], "{ a: '__v1_String__', b: '__v1_String__' }");
        assert_eq!(out[1], "{ c: '__v1_String__' }");
        let again = masks.mask(&["{ d: 'x', e: 2 }"]);
        assert_eq!(again[0], "{ d: '__v1_String__', e: '__v2_Int32__' }");
    }

    #[test]
    fn known_refs_stay_unknown_dollar_strings_are_hidden() {
        let text = "[{ $group: { _id: '$status', total: { $sum: '$price' } } }, \
                    { $match: { total: { $gt: 5 }, label: '$5.00', who: '$nobody' } }, \
                    { $project: { t: '$total', s: '$_id.status', r: '$$ROOT', x: '$$this.price' } }]";
        let (out, masks) = masked(&["status", "price"], text);
        for kept in [
            "'$status'",
            "'$price'",
            "'$total'",
            "'$_id.status'",
            "'$$ROOT'",
            "'$$this.price'",
        ] {
            assert!(out.contains(kept), "{kept} missing: {out}");
        }
        assert!(!out.contains("$5.00"), "{out}");
        assert!(!out.contains("$nobody"), "{out}");
        assert_eq!(masks.restore(&out).unwrap(), text);
    }

    #[test]
    fn dynamic_key_segments_are_hidden() {
        let text = "{ 'scores.507f1f77bcf86cd799439011': 1, 'users.bob@x.com': true, 'byDay.2024-01-02.n': 3 }";
        let (out, masks) = masked(&[], text);
        assert!(!out.contains("507f"), "{out}");
        assert!(!out.contains("bob"), "{out}");
        assert!(!out.contains("2024"), "{out}");
        assert!(out.contains("'scores.__k1__'"), "{out}");
        let restored = masks.restore(&out).unwrap();
        assert_eq!(parse_value(&restored), parse_value(text));
    }

    #[test]
    fn comments_are_dropped() {
        let (out, _) = masked(&[], "{ a: 1 // bob@x.com\n }");
        assert!(!out.contains("bob"), "{out}");
    }

    #[test]
    fn restore_accepts_unquoted_placeholders() {
        let (_, masks) = masked(&[], "{ a: 20 }");
        assert_eq!(
            masks.restore("{ limit: __v1_Int32__ }").unwrap(),
            "{ limit: 20 }"
        );
    }

    #[test]
    fn restore_rejects_unknown_and_embedded_placeholders() {
        let (_, masks) = masked(&[], "{ a: 'x' }");
        assert_eq!(
            masks.restore("{ a: '__v9_String__' }").unwrap_err(),
            "Unknown placeholder __v9_String__."
        );
        assert!(masks.restore("{ a: 'pre __v1_String__' }").is_err());
    }

    #[test]
    fn restored_key_is_quoted_when_needed() {
        let (_, masks) = masked(&[], "{ 'u.bob@x': 1 }");
        assert_eq!(
            masks.restore("{ u.__k1__: 1 }").unwrap(),
            "{ 'u.bob@x': 1 }"
        );
    }

    #[test]
    fn dynamic_segments() {
        for dynamic in [
            "507f1f77bcf86cd799439011",
            "123e4567-e89b-12d3-a456-426614174000",
            "2024",
            "2024-01",
            "2024-01-02T10:00",
            "bob@x",
        ] {
            assert!(is_dynamic_segment(dynamic), "{dynamic}");
        }
        for name in ["name", "createdAt", "_id", "v2", "cafe", "a1b2"] {
            assert!(!is_dynamic_segment(name), "{name}");
        }
    }

    #[test]
    fn collapse_merges_dynamic_and_crowded_keys() {
        let mut paths: Vec<(String, Vec<&str>)> = vec![
            ("scores".into(), vec!["Object"]),
            ("scores.507f1f77bcf86cd799439011".into(), vec!["Int32"]),
            ("scores.507f1f77bcf86cd799439012".into(), vec!["Double"]),
            ("tags".into(), vec!["Object"]),
        ];
        for n in 0..51 {
            paths.push((format!("tags.t{n}"), vec!["Boolean"]));
            paths.push((format!("tags.t{n}.on"), vec!["Boolean"]));
        }
        let collapsed = collapse_paths(
            paths
                .iter()
                .map(|(path, types)| (path.as_str(), types.as_slice())),
        );
        assert_eq!(
            collapsed,
            [
                ("scores".to_owned(), vec!["Object"]),
                ("scores.<key>".to_owned(), vec!["Int32", "Double"]),
                ("tags".to_owned(), vec!["Object"]),
                ("tags.<key>".to_owned(), vec!["Boolean"]),
                ("tags.<key>.on".to_owned(), vec!["Boolean"]),
            ]
        );
    }

    /// Values use letters no key, operator or placeholder type name uses.
    fn value() -> impl Strategy<Value = (String, Option<String>)> {
        let secret = "[QXZW#%]{2,8}";
        prop_oneof![
            secret.prop_map(|s| (format!("'{s}'"), Some(s))),
            secret.prop_map(|s| (format!("/{s}/i"), Some(s))),
            "[QXZW]{2,8}".prop_map(|s| (format!("'${s}'"), Some(s))),
            any::<i32>().prop_map(|n| (n.to_string(), None)),
            any::<bool>().prop_map(|b| (b.to_string(), None)),
            "[0-9a-f]{24}".prop_map(|h| (format!("ObjectId('{h}')"), Some(h))),
            (1970..2100i32).prop_map(|y| (format!("ISODate('{y}-03-04T05:06:07Z')"), None)),
        ]
    }

    fn document() -> impl Strategy<Value = (String, Vec<String>)> {
        let leaf =
            value().prop_map(|(text, secret)| (text, secret.into_iter().collect::<Vec<_>>()));
        leaf.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                prop::collection::vec(("[a-z]{1,6}", inner.clone()), 0..4).prop_map(|members| {
                    let mut secrets = Vec::new();
                    let body: Vec<String> = members
                        .into_iter()
                        .map(|(key, (text, mut s))| {
                            secrets.append(&mut s);
                            format!("{key}: {text}")
                        })
                        .collect();
                    (format!("{{ {} }}", body.join(", ")), secrets)
                }),
                prop::collection::vec(inner, 0..4).prop_map(|items| {
                    let mut secrets = Vec::new();
                    let body: Vec<String> = items
                        .into_iter()
                        .map(|(text, mut s)| {
                            secrets.append(&mut s);
                            text
                        })
                        .collect();
                    (format!("[{}]", body.join(", ")), secrets)
                }),
            ]
        })
    }

    proptest! {
        #[test]
        fn masked_text_holds_no_values((text, secrets) in document()) {
            let mut masks = Masks::new(["name", "price"]);
            let out = masks.mask(&[&text]).remove(0);
            for secret in &secrets {
                prop_assert!(!out.contains(secret.as_str()), "{} leaked in {}", secret, out);
            }
            for token in lex(&out) {
                prop_assert!(
                    !matches!(token.kind, TokenKind::Number(_) | TokenKind::Regex { .. } | TokenKind::Minus),
                    "literal left in {}", out
                );
                if let TokenKind::Ident(word) = &token.kind {
                    prop_assert!(!matches!(word.as_str(), "true" | "false" | "ObjectId" | "ISODate"), "{} left in {}", word, out);
                }
            }
            let restored = masks.restore(&out).unwrap();
            prop_assert_eq!(parse_value(&restored), parse_value(&text));
        }
    }
}
