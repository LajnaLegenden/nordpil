//! The aggregation panel of a collection tab: the stages being built, a
//! preview of each stage's output, and the run, whose pages come from a
//! server cursor that stays open between pages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bson::{Bson, Document, RawDocumentBuf, doc};
use eframe::egui;
use nordpil_backend::{
    AggregatePage, AggregateQuery, Backend, BackendError, Bridge, Command, CursorKey, ErrorKind,
    Namespace, PREVIEW_TIME_LIMIT, PipelineQuery, PipelineText, RequestId, SavedPipeline,
    StageText,
};
use nordpil_query::{FieldHint, ValueHint};

use crate::tabs::{DEFAULT_MAX_TIME, Results, View};
use crate::widgets::code_editor::{CodeEditor, Span};

/// Lines of a document a preview card shows.
const PREVIEW_LINES: usize = 9;
/// Bytes of a document a preview card shows.
const PREVIEW_BYTES: usize = 1200;

/// How long typing must pause before the previews from the edited stage on
/// run again.
pub const PREVIEW_DELAY: Duration = Duration::from_millis(500);
/// Documents a preview shows.
pub const PREVIEW_SIZE: u64 = 10;
/// Previews running at once; the rest wait, so other queries still get a
/// connection from the pool.
pub const MAX_PREVIEWS: usize = 4;

/// Stages the server only takes first; the sample `$limit` goes after them.
const FIRST_ONLY: &[&str] = &[
    "$geoNear",
    "$search",
    "$searchMeta",
    "$vectorSearch",
    "$collStats",
    "$indexStats",
];

/// Sends commands: the app's bridge, or a stand-in in tests.
pub trait Requests {
    fn send(&mut self, command: Command) -> RequestId;
    fn cancel(&mut self, request: RequestId);
}

impl Requests for Bridge<Backend> {
    fn send(&mut self, command: Command) -> RequestId {
        Bridge::send(self, command)
    }

    fn cancel(&mut self, request: RequestId) {
        Bridge::cancel(self, request);
    }
}

/// Cursor keys are shared by every tab, so they come from one counter.
fn next_cursor_key() -> CursorKey {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What a stage's preview last showed.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview {
    /// Not run yet.
    None,
    Documents {
        documents: Vec<Document>,
        /// Each document's start as highlighted text, for its card.
        texts: Vec<(String, Vec<Span>)>,
        /// Read from a sample of the collection.
        sampled: bool,
    },
    Failed(String),
    TimedOut,
    /// Why the stage has no preview, e.g. it is off.
    Skipped(String),
}

pub struct StageCard {
    /// Stays with the card when it moves, for widget state.
    pub id: u64,
    /// E.g. `$group`.
    pub name: String,
    /// The stage's value as typed.
    pub body: String,
    pub editor: CodeEditor,
    pub enabled: bool,
    pub collapsed: bool,
    /// Why the body doesn't read, with line and column.
    pub error: Option<String>,
    pub preview: Preview,
    /// Waiting for a free slot to run its preview.
    pub queued: bool,
    pub request: Option<RequestId>,
}

impl StageCard {
    fn new(tab: u64, id: u64, text: StageText) -> Self {
        let editor = CodeEditor::new(egui::Id::unique(("stage", tab, id)), true).rows(1);
        let mut card = Self {
            id,
            name: text.name,
            body: text.body,
            editor,
            enabled: text.enabled,
            collapsed: text.collapsed,
            error: None,
            preview: Preview::None,
            queued: false,
            request: None,
        };
        card.check();
        card
    }

    /// Reads the body again, for its error.
    pub fn check(&mut self) {
        self.error = nordpil_query::pipeline::stage_document(&self.name, &self.body)
            .err()
            .map(|errors| crate::ui::document_dialogs::describe(&self.body, errors));
    }

    pub fn writes(&self) -> bool {
        nordpil_query::pipeline::is_write(&self.name)
    }

    pub fn text(&self) -> StageText {
        StageText {
            name: self.name.clone(),
            body: self.body.clone(),
            enabled: self.enabled,
            collapsed: self.collapsed,
        }
    }

    pub fn loading(&self) -> bool {
        self.queued || self.request.is_some()
    }
}

/// A run of the whole pipeline, read page by page from its cursor.
pub struct Run {
    pub key: CursorKey,
    pub query: PipelineQuery,
    /// Documents read so far, in order. Kept as BSON, since parsed they take
    /// about eight times the room.
    pub documents: Vec<RawDocumentBuf>,
    /// The cursor has nothing after `documents`.
    pub exhausted: bool,
    pub page: u64,
    /// The shown page.
    pub current: Option<Results>,
    /// `None` until counted, `Some(None)` when there were too many to count.
    pub count: Option<Option<u64>>,
    pub pending_page: Option<RequestId>,
    pub pending_count: Option<RequestId>,
    pub error: Option<String>,
    /// The user stopped it before the page came.
    pub stopped: bool,
}

impl Run {
    fn new(query: PipelineQuery) -> Self {
        Self {
            key: next_cursor_key(),
            query,
            documents: Vec::new(),
            exhausted: false,
            page: 0,
            current: None,
            count: None,
            pending_page: None,
            pending_count: None,
            error: None,
            stopped: false,
        }
    }

    pub fn loading(&self) -> bool {
        self.pending_page.is_some()
    }

    pub fn has_next(&self, page_size: u64) -> bool {
        let end = (self.page + 1) * page_size;
        if let Some(Some(count)) = self.count
            && end >= count
        {
            return false;
        }
        end < self.documents.len() as u64 || !self.exhausted
    }

    /// Shows `page` from the documents read so far, if they reach it. Past
    /// the last document, shows the last page instead.
    fn show_cached(&mut self, page_size: u64) -> bool {
        let read = self.documents.len() as u64;
        if read < (self.page + 1) * page_size && !self.exhausted {
            return false;
        }
        if self.page > 0 && self.page * page_size >= read {
            self.page = read.saturating_sub(1) / page_size;
        }
        self.current = Some(crate::shell::page_of(&self.documents, self.page, page_size));
        true
    }
}

/// A pipeline ending in `$out` or `$merge`, waiting for the user's yes.
pub struct WriteConfirm {
    pub query: PipelineQuery,
    /// `(database, collection)` it writes to.
    pub target: (String, String),
    pub merges: bool,
}

/// A run that writes, and where to.
pub struct Written {
    pub request: Option<RequestId>,
    /// `(database, collection)`.
    pub target: (String, String),
    /// `None` while it runs, then how it went.
    pub result: Option<Result<(), String>>,
}

/// The Explain dialog.
pub struct Explain {
    pub request: Option<RequestId>,
    pub result: Option<Result<String, String>>,
}

/// The Paste pipeline dialog.
pub struct Paste {
    pub editor: CodeEditor,
    pub text: String,
    pub error: Option<String>,
}

pub struct AggregateState {
    tab: u64,
    next_stage: u64,
    pub stages: Vec<StageCard>,
    pub allow_disk_use: bool,
    pub collation: String,
    pub hint: String,
    pub max_time_ms: String,
    pub collation_editor: CodeEditor,
    pub hint_editor: CodeEditor,
    pub show_options: bool,
    /// Previews read a sample of the collection.
    pub sample: bool,
    /// Previews from this stage on are out of date since this moment.
    stale_from: Option<(usize, Instant)>,
    pub run: Option<Run>,
    /// Why Run can't start, e.g. a stage that doesn't read.
    pub errors: Vec<String>,
    pub confirm: Option<WriteConfirm>,
    pub written: Option<Written>,
    pub explain: Option<Explain>,
    pub paste: Option<Paste>,
    pub history: Vec<SavedPipeline>,
    pub view: View,
    /// The saved draft has arrived, so the stages are the user's.
    pub loaded: bool,
    saved: PipelineText,
    edited_at: Option<Instant>,
    /// Focus this stage's editor, with the cursor after this many characters.
    pub focus: Option<(u64, usize)>,
}

impl AggregateState {
    pub fn new(tab: u64) -> Self {
        let editor = |field: &str, hint: &str| {
            CodeEditor::new(egui::Id::unique(("aggregate", tab, field)), false).hint(hint)
        };
        Self {
            tab,
            next_stage: 0,
            stages: Vec::new(),
            allow_disk_use: false,
            collation: String::new(),
            hint: String::new(),
            max_time_ms: String::new(),
            collation_editor: editor("collation", "{ locale: 'en' }").object(),
            hint_editor: editor("hint", "{ field: 1 } or 'index_name'"),
            show_options: false,
            sample: true,
            stale_from: None,
            run: None,
            errors: Vec::new(),
            confirm: None,
            written: None,
            explain: None,
            paste: None,
            history: Vec::new(),
            view: View::List,
            loaded: false,
            saved: PipelineText::default(),
            edited_at: None,
            focus: None,
        }
    }

    fn card(&mut self, text: StageText) -> StageCard {
        self.next_stage += 1;
        StageCard::new(self.tab, self.next_stage, text)
    }

    /// The stages and options as typed, as drafts and history keep them.
    pub fn text(&self) -> PipelineText {
        PipelineText {
            stages: self.stages.iter().map(StageCard::text).collect(),
            allow_disk_use: self.allow_disk_use,
            collation: self.collation.clone(),
            hint: self.hint.clone(),
            max_time_ms: self.max_time_ms.clone(),
        }
    }

    /// Replaces the stages and options, e.g. from the history.
    pub fn set_text(&mut self, text: PipelineText, requests: &mut impl Requests) {
        self.cancel_previews(requests);
        let cards: Vec<StageCard> = text.stages.into_iter().map(|t| self.card(t)).collect();
        self.stages = cards;
        self.allow_disk_use = text.allow_disk_use;
        self.show_options |= !text.collation.is_empty()
            || !text.hint.is_empty()
            || !text.max_time_ms.is_empty()
            || text.allow_disk_use;
        self.collation = text.collation;
        self.hint = text.hint;
        self.max_time_ms = text.max_time_ms;
        self.errors.clear();
        self.edited();
        self.invalidate(0, true);
    }

    /// Puts in the saved draft once.
    pub fn load_draft(&mut self, draft: Option<PipelineText>, requests: &mut impl Requests) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let draft = draft.unwrap_or_default();
        self.saved = draft.clone();
        self.set_text(draft, requests);
        self.edited_at = None;
    }

    /// Something changed; the draft is saved once typing pauses.
    pub fn edited(&mut self) {
        self.edited_at = Some(Instant::now());
    }

    /// The draft to save now, if typing paused long enough or `now` is set.
    pub fn draft_to_save(&mut self, now: bool) -> Option<PipelineText> {
        let due = now
            || self
                .edited_at
                .is_some_and(|at| at.elapsed() >= crate::shell::DRAFT_DELAY);
        if !due || !self.loaded {
            return None;
        }
        self.edited_at = None;
        let text = self.text();
        if text == self.saved {
            return None;
        }
        self.saved = text.clone();
        Some(text)
    }

    pub fn draft_pending(&self) -> bool {
        self.edited_at.is_some()
    }

    /// How long until previews are due, while some are out of date.
    pub fn preview_wait(&self) -> Option<Duration> {
        let (_, at) = self.stale_from?;
        Some(PREVIEW_DELAY.saturating_sub(at.elapsed()))
    }

    /// Field paths of the documents the run read, for export columns.
    pub fn result_fields(&self) -> Option<Vec<String>> {
        let run = self.run.as_ref()?;
        let documents: Vec<Document> = run
            .documents
            .iter()
            .take(100)
            .filter_map(|raw| Document::try_from(raw.as_ref()).ok())
            .collect();
        (!documents.is_empty()).then(|| {
            nordpil_backend::field_paths(&documents)
                .into_iter()
                .map(|field| field.path)
                .collect()
        })
    }

    // --- Editing ---

    /// Adds a stage at `index`, e.g. from the stage picker.
    pub fn insert(&mut self, index: usize, name: &str) {
        let (body, cursor) = match nordpil_query::pipeline::stage(name) {
            Some(stage) => {
                let (before, after) = stage
                    .template
                    .split_once('|')
                    .unwrap_or((stage.template, ""));
                (format!("{before}{after}"), before.chars().count())
            }
            None => ("{  }".to_owned(), 2),
        };
        let card = self.card(StageText {
            name: name.to_owned(),
            body,
            enabled: true,
            collapsed: false,
        });
        self.focus = Some((card.id, cursor));
        let index = index.min(self.stages.len());
        self.stages.insert(index, card);
        self.changed(index, false);
    }

    pub fn duplicate(&mut self, index: usize) {
        let text = self.stages[index].text();
        let card = self.card(text);
        self.stages.insert(index + 1, card);
        self.changed(index + 1, true);
    }

    pub fn remove(&mut self, index: usize, requests: &mut impl Requests) {
        let card = self.stages.remove(index);
        if let Some(request) = card.request {
            requests.cancel(request);
        }
        self.changed(index, true);
    }

    /// Moves the stage at `from` to sit at `to`, counted before the move.
    pub fn move_stage(&mut self, from: usize, to: usize) {
        if from == to || from >= self.stages.len() {
            return;
        }
        let card = self.stages.remove(from);
        let to = to.min(self.stages.len());
        self.stages.insert(to, card);
        self.changed(from.min(to), true);
    }

    pub fn set_enabled(&mut self, index: usize, enabled: bool) {
        self.stages[index].enabled = enabled;
        self.changed(index, true);
    }

    pub fn rename(&mut self, index: usize, name: &str) {
        let card = &mut self.stages[index];
        card.name = name.to_owned();
        card.check();
        self.changed(index, true);
    }

    /// The body of the stage at `index` was typed in.
    pub fn body_edited(&mut self, index: usize) {
        self.stages[index].check();
        self.changed(index, false);
    }

    /// The options changed: every preview uses them.
    pub fn options_edited(&mut self) {
        self.changed(0, false);
    }

    pub fn set_sample(&mut self, sample: bool) {
        self.sample = sample;
        self.invalidate(0, true);
    }

    fn changed(&mut self, from: usize, now: bool) {
        self.errors.clear();
        self.edited();
        self.invalidate(from, now);
    }

    /// Puts `path: value` into the last stage when it is a `$match`, else
    /// into a new `$match` at the end, and focuses it there.
    pub fn add_to_match(&mut self, path: &str, value: &Bson) {
        let last_match = self
            .stages
            .last()
            .is_some_and(|card| card.name == "$match" && card.enabled);
        if !last_match {
            let card = self.card(StageText {
                name: "$match".to_owned(),
                body: String::new(),
                enabled: true,
                collapsed: false,
            });
            self.stages.push(card);
        }
        let index = self.stages.len() - 1;
        let card = &mut self.stages[index];
        let edit = nordpil_query::set_field(&card.body, path, value);
        let cursor = edit.text[..edit.cursor].chars().count();
        card.body = edit.text;
        card.collapsed = false;
        card.check();
        self.focus = Some((card.id, cursor));
        self.changed(index, false);
    }

    // --- Previews ---

    /// Marks the previews from stage `from` on as out of date. They run
    /// again once edits pause, or at once when `now`.
    fn invalidate(&mut self, from: usize, now: bool) {
        let at = if now {
            Instant::now()
                .checked_sub(PREVIEW_DELAY)
                .unwrap_or_else(Instant::now)
        } else {
            Instant::now()
        };
        let from = self
            .stale_from
            .map_or(from, |(earlier, _)| earlier.min(from));
        self.stale_from = Some((from, at));
    }

    fn cancel_previews(&mut self, requests: &mut impl Requests) {
        for card in &mut self.stages {
            if let Some(request) = card.request.take() {
                requests.cancel(request);
            }
            card.queued = false;
        }
    }

    /// Each frame: starts the previews that are due, a few at a time.
    /// Returns how long until more are due, for a repaint.
    pub fn poll_previews(
        &mut self,
        requests: &mut impl Requests,
        namespace: &Namespace,
        sample_size: u64,
    ) -> Option<Duration> {
        if let Some((from, at)) = self.stale_from {
            let waited = at.elapsed();
            if waited < PREVIEW_DELAY {
                return Some(PREVIEW_DELAY - waited);
            }
            self.stale_from = None;
            for card in self.stages.iter_mut().skip(from) {
                if let Some(request) = card.request.take() {
                    requests.cancel(request);
                }
                card.queued = true;
            }
        }
        let mut running = self.stages.iter().filter(|c| c.request.is_some()).count();
        for index in 0..self.stages.len() {
            if running >= MAX_PREVIEWS {
                break;
            }
            if !self.stages[index].queued {
                continue;
            }
            self.stages[index].queued = false;
            match self.preview_query(index, sample_size) {
                Ok(query) => {
                    let request = requests.send(Command::Aggregate {
                        namespace: namespace.clone(),
                        query: Box::new(query),
                    });
                    self.stages[index].request = Some(request);
                    running += 1;
                }
                Err(reason) => self.stages[index].preview = Preview::Skipped(reason),
            }
        }
        None
    }

    /// The query for stage `index`'s preview: the stages that are on up to
    /// it, then its first documents.
    fn preview_query(&self, index: usize, sample_size: u64) -> Result<AggregateQuery, String> {
        let card = &self.stages[index];
        if !card.enabled {
            return Err("Off: the pipeline skips this stage.".to_owned());
        }
        if card.error.is_some() {
            return Err("Fix this stage to see its output.".to_owned());
        }
        let mut pipeline = Vec::new();
        for (position, earlier) in self.stages[..=index].iter().enumerate() {
            if !earlier.enabled {
                continue;
            }
            if position < index && earlier.error.is_some() {
                return Err(format!("Fix stage {} first.", position + 1));
            }
            if position < index && earlier.writes() {
                return Err(format!("{} must be the last stage.", earlier.name));
            }
            let stage = nordpil_query::pipeline::stage_document(&earlier.name, &earlier.body)
                .map_err(|_| "Fix this stage to see its output.".to_owned())?;
            if earlier.writes() {
                let target = write_target(&stage, "")
                    .map(|(_, collection)| collection)
                    .unwrap_or_default();
                return Err(format!(
                    "No preview: this stage writes to {target}. Run the pipeline to write."
                ));
            }
            pipeline.push(stage);
        }
        if self.sample {
            let at = usize::from(
                pipeline
                    .first()
                    .and_then(|stage| stage.keys().next())
                    .is_some_and(|name| FIRST_ONLY.contains(&name.as_str())),
            );
            pipeline.insert(at, doc! { "$limit": to_i64(sample_size) });
        }
        let mut options = self.options().map_err(|errors| errors.join(" "))?;
        options.max_time = Some(PREVIEW_TIME_LIMIT);
        Ok(AggregateQuery {
            pipeline,
            options,
            skip: 0,
            limit: PREVIEW_SIZE,
        })
    }

    /// Takes a preview's documents. False when `request` isn't one.
    pub fn preview_arrived(
        &mut self,
        request: RequestId,
        result: Result<Vec<Document>, BackendError>,
    ) -> bool {
        let sampled = self.sample;
        let Some(card) = self
            .stages
            .iter_mut()
            .find(|card| card.request == Some(request))
        else {
            return false;
        };
        card.request = None;
        card.preview = match result {
            Ok(documents) => Preview::Documents {
                texts: documents.iter().map(preview_text).collect(),
                documents,
                sampled,
            },
            Err(error) if error.kind == ErrorKind::TimedOut => Preview::TimedOut,
            Err(error) => Preview::Failed(error.message),
        };
        true
    }

    pub fn owns_preview(&self, request: RequestId) -> bool {
        self.stages.iter().any(|card| card.request == Some(request))
    }

    /// Fields for completing stage `index`: what the stage before it gave in
    /// its preview, else the collection's sampled fields.
    pub fn fields_for(
        &self,
        index: usize,
        collection: &[FieldHint],
    ) -> (Vec<FieldHint>, Vec<ValueHint>) {
        let before = self.stages[..index]
            .iter()
            .rev()
            .find_map(|card| match &card.preview {
                Preview::Documents { documents, .. } if card.enabled && !documents.is_empty() => {
                    Some(documents)
                }
                _ => None,
            });
        match before {
            Some(documents) => {
                let fields = nordpil_backend::field_paths(documents)
                    .into_iter()
                    .map(|field| FieldHint {
                        path: field.path,
                        detail: field.types.join(", "),
                    })
                    .collect();
                (fields, nordpil_query::value_hints(documents))
            }
            None => (collection.to_vec(), Vec::new()),
        }
    }

    // --- Running ---

    /// The options as typed, read.
    pub fn options(&self) -> Result<nordpil_shell::AggregateOptions, Vec<String>> {
        let mut errors = Vec::new();
        let collation = match self.collation.trim() {
            "" => None,
            text if crate::widgets::code_editor::is_empty_object(text) => None,
            text => match nordpil_query::parse_document(text) {
                Ok(collation) => Some(collation),
                Err(parse) => {
                    errors.push(format!(
                        "Collation: {}",
                        crate::ui::document_dialogs::describe(text, parse)
                    ));
                    None
                }
            },
        };
        let hint = match self.hint.trim() {
            "" => None,
            text => match nordpil_query::parse_value(text) {
                Ok(value @ (Bson::String(_) | Bson::Document(_))) => Some(value),
                Ok(_) => {
                    errors.push(
                        "Hint: use an index name in quotes or a key pattern like { field: 1 }."
                            .to_owned(),
                    );
                    None
                }
                Err(parse) => {
                    errors.push(format!(
                        "Hint: {}",
                        crate::ui::document_dialogs::describe(text, parse)
                    ));
                    None
                }
            },
        };
        let max_time = match self.max_time_ms.trim() {
            "" => DEFAULT_MAX_TIME,
            text => match text.parse() {
                Ok(ms) => Duration::from_millis(ms),
                Err(_) => {
                    errors.push("Max time must be a whole number.".to_owned());
                    DEFAULT_MAX_TIME
                }
            },
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(nordpil_shell::AggregateOptions {
            allow_disk_use: self.allow_disk_use.then_some(true),
            hint,
            collation,
            max_time: Some(max_time),
        })
    }

    /// The stages that are on, read, with the options.
    pub fn query(&self) -> Result<PipelineQuery, Vec<String>> {
        let mut errors = Vec::new();
        let enabled: Vec<(usize, &StageCard)> = self
            .stages
            .iter()
            .enumerate()
            .filter(|(_, card)| card.enabled)
            .collect();
        let mut stages = Vec::new();
        for (position, (index, card)) in enabled.iter().enumerate() {
            match nordpil_query::pipeline::stage_document(&card.name, &card.body) {
                Ok(stage) => stages.push(stage),
                Err(parse) => errors.push(format!(
                    "Stage {} ({}): {}",
                    index + 1,
                    card.name,
                    crate::ui::document_dialogs::describe(&card.body, parse)
                )),
            }
            if card.writes() && position + 1 < enabled.len() {
                errors.push(format!(
                    "Stage {} ({}) must be the last stage.",
                    index + 1,
                    card.name
                ));
            }
        }
        let options = self.options();
        if let Err(option_errors) = &options {
            errors.extend(option_errors.iter().cloned());
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(PipelineQuery {
            stages,
            options: options.unwrap_or_default(),
        })
    }

    /// Runs the pipeline from its first page, or asks first when it writes.
    pub fn start(&mut self, requests: &mut impl Requests, namespace: &Namespace, page_size: u64) {
        let query = match self.query() {
            Ok(query) => query,
            Err(errors) => {
                self.errors = errors;
                return;
            }
        };
        self.errors.clear();
        requests.send(Command::RecordPipeline {
            namespace: namespace.clone(),
            pipeline: Box::new(self.text()),
        });
        let writes = query
            .stages
            .last()
            .and_then(|stage| write_target(stage, &namespace.database).map(|t| (stage, t)));
        if let Some((stage, target)) = writes {
            let merges = stage.contains_key("$merge");
            self.confirm = Some(WriteConfirm {
                query,
                target,
                merges,
            });
            return;
        }
        self.written = None;
        self.close_run(requests);
        let mut run = Run::new(query);
        run.pending_count = Some(requests.send(Command::CountAggregate {
            namespace: namespace.clone(),
            query: Box::new(run.query.clone()),
        }));
        self.run = Some(run);
        self.fetch(requests, namespace, page_size, page_size);
    }

    /// The user said yes to a pipeline that writes.
    pub fn write(&mut self, requests: &mut impl Requests, namespace: &Namespace) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        self.close_run(requests);
        let request = requests.send(Command::WriteAggregate {
            namespace: namespace.clone(),
            query: Box::new(confirm.query),
        });
        self.written = Some(Written {
            request: Some(request),
            target: confirm.target,
            result: None,
        });
    }

    pub fn write_arrived(&mut self, request: RequestId, result: Result<(), BackendError>) -> bool {
        let Some(written) = self
            .written
            .as_mut()
            .filter(|written| written.request == Some(request))
        else {
            return false;
        };
        written.request = None;
        written.result = Some(result.map_err(|error| error.message));
        true
    }

    /// Asks for documents from the end of those read so far, enough to
    /// fill the shown page.
    fn fetch(
        &mut self,
        requests: &mut impl Requests,
        namespace: &Namespace,
        page_size: u64,
        count: u64,
    ) {
        let Some(run) = &mut self.run else {
            return;
        };
        if let Some(request) = run.pending_page.take() {
            requests.cancel(request);
        }
        run.error = None;
        run.stopped = false;
        run.pending_page = Some(requests.send(Command::AggregatePage {
            namespace: namespace.clone(),
            key: run.key,
            query: Box::new(run.query.clone()),
            offset: run.documents.len() as u64,
            count: count.max(page_size),
        }));
    }

    /// Shows `page`, reading on from the cursor when the documents read so
    /// far don't reach it.
    pub fn show_page(
        &mut self,
        requests: &mut impl Requests,
        namespace: &Namespace,
        page: u64,
        page_size: u64,
    ) {
        let Some(run) = &mut self.run else {
            return;
        };
        run.page = page;
        if run.show_cached(page_size) {
            if let Some(request) = run.pending_page.take() {
                requests.cancel(request);
            }
            return;
        }
        let missing = (page + 1) * page_size - run.documents.len() as u64;
        self.fetch(requests, namespace, page_size, missing);
    }

    /// Runs the pipeline again from the start, staying on the shown page.
    pub fn refresh(&mut self, requests: &mut impl Requests, namespace: &Namespace, page_size: u64) {
        let Some(run) = &self.run else {
            self.start(requests, namespace, page_size);
            return;
        };
        let page = run.page;
        let query = run.query.clone();
        self.close_run(requests);
        let mut run = Run::new(query);
        run.page = page;
        run.pending_count = Some(requests.send(Command::CountAggregate {
            namespace: namespace.clone(),
            query: Box::new(run.query.clone()),
        }));
        self.run = Some(run);
        self.fetch(requests, namespace, page_size, (page + 1) * page_size);
    }

    /// Stops the page and count being read, on the server too. Pages read
    /// so far stay.
    pub fn stop(&mut self, requests: &mut impl Requests) {
        if let Some(written) = &mut self.written
            && let Some(request) = written.request.take()
        {
            requests.cancel(request);
            written.result = Some(Err("Stopped.".to_owned()));
        }
        let Some(run) = &mut self.run else {
            return;
        };
        for request in [run.pending_page.take(), run.pending_count.take()]
            .into_iter()
            .flatten()
        {
            requests.cancel(request);
        }
        run.stopped = true;
    }

    /// Whether Stop has anything to stop.
    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(Run::loading)
            || self
                .written
                .as_ref()
                .is_some_and(|written| written.request.is_some())
    }

    /// Ends the run and closes its cursor.
    pub fn close_run(&mut self, requests: &mut impl Requests) {
        if let Some(run) = self.run.take() {
            for request in [run.pending_page, run.pending_count].into_iter().flatten() {
                requests.cancel(request);
            }
            requests.send(Command::CloseCursor(run.key));
        }
    }

    /// Everything in flight, for closing the tab.
    pub fn close(&mut self, requests: &mut impl Requests) {
        self.cancel_previews(requests);
        self.stop(requests);
        self.close_run(requests);
        if let Some(request) = self
            .explain
            .as_mut()
            .and_then(|explain| explain.request.take())
        {
            requests.cancel(request);
        }
    }

    /// Takes a page of the run. False when `request` isn't one.
    pub fn page_arrived(
        &mut self,
        request: RequestId,
        result: Result<AggregatePage, BackendError>,
        page_size: u64,
    ) -> bool {
        let Some(run) = self
            .run
            .as_mut()
            .filter(|run| run.pending_page == Some(request))
        else {
            return false;
        };
        run.pending_page = None;
        match result {
            Ok(page) => {
                run.exhausted = page.exhausted;
                run.documents.extend(
                    page.documents
                        .iter()
                        // Documents from the server always convert.
                        .filter_map(|document| RawDocumentBuf::try_from(document).ok()),
                );
                if !run.show_cached(page_size) {
                    run.current = Some(crate::shell::page_of(&run.documents, run.page, page_size));
                }
            }
            Err(error) => {
                run.current = None;
                run.error = Some(error.message);
            }
        }
        true
    }

    /// Takes the run's count. False when `request` isn't it.
    pub fn count_arrived(
        &mut self,
        request: RequestId,
        result: Result<Option<u64>, BackendError>,
    ) -> bool {
        let Some(run) = self
            .run
            .as_mut()
            .filter(|run| run.pending_count == Some(request))
        else {
            return false;
        };
        run.pending_count = None;
        // A failed count shouldn't hide the documents; show the total as unknown.
        run.count = Some(result.unwrap_or(None));
        true
    }

    pub fn explain(&mut self, requests: &mut impl Requests, namespace: &Namespace) {
        let result = match self.query() {
            Ok(query) => {
                let request = requests.send(Command::ExplainAggregate {
                    namespace: namespace.clone(),
                    query: Box::new(query),
                });
                self.explain = Some(Explain {
                    request: Some(request),
                    result: None,
                });
                return;
            }
            Err(errors) => Err(errors.join("\n")),
        };
        self.explain = Some(Explain {
            request: None,
            result: Some(result),
        });
    }

    pub fn explain_arrived(
        &mut self,
        request: RequestId,
        result: Result<Document, BackendError>,
    ) -> bool {
        let Some(explain) = self
            .explain
            .as_mut()
            .filter(|explain| explain.request == Some(request))
        else {
            return false;
        };
        explain.request = None;
        explain.result = Some(
            result
                .map(|plan| nordpil_query::format_document(&plan, nordpil_query::Style::Pretty(2)))
                .map_err(|error| error.message),
        );
        true
    }

    /// A request ended without a result.
    pub fn cancelled(&mut self, request: RequestId) {
        for card in &mut self.stages {
            if card.request == Some(request) {
                card.request = None;
            }
        }
        if let Some(run) = &mut self.run {
            if run.pending_page == Some(request) {
                run.pending_page = None;
            }
            if run.pending_count == Some(request) {
                run.pending_count = None;
            }
        }
        if let Some(explain) = &mut self.explain
            && explain.request == Some(request)
        {
            explain.request = None;
        }
    }

    /// The enabled stages as one pipeline text, as typed.
    pub fn pipeline_text(&self) -> String {
        nordpil_query::pipeline::join_pipeline(
            self.stages
                .iter()
                .filter(|card| card.enabled)
                .map(|card| (card.name.as_str(), card.body.as_str())),
        )
    }

    /// `db.<collection>.aggregate([...])`.
    pub fn shell_statement(&self, collection: &str) -> String {
        format!(
            "{}.aggregate({})",
            crate::shell::collection_expression(collection),
            self.pipeline_text()
        )
    }

    /// Replaces the stages with the pasted pipeline, or says why it can't.
    pub fn paste(&mut self, text: &str, requests: &mut impl Requests) -> Result<(), String> {
        let stages = nordpil_query::pipeline::split_pipeline(text)
            .map_err(|errors| crate::ui::document_dialogs::describe(text, errors))?;
        let mut pipeline = self.text();
        pipeline.stages = stages
            .into_iter()
            .map(|(name, body)| StageText {
                name,
                body,
                enabled: true,
                collapsed: false,
            })
            .collect();
        self.set_text(pipeline, requests);
        Ok(())
    }
}

/// Where a `$out` or `$merge` stage writes: `(database, collection)`, with
/// `database` where the stage doesn't name one.
pub fn write_target(stage: &Document, database: &str) -> Option<(String, String)> {
    let (name, value) = stage.iter().next()?;
    let named = |spec: &Document, collection_key: &str| {
        Some((
            spec.get_str("db").unwrap_or(database).to_owned(),
            spec.get_str(collection_key).ok()?.to_owned(),
        ))
    };
    match (name.as_str(), value) {
        ("$out" | "$merge", Bson::String(collection)) => {
            Some((database.to_owned(), collection.clone()))
        }
        ("$out", Bson::Document(spec)) => named(spec, "coll"),
        ("$merge", Bson::Document(spec)) => match spec.get("into")? {
            Bson::String(collection) => Some((database.to_owned(), collection.clone())),
            Bson::Document(into) => named(into, "coll"),
            _ => None,
        },
        _ => None,
    }
}

/// The start of a document, highlighted, for a preview card. A cut
/// document ends in a plain `…`.
fn preview_text(document: &Document) -> (String, Vec<Span>) {
    let preview = nordpil_query::preview(document, PREVIEW_BYTES);
    let mut text = preview.text;
    let mut cut = preview.full_len > text.len();
    if let Some((end, _)) = text.match_indices('\n').nth(PREVIEW_LINES - 1) {
        text.truncate(end);
        cut = true;
    }
    let spans = nordpil_query::highlight(&text);
    if cut {
        text.push_str("\n…");
    }
    (text, spans)
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[cfg(all(test, feature = "screenshots"))]
impl AggregateState {
    /// Shows these previews and results as if they had arrived, for
    /// screenshots.
    pub fn show_sample(
        &mut self,
        previews: Vec<Vec<Document>>,
        results: Vec<Document>,
        count: u64,
    ) {
        self.stale_from = None;
        for (card, documents) in self.stages.iter_mut().zip(previews) {
            card.queued = false;
            card.request = None;
            card.preview = Preview::Documents {
                texts: documents.iter().map(preview_text).collect(),
                documents,
                sampled: true,
            };
        }
        let mut run = Run::new(PipelineQuery::default());
        run.documents = results
            .iter()
            .filter_map(|document| RawDocumentBuf::try_from(document).ok())
            .collect();
        run.count = Some(Some(count));
        run.current = Some(Results::new(results));
        self.run = Some(run);
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;
    use uuid::Uuid;

    use super::*;

    /// Stands in for the bridge: keeps what was sent and cancelled.
    #[derive(Default)]
    struct Fake {
        sent: Vec<(RequestId, Command)>,
        cancelled: Vec<RequestId>,
    }

    impl Requests for Fake {
        fn send(&mut self, command: Command) -> RequestId {
            let id = RequestId::new(self.sent.len() as u64 + 1);
            self.sent.push((id, command));
            id
        }

        fn cancel(&mut self, request: RequestId) {
            self.cancelled.push(request);
        }
    }

    impl Fake {
        fn previews(&self) -> Vec<(RequestId, Vec<Document>)> {
            self.sent
                .iter()
                .filter_map(|(id, command)| match command {
                    Command::Aggregate { query, .. } => Some((*id, query.pipeline.clone())),
                    _ => None,
                })
                .collect()
        }

        fn pages(&self) -> Vec<(RequestId, u64, u64)> {
            self.sent
                .iter()
                .filter_map(|(id, command)| match command {
                    Command::AggregatePage { offset, count, .. } => Some((*id, *offset, *count)),
                    _ => None,
                })
                .collect()
        }
    }

    fn namespace() -> Namespace {
        Namespace {
            connection: Uuid::nil(),
            database: "shop".to_owned(),
            collection: "orders".to_owned(),
        }
    }

    fn stage(name: &str, body: &str) -> StageText {
        StageText {
            name: name.to_owned(),
            body: body.to_owned(),
            enabled: true,
            collapsed: false,
        }
    }

    fn state(stages: Vec<StageText>, requests: &mut Fake) -> AggregateState {
        let mut state = AggregateState::new(1);
        state.load_draft(
            Some(PipelineText {
                stages,
                ..PipelineText::default()
            }),
            requests,
        );
        state
    }

    fn numbered(range: std::ops::Range<i32>) -> Vec<Document> {
        range.map(|n| doc! { "n": n }).collect()
    }

    #[test]
    fn previews_run_each_stage_so_far_on_a_sample() {
        let mut requests = Fake::default();
        let mut state = state(
            vec![
                stage("$match", "{ a: 1 }"),
                StageText {
                    enabled: false,
                    ..stage("$limit", "1")
                },
                stage("$project", "{ a: 1 }"),
            ],
            &mut requests,
        );
        state.poll_previews(&mut requests, &namespace(), 500);

        let previews = requests.previews();
        assert_eq!(previews.len(), 2);
        assert_eq!(
            previews[1].1,
            [
                doc! { "$limit": 500_i64 },
                doc! { "$match": { "a": 1 } },
                doc! { "$project": { "a": 1 } },
            ]
        );
        assert_eq!(
            state.stages[1].preview,
            Preview::Skipped("Off: the pipeline skips this stage.".to_owned())
        );

        // Stages that must come first keep their place before the sample.
        let mut requests = Fake::default();
        let mut state = state_with(&mut requests, "$geoNear");
        state.poll_previews(&mut requests, &namespace(), 500);
        let pipeline = &requests.previews()[0].1;
        assert!(pipeline[0].contains_key("$geoNear"));
        assert_eq!(pipeline[1], doc! { "$limit": 500_i64 });
    }

    fn state_with(requests: &mut Fake, name: &str) -> AggregateState {
        let template = nordpil_query::pipeline::stage(name).unwrap().template;
        state(vec![stage(name, &template.replace('|', "1"))], requests)
    }

    #[test]
    fn previews_wait_for_typing_to_pause_and_drop_stale_ones() {
        let mut requests = Fake::default();
        let mut state = state(
            vec![stage("$match", "{}"), stage("$limit", "5")],
            &mut requests,
        );
        state.poll_previews(&mut requests, &namespace(), 500);
        assert_eq!(requests.previews().len(), 2);
        let second = requests.previews()[1].0;

        state.stages[0].body = "{ a: 2 }".to_owned();
        state.body_edited(0);
        let wait = state.poll_previews(&mut requests, &namespace(), 500);
        assert!(wait.is_some_and(|wait| wait > Duration::ZERO));
        assert_eq!(requests.previews().len(), 2);

        std::thread::sleep(PREVIEW_DELAY);
        state.poll_previews(&mut requests, &namespace(), 500);
        assert!(requests.cancelled.contains(&second));
        assert_eq!(requests.previews().len(), 4);
        // The stale answer finds no stage.
        assert!(!state.preview_arrived(second, Ok(Vec::new())));
    }

    #[test]
    fn previews_run_a_few_at_a_time() {
        let mut requests = Fake::default();
        let stages = (0..6).map(|_| stage("$match", "{}")).collect();
        let mut state = state(stages, &mut requests);
        state.poll_previews(&mut requests, &namespace(), 500);
        assert_eq!(requests.previews().len(), MAX_PREVIEWS);

        let first = requests.previews()[0].0;
        assert!(state.preview_arrived(first, Ok(numbered(0..3))));
        state.poll_previews(&mut requests, &namespace(), 500);
        assert_eq!(requests.previews().len(), MAX_PREVIEWS + 1);
        assert!(matches!(
            &state.stages[0].preview,
            Preview::Documents { documents, sampled: true, .. } if documents.len() == 3
        ));
    }

    #[test]
    fn stages_after_an_error_or_a_write_have_no_preview() {
        let mut requests = Fake::default();
        let mut state = state(
            vec![
                stage("$match", "{ a: }"),
                stage("$out", "'copy'"),
                stage("$limit", "1"),
            ],
            &mut requests,
        );
        state.poll_previews(&mut requests, &namespace(), 500);
        assert!(requests.previews().is_empty());
        assert!(
            state.stages[0]
                .error
                .as_deref()
                .unwrap()
                .starts_with("Line 1")
        );
        assert_eq!(
            state.stages[1].preview,
            Preview::Skipped("Fix stage 1 first.".to_owned())
        );

        state.stages[0].body = "{ a: 1 }".to_owned();
        state.body_edited(0);
        let errors = state.query().unwrap_err();
        assert_eq!(errors, ["Stage 2 ($out) must be the last stage."]);
    }

    #[test]
    fn pages_come_from_the_cursor_once_then_from_memory() {
        let mut requests = Fake::default();
        let mut state = state(vec![stage("$sort", "{ n: 1 }")], &mut requests);
        state.start(&mut requests, &namespace(), 10);
        assert!(
            requests
                .sent
                .iter()
                .any(|(_, command)| matches!(command, Command::RecordPipeline { .. }))
        );
        let (first, offset, count) = requests.pages()[0];
        assert_eq!((offset, count), (0, 10));

        assert!(state.page_arrived(
            first,
            Ok(AggregatePage {
                documents: numbered(0..10),
                exhausted: false,
            }),
            10,
        ));
        let run = state.run.as_ref().unwrap();
        assert!(run.has_next(10));
        assert_eq!(run.current.as_ref().unwrap().documents.len(), 10);

        state.show_page(&mut requests, &namespace(), 1, 10);
        let (second, offset, count) = requests.pages()[1];
        assert_eq!((offset, count), (10, 10));
        state.page_arrived(
            second,
            Ok(AggregatePage {
                documents: numbered(10..15),
                exhausted: true,
            }),
            10,
        );
        let run = state.run.as_ref().unwrap();
        assert!(!run.has_next(10));
        assert_eq!(run.current.as_ref().unwrap().documents, numbered(10..15));

        // Back, and other page sizes, need no query.
        state.show_page(&mut requests, &namespace(), 0, 10);
        state.show_page(&mut requests, &namespace(), 2, 5);
        assert_eq!(requests.pages().len(), 2);
        let run = state.run.as_ref().unwrap();
        assert_eq!(run.current.as_ref().unwrap().documents, numbered(10..15));
        // Past the end shows the last page.
        state.show_page(&mut requests, &namespace(), 9, 5);
        assert_eq!(state.run.as_ref().unwrap().page, 2);
    }

    #[test]
    fn stop_cancels_the_page_and_count_and_keeps_what_was_read() {
        let mut requests = Fake::default();
        let mut state = state(vec![stage("$sort", "{ n: 1 }")], &mut requests);
        state.start(&mut requests, &namespace(), 10);
        let count = requests
            .sent
            .iter()
            .find_map(|(id, command)| {
                matches!(command, Command::CountAggregate { .. }).then_some(*id)
            })
            .unwrap();
        let page = requests.pages()[0].0;
        assert!(state.running());

        state.stop(&mut requests);
        assert!(requests.cancelled.contains(&page));
        assert!(requests.cancelled.contains(&count));
        assert!(!state.running());
        assert!(state.run.as_ref().unwrap().stopped);

        // A new run closes the old cursor.
        let key = state.run.as_ref().unwrap().key;
        state.start(&mut requests, &namespace(), 10);
        assert!(
            requests.sent.iter().any(
                |(_, command)| matches!(command, Command::CloseCursor(closed) if *closed == key)
            )
        );
        assert_ne!(state.run.as_ref().unwrap().key, key);
    }

    #[test]
    fn pipelines_that_write_ask_first() {
        let mut requests = Fake::default();
        let mut state = state(
            vec![
                stage("$match", "{}"),
                stage("$merge", "{ into: { db: 'bi', coll: 'daily' } }"),
            ],
            &mut requests,
        );
        state.start(&mut requests, &namespace(), 10);
        assert!(requests.pages().is_empty());
        let confirm = state.confirm.as_ref().unwrap();
        assert_eq!(confirm.target, ("bi".to_owned(), "daily".to_owned()));
        assert!(confirm.merges);

        state.write(&mut requests, &namespace());
        let (request, command) = requests.sent.last().unwrap();
        assert!(matches!(command, Command::WriteAggregate { .. }));
        assert!(state.write_arrived(*request, Ok(())));
        assert_eq!(state.written.as_ref().unwrap().result, Some(Ok(())));
    }

    #[test]
    fn finds_where_writes_go() {
        let target = |stage: Document| write_target(&stage, "shop");
        let at = |db: &str, coll: &str| Some((db.to_owned(), coll.to_owned()));
        assert_eq!(target(doc! { "$out": "copy" }), at("shop", "copy"));
        assert_eq!(
            target(doc! { "$out": { "db": "bi", "coll": "copy" } }),
            at("bi", "copy")
        );
        assert_eq!(target(doc! { "$merge": "copy" }), at("shop", "copy"));
        assert_eq!(
            target(doc! { "$merge": { "into": "copy", "on": "_id" } }),
            at("shop", "copy")
        );
        assert_eq!(target(doc! { "$match": {} }), None);
    }

    #[test]
    fn add_to_match_uses_the_last_match_or_adds_one() {
        let mut requests = Fake::default();
        let mut state = state(vec![stage("$group", "{ _id: '$a' }")], &mut requests);
        state.add_to_match("_id", &Bson::String("x".to_owned()));
        assert_eq!(state.stages.len(), 2);
        assert_eq!(state.stages[1].name, "$match");
        assert_eq!(state.stages[1].body, "{ _id: 'x' }");

        state.add_to_match("n", &Bson::Int32(2));
        assert_eq!(state.stages.len(), 2);
        assert_eq!(state.stages[1].body, "{ _id: 'x', n: 2 }");
    }

    #[test]
    fn pastes_a_pipeline_and_copies_it_back() {
        let mut requests = Fake::default();
        let mut state = state(Vec::new(), &mut requests);
        let text = "[\n  { $match: { a: 1 } },\n  { $limit: 5 }\n]";
        state.paste(text, &mut requests).unwrap();
        assert_eq!(state.stages.len(), 2);
        assert_eq!(state.pipeline_text(), text);
        assert_eq!(
            state.shell_statement("user-logs"),
            format!("db.getCollection('user-logs').aggregate({text})")
        );
        assert!(state.paste("{ a: 1 }", &mut requests).is_err());
    }

    #[test]
    fn moving_a_stage_reruns_previews_from_the_first_one_moved() {
        let mut requests = Fake::default();
        let mut state = state(
            vec![
                stage("$match", "{}"),
                stage("$limit", "1"),
                stage("$skip", "1"),
            ],
            &mut requests,
        );
        state.poll_previews(&mut requests, &namespace(), 500);
        let first = requests.previews()[0].0;
        state.preview_arrived(first, Ok(Vec::new()));

        state.move_stage(2, 1);
        let names: Vec<_> = state.stages.iter().map(|card| card.name.as_str()).collect();
        assert_eq!(names, ["$match", "$skip", "$limit"]);
        state.poll_previews(&mut requests, &namespace(), 500);
        // Stage 1 kept its preview; stages 2 and 3 ran again.
        assert_eq!(requests.previews().len(), 5);
        assert_eq!(requests.previews()[4].1.last(), Some(&doc! { "$limit": 1 }));
    }

    #[test]
    fn drafts_save_once_typing_pauses() {
        let mut requests = Fake::default();
        let mut state = state(vec![stage("$limit", "1")], &mut requests);
        assert_eq!(state.draft_to_save(true), None);

        state.insert(1, "$skip");
        assert_eq!(state.draft_to_save(false), None);
        let draft = state.draft_to_save(true).unwrap();
        assert_eq!(draft.stages[1].name, "$skip");
        assert_eq!(draft.stages[1].body, "10");
        assert_eq!(state.focus, Some((state.stages[1].id, 2)));
    }
}
