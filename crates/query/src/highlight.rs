//! Highlight classes for query text, from tokens plus the token after them.

use std::ops::Range;

use crate::lexer::{TokenKind, lex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    Key,
    /// `$`-prefixed keys such as `$gt`.
    Operator,
    String,
    Number,
    Keyword,
    Function,
    Regex,
    Punctuation,
    Comment,
    Error,
}

/// Spans sorted by start, not overlapping. Bare words that are neither keys
/// nor known keywords get no span, so half-typed keys don't flash red.
pub fn highlight(text: &str) -> Vec<(Range<usize>, Highlight)> {
    let tokens = lex(text);
    let mut spans = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        let next = tokens[index + 1..]
            .iter()
            .map(|token| &token.kind)
            .find(|kind| **kind != TokenKind::Comment);
        let is_key = next == Some(&TokenKind::Colon);
        let class = match &token.kind {
            TokenKind::Ident(name) | TokenKind::String { value: name, .. } if is_key => {
                if name.starts_with('$') {
                    Highlight::Operator
                } else {
                    Highlight::Key
                }
            }
            TokenKind::Number(_) if is_key => Highlight::Key,
            TokenKind::Ident(_) if next == Some(&TokenKind::LParen) => Highlight::Function,
            TokenKind::Ident(name) => match name.as_str() {
                "true" | "false" | "null" | "new" | "NaN" | "Infinity" | "MinKey" | "MaxKey" => {
                    Highlight::Keyword
                }
                _ => continue,
            },
            TokenKind::String { .. } | TokenKind::Template { .. } => Highlight::String,
            TokenKind::Number(_) => Highlight::Number,
            TokenKind::Regex { .. } => Highlight::Regex,
            TokenKind::Comment => Highlight::Comment,
            TokenKind::Unknown => Highlight::Error,
            TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::Colon
            | TokenKind::Comma
            | TokenKind::Minus
            | TokenKind::Dot
            | TokenKind::Semicolon
            | TokenKind::Operator(_) => Highlight::Punctuation,
        };
        spans.push((token.span.clone(), class));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_keys_operators_and_values() {
        let text = "{ age: { $gt: 5 }, 'n': ObjectId('x'), ok: true } // c";
        let classes: Vec<_> = highlight(text)
            .into_iter()
            .filter(|(_, class)| *class != Highlight::Punctuation)
            .map(|(span, class)| (&text[span], class))
            .collect();
        assert_eq!(
            classes,
            [
                ("age", Highlight::Key),
                ("$gt", Highlight::Operator),
                ("5", Highlight::Number),
                ("'n'", Highlight::Key),
                ("ObjectId", Highlight::Function),
                ("'x'", Highlight::String),
                ("ok", Highlight::Key),
                ("true", Highlight::Keyword),
                ("// c", Highlight::Comment),
            ]
        );
    }
}
