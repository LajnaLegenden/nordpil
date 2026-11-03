//! The shell panel of a collection tab: the script, the run in progress,
//! and one result per statement.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bson::{Bson, Document};
use crossbeam_channel::Receiver;
use eframe::egui;
use nordpil_backend::{
    AggregateQuery, FindQuery, FoundDocument, PREVIEW_BYTES, RequestId, SCRIPT_MAX_TIME,
    SavedScript, ScriptRun,
};
use nordpil_query::{FieldHint, ScriptHints, Style, ValueHint};
use nordpil_shell::{Event, Query, QueryKind, ResultValue, ScriptError};

use crate::tabs::{Results, View, page_window};
use crate::widgets::code_editor::{CodeEditor, Completion, Language, Span};

/// How long typing must pause before the draft is saved.
pub const DRAFT_DELAY: Duration = Duration::from_secs(1);

pub struct ShellState {
    pub editor: CodeEditor,
    pub text: String,
    /// The saved draft has arrived, so `text` is the user's.
    pub loaded: bool,
    /// What the draft on disk holds.
    pub saved_text: String,
    /// When the text last changed without being saved.
    pub edited_at: Option<Instant>,
    pub running: Option<Running>,
    /// The script of the last run, for result titles.
    pub script: String,
    pub results: Vec<ShellResult>,
    pub output: Vec<String>,
    pub ending: Option<Ending>,
    pub selected: Option<Selected>,
    pub history: Vec<SavedScript>,
    pub view: View,
    /// Sampled fields per collection of the tab's database, for completion.
    pub fields: HashMap<String, Vec<FieldHint>>,
    /// Collections whose fields were asked for.
    pub sampling: HashSet<String>,
    /// An open or save file dialog, and what it was for.
    pub picker: Option<(FileJob, Receiver<FileResult>)>,
    /// A statement to add to the end of the script, and run, once the
    /// draft has loaded.
    pub queued: Option<String>,
    /// Focus the editor with the cursor at the end of the script.
    pub cursor_to_end: bool,
}

pub struct Running {
    pub request: RequestId,
    pub run: Arc<ScriptRun>,
}

/// Which result is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Result(usize),
    Output,
}

/// How a run that didn't simply finish ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ending {
    Failed(ScriptError),
    Stopped,
    Declined,
    /// Finished with nothing to show.
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileJob {
    Open,
    Save,
}

/// The outcome of a file dialog: the text read, or the path written.
pub type FileResult = Option<Result<(PathBuf, String), String>>;

pub struct ShellResult {
    pub title: String,
    pub elapsed: Duration,
    pub body: Body,
}

pub enum Body {
    /// A cursor: each page re-runs its query.
    Query(Box<QueryPage>),
    /// Documents the script read; paged in memory.
    Documents {
        documents: Vec<Document>,
        page: u64,
        current: Results,
    },
    Value {
        text: String,
        spans: Vec<Span>,
    },
}

pub struct QueryPage {
    pub query: Query,
    pub page: u64,
    pub results: Option<Results>,
    pub pending: Option<RequestId>,
    pub error: Option<String>,
}

/// A page request for a query result.
pub enum PageRequest {
    Find(Box<FindQuery>),
    Aggregate(Box<AggregateQuery>),
}

impl ShellState {
    pub fn new(tab: u64) -> Self {
        Self {
            // The panel frames it, around a scroll area it fills.
            editor: CodeEditor::new(egui::Id::unique(("shell", tab)), true)
                .frameless()
                .rows(1)
                .line_numbers()
                .hint("db.collection.find({})"),
            text: String::new(),
            loaded: false,
            saved_text: String::new(),
            edited_at: None,
            running: None,
            script: String::new(),
            results: Vec::new(),
            output: Vec::new(),
            ending: None,
            selected: None,
            history: Vec::new(),
            view: View::List,
            fields: HashMap::new(),
            sampling: HashSet::new(),
            picker: None,
            queued: None,
            cursor_to_end: false,
        }
    }

    /// Puts in the saved draft, or a starter script, once.
    pub fn load_draft(&mut self, draft: Option<String>, collection: &str) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.saved_text = draft.clone().unwrap_or_default();
        self.text = draft.unwrap_or_else(|| starter(collection));
    }

    /// Once the draft has loaded, adds the queued statement to the end of
    /// the script and puts the cursor after it. Returns the statement to run.
    pub fn append_queued(&mut self) -> Option<String> {
        if !self.loaded {
            return None;
        }
        let statement = self.queued.take()?;
        // With no draft saved, the script is only the starter.
        self.text = if self.saved_text.is_empty() {
            statement.clone()
        } else {
            with_statement(&self.text, &statement)
        };
        self.cursor_to_end = true;
        self.edited();
        Some(statement)
    }

    /// The database a result's documents came from, when the result knows.
    pub fn result_database(&self, index: usize) -> Option<&str> {
        match &self.results.get(index)?.body {
            Body::Query(page) => Some(&page.query.namespace.database),
            _ => None,
        }
    }

    /// The database of the shown result's documents, when it knows.
    pub fn shown_database(&self) -> Option<&str> {
        match self.selected? {
            Selected::Result(index) => self.result_database(index),
            Selected::Output => None,
        }
    }

    /// Clears the last run's results for a new run.
    pub fn start(&mut self, script: String, running: Running) {
        self.script = script;
        self.results.clear();
        self.output.clear();
        self.ending = None;
        self.selected = None;
        self.running = Some(running);
    }

    /// Adds what the run reported. Returns the indexes of new query
    /// results, whose first page needs fetching.
    pub fn take_events(&mut self, page_size: u64) -> Vec<usize> {
        let Some(running) = &self.running else {
            return Vec::new();
        };
        let mut new_queries = Vec::new();
        for event in running.run.take_events() {
            match event {
                Event::Output(line) => self.output.push(line),
                Event::Result {
                    span,
                    value,
                    elapsed,
                    ..
                } => {
                    let title = title(self.script.get(span).unwrap_or_default());
                    let body = match value {
                        ResultValue::Cursor(query) => {
                            new_queries.push(self.results.len());
                            Body::Query(Box::new(QueryPage {
                                query: *query,
                                page: 0,
                                results: None,
                                pending: None,
                                error: None,
                            }))
                        }
                        ResultValue::Documents(documents) => {
                            let current = page_of(&documents, 0, page_size);
                            Body::Documents {
                                documents,
                                page: 0,
                                current,
                            }
                        }
                        ResultValue::Value(value) => value_body(&value),
                    };
                    self.results.push(ShellResult {
                        title,
                        elapsed,
                        body,
                    });
                }
            }
        }
        if self.selected.is_none() {
            self.selected = if self.results.is_empty() {
                (!self.output.is_empty()).then_some(Selected::Output)
            } else {
                Some(Selected::Result(0))
            };
        }
        new_queries
    }

    /// The run ended; `ending` says how, unless it simply finished.
    pub fn finish(&mut self, ending: Option<Ending>) {
        self.running = None;
        self.ending = ending.or_else(|| {
            (self.results.is_empty() && self.output.is_empty()).then_some(Ending::Empty)
        });
    }

    /// The text changed; the draft is saved once typing pauses.
    pub fn edited(&mut self) {
        self.edited_at = Some(Instant::now());
    }

    /// The draft to save now, if typing paused long enough or `now` is set.
    pub fn draft_to_save(&mut self, now: bool) -> Option<String> {
        let due = now || self.edited_at.is_some_and(|at| at.elapsed() >= DRAFT_DELAY);
        if !due || !self.loaded || self.text == self.saved_text {
            return None;
        }
        self.edited_at = None;
        self.saved_text = self.text.clone();
        Some(self.text.clone())
    }

    pub fn result_for_request(&mut self, request: RequestId) -> Option<&mut QueryPage> {
        self.results
            .iter_mut()
            .find_map(|result| match &mut result.body {
                Body::Query(page) if page.pending == Some(request) => Some(&mut **page),
                _ => None,
            })
    }
}

/// `db.<collection>.find({})`, quoting names that aren't identifiers.
pub fn starter(collection: &str) -> String {
    format!("{}.find({{}})", collection_expression(collection))
}

/// `db.<collection>.find(<filter>)`.
pub fn find_statement(collection: &str, filter: &Document) -> String {
    format!(
        "{}.find({})",
        collection_expression(collection),
        nordpil_query::format_document(filter, Style::Compact)
    )
}

/// `db.<collection>`, or `db.getCollection('<collection>')` for names that
/// aren't identifiers.
fn collection_expression(collection: &str) -> String {
    let mut chars = collection.chars();
    let identifier = chars
        .next()
        .is_some_and(nordpil_query::lexer::is_ident_start)
        && chars.all(nordpil_query::lexer::is_ident_continue);
    if identifier {
        format!("db.{collection}")
    } else {
        let quoted =
            nordpil_query::format_value(&Bson::String(collection.to_owned()), Style::Compact);
        format!("db.getCollection({quoted})")
    }
}

/// `script` with `statement` on a line of its own at the end.
fn with_statement(script: &str, statement: &str) -> String {
    if script.trim().is_empty() {
        statement.to_owned()
    } else if script.ends_with('\n') {
        format!("{script}{statement}")
    } else {
        format!("{script}\n{statement}")
    }
}

/// The statement's first line, cut to fit a result tab.
fn title(statement: &str) -> String {
    const MAX: usize = 32;
    let line = statement.lines().next().unwrap_or_default().trim();
    if line.chars().count() > MAX || statement.trim().contains('\n') {
        let cut: String = line.chars().take(MAX).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_owned()
    }
}

fn value_body(value: &Bson) -> Body {
    let text = match value {
        Bson::String(text) => {
            nordpil_query::format_value(&Bson::String(text.clone()), Style::Compact)
        }
        value => nordpil_query::format_value(value, Style::Pretty(2)),
    };
    let spans = nordpil_query::highlight(&text);
    Body::Value { text, spans }
}

/// One page of documents held in memory.
pub fn page_of(documents: &[Document], page: u64, page_size: u64) -> Results {
    let start = usize::try_from(page * page_size).unwrap_or(usize::MAX);
    let end = start.saturating_add(usize::try_from(page_size).unwrap_or(usize::MAX));
    let documents: Vec<Document> = documents
        .get(start.min(documents.len())..end.min(documents.len()))
        .unwrap_or_default()
        .to_vec();
    let previews = documents
        .iter()
        .map(|document| nordpil_query::preview(document, PREVIEW_BYTES))
        .collect();
    Results {
        documents,
        previews,
    }
}

pub fn results_of(found: Vec<FoundDocument>) -> Results {
    let (documents, previews) = found
        .into_iter()
        .map(|found| (found.document, found.preview))
        .unzip();
    Results {
        documents,
        previews,
    }
}

impl QueryPage {
    /// The request for `self.page`.
    pub fn request(&self, page_size: u64) -> PageRequest {
        match &self.query.kind {
            QueryKind::Find { filter, options } => {
                let (skip, limit) = page_window(options.skip, options.limit, self.page, page_size);
                PageRequest::Find(Box::new(FindQuery {
                    filter: filter.clone(),
                    projection: options.projection.clone(),
                    sort: options.sort.clone(),
                    collation: options.collation.clone(),
                    hint: options.hint.clone(),
                    skip,
                    limit: i64::try_from(limit).unwrap_or(i64::MAX),
                    max_time: Some(options.max_time.unwrap_or(SCRIPT_MAX_TIME)),
                }))
            }
            QueryKind::Aggregate { pipeline, options } => {
                PageRequest::Aggregate(Box::new(AggregateQuery {
                    pipeline: pipeline.clone(),
                    options: options.clone(),
                    skip: self.page * page_size,
                    limit: page_size,
                }))
            }
        }
    }

    /// A full page suggests more, unless the query's own limit is reached.
    pub fn has_next(&self, page_size: u64) -> bool {
        let Some(results) = &self.results else {
            return false;
        };
        let shown = results.documents.len() as u64;
        let limit = match &self.query.kind {
            QueryKind::Find { options, .. } => options.limit,
            QueryKind::Aggregate { .. } => 0,
        };
        shown == page_size && (limit == 0 || (self.page + 1) * page_size < limit)
    }
}

/// Highlighting and completion for scripts, with what the tab knows about
/// its database.
pub struct ShellLanguage<'a> {
    pub collections: &'a [String],
    pub collection: &'a str,
    /// The tab's own sampled fields and values.
    pub own_fields: &'a [FieldHint],
    pub own_values: &'a [ValueHint],
    pub fields: &'a HashMap<String, Vec<FieldHint>>,
    /// A collection completion wanted fields for that aren't sampled yet.
    pub wanted: RefCell<Option<String>>,
}

impl Language for ShellLanguage<'_> {
    fn highlight(&self, text: &str) -> Vec<Span> {
        nordpil_query::highlight_script(text)
    }

    fn is_word_char(&self, c: char) -> bool {
        nordpil_query::is_word_char(c)
    }

    fn complete(&self, text: &str, cursor: usize, explicit: bool) -> Vec<Completion> {
        let target = nordpil_query::script_target(text, cursor);
        let (fields, values): (&[FieldHint], &[ValueHint]) = match target.as_deref() {
            Some(name) if name == self.collection => (self.own_fields, self.own_values),
            Some(name) => match self.fields.get(name) {
                Some(fields) => (fields, &[]),
                None => {
                    *self.wanted.borrow_mut() = Some(name.to_owned());
                    (&[], &[])
                }
            },
            None => (&[], &[]),
        };
        let hints = ScriptHints {
            collections: self.collections,
            fields,
            values,
        };
        nordpil_query::complete_script(text, cursor, &hints, explicit)
            .into_iter()
            .map(|suggestion| Completion {
                label: suggestion.label,
                insert: suggestion.insert,
                cursor: suggestion.cursor,
                delete_after: suggestion.delete_after,
                detail: suggestion.detail,
            })
            .collect()
    }
}

/// The text to run: the selection if there is one, else the statement at
/// the cursor.
pub fn statement_to_run(text: &str, selection: Range<usize>) -> Option<String> {
    if selection.start < selection.end {
        return text.get(selection).map(str::to_owned);
    }
    let statements = nordpil_query::split_statements(text);
    let index = nordpil_query::statement_at(text, &statements, selection.start)?;
    text.get(statements[index].span.clone()).map(str::to_owned)
}

/// Shows a duration like `12 ms` or `1.5 s`.
pub fn elapsed_text(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(1) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{:.1} s", elapsed.as_secs_f64())
    }
}

/// Runs a file dialog off the UI thread, then reads or writes the file.
pub fn pick_file(ctx: &egui::Context, job: FileJob, text: String) -> Receiver<FileResult> {
    let (sender, receiver) = crossbeam_channel::bounded(1);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let dialog = rfd::FileDialog::new()
            .add_filter("JavaScript", &["js", "mongodb"])
            .add_filter("All files", &["*"]);
        let result = match job {
            FileJob::Open => dialog.pick_file().map(|path| {
                std::fs::read_to_string(&path)
                    .map(|text| (path.clone(), text))
                    .map_err(|error| format!("Could not read {}: {error}", path.display()))
            }),
            FileJob::Save => dialog.set_file_name("script.js").save_file().map(|path| {
                std::fs::write(&path, &text)
                    .map(|()| (path.clone(), text))
                    .map_err(|error| format!("Could not write {}: {error}", path.display()))
            }),
        };
        let _ = sender.send(result);
        ctx.request_repaint();
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_quotes_odd_names() {
        assert_eq!(starter("users"), "db.users.find({})");
        assert_eq!(
            starter("user-logs"),
            "db.getCollection('user-logs').find({})"
        );
    }

    #[test]
    fn find_statements_quote_odd_names() {
        let filter = bson::doc! { "_id": 7 };
        assert_eq!(
            find_statement("users", &filter),
            "db.users.find({ _id: 7 })"
        );
        assert_eq!(
            find_statement("user-logs", &filter),
            "db.getCollection('user-logs').find({ _id: 7 })"
        );
    }

    #[test]
    fn queued_statements_go_on_a_line_of_their_own() {
        assert_eq!(with_statement("a()", "b()"), "a()\nb()");
        assert_eq!(with_statement("a()\n", "b()"), "a()\nb()");
        assert_eq!(with_statement("  \n", "b()"), "b()");
    }

    #[test]
    fn a_queued_statement_waits_for_the_draft() {
        let mut shell = ShellState::new(1);
        shell.queued = Some("db.users.find({ _id: 7 })".to_owned());
        assert_eq!(shell.append_queued(), None);

        shell.load_draft(Some("db.users.count()".to_owned()), "users");
        assert_eq!(
            shell.append_queued().as_deref(),
            Some("db.users.find({ _id: 7 })")
        );
        assert_eq!(shell.text, "db.users.count()\ndb.users.find({ _id: 7 })");
        assert!(shell.cursor_to_end);
        assert_eq!(shell.append_queued(), None);
    }

    #[test]
    fn a_queued_statement_replaces_the_starter() {
        let mut shell = ShellState::new(1);
        shell.queued = Some("db.users.find({ _id: 7 })".to_owned());
        shell.load_draft(None, "users");
        shell.append_queued();
        assert_eq!(shell.text, "db.users.find({ _id: 7 })");
    }

    #[test]
    fn runs_the_selection_or_the_statement_at_the_cursor() {
        let text = "db.a.find()\ndb.b\n  .find()\n";
        assert_eq!(statement_to_run(text, 0..4).as_deref(), Some("db.a"));
        assert_eq!(
            statement_to_run(text, 14..14).as_deref(),
            Some("db.b\n  .find()")
        );
        assert_eq!(statement_to_run("", 0..0), None);
    }

    #[test]
    fn titles_show_the_first_line() {
        assert_eq!(title("db.users.find({})"), "db.users.find({})");
        assert_eq!(title("db.users\n  .find()"), "db.users…");
    }

    #[test]
    fn pages_documents_in_memory() {
        let documents: Vec<Document> = (0..5).map(|n| bson::doc! { "n": n }).collect();
        let page = page_of(&documents, 2, 2);
        assert_eq!(page.documents, [bson::doc! { "n": 4 }]);
        assert!(page_of(&documents, 9, 2).documents.is_empty());
    }
}
