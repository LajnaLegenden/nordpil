//! Ask AI: an agent writes the tab's find query or pipeline from a request.
//! The agent sees field paths, types, index keys and the current query with
//! its values masked; the app checks the answer and puts the values back.

use std::time::{Duration, Instant};

use eframe::egui;
use nordpil_backend::RequestId;
use nordpil_backend::ai::{AgentAnswer, AnswerKind, CurrentQuery, SchemaField, Surface, Turn};
use nordpil_backend::references::Mapping;
use nordpil_backend::settings::AiAgent;
use nordpil_query::FieldHint;
use nordpil_query::mask::{Masks, collapse_paths};
use nordpil_query::pipeline::{is_write, split_pipeline, stage_document};

use crate::tabs::{QueryInputs, build_query};
use crate::ui::document_dialogs::describe;
use crate::widgets::code_editor::CodeEditor;

/// How long a request waits for the indexes and linked collections' fields.
pub const GATHER_TIME: Duration = Duration::from_secs(5);

/// The agent's answer with its values back, as shown and edited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub pipeline_kind: bool,
    pub filter: String,
    pub project: String,
    pub sort: String,
    pub limit: String,
    pub pipeline: String,
}

pub struct PreviewEditors {
    pub filter: CodeEditor,
    pub project: CodeEditor,
    pub sort: CodeEditor,
    pub pipeline: CodeEditor,
}

/// A request on its way: waiting for schema parts, then running.
pub struct Pending {
    /// The request text as the agent gets it.
    pub text: String,
    /// This is the retry after an unusable answer.
    pub retry: bool,
    pub since: Instant,
    /// `None` while gathering the indexes and linked fields.
    pub request: Option<RequestId>,
}

pub struct AskAi {
    /// The tab it writes to.
    pub tab_id: u64,
    pub surface: Surface,
    pub agent: AiAgent,
    pub prompt: String,
    pub refine: String,
    /// Same for the whole conversation, so values keep their placeholders.
    pub masks: Option<Masks>,
    pub turns: Vec<Turn>,
    pub pending: Option<Pending>,
    pub preview: Option<Preview>,
    pub editors: PreviewEditors,
    pub explanation: String,
    pub error: Option<String>,
    /// Exactly what the agent got last.
    pub sent: Option<String>,
    /// Focus the prompt on the first frame.
    pub focus: bool,
}

impl AskAi {
    pub fn new(tab_id: u64, surface: Surface, agent: AiAgent, prompt: String) -> Self {
        let editor = |name: &str, rows: usize| {
            CodeEditor::new(egui::Id::unique(("ask_ai", tab_id, name)), true).rows(rows)
        };
        Self {
            tab_id,
            surface,
            agent,
            prompt,
            refine: String::new(),
            masks: None,
            turns: Vec::new(),
            pending: None,
            preview: None,
            editors: PreviewEditors {
                filter: editor("filter", 3),
                project: editor("project", 1),
                sort: editor("sort", 1),
                pipeline: editor("pipeline", 10).line_numbers(),
            },
            explanation: String::new(),
            error: None,
            sent: None,
            focus: true,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts over with the prompt: a new conversation.
    pub fn start(&mut self) {
        let text = self.prompt.trim().to_owned();
        if text.is_empty() || self.is_busy() {
            return;
        }
        self.turns.clear();
        self.masks = None;
        self.preview = None;
        self.explanation.clear();
        self.queue(text, false);
    }

    /// Asks for a change to the shown answer.
    pub fn refine(&mut self) {
        let text = self.refine.trim().to_owned();
        if text.is_empty() || self.is_busy() {
            return;
        }
        self.queue(text, false);
    }

    pub fn queue(&mut self, text: String, retry: bool) {
        self.error = None;
        self.pending = Some(Pending {
            text,
            retry,
            since: Instant::now(),
            request: None,
        });
    }
}

/// The schema as the agent gets it: dynamic keys merged into `<key>`.
pub fn schema(fields: &[FieldHint]) -> Vec<SchemaField> {
    let types: Vec<Vec<String>> = fields
        .iter()
        .map(|field| {
            field
                .detail
                .split(", ")
                .filter(|kind| !kind.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect();
    collapse_paths(
        fields
            .iter()
            .zip(&types)
            .map(|(field, types)| (field.path.as_str(), types.as_slice())),
    )
    .into_iter()
    .map(|(path, types)| SchemaField { path, types })
    .collect()
}

/// Collections the agent may join: those the user linked a field of this
/// collection to, then those the request names. Never this collection.
pub fn linked_collections(
    own: &str,
    fields: &[FieldHint],
    mappings: &[Mapping],
    collections: &[String],
    request: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let mut linked: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut add = |name: &str, via: Option<(String, String)>| {
        if name == own {
            return;
        }
        let at = match linked.iter().position(|(linked, _)| linked == name) {
            Some(at) => at,
            None if linked.len() < nordpil_backend::ai::MAX_LINKED => {
                linked.push((name.to_owned(), Vec::new()));
                linked.len() - 1
            }
            None => return,
        };
        if let Some(via) = via {
            linked[at].1.push(via);
        }
    };
    for mapping in mappings {
        let used = fields
            .iter()
            .any(|field| field.path.rsplit('.').next() == Some(mapping.key.as_str()));
        if used {
            let field = mapping.field.clone().unwrap_or_else(|| "_id".to_owned());
            add(&mapping.collection, Some((mapping.key.clone(), field)));
        }
    }
    let words: Vec<&str> = request
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .filter(|word| !word.is_empty())
        .collect();
    for name in collections {
        if words.contains(&name.as_str()) {
            add(name, None);
        }
    }
    linked
}

/// The query to show the agent, masked; `None` when there is none.
pub fn masked_current(masks: &mut Masks, current: &Preview) -> Option<CurrentQuery> {
    if current.pipeline_kind {
        let pipeline = masks.mask(&[&current.pipeline]).remove(0);
        let empty = matches!(pipeline.trim(), "" | "[]");
        return (!empty).then_some(CurrentQuery::Pipeline(pipeline));
    }
    let masked = masks.mask(&[
        &current.filter,
        &current.project,
        &current.sort,
        &current.limit,
    ]);
    let blank =
        |text: &str| crate::widgets::code_editor::is_empty_object(text) || text.trim().is_empty();
    if masked.iter().all(|text| blank(text)) {
        return None;
    }
    let part = |text: &String| {
        if blank(text) {
            String::new()
        } else {
            text.clone()
        }
    };
    Some(CurrentQuery::Find {
        filter: part(&masked[0]),
        projection: part(&masked[1]),
        sort: part(&masked[2]),
        limit: part(&masked[3]),
    })
}

/// Why an answer can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// Found in the masked answer, so the message holds no values and can go
    /// back to the agent.
    Retry(String),
    /// Found after the values went back; shown to the user only.
    Final {
        message: String,
        preview: Box<Preview>,
    },
}

/// Checks the answer, puts the values back, and checks it again.
pub fn check_answer(
    answer: &AgentAnswer,
    masks: &Masks,
    read_only: bool,
) -> Result<Preview, Rejected> {
    let retry = |message: String| Rejected::Retry(message);
    let restore = |text: &str| masks.restore(text).map_err(retry);
    match answer.kind {
        AnswerKind::Find => {
            for (name, text) in [
                ("filter", &answer.filter),
                ("projection", &answer.projection),
                ("sort", &answer.sort),
            ] {
                if !text.trim().is_empty() {
                    nordpil_query::parse_document(text).map_err(|errors| {
                        retry(format!("The {name}: {}", describe(text, errors)))
                    })?;
                }
            }
            let limit = answer.limit.trim();
            let placeholder = limit.trim_matches(['\'', '"']).starts_with("__v");
            if !limit.is_empty() && !placeholder && limit.parse::<u64>().is_err() {
                return Err(retry("The limit must be a whole number.".to_owned()));
            }
            let preview = Preview {
                pipeline_kind: false,
                filter: restore(&answer.filter)?,
                project: restore(&answer.projection)?,
                sort: restore(&answer.sort)?,
                limit: restore(limit)?.trim_matches(['\'', '"']).to_owned(),
                pipeline: String::new(),
            };
            let inputs = QueryInputs {
                filter: preview.filter.clone(),
                project: preview.project.clone(),
                sort: preview.sort.clone(),
                limit: preview.limit.clone(),
                ..QueryInputs::default()
            };
            match build_query(&inputs) {
                Ok(_) => Ok(preview),
                Err(errors) => Err(Rejected::Final {
                    message: errors.join(" "),
                    preview: Box::new(preview),
                }),
            }
        }
        AnswerKind::Pipeline => {
            check_pipeline(&answer.pipeline, read_only).map_err(retry)?;
            let preview = Preview {
                pipeline_kind: true,
                pipeline: restore(&answer.pipeline)?,
                ..Preview::default()
            };
            match check_pipeline(&preview.pipeline, read_only) {
                Ok(()) => Ok(preview),
                Err(message) => Err(Rejected::Final {
                    message,
                    preview: Box::new(preview),
                }),
            }
        }
    }
}

fn check_pipeline(text: &str, read_only: bool) -> Result<(), String> {
    let stages = split_pipeline(text).map_err(|errors| describe(text, errors))?;
    let last = stages.len().saturating_sub(1);
    for (index, (name, body)) in stages.iter().enumerate() {
        stage_document(name, body).map_err(|errors| {
            format!("Stage {} ({name}): {}", index + 1, describe(body, errors))
        })?;
        if is_write(name) && read_only {
            return Err("The connection is read-only: don't use $out or $merge.".to_owned());
        }
        if is_write(name) && index != last {
            return Err(format!("{name} must be the last stage."));
        }
    }
    Ok(())
}

/// The masked answer as raw text for the preview, when it can't be used.
pub fn raw_preview(answer: &AgentAnswer, masks: &Masks) -> Preview {
    let restore = |text: &str| masks.restore(text).unwrap_or_else(|_| text.to_owned());
    Preview {
        pipeline_kind: answer.kind == AnswerKind::Pipeline,
        filter: restore(&answer.filter),
        project: restore(&answer.projection),
        sort: restore(&answer.sort),
        limit: restore(&answer.limit),
        pipeline: restore(&answer.pipeline),
    }
}

/// The text asking the agent to fix an answer.
pub fn retry_request(error: &str, request: &str) -> String {
    format!("Your answer could not be used: {error} Answer this request again, fixed: {request}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(kind: AnswerKind) -> AgentAnswer {
        AgentAnswer {
            kind,
            filter: String::new(),
            projection: String::new(),
            sort: String::new(),
            limit: String::new(),
            pipeline: String::new(),
            explanation: String::new(),
        }
    }

    fn hint(path: &str, detail: &str) -> FieldHint {
        FieldHint {
            path: path.to_owned(),
            detail: detail.to_owned(),
        }
    }

    #[test]
    fn find_answers_get_their_values_back() {
        let mut masks = Masks::new(["status"]);
        let masked = masks.mask(&["{ status: 'paid' }", "", "", "20"]);
        let answer = AgentAnswer {
            filter: format!(
                "{{ status: {}, total: {{ $gt: 100 }} }}",
                &masked[0][10..25]
            ),
            sort: "{ createdAt: -1 }".to_owned(),
            limit: masked[3].clone(),
            ..answer(AnswerKind::Find)
        };
        let preview = check_answer(&answer, &masks, false).unwrap();
        assert_eq!(preview.filter, "{ status: 'paid', total: { $gt: 100 } }");
        assert_eq!(preview.limit, "20");
    }

    #[test]
    fn bad_syntax_and_unknown_placeholders_are_retried() {
        let masks = Masks::new(Vec::<&str>::new());
        let broken = AgentAnswer {
            filter: "{ total: ".to_owned(),
            ..answer(AnswerKind::Find)
        };
        assert!(matches!(
            check_answer(&broken, &masks, false),
            Err(Rejected::Retry(_))
        ));
        let invented = AgentAnswer {
            filter: "{ a: '__v7_String__' }".to_owned(),
            ..answer(AnswerKind::Find)
        };
        assert_eq!(
            check_answer(&invented, &masks, false),
            Err(Rejected::Retry(
                "Unknown placeholder __v7_String__.".to_owned()
            ))
        );
    }

    #[test]
    fn write_stages_must_be_last_and_are_refused_when_read_only() {
        let masks = Masks::new(Vec::<&str>::new());
        let pipeline = |text: &str| AgentAnswer {
            pipeline: text.to_owned(),
            ..answer(AnswerKind::Pipeline)
        };
        let out = pipeline("[{ $match: {} }, { $out: 'copy' }]");
        assert!(check_answer(&out, &masks, false).is_ok());
        assert!(matches!(
            check_answer(&out, &masks, true),
            Err(Rejected::Retry(_))
        ));
        let early = pipeline("[{ $out: 'copy' }, { $match: {} }]");
        assert!(matches!(
            check_answer(&early, &masks, false),
            Err(Rejected::Retry(_))
        ));
    }

    #[test]
    fn schema_collapses_dynamic_keys_from_hints() {
        let fields = [
            hint("scores", "Object"),
            hint("scores.507f1f77bcf86cd799439011", "Int32"),
            hint("scores.507f1f77bcf86cd799439012", "Double"),
        ];
        let schema = schema(&fields);
        assert_eq!(schema.len(), 2);
        assert_eq!(schema[1].path, "scores.<key>");
        assert_eq!(schema[1].types, ["Int32", "Double"]);
    }

    #[test]
    fn linked_collections_come_from_mappings_and_the_request() {
        let fields = [hint("ownerRef", "ObjectId"), hint("items.sku", "String")];
        let mappings = [
            Mapping {
                key: "ownerRef".to_owned(),
                collection: "users".to_owned(),
                field: None,
            },
            Mapping {
                key: "unused".to_owned(),
                collection: "ghosts".to_owned(),
                field: None,
            },
            Mapping {
                key: "parent".to_owned(),
                collection: "orders".to_owned(),
                field: None,
            },
        ];
        let collections = ["users", "products", "orders", "ghosts"].map(str::to_owned);
        let linked = linked_collections(
            "orders",
            &fields,
            &mappings,
            &collections,
            "join products, and orders too",
        );
        assert_eq!(
            linked,
            [
                (
                    "users".to_owned(),
                    vec![("ownerRef".to_owned(), "_id".to_owned())]
                ),
                ("products".to_owned(), Vec::new()),
            ]
        );
    }

    #[test]
    fn an_empty_query_is_not_sent() {
        let mut masks = Masks::default();
        assert_eq!(masked_current(&mut masks, &Preview::default()), None);
        let filter = Preview {
            filter: "{  }".to_owned(),
            ..Preview::default()
        };
        assert_eq!(masked_current(&mut masks, &filter), None);
    }
}
