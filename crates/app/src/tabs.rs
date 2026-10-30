//! One open collection: its query inputs, results and view settings.

use std::time::Duration;

use bson::{Bson, Document};
use eframe::egui;
use nordpil_backend::{FindQuery, FoundDocument, IndexInfo, Namespace, RequestId, SavedQuery};
use nordpil_query::{FieldHint, Preview, ValueHint};

use crate::connections::Selection;
use crate::widgets::code_editor::{CodeEditor, is_empty_object};

pub const PAGE_SIZES: [u64; 4] = [10, 20, 50, 100];
const DEFAULT_PAGE_SIZE: u64 = 20;
/// Like Compass, queries without a max time get one, so a bad filter on a
/// huge collection can't run forever.
const DEFAULT_MAX_TIME: Duration = Duration::from_secs(60);

/// What a tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Documents,
    Indexes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    Json,
    Table,
}

/// The query inputs as typed; also what the history stores.
pub type QueryInputs = nordpil_backend::QueryText;

pub struct QueryEditors {
    pub filter: CodeEditor,
    pub project: CodeEditor,
    pub sort: CodeEditor,
    pub collation: CodeEditor,
    pub hint: CodeEditor,
}

/// The query behind the shown results. Paging re-runs it with another window.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedQuery {
    pub query: FindQuery,
    /// The user's skip and limit (0 = none); pages are windows inside them.
    pub skip: u64,
    pub limit: u64,
}

pub struct Results {
    pub documents: Vec<Document>,
    /// Formatted, highlighted text for the JSON view, cut for huge documents.
    pub previews: Vec<Preview>,
}

pub struct CollectionTab {
    pub id: u64,
    pub target: Selection,
    pub inputs: QueryInputs,
    pub editors: QueryEditors,
    pub show_options: bool,
    pub view: View,
    pub page: u64,
    pub page_size: u64,
    pub applied: Option<AppliedQuery>,
    pub results: Option<Results>,
    /// `None` until counted, `Some(None)` when there were too many to count.
    pub count: Option<Option<u64>>,
    pub pending_find: Option<RequestId>,
    pub pending_count: Option<RequestId>,
    /// From running the query on the server.
    pub error: Option<String>,
    /// From reading the query inputs.
    pub input_errors: Vec<String>,
    /// This collection's favorite and recent queries.
    pub history: Vec<SavedQuery>,
    /// Field paths sampled from the collection, for autocomplete.
    pub fields: Vec<FieldHint>,
    /// Values in the current results, for autocomplete.
    pub values: Vec<ValueHint>,
    pub panel: Panel,
    /// `None` until loaded.
    pub indexes: Option<Result<Vec<IndexInfo>, String>>,
    pub indexes_request: Option<RequestId>,
}

impl CollectionTab {
    pub fn new(id: u64, target: Selection) -> Self {
        let editor = |field: &str, hint: &str| {
            CodeEditor::new(egui::Id::unique(("query", id, field)), false).hint(hint)
        };
        Self {
            id,
            target,
            inputs: QueryInputs::default(),
            editors: QueryEditors {
                filter: editor("filter", "{ field: 'value' }").frameless().object(),
                project: editor("project", "{ field: 1 }").object(),
                sort: editor("sort", "{ field: -1 }").object(),
                collation: editor("collation", "{ locale: 'en' }").object(),
                hint: editor("hint", "{ field: 1 } or 'index_name'"),
            },
            show_options: false,
            view: View::List,
            page: 0,
            page_size: DEFAULT_PAGE_SIZE,
            applied: None,
            results: None,
            count: None,
            pending_find: None,
            pending_count: None,
            error: None,
            input_errors: Vec::new(),
            history: Vec::new(),
            fields: Vec::new(),
            values: Vec::new(),
            panel: Panel::Documents,
            indexes: None,
            indexes_request: None,
        }
    }

    pub fn namespace(&self) -> Namespace {
        Namespace {
            connection: self.target.connection,
            database: self.target.database.clone(),
            collection: self.target.collection.clone(),
        }
    }

    /// Fills in a saved query, opening the options when it uses any.
    pub fn apply_inputs(&mut self, inputs: QueryInputs) {
        let uses_options = QueryInputs {
            filter: String::new(),
            ..inputs.clone()
        } != QueryInputs::default();
        self.show_options |= uses_options;
        self.inputs = inputs;
    }

    pub fn is_loading(&self) -> bool {
        self.pending_find.is_some()
    }

    /// Find options for the current page.
    pub fn page_query(&self) -> Option<FindQuery> {
        let applied = self.applied.as_ref()?;
        let offset = self.page * self.page_size;
        let limit = match applied.limit {
            0 => self.page_size,
            limit => self.page_size.min(limit.saturating_sub(offset)),
        };
        Some(FindQuery {
            skip: applied.skip + offset,
            limit: i64::try_from(limit).unwrap_or(i64::MAX),
            ..applied.query.clone()
        })
    }

    /// Matching documents after the user's skip and limit, if counted.
    pub fn total(&self) -> Option<Option<u64>> {
        let applied = self.applied.as_ref()?;
        self.count.map(|count| {
            count.map(|count| {
                let after_skip = count.saturating_sub(applied.skip);
                match applied.limit {
                    0 => after_skip,
                    limit => after_skip.min(limit),
                }
            })
        })
    }

    pub fn has_next_page(&self) -> bool {
        let Some(results) = &self.results else {
            return false;
        };
        let shown_up_to = self.page * self.page_size + results.documents.len() as u64;
        if let Some(applied) = &self.applied
            && applied.limit > 0
            && shown_up_to >= applied.limit
        {
            return false;
        }
        match self.total() {
            Some(Some(total)) => shown_up_to < total,
            // Unknown total: a full page suggests there's more.
            _ => results.documents.len() as u64 == self.page_size,
        }
    }

    pub fn set_documents(&mut self, found: Vec<FoundDocument>) {
        let (documents, previews): (Vec<Document>, Vec<Preview>) = found
            .into_iter()
            .map(|found| (found.document, found.preview))
            .unzip();
        self.values = nordpil_query::value_hints(&documents);
        self.results = Some(Results {
            documents,
            previews,
        });
    }
}

/// Reads the query inputs. Errors name the input and column.
pub fn build_query(inputs: &QueryInputs) -> Result<AppliedQuery, Vec<String>> {
    let mut errors = Vec::new();
    let filter = document("Filter", &inputs.filter, &mut errors).unwrap_or_default();
    let projection = document("Project", &inputs.project, &mut errors).filter(|d| !d.is_empty());
    let sort = document("Sort", &inputs.sort, &mut errors).filter(|d| !d.is_empty());
    let collation = document("Collation", &inputs.collation, &mut errors).filter(|d| !d.is_empty());
    let hint = match inputs.hint.trim() {
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
            Err(parse_errors) => {
                push_parse_errors("Hint", text, &parse_errors, &mut errors);
                None
            }
        },
    };
    let skip = number("Skip", &inputs.skip, &mut errors).unwrap_or(0);
    let limit = number("Limit", &inputs.limit, &mut errors).unwrap_or(0);
    let max_time = number("Max time", &inputs.max_time_ms, &mut errors)
        .map_or(DEFAULT_MAX_TIME, Duration::from_millis);

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(AppliedQuery {
        query: FindQuery {
            filter,
            projection,
            sort,
            collation,
            hint,
            max_time: Some(max_time),
            ..FindQuery::default()
        },
        skip,
        limit,
    })
}

/// What the history keeps of `inputs`: an editor holding only the `{  }` it
/// put in on focus counts as empty.
pub fn history_entry(inputs: &QueryInputs) -> QueryInputs {
    let clear = |text: &String| {
        if is_empty_object(text) {
            String::new()
        } else {
            text.clone()
        }
    };
    QueryInputs {
        filter: clear(&inputs.filter),
        project: clear(&inputs.project),
        sort: clear(&inputs.sort),
        collation: clear(&inputs.collation),
        ..inputs.clone()
    }
}

fn document(name: &str, text: &str, errors: &mut Vec<String>) -> Option<Document> {
    nordpil_query::parse_document(text)
        .map_err(|parse_errors| push_parse_errors(name, text, &parse_errors, errors))
        .ok()
}

fn push_parse_errors(
    name: &str,
    text: &str,
    parse_errors: &[nordpil_query::ParseError],
    errors: &mut Vec<String>,
) {
    for error in parse_errors {
        let column = text[..error.span.start.min(text.len())].chars().count() + 1;
        errors.push(format!("{name}, column {column}: {}", error.message));
    }
}

/// Empty means "not set".
fn number(name: &str, text: &str, errors: &mut Vec<String>) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.parse()
        .map_err(|_| errors.push(format!("{name} must be a whole number.")))
        .ok()
}

#[cfg(test)]
mod tests {
    use bson::doc;
    use uuid::Uuid;

    use super::*;

    fn tab_with(skip: u64, limit: u64) -> CollectionTab {
        let mut tab = CollectionTab::new(
            1,
            Selection {
                connection: Uuid::nil(),
                database: "db".to_owned(),
                collection: "c".to_owned(),
            },
        );
        tab.applied = Some(AppliedQuery {
            query: FindQuery::default(),
            skip,
            limit,
        });
        tab
    }

    #[test]
    fn builds_query_from_inputs() {
        let inputs = QueryInputs {
            filter: "{ a: 1 }".to_owned(),
            sort: "{ b: -1 }".to_owned(),
            hint: "'a_1'".to_owned(),
            skip: "5".to_owned(),
            max_time_ms: "1500".to_owned(),
            ..QueryInputs::default()
        };

        let applied = build_query(&inputs).unwrap();

        assert_eq!(applied.query.filter, doc! { "a": 1 });
        assert_eq!(applied.query.sort, Some(doc! { "b": -1 }));
        assert_eq!(applied.query.projection, None);
        assert_eq!(applied.query.hint, Some(Bson::String("a_1".to_owned())));
        assert_eq!(applied.query.max_time, Some(Duration::from_millis(1500)));
        assert_eq!((applied.skip, applied.limit), (5, 0));
    }

    #[test]
    fn history_entry_drops_braces_left_by_focus() {
        let blank = QueryInputs {
            filter: "{  }".to_owned(),
            sort: "{}".to_owned(),
            ..QueryInputs::default()
        };
        let set = QueryInputs {
            filter: "{ a: 1 }".to_owned(),
            ..blank.clone()
        };

        assert!(history_entry(&blank).is_empty());
        assert_eq!(history_entry(&set).filter, "{ a: 1 }");
        assert_eq!(history_entry(&set).sort, "");
    }

    #[test]
    fn reports_every_bad_input() {
        let inputs = QueryInputs {
            filter: "{ a: }".to_owned(),
            hint: "5".to_owned(),
            limit: "-1".to_owned(),
            ..QueryInputs::default()
        };

        assert_eq!(
            build_query(&inputs).unwrap_err(),
            [
                "Filter, column 6: Expected a value.",
                "Hint: use an index name in quotes or a key pattern like { field: 1 }.",
                "Limit must be a whole number.",
            ]
        );
    }

    #[test]
    fn pages_stay_inside_user_skip_and_limit() {
        let mut tab = tab_with(10, 45);
        tab.page = 2;

        let query = tab.page_query().unwrap();

        assert_eq!((query.skip, query.limit), (10 + 40, 5));
        tab.count = Some(Some(100));
        assert_eq!(tab.total(), Some(Some(45)));
    }
}
