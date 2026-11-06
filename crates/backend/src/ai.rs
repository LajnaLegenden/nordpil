//! Asks a command-line agent (Claude Code, Codex or the user's own program)
//! to write a find query or an aggregation pipeline.
//!
//! The agent never sees data: the prompt holds field paths and types, index
//! keys, the collections the user linked, and the current query with every
//! value masked (see [`nordpil_query::mask`]). The built-in agents run
//! without tools, in an empty directory, so they can't look anything up.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use mongodb::bson::{Bson, Document};
use nordpil_query::mask::{DYNAMIC_KEY, collapse_paths, is_dynamic_segment};
use nordpil_query::{Style, format_document};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::settings::{AiAgent, AiCustom, AiPreset, AiSettings};
use crate::{BackendError, ErrorKind, FieldInfo, IndexInfo};

/// Placeholder in a custom agent's arguments for the answer's JSON Schema file.
pub const SCHEMA_FILE: &str = "{schema_file}";
/// Linked collections sent at most.
pub const MAX_LINKED: usize = 5;
const LOOKUP_TIME_LIMIT: Duration = Duration::from_secs(5);
const STDERR_LINES: usize = 20;

/// Where the answer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The Documents panel: a find query, or a pipeline when find can't do it.
    Find,
    /// The Aggregate panel: always a pipeline.
    Pipeline,
}

/// A field path with the types seen at it. Map-like keys read `<key>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaField {
    pub path: String,
    pub types: Vec<String>,
}

/// An index's key pattern, e.g. `{ createdAt: -1 }`, and flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKeys {
    /// `None` when the name looks like it holds values.
    pub name: Option<String>,
    pub keys: String,
    pub unique: bool,
    pub sparse: bool,
    pub hidden: bool,
}

/// Another collection the agent may join with `$lookup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedCollection {
    pub name: String,
    /// `(key here, field there)` pairs from the user's reference mappings.
    pub via: Vec<(String, String)>,
    pub fields: Vec<SchemaField>,
}

/// The tab's query with its values masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentQuery {
    Find {
        filter: String,
        projection: String,
        sort: String,
        limit: String,
    },
    Pipeline(String),
}

/// An earlier request and the agent's answer to it, values still masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub request: String,
    pub answer: AgentAnswer,
}

/// Everything the agent is told. Nothing here holds a database value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub surface: Surface,
    pub schema: Vec<SchemaField>,
    pub indexes: Vec<IndexKeys>,
    pub linked: Vec<LinkedCollection>,
    pub current: Option<CurrentQuery>,
    pub turns: Vec<Turn>,
    pub request: String,
    pub read_only: bool,
    /// E.g. `2026-11-06T10:22:00Z`, for requests like "last week".
    pub now: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerKind {
    Find,
    Pipeline,
}

/// The agent's answer, values still masked. Parts not used are empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAnswer {
    pub kind: AnswerKind,
    pub filter: String,
    pub projection: String,
    pub sort: String,
    pub limit: String,
    pub pipeline: String,
    pub explanation: String,
}

/// The agent to run and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Agent {
    Claude(AiPreset),
    Codex(AiPreset),
    Custom(AiCustom),
}

impl Agent {
    /// `None` for [`AiAgent::Off`].
    pub fn from_settings(settings: &AiSettings, choice: AiAgent) -> Option<Self> {
        match choice {
            AiAgent::Off => None,
            AiAgent::Claude => Some(Self::Claude(settings.claude.clone())),
            AiAgent::Codex => Some(Self::Codex(settings.codex.clone())),
            AiAgent::Custom => Some(Self::Custom(settings.custom.clone())),
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Claude(_) => "Claude",
            Self::Codex(_) => "Codex",
            Self::Custom(custom) => &custom.program,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    pub agent: Agent,
    pub timeout: Duration,
    pub prompt: Prompt,
}

/// Field paths with dynamic keys merged into `<key>`.
pub fn schema_fields(fields: &[FieldInfo]) -> Vec<SchemaField> {
    collapse_paths(
        fields
            .iter()
            .map(|field| (field.path.as_str(), field.types.as_slice())),
    )
    .into_iter()
    .map(|(path, types)| SchemaField {
        path,
        types: types.into_iter().map(str::to_owned).collect(),
    })
    .collect()
}

/// Key patterns only: never `partialFilterExpression` or other options that
/// can hold values.
pub fn index_keys(indexes: &[IndexInfo]) -> Vec<IndexKeys> {
    indexes
        .iter()
        .map(|index| {
            let mut keys = Document::new();
            for (path, direction) in index.keys() {
                let path = collapse_path(&path);
                let direction = match direction {
                    Bson::String(kind) => Bson::String(kind),
                    Bson::Int32(n) => Bson::Int32(n.signum()),
                    Bson::Int64(n) => Bson::Int32(n.signum() as i32),
                    Bson::Double(n) => Bson::Int32(if n < 0.0 { -1 } else { 1 }),
                    _ => Bson::Int32(1),
                };
                keys.insert(path, direction);
            }
            let flag = |name: &str| index.spec.get_bool(name).unwrap_or(false);
            let name_is_safe = index
                .name
                .split(['.', '_'])
                .all(|part| !is_dynamic_segment(part) || part.len() <= 2);
            IndexKeys {
                name: name_is_safe.then(|| index.name.clone()),
                keys: format_document(&keys, Style::Compact),
                unique: flag("unique"),
                sparse: flag("sparse"),
                hidden: flag("hidden"),
            }
        })
        .collect()
}

fn collapse_path(path: &str) -> String {
    path.split('.')
        .map(|segment| {
            if is_dynamic_segment(segment) {
                DYNAMIC_KEY
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The system prompt and the user message.
pub fn build_prompt(prompt: &Prompt) -> (String, String) {
    (system_prompt(prompt), user_message(prompt))
}

fn system_prompt(prompt: &Prompt) -> String {
    let surface = match prompt.surface {
        Surface::Find => {
            "Prefer kind \"find\". Answer kind \"pipeline\" only when a find query \
             can't do it, e.g. grouping, counting per value, joins or computed fields."
        }
        Surface::Pipeline => "Always answer kind \"pipeline\".",
    };
    let writes = if prompt.read_only {
        "The connection is read-only: never use $out or $merge."
    } else {
        "Use $out or $merge only when the request asks to write results \
         somewhere; it must be the last stage."
    };
    format!(
        "You write MongoDB queries for nordpil, a MongoDB GUI. You never see \
the data, only the shape of one collection (\"the collection\") and the \
user's request. Answer with one JSON object that matches the given JSON \
Schema, and nothing else.

Syntax: mongosh shell syntax, e.g. {{ age: {{ $gt: 30 }}, name: /^an/i, \
_id: ObjectId('...'), at: ISODate('2024-01-01T00:00:00Z') }}. Unquoted keys \
are fine. No JavaScript functions, variables or comments.

Kind \"find\": filter, projection and sort are documents and limit is a whole \
number; leave a part empty when it isn't needed, and pipeline empty. \
Kind \"pipeline\": pipeline is the whole array of stages, e.g. \
[{{ $match: {{ ... }} }}, {{ $group: {{ ... }} }}]; leave the find parts empty. \
{surface}

Hidden values: quoted strings like '__v3_Date__' stand for values in the \
user's current query; the word after the number is the value's BSON type. \
The app puts the real values back. Where you need that value, write the \
placeholder exactly as given, as a whole value on its own. Never change, \
combine or invent placeholders. Key parts like '__k2__' are hidden key names; \
keep them the same way. <key> in a field path stands for keys that differ \
between documents, such as ids or dates.

{writes} Prefer conditions and sorts that the listed indexes can serve when \
that doesn't change the result. explanation is one short sentence on what \
the query does.

The current time is {now}.",
        now = prompt.now,
    )
}

fn user_message(prompt: &Prompt) -> String {
    let mut out = String::from("## Collection fields\n");
    write_fields(&mut out, &prompt.schema);
    if !prompt.indexes.is_empty() {
        out.push_str("\n## Indexes\n");
        for index in &prompt.indexes {
            let mut flags = Vec::new();
            for (on, flag) in [
                (index.unique, "unique"),
                (index.sparse, "sparse"),
                (index.hidden, "hidden"),
            ] {
                if on {
                    flags.push(flag);
                }
            }
            let name = index.name.as_deref().unwrap_or("(name hidden)");
            out.push_str(&format!("- {name}: {}", index.keys));
            if !flags.is_empty() {
                out.push_str(&format!(" ({})", flags.join(", ")));
            }
            out.push('\n');
        }
    }
    for linked in &prompt.linked {
        out.push_str(&format!("\n## Linked collection `{}`\n", linked.name));
        for (key, field) in &linked.via {
            out.push_str(&format!(
                "`{key}` holds `{}.{field}` values.\n",
                linked.name
            ));
        }
        write_fields(&mut out, &linked.fields);
    }
    if let Some(current) = &prompt.current {
        out.push_str("\n## Current query (values hidden)\n");
        match current {
            CurrentQuery::Find {
                filter,
                projection,
                sort,
                limit,
            } => {
                for (label, text) in [
                    ("filter", filter),
                    ("projection", projection),
                    ("sort", sort),
                    ("limit", limit),
                ] {
                    if !text.trim().is_empty() {
                        out.push_str(&format!("{label}: {}\n", text.trim()));
                    }
                }
            }
            CurrentQuery::Pipeline(pipeline) => {
                out.push_str(pipeline.trim());
                out.push('\n');
            }
        }
    }
    if !prompt.turns.is_empty() {
        out.push_str("\n## Earlier in this conversation\n");
        for turn in &prompt.turns {
            out.push_str(&format!(
                "Request: {}\nYour answer: {}\n",
                turn.request.trim(),
                answer_json(&turn.answer)
            ));
        }
    }
    out.push_str("\n## Request\n");
    out.push_str(prompt.request.trim());
    out.push('\n');
    out
}

fn write_fields(out: &mut String, fields: &[SchemaField]) {
    if fields.is_empty() {
        out.push_str("(no fields sampled)\n");
    }
    for field in fields {
        out.push_str(&format!("- {}: {}\n", field.path, field.types.join(", ")));
    }
}

fn answer_json(answer: &AgentAnswer) -> String {
    let kind = match answer.kind {
        AnswerKind::Find => "find",
        AnswerKind::Pipeline => "pipeline",
    };
    json!({
        "kind": kind,
        "filter": answer.filter,
        "projection": answer.projection,
        "sort": answer.sort,
        "limit": answer.limit,
        "pipeline": answer.pipeline,
        "explanation": answer.explanation,
    })
    .to_string()
}

/// The answer's JSON Schema, strict enough for Codex's structured output.
pub fn answer_schema(surface: Surface) -> Value {
    let kinds = match surface {
        Surface::Find => json!(["find", "pipeline"]),
        Surface::Pipeline => json!(["pipeline"]),
    };
    let text = |description: &str| json!({ "type": "string", "description": description });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "filter", "projection", "sort", "limit", "pipeline", "explanation"],
        "properties": {
            "kind": { "type": "string", "enum": kinds },
            "filter": text("Find: the filter document, or empty."),
            "projection": text("Find: the projection document, or empty."),
            "sort": text("Find: the sort document, or empty."),
            "limit": text("Find: a whole number, or empty."),
            "pipeline": text("Pipeline: the array of stages, or empty."),
            "explanation": text("One short sentence on what the query does."),
        },
    })
}

/// Reads the answer object; numbers and nulls in it are taken as text.
pub fn parse_answer(value: &Value, surface: Surface) -> Result<AgentAnswer, BackendError> {
    let Value::Object(object) = value else {
        return Err(BackendError::new(
            "The agent's answer is not a JSON object.",
        ));
    };
    let field = |name: &str| match object.get(name) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let pipeline = field("pipeline");
    let kind = match (surface, field("kind").as_str()) {
        (Surface::Pipeline, _) | (_, "pipeline") => AnswerKind::Pipeline,
        (_, "find") => AnswerKind::Find,
        _ if !pipeline.trim().is_empty() => AnswerKind::Pipeline,
        _ => AnswerKind::Find,
    };
    Ok(AgentAnswer {
        kind,
        filter: field("filter"),
        projection: field("projection"),
        sort: field("sort"),
        limit: field("limit"),
        pipeline,
        explanation: field("explanation"),
    })
}

/// The last whole JSON object in `text`, so chatty output still works.
pub fn last_json_object(text: &str) -> Option<Value> {
    let mut found = None;
    let mut at = 0;
    while let Some(offset) = text[at..].find('{') {
        let start = at + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value @ Value::Object(_))) => {
                found = Some(value);
                at = start + values.byte_offset();
            }
            _ => at = start + 1,
        }
    }
    found
}

/// How to start the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    /// A name to look up on `PATH`, or a path.
    program: String,
    args: Vec<String>,
    stdin: String,
    /// Codex writes its last message here.
    output_file: Option<PathBuf>,
}

fn invocation(agent: &Agent, prompt: &Prompt, dir: &Path) -> Result<Invocation, BackendError> {
    let (system, user) = build_prompt(prompt);
    let schema = answer_schema(prompt.surface);
    let schema_path = dir.join("schema.json");
    let write_schema = || {
        std::fs::write(&schema_path, schema.to_string()).map_err(|error| {
            BackendError::new("Could not write the agent's files.").with_detail(error)
        })
    };
    let program = |preset: &AiPreset, name: &str| {
        if preset.path.trim().is_empty() {
            name.to_owned()
        } else {
            preset.path.trim().to_owned()
        }
    };
    let model = |preset: &AiPreset, default: &str| {
        let model = preset.model.trim();
        let model = if model.is_empty() { default } else { model };
        ["--model".to_owned(), model.to_owned()]
    };
    Ok(match agent {
        Agent::Claude(preset) => {
            let mut args: Vec<String> = [
                "-p",
                "--output-format",
                "json",
                "--json-schema",
                &schema.to_string(),
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--system-prompt",
                &system,
            ]
            .map(str::to_owned)
            .to_vec();
            args.extend(model(preset, AiPreset::CLAUDE_MODEL));
            Invocation {
                program: program(preset, "claude"),
                args,
                stdin: user,
                output_file: None,
            }
        }
        Agent::Codex(preset) => {
            write_schema()?;
            let output = dir.join("answer.json");
            let mut args: Vec<String> = [
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--sandbox",
                "read-only",
                "--cd",
            ]
            .map(str::to_owned)
            .to_vec();
            args.push(dir.display().to_string());
            args.push("--output-schema".to_owned());
            args.push(schema_path.display().to_string());
            args.push("--output-last-message".to_owned());
            args.push(output.display().to_string());
            args.extend(model(preset, AiPreset::CODEX_MODEL));
            args.push("-".to_owned());
            Invocation {
                program: program(preset, "codex"),
                args,
                stdin: format!("{system}\n\n{user}"),
                output_file: Some(output),
            }
        }
        Agent::Custom(custom) => {
            if custom.program.trim().is_empty() {
                return Err(BackendError::new(
                    "Set the custom agent's program in Settings.",
                ));
            }
            if custom.args.iter().any(|arg| arg.contains(SCHEMA_FILE)) {
                write_schema()?;
            }
            let schema_file = schema_path.display().to_string();
            Invocation {
                program: custom.program.trim().to_owned(),
                args: custom
                    .args
                    .iter()
                    .map(|arg| arg.replace(SCHEMA_FILE, &schema_file))
                    .collect(),
                stdin: format!(
                    "{system}\n\n{user}\nAnswer with one JSON object matching this JSON Schema:\n{schema}\n"
                ),
                output_file: None,
            }
        }
    })
}

/// Runs the agent and reads its answer, values still masked.
pub async fn ask(request: &AgentRequest) -> Result<AgentAnswer, BackendError> {
    let dir = TempDir::new()?;
    let invocation = invocation(&request.agent, &request.prompt, &dir.0)?;
    let (program, path) = resolve(&invocation.program).await?;
    let run = run(&program, path.as_deref(), &invocation, &dir.0);
    let output = match tokio::time::timeout(request.timeout, run).await {
        Ok(output) => output?,
        Err(_) => {
            return Err(BackendError::new(format!(
                "{} took longer than {} s and was stopped.",
                request.agent.label(),
                request.timeout.as_secs()
            ))
            .with_kind(ErrorKind::TimedOut));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(failed(&request.agent, &output.status, &stdout, &stderr));
    }
    let value = match &request.agent {
        Agent::Claude(_) => claude_answer(&stdout)?,
        Agent::Codex(_) => invocation
            .output_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| last_json_object(&text))
            .or_else(|| last_json_object(&stdout))
            .ok_or_else(|| no_answer(&stdout, &stderr))?,
        Agent::Custom(_) => last_json_object(&stdout).ok_or_else(|| no_answer(&stdout, &stderr))?,
    };
    parse_answer(&value, request.prompt.surface)
}

fn claude_answer(stdout: &str) -> Result<Value, BackendError> {
    let result = last_json_object(stdout).ok_or_else(|| no_answer(stdout, ""))?;
    if result.get("is_error").and_then(Value::as_bool) == Some(true) {
        let message = result
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(BackendError::new(format!("Claude failed: {message}")));
    }
    if let Some(answer @ Value::Object(_)) = result.get("structured_output") {
        return Ok(answer.clone());
    }
    result
        .get("result")
        .and_then(Value::as_str)
        .and_then(last_json_object)
        .ok_or_else(|| no_answer(stdout, ""))
}

fn no_answer(stdout: &str, stderr: &str) -> BackendError {
    BackendError::new("The agent's output held no JSON answer.")
        .with_detail(tail(&format!("{stdout}\n{stderr}")))
}

fn failed(
    agent: &Agent,
    status: &std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
) -> BackendError {
    let last = stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim);
    let code = status
        .code()
        .map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
    let message = match last {
        Some(line) => format!("{} stopped with {code}: {line}", agent.label()),
        None => format!("{} stopped with {code}.", agent.label()),
    };
    BackendError::new(message).with_detail(tail(&format!("{stderr}\n{stdout}")))
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

async fn run(
    program: &Path,
    path: Option<&str>,
    invocation: &Invocation,
    dir: &Path,
) -> Result<std::process::Output, BackendError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(&invocation.args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .kill_on_drop(true);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        BackendError::new(format!("Could not start {}.", program.display())).with_detail(error)
    })?;
    // Agents start helpers of their own; stop the whole group with it.
    let _group = ProcessGroup(child.id());
    let stdin = child.stdin.take();
    let text = invocation.stdin.clone();
    let write = async move {
        if let Some(mut stdin) = stdin {
            // A closed pipe means the agent quit early; its exit status says why.
            let _ = stdin.write_all(text.as_bytes()).await;
        }
    };
    let (_, output) = tokio::join!(write, child.wait_with_output());
    output.map_err(|error| BackendError::new("The agent stopped unexpectedly.").with_detail(error))
}

/// Kills the agent's process group when dropped: after it ends, on timeout,
/// or when the request is cancelled.
struct ProcessGroup(Option<u32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

/// A fresh directory the agent runs in, removed afterwards.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, BackendError> {
        let dir = std::env::temp_dir().join(format!("nordpil-agent-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).map_err(|error| {
            BackendError::new("Could not create a folder for the agent.").with_detail(error)
        })?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The login shell's `PATH`, read once: apps started from a desktop menu
/// get a short `PATH` without tools installed by version managers.
static LOGIN_PATH: OnceCell<Option<String>> = OnceCell::const_new();

/// The program's path, and the `PATH` to give it.
async fn resolve(program: &str) -> Result<(PathBuf, Option<String>), BackendError> {
    let login_path = LOGIN_PATH.get_or_init(login_path).await.clone();
    if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
        return Ok((PathBuf::from(program), login_path));
    }
    let search = login_path
        .clone()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let found = std::env::split_paths(&search)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file());
    match found {
        Some(found) => Ok((found, login_path)),
        // Windows finds `.exe` files itself.
        None if cfg!(windows) => Ok((PathBuf::from(program), None)),
        None => Err(BackendError::new(format!(
            "Could not find `{program}`. Install it, or set its path in Settings."
        ))),
    }
}

async fn login_path() -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())?;
    let output = tokio::time::timeout(
        LOOKUP_TIME_LIMIT,
        tokio::process::Command::new(shell)
            .args(["-lic", "printf '\\n__NORDPIL_PATH__%s__END__\\n' \"$PATH\""])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let start = stdout.rfind("__NORDPIL_PATH__")? + "__NORDPIL_PATH__".len();
    let end = start + stdout[start..].find("__END__")?;
    Some(stdout[start..end].to_owned()).filter(|path| !path.is_empty())
}

#[cfg(test)]
mod tests;
