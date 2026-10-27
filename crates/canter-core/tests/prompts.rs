use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{Model, ModelStream, Request};
use canter_protocol::{PermissionMode, ReasoningEffort, SessionSettings};
use canter_store::Store;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

struct NoRequests;
impl Model for NoRequests {
    fn stream(&self, _: Request, _: CancellationToken) -> ModelStream<'_> {
        panic!("opening a session must not call the model")
    }
}

fn settings(cwd: &Path) -> SessionSettings {
    SessionSettings {
        cwd: cwd.to_string_lossy().into_owned(),
        name: None,
        model: "test-model".into(),
        reasoning_effort: ReasoningEffort::High,
        approval_policy: PermissionMode::AlwaysAsk,
    }
}

fn runtime(home: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "CANTER_HOME".into(),
                home.join(".canter").to_string_lossy().into_owned(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}

fn contexts(store: &Store) -> Vec<Value> {
    store
        .model_history::<Value>("prompt")
        .unwrap()
        .into_iter()
        .filter(|entry| entry["type"] == "context")
        .collect()
}

#[tokio::test]
async fn prefix_renders_ordered_sections_and_allocates_the_whole_message() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project&\"<>");
    let cwd = root.join("blank/nested");
    let home = temp.path().join("home");
    let skill = home.join(".agents/skills/example/SKILL.md");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(home.join(".agents/AGENTS.md"), " global <raw> & text \n\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "root instruction").unwrap();
    std::fs::write(root.join("blank/AGENTS.md"), " \n\t\r\n").unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "nested instruction\n").unwrap();
    std::fs::write(
        &skill,
        "---\nname: example\ndescription: Example skill\n---\nBody stays out of the prefix.",
    )
    .unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        "prompt".into(),
        settings(&cwd),
        runtime(&home),
        Arc::new(NoRequests),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let records = contexts(&store);
    assert_eq!(records.len(), 1);
    let message = &records[0]["messages"][0];
    let text = message["text"].as_str().unwrap();
    let core = records[0]["segments"][0]["text"].as_str().unwrap();
    let after_core = text
        .strip_prefix(core)
        .and_then(|rest| rest.strip_prefix("\n\n"))
        .unwrap();
    let escaped_root = format!("{}/project&amp;&quot;&lt;&gt;", temp.path().display());
    let expected = format!(
        "<global_instructions path=\"{}/.agents/AGENTS.md\">\n global <raw> & text \n\n</global_instructions>\n\n\
         <skills>\nWhen a task matches one of these skills, read its SKILL.md in full first, then follow its instructions.\n\
         - example: Example skill (SKILL.md: {})\n</skills>\n\n\
         <project_instructions path=\"{escaped_root}/AGENTS.md\">\nroot instruction\n</project_instructions>\n\n\
         <project_instructions path=\"{escaped_root}/blank/nested/AGENTS.md\">\nnested instruction\n</project_instructions>\n\n\
         <environment>\n",
        home.display(),
        skill.display()
    );
    assert!(after_core.starts_with(&expected), "{after_core}");
    assert!(text.ends_with("\n</environment>"));
    let report = serde_json::to_value(session.context().await).unwrap();
    let sections = report["prefix"].as_array().unwrap();
    assert_eq!(
        sections
            .iter()
            .map(|section| &section["kind"])
            .collect::<Vec<_>>(),
        [
            "system",
            "globalInstructions",
            "skills",
            "projectInstructions",
            "projectInstructions",
            "environment"
        ]
    );
    assert_eq!(
        sections
            .iter()
            .map(|section| section["tokens"].as_u64().unwrap())
            .sum::<u64>(),
        (serde_json::to_string(message).unwrap().len() as u64).div_ceil(3) + 8
    );
    let segments = records[0]["segments"].as_array().unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|segment| &segment["kind"])
            .collect::<Vec<_>>(),
        [
            "core",
            "global_instructions",
            "skills",
            "project_instructions",
            "project_instructions",
            "environment"
        ]
    );
    assert_eq!(segments[1]["text"], " global <raw> & text \n\n");
    assert_eq!(segments[3]["path"], json!(root.join("AGENTS.md")));
    assert_eq!(segments[3]["text"], "root instruction");
    session.close().await.unwrap();
}

#[tokio::test]
async fn reopening_only_appends_when_segments_or_messages_change_and_upgrades_legacy_records() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let instructions = temp.path().join("AGENTS.md");
    std::fs::write(&instructions, "original instructions").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        "prompt".into(),
        settings(temp.path()),
        runtime(&home),
        Arc::new(NoRequests),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    session.close().await.unwrap();
    drop(session);
    let original = contexts(&store);

    let reopen = || {
        Session::open(
            store.clone(),
            "prompt".into(),
            runtime(&home),
            Arc::new(NoRequests),
            CoreOptions::default(),
        )
    };
    let (session, _) = reopen().await.unwrap();
    assert_eq!(
        contexts(&store),
        original,
        "unchanged prefix must not append"
    );
    session.close().await.unwrap();
    drop(session);

    // An old writer knows only messages. Even identical rendered bytes must migrate once.
    let legacy = json!({"type": "context", "messages": original[0]["messages"]});
    store
        .open("prompt")
        .unwrap()
        .append_model_history(&legacy)
        .unwrap();
    let (session, _) = reopen().await.unwrap();
    let upgraded = contexts(&store);
    assert_eq!(
        upgraded.len(),
        3,
        "missing segments must append a new context record"
    );
    assert_eq!(upgraded[0], original[0]);
    assert_eq!(upgraded[1], legacy);
    assert_eq!(upgraded[2]["messages"], legacy["messages"]);
    assert!(upgraded[2]["segments"].is_array());
    session.close().await.unwrap();
    drop(session);

    let (session, _) = reopen().await.unwrap();
    assert_eq!(
        contexts(&store),
        upgraded,
        "migration must happen only once"
    );
    session.close().await.unwrap();
    drop(session);

    std::fs::write(&instructions, "changed instructions").unwrap();
    let (session, _) = reopen().await.unwrap();
    let changed = contexts(&store);
    assert_eq!(changed.len(), 4);
    assert_eq!(changed[..3], upgraded);
    assert_ne!(changed[3]["segments"], changed[2]["segments"]);
    assert_ne!(changed[3]["messages"], changed[2]["messages"]);
    session.close().await.unwrap();
    drop(session);

    // A converter revision can change messages while leaving source segments identical.
    let mut different_messages = changed[3].clone();
    different_messages["messages"][0]["text"] = json!("previous converter output");
    store
        .open("prompt")
        .unwrap()
        .append_model_history(&different_messages)
        .unwrap();
    let (session, _) = reopen().await.unwrap();
    let rendered = contexts(&store);
    assert_eq!(rendered.len(), 6);
    assert_eq!(rendered[4], different_messages);
    assert_eq!(rendered[5], changed[3]);
    session.close().await.unwrap();

    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum LegacyEntry {
        Context {
            messages: Vec<canter_model::Message>,
        },
        #[serde(other)]
        Unknown,
    }
    let old_reader = store.model_history::<LegacyEntry>("prompt").unwrap();
    let old_messages = old_reader
        .into_iter()
        .filter_map(|entry| match entry {
            LegacyEntry::Context { messages } => Some(serde_json::to_value(messages).unwrap()),
            LegacyEntry::Unknown => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        old_messages,
        rendered
            .iter()
            .map(|entry| entry["messages"].clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn reopening_unknown_segment_without_text_appends_context_and_preserves_old_lines() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        "prompt".into(),
        settings(temp.path()),
        runtime(&home),
        Arc::new(NoRequests),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    session.close().await.unwrap();
    drop(session);

    let mut future = contexts(&store)[0].clone();
    future["segments"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind": "future_kind", "items": [1]}));
    store
        .open("prompt")
        .unwrap()
        .append_model_history(&future)
        .unwrap();
    let previous = contexts(&store);
    let history_path = temp.path().join("sessions/prompt/model-history.jsonl");
    let previous_bytes = std::fs::read(&history_path).unwrap();

    let (session, _) = Session::open(
        store.clone(),
        "prompt".into(),
        runtime(&home),
        Arc::new(NoRequests),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let reopened = contexts(&store);
    assert_eq!(reopened.len(), previous.len() + 1);
    assert_eq!(reopened[..previous.len()], previous);
    assert!(
        std::fs::read(&history_path)
            .unwrap()
            .starts_with(&previous_bytes)
    );
    let segments = reopened.last().unwrap()["segments"].as_array().unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|segment| &segment["kind"])
            .collect::<Vec<_>>(),
        ["core", "environment"]
    );
    session.close().await.unwrap();
}
