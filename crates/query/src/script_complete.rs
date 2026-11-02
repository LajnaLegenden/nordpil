//! Completion in shell scripts: collections after `db.`, collection and
//! cursor methods, the filter completion inside query arguments, and the
//! names the script declares.

use crate::complete::{
    CONSTRUCTORS, FieldHint, Mode, Suggestion, at_value, complete, escape, is_word_char, rank,
};
use crate::lexer::{Token, TokenKind, lex, lex_script};
use crate::script::declared_names;
use crate::values::ValueHint;

/// What completion knows besides the text.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScriptHints<'a> {
    /// Collections of the database `db` refers to.
    pub collections: &'a [String],
    /// Sampled fields of the collection that [`script_target`] names.
    pub fields: &'a [FieldHint],
    /// Values seen in that collection, for its fields.
    pub values: &'a [ValueHint],
}

/// Most suggestions shown at once.
const MAX_SUGGESTIONS: usize = 50;

/// Inserts mark the cursor with `|`.
const COLLECTION_METHODS: &[(&str, &str, &str)] = &[
    ("find", "find(|)", "filter, projection → cursor"),
    ("findOne", "findOne(|)", "filter, projection → document"),
    ("aggregate", "aggregate([|])", "pipeline → cursor"),
    ("countDocuments", "countDocuments(|)", "filter → number"),
    (
        "estimatedDocumentCount",
        "estimatedDocumentCount()",
        "→ number",
    ),
    ("distinct", "distinct('|')", "field, filter → values"),
    ("insertOne", "insertOne({ | })", "document"),
    ("insertMany", "insertMany([|])", "documents"),
    ("updateOne", "updateOne(|)", "filter, update"),
    ("updateMany", "updateMany(|)", "filter, update"),
    ("replaceOne", "replaceOne(|)", "filter, document"),
    ("deleteOne", "deleteOne(|)", "filter"),
    ("deleteMany", "deleteMany(|)", "filter"),
    (
        "findOneAndUpdate",
        "findOneAndUpdate(|)",
        "filter, update → document",
    ),
    (
        "findOneAndReplace",
        "findOneAndReplace(|)",
        "filter, document → document",
    ),
    (
        "findOneAndDelete",
        "findOneAndDelete(|)",
        "filter → document",
    ),
    ("bulkWrite", "bulkWrite([|])", "operations"),
];

const CURSOR_METHODS: &[(&str, &str, &str)] = &[
    ("sort", "sort({ | })", "keys"),
    ("limit", "limit(|)", "count"),
    ("skip", "skip(|)", "count"),
    ("project", "project({ | })", "projection"),
    ("projection", "projection({ | })", "projection"),
    ("hint", "hint(|)", "index name or keys"),
    ("collation", "collation({ | })", "collation"),
    ("maxTimeMS", "maxTimeMS(|)", "milliseconds"),
    ("toArray", "toArray()", "→ array"),
    ("forEach", "forEach(|)", "function"),
    ("map", "map(|)", "function → array"),
    ("next", "next()", "→ document"),
    ("hasNext", "hasNext()", "→ boolean"),
    ("itcount", "itcount()", "→ number"),
];

/// Cursor methods that return the cursor, so the chain stays a cursor.
const CURSOR_CHAIN: &[&str] = &[
    "sort",
    "limit",
    "skip",
    "project",
    "projection",
    "hint",
    "collation",
    "maxTimeMS",
    "batchSize",
];

const DB_METHODS: &[(&str, &str, &str)] =
    &[("getCollection", "getCollection('|')", "name → collection")];

const GLOBALS: &[(&str, &str, &str)] = &[
    ("db", "db", "current database"),
    ("print", "print(|)", "print values"),
    ("printjson", "printjson(|)", "print as JSON"),
    ("console.log", "console.log(|)", "print values"),
];

const COMMANDS: &[(&str, &str, &str)] = &[
    ("use", "use |", "switch database for this run"),
    ("show dbs", "show dbs", "list databases"),
    ("show collections", "show collections", "list collections"),
];

/// What an expression evaluates to, as far as completion can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Db,
    Collection(String),
    /// A cursor over the named collection.
    Cursor(String),
    Other,
}

/// What an argument of a known method holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Argument {
    Query(Mode),
    Pipeline,
    /// A field name in quotes, e.g. `distinct('|')`.
    FieldName,
    /// A collection name in quotes, `getCollection('|')`.
    CollectionName,
}

/// A method call whose arguments the cursor is in.
#[derive(Debug)]
struct OpenCall {
    target: Target,
    method: String,
    /// Byte offset right after the `(`.
    args_start: usize,
    /// Which argument, from 0.
    argument: usize,
    /// The cursor is directly in the parentheses, not deeper.
    direct: bool,
}

/// The collection the method call around `cursor` works on, so its fields
/// can be sampled for completion.
pub fn script_target(text: &str, cursor: usize) -> Option<String> {
    let before = text.get(..cursor)?;
    let tokens = significant(before);
    match open_call(&tokens)?.target {
        Target::Collection(name) | Target::Cursor(name) => Some(name),
        Target::Db | Target::Other => None,
    }
}

/// Suggestions for the word that ends at byte offset `cursor`.
pub fn complete_script(
    text: &str,
    cursor: usize,
    hints: &ScriptHints<'_>,
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
    let context_end = word_start - quote.map_or(0, char::len_utf8);
    let tokens = significant(&before[..context_end]);
    let call = open_call(&tokens);

    if let Some(call) = &call
        && let Some(argument) = argument(&call.target, &call.method, call.argument)
    {
        return match argument {
            Argument::Query(_) | Argument::Pipeline if quote.is_none() || !call.direct => {
                let query = Query {
                    call,
                    argument,
                    quoted: quote.is_some(),
                };
                in_query(text, cursor, word, &query, hints, explicit)
            }
            Argument::FieldName if call.direct => {
                let names = hints.fields.iter().map(|field| field.path.as_str());
                quoted_names(names, "field", word, quote, after, explicit)
            }
            Argument::CollectionName if call.direct => {
                let names = hints.collections.iter().map(String::as_str);
                quoted_names(names, "collection", word, quote, after, explicit)
            }
            _ => Vec::new(),
        };
    }
    if quote.is_some() || (word.is_empty() && !explicit) {
        return Vec::new();
    }
    let mut candidates = match word.rsplit_once('.') {
        Some((receiver, _)) => members(&tokens, receiver, hints),
        None if key_position(&tokens) => Vec::new(),
        None => {
            let mut list = names(&declared_names(text), "variable");
            list.extend(snippets(GLOBALS, ""));
            if statement_start(&tokens, &before[..word_start]) {
                list.extend(snippets(COMMANDS, ""));
            }
            list.extend(snippets(CONSTRUCTORS, ""));
            list
        }
    };
    let segment = word.rsplit_once('.').map_or(word, |(_, segment)| segment);
    rank(&mut candidates, segment);
    candidates.truncate(MAX_SUGGESTIONS);
    candidates
}

/// A query argument the cursor is in.
struct Query<'a> {
    call: &'a OpenCall,
    argument: Argument,
    /// The word is in quotes.
    quoted: bool,
}

/// Completion inside a query argument, delegated to the filter completion
/// on the argument's text, plus the script's names where a value goes.
fn in_query(
    text: &str,
    cursor: usize,
    word: &str,
    query: &Query<'_>,
    hints: &ScriptHints<'_>,
    explicit: bool,
) -> Vec<Suggestion> {
    let args_start = query.call.args_start;
    let (start, mode) = match query.argument {
        Argument::Query(mode) => (args_start, mode),
        _ => pipeline_stage(text, args_start, cursor - word.len())
            .unwrap_or((args_start, Mode::Document)),
    };
    let mut list = complete(
        &text[start..],
        cursor - start,
        mode,
        hints.fields,
        hints.values,
        explicit,
    );
    let before_word = &text[start..cursor - word.len()];
    let typed = !word.is_empty() || explicit;
    let plain = !query.quoted && !word.starts_with('$') && !word.contains('.');
    if typed && plain && at_value(before_word) {
        let mut variables = names(&declared_names(text), "variable");
        rank(&mut variables, word);
        list.extend(variables);
        list.truncate(MAX_SUGGESTIONS);
    }
    list
}

/// Inside `aggregate([...])`: the innermost `$match`, `$sort` or `$project`
/// object around the cursor, as the start of its `{` and the mode for it.
fn pipeline_stage(text: &str, args_start: usize, end: usize) -> Option<(usize, Mode)> {
    let region = text.get(args_start..end)?;
    // Each open bracket: whether it is an object, where it starts, and the
    // key it is the value of.
    let mut stack: Vec<(bool, usize, Option<String>)> = Vec::new();
    let mut key: Option<String> = None;
    let tokens = lex(region);
    for (index, token) in tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::LBrace | TokenKind::LBracket => {
                stack.push((
                    token.kind == TokenKind::LBrace,
                    args_start + token.span.start,
                    key.take(),
                ));
            }
            TokenKind::RBrace | TokenKind::RBracket => {
                stack.pop();
                key = None;
            }
            TokenKind::Comma => key = None,
            TokenKind::Ident(name) | TokenKind::String { value: name, .. }
                if tokens.get(index + 1).map(|token| &token.kind) == Some(&TokenKind::Colon) =>
            {
                key = Some(name.clone());
            }
            _ => {}
        }
    }
    stack
        .iter()
        .rev()
        .find_map(|(object, start, key)| match key.as_deref() {
            Some("$match") if *object => Some((*start, Mode::Filter)),
            Some("$sort" | "$project") if *object => Some((*start, Mode::Fields)),
            _ => None,
        })
}

/// Names in quotes, closing the quote. Before a quote is typed, only on
/// request, with quotes added.
fn quoted_names<'a>(
    names: impl Iterator<Item = &'a str>,
    detail: &str,
    word: &str,
    quote: Option<char>,
    after: &str,
    explicit: bool,
) -> Vec<Suggestion> {
    if quote.is_none() && (!explicit || !word.is_empty()) {
        return Vec::new();
    }
    let mut list: Vec<Suggestion> = names
        .map(|name| match quote {
            Some(quote) => {
                let mut suggestion =
                    Suggestion::new(name, &format!("{}{quote}", escape(name, quote)), detail);
                if after.starts_with(quote) {
                    suggestion.delete_after = quote.len_utf8();
                }
                suggestion
            }
            None => Suggestion::new(name, &format!("'{}'", escape(name, '\'')), detail),
        })
        .collect();
    rank(&mut list, word);
    list.truncate(MAX_SUGGESTIONS);
    list
}

/// Members after a dot: `receiver` is the dotted word before the last dot,
/// e.g. `db.users` or empty after `find(...)`.
fn members(tokens: &[Token], receiver: &str, hints: &ScriptHints<'_>) -> Vec<Suggestion> {
    let target = if receiver.is_empty() {
        expression_type(tokens, tokens.len(), 0)
    } else {
        // The word holds the whole chain, unless it follows a call.
        let chain: Vec<Token> = significant(receiver);
        expression_type(&chain, chain.len(), 0)
    };
    let prefix = format!("{receiver}.");
    match target {
        Target::Db => {
            let mut list: Vec<Suggestion> = hints
                .collections
                .iter()
                .map(|name| {
                    let insert = if is_identifier(name) {
                        format!("{prefix}{name}")
                    } else {
                        format!("{prefix}getCollection('{}')", escape(name, '\''))
                    };
                    Suggestion::new(name, &insert, "collection")
                })
                .collect();
            list.extend(snippets(DB_METHODS, &prefix));
            list
        }
        Target::Collection(_) => snippets(COLLECTION_METHODS, &prefix),
        Target::Cursor(_) => snippets(CURSOR_METHODS, &prefix),
        Target::Other => Vec::new(),
    }
}

fn snippets(list: &[(&str, &str, &str)], prefix: &str) -> Vec<Suggestion> {
    list.iter()
        .map(|&(label, insert, detail)| {
            Suggestion::snippet(label, &format!("{prefix}{insert}"), detail)
        })
        .collect()
}

fn names(names: &[String], detail: &str) -> Vec<Suggestion> {
    names
        .iter()
        .map(|name| Suggestion::new(name, name, detail))
        .collect()
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(crate::lexer::is_ident_start)
        && chars.all(crate::lexer::is_ident_continue)
}

/// Script tokens without comments.
fn significant(text: &str) -> Vec<Token> {
    lex_script(text)
        .into_iter()
        .filter(|token| token.kind != TokenKind::Comment)
        .collect()
}

/// The quote at the end of `text`, if it opens a string.
fn opening_quote(text: &str) -> Option<char> {
    let quote = text
        .chars()
        .next_back()
        .filter(|c| matches!(c, '\'' | '"'))?;
    let opens = matches!(
        lex_script(text).last().map(|token| &token.kind),
        Some(TokenKind::String {
            terminated: false,
            ..
        })
    );
    opens.then_some(quote)
}

/// The innermost open method call, if the cursor is in one.
fn open_call(tokens: &[Token]) -> Option<OpenCall> {
    // Each open bracket, with its call for a `(` after `.method`.
    let mut stack: Vec<(TokenKind, Option<OpenCall>)> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::LParen => {
                let kind = |back: usize| index.checked_sub(back).map(|i| &tokens[i].kind);
                let call = match (kind(2), kind(1)) {
                    (Some(TokenKind::Dot), Some(TokenKind::Ident(method))) => Some(OpenCall {
                        target: expression_type(tokens, index - 2, 0),
                        method: method.clone(),
                        args_start: token.span.end,
                        argument: 0,
                        direct: true,
                    }),
                    _ => None,
                };
                stack.push((TokenKind::LParen, call));
            }
            TokenKind::LBracket | TokenKind::LBrace => stack.push((token.kind.clone(), None)),
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                stack.pop();
            }
            TokenKind::Comma => {
                if let Some((TokenKind::LParen, Some(call))) = stack.last_mut() {
                    call.argument += 1;
                }
            }
            _ => {}
        }
    }
    let innermost = stack.iter().rposition(|(_, call)| call.is_some())?;
    let direct = innermost == stack.len() - 1;
    let (_, call) = stack.swap_remove(innermost);
    call.map(|call| OpenCall { direct, ..call })
}

/// What the expression made of `tokens[..end]` evaluates to.
fn expression_type(tokens: &[Token], end: usize, depth: usize) -> Target {
    if end == 0 || depth > 32 {
        return Target::Other;
    }
    let dot_before = |index: usize| index >= 1 && tokens[index - 1].kind == TokenKind::Dot;
    match &tokens[end - 1].kind {
        TokenKind::Ident(name) if dot_before(end - 1) => {
            match expression_type(tokens, end - 2, depth + 1) {
                Target::Db if !DB_METHODS.iter().any(|(method, ..)| method == name) => {
                    Target::Collection(name.clone())
                }
                _ => Target::Other,
            }
        }
        TokenKind::Ident(name) if name == "db" => Target::Db,
        TokenKind::RParen => {
            let Some(open) = matching_open(tokens, end - 1) else {
                return Target::Other;
            };
            if open < 2 || !dot_before(open - 1) {
                return Target::Other;
            }
            let TokenKind::Ident(method) = &tokens[open - 1].kind else {
                return Target::Other;
            };
            match (
                expression_type(tokens, open - 2, depth + 1),
                method.as_str(),
            ) {
                (Target::Db, "getCollection") => match tokens.get(open + 1).map(|t| &t.kind) {
                    Some(TokenKind::String { value, .. }) => Target::Collection(value.clone()),
                    _ => Target::Other,
                },
                (Target::Collection(name), "find" | "aggregate") => Target::Cursor(name),
                (Target::Cursor(name), method) if CURSOR_CHAIN.contains(&method) => {
                    Target::Cursor(name)
                }
                _ => Target::Other,
            }
        }
        _ => Target::Other,
    }
}

/// The index of the `(` that the `)` at `close` closes.
fn matching_open(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for index in (0..=close).rev() {
        match tokens[index].kind {
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth += 1,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth -= 1;
                if depth == 0 {
                    return (tokens[index].kind == TokenKind::LParen).then_some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// What the argument at `index` of `target.method(...)` holds.
fn argument(target: &Target, method: &str, index: usize) -> Option<Argument> {
    use Argument::{CollectionName, FieldName, Pipeline, Query};
    Some(match (target, method, index) {
        (Target::Db, "getCollection", 0) => CollectionName,
        (Target::Collection(_), method, index) => match (method, index) {
            ("find" | "findOne", 0) => Query(Mode::Filter),
            ("find" | "findOne", 1) => Query(Mode::Fields),
            (
                "countDocuments" | "deleteOne" | "deleteMany" | "updateOne" | "updateMany"
                | "replaceOne" | "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete",
                0,
            ) => Query(Mode::Filter),
            ("distinct", 0) => FieldName,
            ("distinct", 1) => Query(Mode::Filter),
            ("aggregate", 0) => Pipeline,
            (
                "updateOne" | "updateMany" | "replaceOne" | "findOneAndUpdate"
                | "findOneAndReplace",
                1,
            )
            | ("insertOne" | "insertMany" | "bulkWrite", 0) => Query(Mode::Document),
            _ => return None,
        },
        (Target::Cursor(_), method, 0) => match method {
            "sort" | "project" | "projection" | "hint" => Query(Mode::Fields),
            "collation" => Query(Mode::Collation),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether the word is a key in an object literal.
fn key_position(tokens: &[Token]) -> bool {
    let mut depth_kinds: Vec<&TokenKind> = Vec::new();
    for token in tokens {
        match &token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth_kinds.push(&token.kind);
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth_kinds.pop();
            }
            _ => {}
        }
    }
    depth_kinds.last() == Some(&&TokenKind::LBrace)
        && matches!(
            tokens.last().map(|token| &token.kind),
            Some(TokenKind::Comma | TokenKind::LBrace)
        )
        // `{ a, b }` in a block is rare; `{` after `)` or `=>` opens a block.
        && !block_brace(tokens)
}

/// Whether the innermost open `{` opens a block rather than an object.
fn block_brace(tokens: &[Token]) -> bool {
    let mut depth = 0usize;
    for index in (0..tokens.len()).rev() {
        match tokens[index].kind {
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth += 1,
            TokenKind::LParen | TokenKind::LBracket => depth = depth.saturating_sub(1),
            TokenKind::LBrace if depth == 0 => {
                return match index.checked_sub(1).map(|i| &tokens[i].kind) {
                    None | Some(TokenKind::RParen | TokenKind::Semicolon | TokenKind::RBrace) => {
                        true
                    }
                    Some(TokenKind::Operator(op)) => op == "=>",
                    Some(TokenKind::Ident(word)) => {
                        matches!(word.as_str(), "else" | "do" | "try" | "finally")
                    }
                    _ => false,
                };
            }
            TokenKind::LBrace => depth -= 1,
            _ => {}
        }
    }
    false
}

/// Whether a new statement starts at the word: outside brackets, after
/// nothing, a `;`, a `}` or a line break.
fn statement_start(tokens: &[Token], before_word: &str) -> bool {
    let mut depth = 0isize;
    for token in tokens {
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth -= 1,
            _ => {}
        }
    }
    let Some(last) = tokens.last() else {
        return true;
    };
    depth <= 0
        && (matches!(last.kind, TokenKind::Semicolon | TokenKind::RBrace)
            || before_word[last.span.end..].contains('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints<'a>(collections: &'a [String], fields: &'a [FieldHint]) -> ScriptHints<'a> {
        ScriptHints {
            collections,
            fields,
            values: &[],
        }
    }

    fn labels(text: &str, hints: &ScriptHints<'_>) -> Vec<String> {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        complete_script(&text, cursor, hints, false)
            .into_iter()
            .map(|suggestion| suggestion.label)
            .collect()
    }

    fn collections() -> Vec<String> {
        vec![
            "users".to_owned(),
            "user-logs".to_owned(),
            "orders".to_owned(),
        ]
    }

    fn fields() -> Vec<FieldHint> {
        ["name", "age"]
            .map(|path| FieldHint {
                path: path.to_owned(),
                detail: String::new(),
            })
            .into()
    }

    #[test]
    fn suggests_collections_after_db() {
        let collections = collections();
        let hints = hints(&collections, &[]);
        assert_eq!(labels("db.us|", &hints), ["users", "user-logs"]);
        let found = complete_script("x = db.us", 9, &hints, false);
        assert_eq!(found[1].insert, "db.getCollection('user-logs')");
    }

    #[test]
    fn suggests_methods_on_collections_and_cursors() {
        let collections = collections();
        let hints = hints(&collections, &[]);
        assert_eq!(
            labels("db.users.find|", &hints),
            [
                "find",
                "findOne",
                "findOneAndUpdate",
                "findOneAndReplace",
                "findOneAndDelete"
            ]
        );
        assert_eq!(labels("db.users.find({}).so|", &hints), ["sort"]);
        assert_eq!(
            labels("db.getCollection('a b').find().limit(1).sk|", &hints),
            ["skip"]
        );
        let found = complete_script("db.users.co", 11, &hints, false);
        assert_eq!(found[0].insert, "db.users.countDocuments()");
        assert_eq!(found[0].cursor, "db.users.countDocuments(".len());
    }

    #[test]
    fn delegates_query_arguments_to_filter_completion() {
        let fields = fields();
        let hints = hints(&[], &fields);
        assert_eq!(labels("db.users.find({ na| })", &hints), ["name"]);
        assert_eq!(
            labels("db.users.find({ age: { $g| } })", &hints),
            ["$gt", "$gte"]
        );
        assert_eq!(labels("db.users.find({}, { a| })", &hints), ["age"]);
        assert_eq!(
            labels("db.users.aggregate([{ $match: { na| } }])", &hints),
            ["name"]
        );
        assert_eq!(labels("db.users.distinct('a|')", &hints), ["age"]);
        assert_eq!(
            script_target("db.users.find({}).sort({ ", 25).as_deref(),
            Some("users")
        );
    }

    #[test]
    fn suggests_declared_names_and_globals() {
        let hints = ScriptHints::default();
        assert_eq!(labels("const ids = 1\nid|", &hints), ["ids"]);
        assert_eq!(
            labels(
                "const ids = 1\ndb.users.find({ _id: { $in: id| } })",
                &hints
            ),
            ["ids"]
        );
        assert_eq!(labels("pri|", &hints), ["print", "printjson"]);
        assert_eq!(labels("sh|", &hints), ["show dbs", "show collections"]);
        assert!(labels("x = { sh| }", &hints).is_empty());
    }
}
