use std::time::Duration;

use mongodb::bson::{Bson, Document, doc, oid::ObjectId};
use nordpil_query::mask::Masks;
use proptest::prelude::*;

use super::*;
use crate::field_paths;

fn prompt(surface: Surface) -> Prompt {
    Prompt {
        surface,
        schema: vec![SchemaField {
            path: "total".to_owned(),
            types: vec!["Int32".to_owned()],
        }],
        indexes: Vec::new(),
        linked: Vec::new(),
        current: None,
        turns: Vec::new(),
        request: "orders over 100".to_owned(),
        read_only: false,
        now: "2026-11-06T10:00:00Z".to_owned(),
    }
}

#[test]
fn claude_runs_without_tools_or_mcp_servers() {
    let dir = std::env::temp_dir();
    let agent = Agent::Claude(AiPreset {
        path: String::new(),
        model: "sonnet".to_owned(),
    });
    let run = invocation(&agent, &prompt(Surface::Find), &dir).unwrap();
    assert_eq!(run.program, "claude");
    let at = |flag: &str| run.args.iter().position(|arg| arg == flag).unwrap();
    assert_eq!(run.args[at("--tools") + 1], "");
    assert!(run.args.contains(&"--strict-mcp-config".to_owned()));
    assert!(run.args.contains(&"--no-session-persistence".to_owned()));
    assert_eq!(run.args[at("--model") + 1], "sonnet");
    assert!(run.args[at("--system-prompt") + 1].contains("Hidden values"));
    assert!(run.stdin.contains("## Request\norders over 100"));
}

#[test]
fn codex_runs_read_only_without_user_config() {
    let dir = std::env::temp_dir().join(format!("nordpil-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let agent = Agent::Codex(AiPreset {
        path: "/opt/codex".to_owned(),
        model: String::new(),
    });
    let run = invocation(&agent, &prompt(Surface::Pipeline), &dir).unwrap();
    assert_eq!(run.program, "/opt/codex");
    let at = |flag: &str| run.args.iter().position(|arg| arg == flag).unwrap();
    assert_eq!(run.args[at("--sandbox") + 1], "read-only");
    assert!(run.args.contains(&"--ignore-user-config".to_owned()));
    assert!(run.args.contains(&"--ephemeral".to_owned()));
    assert_eq!(run.args[at("--model") + 1], AiPreset::CODEX_MODEL);
    let schema = std::fs::read_to_string(&run.args[at("--output-schema") + 1]).unwrap();
    assert!(schema.contains(r#""enum":["pipeline"]"#), "{schema}");
    assert!(run.stdin.contains("Always answer kind \"pipeline\""));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn read_only_forbids_writes_in_the_prompt() {
    let mut read_only = prompt(Surface::Pipeline);
    read_only.read_only = true;
    let (system, _) = build_prompt(&read_only);
    assert!(system.contains("never use $out or $merge"));
}

#[test]
fn last_json_object_skips_chatter_and_nested_objects() {
    let text = "Thinking { not json\n{\"a\": {\"b\": 1}}\nmore {\"kind\": \"find\"} done";
    assert_eq!(last_json_object(text), Some(json!({ "kind": "find" })));
    assert_eq!(last_json_object("no json"), None);
}

#[test]
fn parse_answer_takes_numbers_as_text_and_infers_the_kind() {
    let answer = parse_answer(
        &json!({ "filter": "{}", "limit": 20, "sort": null }),
        Surface::Find,
    )
    .unwrap();
    assert_eq!(answer.kind, AnswerKind::Find);
    assert_eq!(answer.limit, "20");
    assert_eq!(answer.sort, "");
    let answer = parse_answer(&json!({ "pipeline": "[]" }), Surface::Find).unwrap();
    assert_eq!(answer.kind, AnswerKind::Pipeline);
    let answer = parse_answer(&json!({ "kind": "find" }), Surface::Pipeline).unwrap();
    assert_eq!(answer.kind, AnswerKind::Pipeline);
}

#[test]
fn index_keys_drop_options_and_dynamic_names() {
    let index = |name: &str, spec: Document| IndexInfo {
        name: name.to_owned(),
        spec,
        size: None,
        ops: None,
        since: None,
    };
    let keys = index_keys(&[
        index(
            "status_1",
            doc! { "key": { "status": 1 }, "unique": true, "partialFilterExpression": { "status": "SECRET" } },
        ),
        index(
            "scores.507f1f77bcf86cd799439011_-1",
            doc! { "key": { "scores.507f1f77bcf86cd799439011": -1 } },
        ),
    ]);
    assert_eq!(keys[0].name.as_deref(), Some("status_1"));
    assert_eq!(keys[0].keys, "{ status: 1 }");
    assert!(keys[0].unique);
    assert_eq!(keys[1].name, None);
    assert_eq!(keys[1].keys, "{ 'scores.<key>': -1 }");
    let mut with_indexes = prompt(Surface::Find);
    with_indexes.indexes = keys;
    let (system, user) = build_prompt(&with_indexes);
    assert!(!format!("{system}{user}").contains("SECRET"));
    assert!(!format!("{system}{user}").contains("507f"));
}

/// Values use letters that no key, type name or prompt text uses.
fn value() -> impl Strategy<Value = Bson> {
    prop_oneof![
        "[QXZW%]{2,8}".prop_map(Bson::String),
        any::<i32>().prop_map(Bson::Int32),
        any::<bool>().prop_map(Bson::Boolean),
        any::<[u8; 12]>().prop_map(|bytes| Bson::ObjectId(ObjectId::from_bytes(bytes))),
    ]
}

fn key() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-z]{1,6}",
        1 => any::<[u8; 12]>().prop_map(|bytes| ObjectId::from_bytes(bytes).to_hex()),
        1 => "[QXZW]{2,5}@[a-z]{2,4}",
    ]
}

fn document() -> impl Strategy<Value = Document> {
    let leaf = value();
    let tree = leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            prop::collection::vec((key(), inner.clone()), 0..4)
                .prop_map(|members| Bson::Document(members.into_iter().collect())),
            prop::collection::vec(inner, 0..3).prop_map(Bson::Array),
        ]
    });
    prop::collection::vec((key(), tree), 1..5).prop_map(|members| members.into_iter().collect())
}

/// Every string value, ObjectId and dynamic key in the documents.
fn secrets(value: &Bson, out: &mut Vec<String>) {
    match value {
        Bson::String(text) => out.push(text.clone()),
        Bson::ObjectId(id) => out.push(id.to_hex()),
        Bson::Document(document) => {
            for (key, value) in document {
                if !key.bytes().all(|b| b.is_ascii_lowercase()) {
                    out.push(key.split('@').next().unwrap_or(key).to_owned());
                }
                secrets(value, out);
            }
        }
        Bson::Array(items) => items.iter().for_each(|item| secrets(item, out)),
        _ => {}
    }
}

proptest! {
    #[test]
    fn the_prompt_holds_no_values(documents in prop::collection::vec(document(), 1..4)) {
        let schema = schema_fields(&field_paths(&documents));
        let mut masks = Masks::new(schema.iter().map(|field| field.path.as_str()));
        let filter = nordpil_query::format_document(&documents[0], nordpil_query::Style::Compact);
        let masked = masks.mask(&[&filter, "{ _id: -1 }", "20"]);
        let earlier = AgentAnswer {
            kind: AnswerKind::Find,
            filter: masked[0].clone(),
            projection: String::new(),
            sort: masked[1].clone(),
            limit: masked[2].clone(),
            pipeline: String::new(),
            explanation: "Finds them.".to_owned(),
        };
        let prompt = Prompt {
            surface: Surface::Find,
            schema,
            indexes: Vec::new(),
            linked: Vec::new(),
            current: Some(CurrentQuery::Find {
                filter: masked[0].clone(),
                projection: String::new(),
                sort: masked[1].clone(),
                limit: masked[2].clone(),
            }),
            turns: vec![Turn { request: "first".to_owned(), answer: earlier }],
            request: "newest first".to_owned(),
            read_only: false,
            now: "2026-11-06T10:00:00Z".to_owned(),
        };
        let (system, user) = build_prompt(&prompt);
        let sent = format!("{system}\n{user}");
        let mut hidden = Vec::new();
        for document in &documents {
            secrets(&Bson::Document(document.clone()), &mut hidden);
        }
        for secret in hidden {
            prop_assert!(!sent.contains(&secret), "{} leaked:\n{}", secret, sent);
        }
    }
}

#[cfg(unix)]
mod agents {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use super::*;

    const ANSWER: &str = r#"{"kind":"find","filter":"{ total: { $gt: 100 } }","projection":"","sort":"","limit":"","pipeline":"","explanation":"Big orders."}"#;

    /// A shell script standing in for an agent, in its own folder.
    fn script(body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nordpil-agent-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    fn request(program: &Path, args: &[&str], timeout: Duration) -> AgentRequest {
        AgentRequest {
            agent: Agent::Custom(AiCustom {
                program: program.display().to_string(),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            }),
            timeout,
            prompt: prompt(Surface::Find),
        }
    }

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[tokio::test]
    async fn reads_the_last_json_object_after_chatter() {
        let (dir, agent) = script(&format!(
            "cat > \"$(dirname \"$0\")/stdin.txt\"\necho 'Let me think {{ a bit'\necho '{ANSWER}'"
        ));
        let answer = ask(&request(&agent, &[], Duration::from_secs(20)))
            .await
            .unwrap();
        assert_eq!(answer.kind, AnswerKind::Find);
        assert_eq!(answer.filter, "{ total: { $gt: 100 } }");
        let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
        assert!(stdin.contains("## Request\norders over 100"), "{stdin}");
        assert!(stdin.contains("matching this JSON Schema"), "{stdin}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn passes_the_schema_file() {
        let (dir, agent) = script(&format!(
            "cat > /dev/null\ngrep -q additionalProperties \"$2\" && echo '{ANSWER}'"
        ));
        let answer = ask(&request(
            &agent,
            &["--schema", SCHEMA_FILE],
            Duration::from_secs(20),
        ))
        .await
        .unwrap();
        assert_eq!(answer.explanation, "Big orders.");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn output_without_json_is_an_error() {
        let (dir, agent) = script("cat > /dev/null\necho 'I cannot help with that.'");
        let error = ask(&request(&agent, &[], Duration::from_secs(20)))
            .await
            .unwrap_err();
        assert_eq!(error.message, "The agent's output held no JSON answer.");
        assert!(error.detail.unwrap().contains("I cannot help"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failing_agent_reports_its_last_line() {
        let (dir, agent) = script("cat > /dev/null\necho 'ERROR: usage limit' >&2\nexit 3");
        let error = ask(&request(&agent, &[], Duration::from_secs(20)))
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .ends_with("stopped with exit code 3: ERROR: usage limit"),
            "{}",
            error.message
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The agent and a helper it started both write their pid, then hang.
    fn hanging_agent() -> (PathBuf, PathBuf) {
        script(
            "cat > /dev/null\nsleep 60 &\necho $! > \"$(dirname \"$0\")/helper.pid\"\n\
             echo $$ > \"$(dirname \"$0\")/agent.pid\"\nwait",
        )
    }

    async fn pids(dir: &Path) -> Vec<String> {
        for _ in 0..100 {
            let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
            if let (Some(agent), Some(helper)) = (read("agent.pid"), read("helper.pid")) {
                return vec![agent, helper];
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the agent never started");
    }

    async fn assert_dead(pids: &[String]) {
        for _ in 0..100 {
            if pids.iter().all(|pid| !alive(pid)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("still running: {pids:?}");
    }

    #[tokio::test]
    async fn a_slow_agent_times_out_and_is_killed_with_its_helpers() {
        let (dir, agent) = hanging_agent();
        let error = ask(&request(&agent, &[], Duration::from_secs(2)))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::TimedOut);
        assert_dead(&pids(&dir).await).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancelling_kills_the_agent_and_its_helpers() {
        let (dir, agent) = hanging_agent();
        let request = request(&agent, &[], Duration::from_secs(60));
        let found = tokio::select! {
            _ = ask(&request) => None,
            pids = pids(&dir) => Some(pids),
        };
        let pids = found.expect("the agent ended by itself");
        assert_dead(&pids).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_missing_program_says_how_to_fix_it() {
        let error = ask(&AgentRequest {
            agent: Agent::Custom(AiCustom {
                program: "nordpil-no-such-agent".to_owned(),
                args: Vec::new(),
            }),
            timeout: Duration::from_secs(5),
            prompt: prompt(Surface::Find),
        })
        .await
        .unwrap_err();
        assert!(
            error.message.contains("set its path in Settings"),
            "{}",
            error.message
        );
    }
}
