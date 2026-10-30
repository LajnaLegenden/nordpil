//! Builds a syntax tree from tokens. It recovers from errors, so it always
//! returns as much of the tree as it could read plus a list of errors.

use std::ops::Range;

use crate::lexer::{Token, TokenKind, lex};

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    String(String),
    /// The literal as written.
    Number(String),
    Regex {
        pattern: String,
        flags: String,
    },
    /// `true`, `NaN`, `MinKey` and other bare words.
    Ident(String),
    Call {
        callee: String,
        /// Written with `new`.
        new: bool,
        args: Vec<Node>,
    },
    Negative(Box<Node>),
    /// Where a value was expected but could not be read.
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_span: Range<usize>,
    pub value: Node,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// `None` for empty input (only whitespace and comments).
    pub root: Option<Node>,
    pub errors: Vec<ParseError>,
}

/// Deeper nesting is an error, so hostile input can't overflow the stack.
const MAX_DEPTH: usize = 200;

pub fn parse(text: &str) -> Parsed {
    let tokens: Vec<Token> = lex(text)
        .into_iter()
        .filter(|token| token.kind != TokenKind::Comment)
        .collect();
    let mut parser = Parser {
        tokens,
        pos: 0,
        end: text.len(),
        depth: 0,
        errors: Vec::new(),
    };
    let root = (!parser.at_end()).then(|| parser.value());
    if let Some(token) = parser.tokens.get(parser.pos) {
        let span = token.span.start..parser.end;
        parser.error("Unexpected text after the value.", span);
    }
    Parsed {
        root,
        errors: parser.errors,
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    end: usize,
    depth: usize,
    errors: Vec<ParseError>,
}

impl Parser {
    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|token| &token.kind)
    }

    fn peek_at(&self, offset: usize) -> Option<&TokenKind> {
        self.tokens.get(self.pos + offset).map(|token| &token.kind)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// Span of the next token, or an empty span at the end of the input.
    fn here(&self) -> Range<usize> {
        self.tokens
            .get(self.pos)
            .map_or(self.end..self.end, |token| token.span.clone())
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }

    fn eat(&mut self, kind: &TokenKind) -> Option<Range<usize>> {
        (self.peek() == Some(kind)).then(|| self.bump().span)
    }

    /// At a closing bracket of any kind, or the end. A missing comma isn't
    /// reported there, since the wrong or missing closer is reported instead.
    fn at_closer(&self) -> bool {
        matches!(
            self.peek(),
            Some(TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen) | None
        )
    }

    fn error(&mut self, message: impl Into<String>, span: Range<usize>) {
        self.errors.push(ParseError {
            message: message.into(),
            span,
        });
    }

    fn missing(&mut self, message: &str) -> Node {
        let span = self.here();
        self.error(message, span.clone());
        Node {
            kind: NodeKind::Missing,
            span: span.start..span.start,
        }
    }

    fn value(&mut self) -> Node {
        self.depth += 1;
        let node = if self.depth > MAX_DEPTH {
            let span = self.here().start..self.end;
            self.error("Nested too deeply.", span.clone());
            self.pos = self.tokens.len();
            Node {
                kind: NodeKind::Missing,
                span,
            }
        } else {
            self.value_inner()
        };
        self.depth -= 1;
        node
    }

    fn value_inner(&mut self) -> Node {
        let Some(kind) = self.peek().cloned() else {
            return self.missing("Expected a value.");
        };
        match kind {
            TokenKind::LBrace => self.object(),
            TokenKind::LBracket => self.array(),
            TokenKind::Minus => {
                let start = self.bump().span.start;
                let operand = self.value();
                Node {
                    span: start..operand.span.end,
                    kind: NodeKind::Negative(Box::new(operand)),
                }
            }
            TokenKind::Ident(name) if name == "new" => {
                let start = self.bump().span.start;
                match self.peek().cloned() {
                    Some(TokenKind::Ident(callee))
                        if self.peek_at(1) == Some(&TokenKind::LParen) =>
                    {
                        self.bump();
                        self.call(callee, true, start)
                    }
                    _ => self.missing("Expected a constructor call after new, e.g. new Date()."),
                }
            }
            TokenKind::Ident(callee) if self.peek_at(1) == Some(&TokenKind::LParen) => {
                let start = self.bump().span.start;
                self.call(callee, false, start)
            }
            TokenKind::Ident(name) => self.leaf(NodeKind::Ident(name)),
            TokenKind::Number(text) => self.leaf(NodeKind::Number(text)),
            TokenKind::String {
                value,
                terminated,
                error,
            } => {
                let span = self.here();
                if !terminated {
                    self.error("This string is missing its closing quote.", span.clone());
                }
                if let Some(message) = error {
                    self.error(message, span);
                }
                self.leaf(NodeKind::String(value))
            }
            TokenKind::Regex {
                pattern,
                flags,
                terminated,
            } => {
                if !terminated {
                    let span = self.here();
                    self.error("This regex is missing its closing /.", span);
                }
                self.leaf(NodeKind::Regex { pattern, flags })
            }
            TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen | TokenKind::Comma => {
                // Leave closers for the enclosing object or array to handle.
                self.missing("Expected a value.")
            }
            TokenKind::Colon | TokenKind::LParen | TokenKind::Unknown | TokenKind::Comment => {
                let span = self.bump().span;
                self.error("Expected a value.", span.clone());
                Node {
                    kind: NodeKind::Missing,
                    span,
                }
            }
        }
    }

    fn leaf(&mut self, kind: NodeKind) -> Node {
        Node {
            kind,
            span: self.bump().span,
        }
    }

    fn object(&mut self) -> Node {
        let start = self.bump().span.start;
        let mut members = Vec::new();
        loop {
            if let Some(span) = self.eat(&TokenKind::RBrace) {
                return Node {
                    kind: NodeKind::Object(members),
                    span: start..span.end,
                };
            }
            let Some(kind) = self.peek().cloned() else {
                self.error("Missing } to close this object.", start..self.end);
                return Node {
                    kind: NodeKind::Object(members),
                    span: start..self.end,
                };
            };
            let key = match kind {
                TokenKind::Ident(name) | TokenKind::Number(name) => name,
                TokenKind::String { value, .. } => value,
                TokenKind::RBracket | TokenKind::RParen => {
                    let span = self.here();
                    self.error("Expected } to close this object.", span);
                    return Node {
                        kind: NodeKind::Object(members),
                        span: start..self.here().start,
                    };
                }
                TokenKind::Colon => {
                    // `{ : 1 }`: report the missing name once and skip its value.
                    let span = self.bump().span;
                    self.error("Expected a field name.", span);
                    if !self.at_closer() && self.peek() != Some(&TokenKind::Comma) {
                        self.value();
                    }
                    self.eat(&TokenKind::Comma);
                    continue;
                }
                _ => {
                    let span = self.bump().span;
                    self.error("Expected a field name.", span);
                    continue;
                }
            };
            let key_span = self.bump().span;

            let value = if self.eat(&TokenKind::Colon).is_some() {
                self.value()
            } else if matches!(
                self.peek(),
                Some(TokenKind::Comma | TokenKind::RBrace) | None
            ) {
                self.missing("Expected : after the field name.")
            } else {
                // Read `{ a 1 }` as `{ a: 1 }` so one typo gives one error.
                let span = self.here();
                self.error("Expected : after the field name.", span);
                self.value()
            };
            members.push(Member {
                key,
                key_span,
                value,
            });

            if self.eat(&TokenKind::Comma).is_none() && !self.at_closer() {
                let span = self.here();
                self.error("Expected , or } after the value.", span);
                if !matches!(
                    self.peek(),
                    Some(TokenKind::Ident(_) | TokenKind::String { .. } | TokenKind::Number(_))
                ) {
                    self.bump();
                }
            }
        }
    }

    fn array(&mut self) -> Node {
        let start = self.bump().span.start;
        let mut items = Vec::new();
        loop {
            if let Some(span) = self.eat(&TokenKind::RBracket) {
                return Node {
                    kind: NodeKind::Array(items),
                    span: start..span.end,
                };
            }
            if self.at_end() {
                self.error("Missing ] to close this array.", start..self.end);
                return Node {
                    kind: NodeKind::Array(items),
                    span: start..self.end,
                };
            }
            if matches!(self.peek(), Some(TokenKind::RBrace | TokenKind::RParen)) {
                let span = self.here();
                self.error("Expected ] to close this array.", span);
                return Node {
                    kind: NodeKind::Array(items),
                    span: start..self.here().start,
                };
            }
            let before = self.pos;
            items.push(self.value());
            if self.eat(&TokenKind::Comma).is_none() && !self.at_closer() {
                let span = self.here();
                self.error("Expected , or ] after the value.", span);
                if self.pos == before {
                    self.bump();
                }
            }
        }
    }

    fn call(&mut self, callee: String, new: bool, start: usize) -> Node {
        self.bump(); // (
        let mut args = Vec::new();
        loop {
            if let Some(span) = self.eat(&TokenKind::RParen) {
                return Node {
                    kind: NodeKind::Call { callee, new, args },
                    span: start..span.end,
                };
            }
            if self.at_end() || matches!(self.peek(), Some(TokenKind::RBrace | TokenKind::RBracket))
            {
                let span = self.here();
                self.error("Missing ) to close the call.", span);
                return Node {
                    kind: NodeKind::Call { callee, new, args },
                    span: start..self.here().start,
                };
            }
            let before = self.pos;
            args.push(self.value());
            if self.eat(&TokenKind::Comma).is_none() && !self.at_closer() {
                let span = self.here();
                self.error("Expected , or ) after the argument.", span);
                if self.pos == before {
                    self.bump();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<String> {
        parse(text)
            .errors
            .into_iter()
            .map(|error| format!("{:?}: {}", error.span, error.message))
            .collect()
    }

    #[test]
    fn parses_nested_values() {
        let parsed = parse("{ a: [1, 'x'], b: new Date(5), c: -2 }");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let Some(Node {
            kind: NodeKind::Object(members),
            span,
        }) = parsed.root
        else {
            panic!("expected an object");
        };
        assert_eq!(span, 0..38);
        let keys: Vec<_> = members.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert!(matches!(
            &members[1].value.kind,
            NodeKind::Call { callee, new: true, args } if callee == "Date" && args.len() == 1
        ));
    }

    #[test]
    fn empty_input_has_no_root_and_no_errors() {
        let parsed = parse("  // just a comment\n");
        assert_eq!(parsed.root, None);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn allows_trailing_commas() {
        assert!(messages("{ a: 1, b: [1, 2,], }").is_empty());
    }

    #[test]
    fn reports_errors_while_typing() {
        insta::assert_debug_snapshot!(
            [
                "{ a: ",
                "{ a: 1",
                "{ a 1 }",
                "{ a: 1 b: 2 }",
                "{ a: [1, 2 }",
                "{ a: 'x }",
                "{ a: 1 } }",
                "{ : 1 }",
                "{ a: ObjectId('x' }",
            ]
            .map(|text| (text, messages(text)))
        );
    }

    #[test]
    fn survives_deep_nesting() {
        let text = "[".repeat(10_000);
        let parsed = parse(&text);
        assert!(
            parsed
                .errors
                .iter()
                .any(|e| e.message == "Nested too deeply.")
        );
    }
}
