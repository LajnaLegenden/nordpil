//! Shell scripts: top-level statements, mongosh's `use` and `show`
//! commands, highlighting and declared names.

use std::ops::Range;

use crate::highlight::Highlight;
use crate::lexer::{Token, TokenKind, lex_script};

/// Words highlighted as keywords in scripts.
const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "in",
    "Infinity",
    "instanceof",
    "let",
    "NaN",
    "new",
    "null",
    "of",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// Words that start a statement without a value.
const CONTROL: &[&str] = &[
    "if", "for", "while", "do", "switch", "try", "throw", "return", "break", "continue", "with",
    "debugger", "import", "export",
];

/// Words after which a statement goes on, even past a line break.
const CONTINUING: &[&str] = &[
    "else",
    "do",
    "try",
    "finally",
    "new",
    "typeof",
    "instanceof",
    "in",
    "of",
    "void",
    "delete",
    "await",
    "extends",
];

/// Words whose `(…)` is a header followed by a body: `if (a)` then `b()`.
const HEADERS: &[&str] = &["if", "for", "while", "with", "switch", "catch"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// From the first token to the last, without a closing `;`.
    pub span: Range<usize>,
    pub kind: StatementKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementKind {
    /// Its value is shown, e.g. `db.users.find()`.
    Expression,
    /// `const`, `let`, `var`, `function` or `class`.
    Declaration,
    /// `if`, loops, blocks and other statements without a value.
    Control,
    /// mongosh's `use <database>`.
    Use(String),
    /// mongosh's `show dbs` or `show collections`.
    Show(Show),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Databases,
    Collections,
}

/// What the splitter knows about the statement being read.
#[derive(Default)]
struct Reading {
    /// The `(` at depth 0 that is open belongs to `if`, `for`, ….
    header_open: bool,
    /// The last token closed such a header, so a body follows.
    header_closed: bool,
    /// A `{` at depth 0 was seen.
    braced: bool,
}

/// Splits a script into top-level statements. A statement ends at a `;`
/// or at a line break where JavaScript would end it too, roughly: not
/// inside brackets, not after an operator, and not before `.`, an operator
/// or `else`. Never fails; half-typed text gives statements that won't run.
pub fn split_statements(text: &str) -> Vec<Statement> {
    let tokens: Vec<Token> = lex_script(text)
        .into_iter()
        .filter(|token| token.kind != TokenKind::Comment)
        .collect();
    let mut statements = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut reading = Reading::default();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind == TokenKind::Semicolon && depth == 0 {
            if let Some(first) = start.take() {
                statements.push(statement(text, &tokens[first..index]));
            }
            reading = Reading::default();
            continue;
        }
        let first = *start.get_or_insert(index);
        reading.header_closed = false;
        match &token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                if depth == 0 {
                    match token.kind {
                        TokenKind::LBrace => reading.braced = true,
                        TokenKind::LParen => {
                            reading.header_open =
                                index > first && is_header(&tokens[index - 1], &tokens[first]);
                        }
                        _ => {}
                    }
                }
                depth += 1;
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 && token.kind == TokenKind::RParen {
                    reading.header_closed = std::mem::take(&mut reading.header_open);
                }
            }
            _ => {}
        }
        if depth > 0 {
            continue;
        }
        let Some(next) = tokens.get(index + 1) else {
            continue;
        };
        let line_break = text[token.span.end..next.span.start].contains('\n');
        if line_break && !continues(&tokens[first..=index], next, &reading) {
            statements.push(statement(text, &tokens[first..=index]));
            start = None;
            reading = Reading::default();
        }
    }
    if let Some(first) = start {
        statements.push(statement(text, &tokens[first..]));
    }
    statements
}

fn word(token: &Token) -> Option<&str> {
    match &token.kind {
        TokenKind::Ident(word) => Some(word),
        _ => None,
    }
}

/// Whether `before`, the token before a `(`, makes it a header.
fn is_header(before: &Token, first: &Token) -> bool {
    match word(before) {
        // The `while` of `do … while (…)` ends the statement.
        Some("while") => word(first) != Some("do"),
        Some(word) => HEADERS.contains(&word),
        None => false,
    }
}

/// The first word of a statement, looking past `async`.
fn head(tokens: &[Token]) -> Option<&str> {
    match word(tokens.first()?)? {
        "async" => tokens.get(1).and_then(word),
        head => Some(head),
    }
}

/// Whether the statement in `tokens` goes on past the line break before `next`.
fn continues(tokens: &[Token], next: &Token, reading: &Reading) -> bool {
    let Some(last) = tokens.last() else {
        return false;
    };
    match &last.kind {
        TokenKind::Operator(op) if op != "++" && op != "--" => return true,
        TokenKind::Dot | TokenKind::Comma | TokenKind::Colon => return true,
        TokenKind::Ident(word) if CONTINUING.contains(&word.as_str()) => return true,
        _ => {}
    }
    if reading.header_closed {
        return true;
    }
    let head = head(tokens);
    // `function f()` or `class A extends B` with the body on the next line.
    if matches!(head, Some("function" | "class")) && !reading.braced {
        return true;
    }
    let block = tokens.first().map(|token| &token.kind) == Some(&TokenKind::LBrace)
        || head.is_some_and(|head| CONTROL.contains(&head) || matches!(head, "function" | "class"));
    match &next.kind {
        TokenKind::Dot => true,
        TokenKind::Operator(op) => !(op.starts_with(['!', '~']) || op == "++" || op == "--"),
        TokenKind::Ident(word) => match word.as_str() {
            "else" | "catch" | "finally" => true,
            "while" => head == Some("do"),
            _ => false,
        },
        // Like JavaScript, `a` then `(b)` on the next line is the call `a(b)`.
        TokenKind::LParen | TokenKind::LBracket | TokenKind::Template { .. } => !block,
        _ => false,
    }
}

fn statement(text: &str, tokens: &[Token]) -> Statement {
    let span = tokens[0].span.start..tokens[tokens.len() - 1].span.end;
    let kind = match head(tokens) {
        Some("const" | "let" | "var" | "function" | "class") => StatementKind::Declaration,
        Some(head) if CONTROL.contains(&head) => StatementKind::Control,
        Some("use") if tokens.len() >= 2 => {
            use_name(&tokens[1..], &text[tokens[1].span.start..span.end])
                .map_or(StatementKind::Expression, StatementKind::Use)
        }
        Some("show") if tokens.len() == 2 => match word(&tokens[1]) {
            Some("dbs" | "databases") => StatementKind::Show(Show::Databases),
            Some("collections" | "tables") => StatementKind::Show(Show::Collections),
            _ => StatementKind::Expression,
        },
        _ if tokens[0].kind == TokenKind::LBrace => StatementKind::Control,
        _ => StatementKind::Expression,
    };
    Statement { span, kind }
}

/// The database in `use <name>`: a quoted string, or the rest of the line
/// when it is one word that can't be JavaScript, e.g. `my-db`.
fn use_name(tokens: &[Token], rest: &str) -> Option<String> {
    if let [
        Token {
            kind:
                TokenKind::String {
                    value,
                    terminated: true,
                    ..
                },
            ..
        },
    ] = tokens
    {
        return Some(value.clone());
    }
    let rest = rest.trim();
    let valid = !rest.is_empty()
        && !rest
            .contains(|c: char| c.is_whitespace() || "()[]{}=,;:'\"`/\\.$+*%<>!&|?^~".contains(c));
    valid.then(|| rest.to_owned())
}

/// The statement to run for Ctrl+Enter: the one around `cursor`, else one
/// on the cursor's line, preferring the one before it.
pub fn statement_at(text: &str, statements: &[Statement], cursor: usize) -> Option<usize> {
    let same_line = |range: Range<usize>| text.get(range).is_some_and(|gap| !gap.contains('\n'));
    if let Some(index) = statements
        .iter()
        .position(|statement| statement.span.start <= cursor && cursor <= statement.span.end)
    {
        return Some(index);
    }
    let before = statements
        .iter()
        .rposition(|statement| statement.span.end <= cursor)
        .filter(|&index| same_line(statements[index].span.end..cursor));
    before.or_else(|| {
        statements
            .iter()
            .position(|statement| statement.span.start >= cursor)
            .filter(|&index| same_line(cursor..statements[index].span.start))
    })
}

/// Highlight spans for a script, sorted and not overlapping.
pub fn highlight_script(text: &str) -> Vec<(Range<usize>, Highlight)> {
    let tokens = lex_script(text);
    let commands: Vec<usize> = split_statements(text)
        .into_iter()
        .filter(|statement| {
            matches!(
                statement.kind,
                StatementKind::Use(_) | StatementKind::Show(_)
            )
        })
        .map(|statement| statement.span.start)
        .collect();
    let significant: Vec<&Token> = tokens
        .iter()
        .filter(|token| token.kind != TokenKind::Comment)
        .collect();
    let mut position = 0usize;
    let mut spans = Vec::with_capacity(tokens.len());
    for token in &tokens {
        let class = if token.kind == TokenKind::Comment {
            Highlight::Comment
        } else {
            let previous = position
                .checked_sub(1)
                .map(|index| &significant[index].kind);
            let next = significant.get(position + 1).map(|token| &token.kind);
            position += 1;
            match class(token, previous, next, &commands) {
                Some(class) => class,
                None => continue,
            }
        };
        spans.push((token.span.clone(), class));
    }
    spans
}

fn class(
    token: &Token,
    previous: Option<&TokenKind>,
    next: Option<&TokenKind>,
    commands: &[usize],
) -> Option<Highlight> {
    let is_key = next == Some(&TokenKind::Colon)
        && matches!(previous, Some(TokenKind::LBrace | TokenKind::Comma));
    Some(match &token.kind {
        TokenKind::Ident(name) | TokenKind::String { value: name, .. } if is_key => {
            if name.starts_with('$') {
                Highlight::Operator
            } else {
                Highlight::Key
            }
        }
        TokenKind::Number(_) if is_key => Highlight::Key,
        TokenKind::Ident(_) if commands.contains(&token.span.start) => Highlight::Keyword,
        TokenKind::Ident(name) if KEYWORDS.contains(&name.as_str()) => Highlight::Keyword,
        TokenKind::Ident(_) if next == Some(&TokenKind::LParen) => Highlight::Function,
        TokenKind::Ident(_) => return None,
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
    })
}

/// Names the script declares with `const`, `let`, `var`, `function` or
/// `class`, in order, without repeats. Destructured names count too.
pub fn declared_names(text: &str) -> Vec<String> {
    let tokens: Vec<TokenKind> = lex_script(text)
        .into_iter()
        .map(|token| token.kind)
        .filter(|kind| *kind != TokenKind::Comment)
        .collect();
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    };
    for (index, kind) in tokens.iter().enumerate() {
        let TokenKind::Ident(word) = kind else {
            continue;
        };
        if !matches!(
            word.as_str(),
            "const" | "let" | "var" | "function" | "class"
        ) {
            continue;
        }
        match tokens.get(index + 1) {
            Some(TokenKind::Ident(name)) => add(name),
            Some(TokenKind::LBrace | TokenKind::LBracket) => {
                let mut depth = 0usize;
                for (offset, kind) in tokens[index + 1..].iter().enumerate() {
                    match kind {
                        TokenKind::LBrace | TokenKind::LBracket => depth += 1,
                        TokenKind::RBrace | TokenKind::RBracket => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        TokenKind::Ident(name)
                            if tokens.get(index + offset + 2) != Some(&TokenKind::Colon) =>
                        {
                            add(name);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &str) -> Vec<(&str, StatementKind)> {
        split_statements(text)
            .into_iter()
            .map(|statement| (&text[statement.span], statement.kind))
            .collect()
    }

    #[test]
    fn splits_on_semicolons_and_line_breaks() {
        let text = "const a = 1; db.users.find({ a })\n// note\ndb.orders.countDocuments()";
        assert_eq!(
            split(text),
            [
                ("const a = 1", StatementKind::Declaration),
                ("db.users.find({ a })", StatementKind::Expression),
                ("db.orders.countDocuments()", StatementKind::Expression),
            ]
        );
    }

    #[test]
    fn keeps_chains_and_open_brackets_together() {
        let text = "db.users\n  .find({\n    age: 1\n  })\n  .sort({ a: 1 })\nx = 1 +\n  2";
        assert_eq!(
            split(text),
            [
                (
                    "db.users\n  .find({\n    age: 1\n  })\n  .sort({ a: 1 })",
                    StatementKind::Expression
                ),
                ("x = 1 +\n  2", StatementKind::Expression),
            ]
        );
    }

    #[test]
    fn keeps_control_statements_whole() {
        let text = "if (a)\n  b()\nelse\n  c()\nfor (const x of xs)\n  print(x)\ndo {\n} while (x)\nfunction f()\n{\n}\ntry { a() }\ncatch (e) {}\nfinally {}\nnext()";
        let kinds: Vec<_> = split(text).into_iter().map(|(_, kind)| kind).collect();
        assert_eq!(
            kinds,
            [
                StatementKind::Control,
                StatementKind::Control,
                StatementKind::Control,
                StatementKind::Declaration,
                StatementKind::Control,
                StatementKind::Expression,
            ]
        );
        assert_eq!(split(text)[5].0, "next()");
    }

    #[test]
    fn a_block_ends_before_a_parenthesis_but_an_expression_does_not() {
        assert_eq!(split("if (a) {}\n(b)").len(), 2);
        assert_eq!(split("a\n(b)").len(), 1);
    }

    #[test]
    fn reads_use_and_show() {
        assert_eq!(
            split("use my-db\nshow dbs\nshow collections\nuse 'x y'\nuse = 5"),
            [
                ("use my-db", StatementKind::Use("my-db".to_owned())),
                ("show dbs", StatementKind::Show(Show::Databases)),
                ("show collections", StatementKind::Show(Show::Collections)),
                ("use 'x y'", StatementKind::Use("x y".to_owned())),
                ("use = 5", StatementKind::Expression),
            ]
        );
    }

    #[test]
    fn finds_the_statement_at_the_cursor() {
        let text = "a()\nb() \n\nc()";
        let statements = split_statements(text);
        assert_eq!(statement_at(text, &statements, 1), Some(0));
        // After the end of `b()` on its line.
        assert_eq!(statement_at(text, &statements, 8), Some(1));
        // On the blank line.
        assert_eq!(statement_at(text, &statements, 9), None);
    }

    #[test]
    fn highlights_keywords_calls_keys_and_commands() {
        let text = "use shop\nconst n = db.users.find({ $or: [], age: 1 }) // c";
        let classes: Vec<_> = highlight_script(text)
            .into_iter()
            .filter(|(_, class)| *class != Highlight::Punctuation)
            .map(|(span, class)| (&text[span], class))
            .collect();
        assert_eq!(
            classes,
            [
                ("use", Highlight::Keyword),
                ("const", Highlight::Keyword),
                ("find", Highlight::Function),
                ("$or", Highlight::Operator),
                ("age", Highlight::Key),
                ("1", Highlight::Number),
                ("// c", Highlight::Comment),
            ]
        );
    }

    #[test]
    fn lists_declared_names() {
        let text =
            "const a = 1\nlet { b, c: d } = x\nfunction f() {}\nfor (const e of a) {}\nvar a";
        assert_eq!(declared_names(text), ["a", "b", "d", "f", "e"]);
    }
}
