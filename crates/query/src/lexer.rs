//! Splits query text into tokens with byte spans. Never fails: problems
//! become [`TokenKind::Unknown`] tokens or `terminated: false` literals, so
//! highlighting and autocomplete work on half-typed input.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Colon,
    Comma,
    Minus,
    /// The unescaped value. `error` describes a bad escape sequence.
    String {
        value: String,
        terminated: bool,
        error: Option<String>,
    },
    /// The literal as written, e.g. `1.5e3` or `0x1F`.
    Number(String),
    /// Letters, digits, `_`, `$` and inner dots, e.g. `$gt` or `address.city`.
    Ident(String),
    /// `\/` in the pattern is unescaped to `/`.
    Regex {
        pattern: String,
        flags: String,
        terminated: bool,
    },
    Comment,
    /// Script mode only: the `.` of a member access, e.g. in `db.users`.
    Dot,
    /// Script mode only.
    Semicolon,
    /// Script mode only: a run of operator characters, e.g. `=`, `=>`,
    /// `===`, `&&` or `-`.
    Operator(String),
    /// Script mode only: a backtick template, `${…}` parts included.
    Template {
        terminated: bool,
    },
    Unknown,
}

impl TokenKind {
    /// Whether this token can end a value. A `/` after such a token would be
    /// division rather than a regex.
    fn ends_value(&self) -> bool {
        matches!(
            self,
            Self::RBrace
                | Self::RBracket
                | Self::RParen
                | Self::String { .. }
                | Self::Number(_)
                | Self::Ident(_)
                | Self::Regex { .. }
                | Self::Template { .. }
        )
    }
}

/// Script-mode words after which a value starts, so a `/` after them opens
/// a regex.
const BEFORE_VALUE: &[&str] = &[
    "return",
    "typeof",
    "case",
    "do",
    "else",
    "in",
    "instanceof",
    "new",
    "of",
    "throw",
    "void",
    "yield",
    "await",
    "delete",
];

/// Characters that make up script-mode operators. `/` is handled apart,
/// since it can also start a regex or a comment.
fn is_operator_char(c: char) -> bool {
    matches!(
        c,
        '=' | '+' | '-' | '*' | '%' | '<' | '>' | '!' | '&' | '|' | '?' | '^' | '~'
    )
}

pub fn lex(text: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        text,
        pos: 0,
        tokens: Vec::new(),
        script: false,
    };
    lexer.run();
    lexer.tokens
}

/// Lexes JavaScript for the shell: identifiers stop at dots, and `.`, `;`,
/// operators and template literals get their own tokens.
pub fn lex_script(text: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        text,
        pos: 0,
        tokens: Vec::new(),
        script: true,
    };
    lexer.run();
    lexer.tokens
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    script: bool,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.text[self.pos..].chars();
        chars.next();
        chars.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat_while(&mut self, f: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&f) {
            self.bump();
        }
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: start..self.pos,
        });
    }

    fn previous_ends_value(&self) -> bool {
        self.tokens
            .iter()
            .rev()
            .find(|token| token.kind != TokenKind::Comment)
            .is_some_and(|token| match &token.kind {
                TokenKind::Ident(word) if self.script => !BEFORE_VALUE.contains(&word.as_str()),
                kind => kind.ends_value(),
            })
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            if c.is_whitespace() {
                self.bump();
                continue;
            }
            let kind = match c {
                '-' | '/' | '.' | ';' | '`' if self.script => match self.script_token(c) {
                    Some(kind) => kind,
                    None => continue,
                },
                c if self.script && is_operator_char(c) => {
                    self.eat_while(is_operator_char);
                    TokenKind::Operator(self.text[start..self.pos].to_owned())
                }
                '{' | '}' | '[' | ']' | '(' | ')' | ':' | ',' | '-' => {
                    self.bump();
                    match c {
                        '{' => TokenKind::LBrace,
                        '}' => TokenKind::RBrace,
                        '[' => TokenKind::LBracket,
                        ']' => TokenKind::RBracket,
                        '(' => TokenKind::LParen,
                        ')' => TokenKind::RParen,
                        ':' => TokenKind::Colon,
                        ',' => TokenKind::Comma,
                        _ => TokenKind::Minus,
                    }
                }
                '"' | '\'' => self.string(c),
                '/' if self.peek_second() == Some('/') => {
                    self.eat_while(|c| c != '\n');
                    TokenKind::Comment
                }
                '/' if self.peek_second() == Some('*') => {
                    self.bump();
                    self.bump();
                    match self.text[self.pos..].find("*/") {
                        Some(end) => self.pos += end + 2,
                        None => self.pos = self.text.len(),
                    }
                    TokenKind::Comment
                }
                '/' if !self.previous_ends_value() => self.regex(),
                '0'..='9' => self.number(),
                '.' if self.peek_second().is_some_and(|c| c.is_ascii_digit()) => self.number(),
                c if is_ident_start(c) => {
                    self.bump();
                    if self.script {
                        self.eat_while(is_ident_continue);
                    } else {
                        // Dots make paths like `address.city` one token, also mid-typing (`address.`).
                        self.eat_while(|c| is_ident_continue(c) || c == '.');
                    }
                    TokenKind::Ident(self.text[start..self.pos].to_owned())
                }
                _ => {
                    self.bump();
                    TokenKind::Unknown
                }
            };
            self.push(kind, start);
        }
    }

    /// Script-mode tokens that start with a character value mode reads
    /// differently. `None` when the character belongs to a token value mode
    /// reads the same way; the caller then reads it without a script token.
    fn script_token(&mut self, c: char) -> Option<TokenKind> {
        let start = self.pos;
        let second = self.peek_second();
        Some(match c {
            '.' if second.is_some_and(|c| c.is_ascii_digit()) => self.number(),
            '.' => {
                self.bump();
                TokenKind::Dot
            }
            ';' => {
                self.bump();
                TokenKind::Semicolon
            }
            '`' => self.template(),
            '/' if matches!(second, Some('/' | '*')) || !self.previous_ends_value() => {
                return self.read_shared(start);
            }
            _ => {
                // `-` or a division.
                self.bump();
                self.eat_while(is_operator_char);
                TokenKind::Operator(self.text[start..self.pos].to_owned())
            }
        })
    }

    /// Reads a comment or regex at `start` and pushes it, as value mode does.
    fn read_shared(&mut self, start: usize) -> Option<TokenKind> {
        let kind = if self.peek_second() == Some('/') {
            self.eat_while(|c| c != '\n');
            TokenKind::Comment
        } else if self.peek_second() == Some('*') {
            self.bump();
            self.bump();
            match self.text[self.pos..].find("*/") {
                Some(end) => self.pos += end + 2,
                None => self.pos = self.text.len(),
            }
            TokenKind::Comment
        } else {
            self.regex()
        };
        self.push(kind, start);
        None
    }

    /// A template literal. Strings, templates and braces inside `${…}` are
    /// skipped whole, so a backtick or brace in them doesn't end it.
    fn template(&mut self) -> TokenKind {
        self.bump();
        loop {
            match self.bump() {
                None => return TokenKind::Template { terminated: false },
                Some('`') => return TokenKind::Template { terminated: true },
                Some('\\') => {
                    self.bump();
                }
                Some('$') if self.peek() == Some('{') => {
                    self.bump();
                    if !self.substitution() {
                        return TokenKind::Template { terminated: false };
                    }
                }
                Some(_) => {}
            }
        }
    }

    /// Skips a template's `${…}` after its `{`. False if the text ends first.
    fn substitution(&mut self) -> bool {
        let mut depth = 1;
        while let Some(c) = self.peek() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return true;
                    }
                }
                '\'' | '"' => {
                    self.string(c);
                    continue;
                }
                '`' => {
                    self.template();
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
        false
    }

    fn string(&mut self, quote: char) -> TokenKind {
        self.bump();
        let mut value = String::new();
        let mut error = None;
        loop {
            match self.bump() {
                None | Some('\n') => {
                    return TokenKind::String {
                        value,
                        terminated: false,
                        error,
                    };
                }
                Some(c) if c == quote => {
                    return TokenKind::String {
                        value,
                        terminated: true,
                        error,
                    };
                }
                Some('\\') => match self.escape() {
                    Ok(Some(c)) => value.push(c),
                    Ok(None) => {}
                    Err(message) => {
                        error.get_or_insert(message);
                    }
                },
                Some(c) => value.push(c),
            }
        }
    }

    /// Reads an escape after `\`. `Ok(None)` is a line continuation.
    fn escape(&mut self) -> Result<Option<char>, String> {
        let Some(c) = self.bump() else {
            return Err("Unfinished escape sequence.".to_owned());
        };
        Ok(Some(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'v' => '\u{b}',
            '0' => '\0',
            '\n' => return Ok(None),
            'x' => self.hex_escape(2)?,
            'u' if self.peek() == Some('{') => {
                self.bump();
                let start = self.pos;
                self.eat_while(|c| c.is_ascii_hexdigit());
                let digits = &self.text[start..self.pos];
                if self.bump() != Some('}') {
                    return Err("Expected } to end the \\u{...} escape.".to_owned());
                }
                char_from_hex(digits)?
            }
            'u' => self.hex_escape(4)?,
            other => other,
        }))
    }

    fn hex_escape(&mut self, len: usize) -> Result<char, String> {
        let start = self.pos;
        for _ in 0..len {
            match self.peek() {
                Some(c) if c.is_ascii_hexdigit() => {
                    self.bump();
                }
                _ => return Err(format!("Expected {len} hex digits in the escape.")),
            }
        }
        char_from_hex(&self.text[start..self.pos])
    }

    fn regex(&mut self) -> TokenKind {
        self.bump();
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            match self.bump() {
                None | Some('\n') => {
                    return TokenKind::Regex {
                        pattern,
                        flags: String::new(),
                        terminated: false,
                    };
                }
                Some('/') if !in_class => break,
                Some('\\') => match self.bump() {
                    Some('/') => pattern.push('/'),
                    Some(c) => {
                        pattern.push('\\');
                        pattern.push(c);
                    }
                    None => pattern.push('\\'),
                },
                Some(c) => {
                    match c {
                        '[' => in_class = true,
                        ']' => in_class = false,
                        _ => {}
                    }
                    pattern.push(c);
                }
            }
        }
        let start = self.pos;
        self.eat_while(|c| c.is_ascii_alphabetic());
        TokenKind::Regex {
            pattern,
            flags: self.text[start..self.pos].to_owned(),
            terminated: true,
        }
    }

    fn number(&mut self) -> TokenKind {
        let start = self.pos;
        if self.peek() == Some('0') && matches!(self.peek_second(), Some('x' | 'X')) {
            self.bump();
            self.bump();
            self.eat_while(|c| c.is_ascii_hexdigit() || c == '_');
        } else {
            self.eat_while(|c| c.is_ascii_digit() || c == '_');
            if self.peek() == Some('.') {
                self.bump();
                self.eat_while(|c| c.is_ascii_digit() || c == '_');
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                self.eat_while(|c| c.is_ascii_digit());
            }
        }
        // Letters glued to a number (e.g. `12abc`) are part of the bad literal.
        self.eat_while(is_ident_continue);
        TokenKind::Number(self.text[start..self.pos].to_owned())
    }
}

fn char_from_hex(digits: &str) -> Result<char, String> {
    u32::from_str_radix(digits, 16)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| format!("\\u{{{digits}}} is not a valid character."))
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

pub fn is_ident_continue(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        lex(text).into_iter().map(|token| token.kind).collect()
    }

    fn string(value: &str) -> TokenKind {
        TokenKind::String {
            value: value.to_owned(),
            terminated: true,
            error: None,
        }
    }

    #[test]
    fn lexes_a_query() {
        assert_eq!(
            kinds("{ age: { $gt: -5 }, 'a.b': \"x\" }"),
            [
                TokenKind::LBrace,
                TokenKind::Ident("age".to_owned()),
                TokenKind::Colon,
                TokenKind::LBrace,
                TokenKind::Ident("$gt".to_owned()),
                TokenKind::Colon,
                TokenKind::Minus,
                TokenKind::Number("5".to_owned()),
                TokenKind::RBrace,
                TokenKind::Comma,
                string("a.b"),
                TokenKind::Colon,
                string("x"),
                TokenKind::RBrace,
            ]
        );
    }

    #[test]
    fn unescapes_strings() {
        assert_eq!(kinds(r"'it\'s å\x41\u{1F600}'"), [string("it's åA😀")]);
    }

    #[test]
    fn marks_unterminated_string() {
        assert_eq!(
            kinds("'abc"),
            [TokenKind::String {
                value: "abc".to_owned(),
                terminated: false,
                error: None
            }]
        );
    }

    #[test]
    fn tells_regex_from_comments() {
        assert_eq!(
            kinds("/a\\/b[/]/i // note\n/* block */"),
            [
                TokenKind::Regex {
                    pattern: "a/b[/]".to_owned(),
                    flags: "i".to_owned(),
                    terminated: true
                },
                TokenKind::Comment,
                TokenKind::Comment,
            ]
        );
    }

    #[test]
    fn script_mode_splits_members_and_operators() {
        let kinds: Vec<_> = lex_script("const n = db.users.find({ a: -1 }).length / 2; // x")
            .into_iter()
            .map(|token| token.kind)
            .collect();
        let ident = |name: &str| TokenKind::Ident(name.to_owned());
        let operator = |text: &str| TokenKind::Operator(text.to_owned());
        assert_eq!(
            kinds,
            [
                ident("const"),
                ident("n"),
                operator("="),
                ident("db"),
                TokenKind::Dot,
                ident("users"),
                TokenKind::Dot,
                ident("find"),
                TokenKind::LParen,
                TokenKind::LBrace,
                ident("a"),
                TokenKind::Colon,
                operator("-"),
                TokenKind::Number("1".to_owned()),
                TokenKind::RBrace,
                TokenKind::RParen,
                TokenKind::Dot,
                ident("length"),
                operator("/"),
                TokenKind::Number("2".to_owned()),
                TokenKind::Semicolon,
                TokenKind::Comment,
            ]
        );
    }

    #[test]
    fn script_mode_reads_templates_and_regex_after_keywords() {
        let kinds: Vec<_> = lex_script("`a ${ { b: '`' } } c` return /x/")
            .into_iter()
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                TokenKind::Template { terminated: true },
                TokenKind::Ident("return".to_owned()),
                TokenKind::Regex {
                    pattern: "x".to_owned(),
                    flags: String::new(),
                    terminated: true
                },
            ]
        );
    }

    #[test]
    fn spans_are_byte_ranges() {
        let tokens = lex("{ å: 1 }");
        assert_eq!(tokens[1].span, 2..4);
        assert_eq!(tokens[3].span, 6..7);
    }
}
