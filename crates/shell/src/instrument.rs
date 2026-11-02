//! Rewrites a script before it runs: each top-level statement first tells
//! the engine it started (`__at`), expression statements hand their value
//! to `__result`, and `use` and `show` become calls. The whole script still
//! runs as one, so hoisting and `const` work as written.

use nordpil_query::lexer::{TokenKind, lex_script};
use nordpil_query::{Show, Statement, StatementKind, split_statements};

pub struct Instrumented {
    pub code: String,
    pub statements: Vec<Statement>,
    edits: Vec<Edit>,
}

/// Text put in place of `removed` bytes of the script at `original`.
struct Edit {
    original: usize,
    removed: usize,
    /// Where the new text starts in the code.
    at: usize,
    inserted: usize,
}

pub fn instrument(script: &str) -> Instrumented {
    let statements = split_statements(script);
    let mut code = String::with_capacity(script.len() + statements.len() * 32);
    let mut edits = Vec::new();
    let mut edit = |code: &mut String, original: usize, removed: usize, text: &str| {
        edits.push(Edit {
            original,
            removed,
            at: code.len(),
            inserted: text.len(),
        });
        code.push_str(text);
    };
    let mut copied = 0;
    for (index, statement) in statements.iter().enumerate() {
        let span = statement.span.clone();
        code.push_str(&script[copied..span.start]);
        let text = &script[span.clone()];
        let marker = format!(";__at({index});");
        match &statement.kind {
            StatementKind::Expression if wrappable(text) => {
                edit(
                    &mut code,
                    span.start,
                    0,
                    &format!("{marker}__result({index},("),
                );
                code.push_str(text);
                edit(&mut code, span.end, 0, "));");
            }
            StatementKind::Use(name) => {
                let name = serde_json::Value::String(name.clone());
                edit(
                    &mut code,
                    span.start,
                    span.len(),
                    &format!("{marker}__use({name});"),
                );
            }
            StatementKind::Show(show) => {
                let what = match show {
                    Show::Databases => "dbs",
                    Show::Collections => "collections",
                };
                let call = format!("{marker}__result({index},__show('{what}'));");
                edit(&mut code, span.start, span.len(), &call);
            }
            _ => {
                edit(&mut code, span.start, 0, &marker);
                code.push_str(text);
            }
        }
        copied = span.end;
    }
    code.push_str(&script[copied..]);
    Instrumented {
        code,
        statements,
        edits,
    }
}

/// Whether wrapping `text` in parentheses keeps its errors where they are:
/// brackets balance, literals end, and it doesn't stop mid-expression.
fn wrappable(text: &str) -> bool {
    let mut depth = 0usize;
    let mut last = None;
    for token in lex_script(text) {
        match &token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let Some(less) = depth.checked_sub(1) else {
                    return false;
                };
                depth = less;
            }
            TokenKind::String {
                terminated: false, ..
            }
            | TokenKind::Template { terminated: false }
            | TokenKind::Regex {
                terminated: false, ..
            }
            | TokenKind::Unknown => return false,
            TokenKind::Comment => continue,
            _ => {}
        }
        last = Some(token.kind);
    }
    depth == 0
        && !matches!(
            last,
            Some(TokenKind::Operator(_) | TokenKind::Dot | TokenKind::Comma | TokenKind::Colon)
        )
}

impl Instrumented {
    /// The script's byte offset for a byte offset in the code. Inside
    /// inserted text, the place it was inserted at.
    fn original_offset(&self, offset: usize) -> usize {
        let mut shift = 0isize;
        for edit in &self.edits {
            if offset < edit.at {
                break;
            }
            if offset < edit.at + edit.inserted {
                return edit.original;
            }
            shift += edit.inserted as isize - edit.removed as isize;
        }
        offset.saturating_add_signed(-shift)
    }

    /// The script's 1-based line and column for a 1-based line and column
    /// in the code. Columns count characters.
    pub fn original_position(&self, script: &str, line: usize, column: usize) -> (usize, usize) {
        let offset = offset_of(&self.code, line, column);
        let original = self.original_offset(offset).min(script.len());
        let original = (0..=original)
            .rev()
            .find(|&index| script.is_char_boundary(index))
            .unwrap_or(0);
        position_of(script, original)
    }
}

fn offset_of(text: &str, line: usize, column: usize) -> usize {
    let line_start = if line <= 1 {
        0
    } else {
        text.match_indices('\n')
            .nth(line - 2)
            .map_or(text.len(), |(index, _)| index + 1)
    };
    text[line_start..]
        .char_indices()
        .nth(column.saturating_sub(1))
        .map_or(text.len(), |(index, _)| line_start + index)
}

fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    (line, before[line_start..].chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_expressions_and_marks_every_statement() {
        let script = "const a = 1\ndb.x.find({ a })\nuse other\nshow dbs";
        let instrumented = instrument(script);
        assert_eq!(
            instrumented.code,
            ";__at(0);const a = 1\n;__at(1);__result(1,(db.x.find({ a })));\n\
             ;__at(2);__use(\"other\");\n;__at(3);__result(3,__show('dbs'));"
        );
    }

    #[test]
    fn leaves_broken_expressions_unwrapped() {
        let instrumented = instrument("db.x.find({");
        assert_eq!(instrumented.code, ";__at(0);db.x.find({");
    }

    #[test]
    fn maps_positions_back_to_the_script() {
        let script = "a()\nb(  c )";
        let instrumented = instrument(script);
        // `c` in the code: line 2, after `;__at(1);__result(1,(b(  `.
        let column = ";__at(1);__result(1,(b(  ".len() + 1;
        assert_eq!(instrumented.original_position(script, 2, column), (2, 5));
        // Inside the inserted text: where it was inserted.
        assert_eq!(instrumented.original_position(script, 2, 3), (2, 1));
    }
}
