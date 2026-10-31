#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn answer() -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"Summary or answer."},"finish_reason":"stop"}]})
    ))
}

fn calls(calls: &[(&str, Value)]) -> Reply {
    let calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({
                "index":i,"id":format!("call-{i}"),"type":"function",
                "function":{"name":name,"arguments":args.to_string()}
            })
        })
        .collect();
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]})
    ))
}

fn runtime(root: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            ("HOME".into(), root.join("home").display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("state").display().to_string(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
}

impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in ["home", "state", "project/.git"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        std::fs::write(
            root.path().join("project/AGENTS.md"),
            "Project instructions.",
        )
        .unwrap();
        std::fs::write(root.path().join("state/AGENTS.md"), "Global instructions.").unwrap();
        let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
        let server = Server::start(replies).await;
        let (session, _) = Session::create(
            store.clone(), "files".into(),
            serde_json::from_value(json!({"cwd":root.path().join("project"),"model":"fake","name":null,"approvalPolicy":"allowAll"})).unwrap(),
            runtime(root.path()),
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
            CoreOptions { command_rules_path: Some(root.path().join("state/rules.json")), ..Default::default() },
        ).await.unwrap();
        Self {
            root,
            store,
            session,
            server,
        }
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        std::fs::write(self.root.path().join("project").join(path), content).unwrap();
    }

    async fn events(&self) -> Subscription {
        self.session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap()
    }

    async fn send(&self) -> String {
        let mut events = self.events().await;
        let response = self
            .session
            .input(
                serde_json::from_value(json!({
                    "input":[{"type":"text","text":"Continue the task."}],"clientName":"test"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finished(&mut events).await, TurnStatus::Completed);
        response.turn_id.unwrap()
    }

    async fn compact(&self) -> Value {
        let mut events = self.events().await;
        self.session.compact().await.unwrap();
        assert_eq!(finished(&mut events).await, TurnStatus::Completed);
        self.card().await
    }

    async fn card(&self) -> Value {
        let snapshot = self.session.snapshot().await.session;
        serde_json::to_value(
            snapshot
                .turns
                .iter()
                .rev()
                .flat_map(|turn| turn.items.iter().rev())
                .find(|item| matches!(item, Item::ContextCompaction { .. }))
                .unwrap(),
        )
        .unwrap()
    }

    fn messages(&self) -> Vec<Value> {
        self.server.requests.lock().unwrap().last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn reopen(&mut self) {
        self.session.close().await.unwrap();
        self.session = Session::open(
            self.store.clone(),
            "files".into(),
            runtime(self.root.path()),
            Arc::new(ChatCompletions::new(&self.server.base_url, ApiKey::new(KEY)).unwrap()),
            CoreOptions {
                command_rules_path: Some(self.root.path().join("state/rules.json")),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .0;
    }

    async fn close(self) {
        self.session.close().await.unwrap();
        self.server.stop().await;
    }
}

async fn finished(events: &mut Subscription) -> TurnStatus {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventPayload::TurnCompleted { turn } =
                events.recv().await.unwrap().unwrap().payload
            {
                return turn.status;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn compaction_rereads_successfully_used_files_from_disk() {
    let fixture = Fixture::new(vec![
        calls(&[("read", json!({"path":"note.txt"}))]),
        answer(),
        answer(),
        answer(),
    ])
    .await;
    fixture.write("note.txt", "old content\n");
    fixture.send().await;
    fixture.write("note.txt", "new content\nsecond line\n");
    let card = fixture.compact().await;
    assert_eq!(
        card["restoredFiles"],
        json!([{"path":"note.txt","lines":2}])
    );
    fixture.send().await;
    let messages = fixture.messages();
    let restored = messages
        .iter()
        .find_map(|message| {
            message["content"]
                .as_str()
                .filter(|text| text.contains("Automatically reread after compaction"))
        })
        .unwrap();
    assert!(restored.contains("note.txt (lines 1-2)"), "{restored}");
    assert!(
        restored.ends_with("1: new content\n2: second line\n"),
        "{restored}"
    );
    assert!(!restored.contains("old content"));
    fixture.close().await;
}

#[tokio::test]
async fn recent_five_include_writes_and_edits_and_accumulate_across_compactions() {
    let mut replies = Vec::new();
    for i in 0..6 {
        replies.extend([
            calls(&[(
                "write",
                json!({"path":format!("{i}.txt"),"content":format!("file {i}\n")}),
            )]),
            answer(),
        ]);
    }
    replies.extend([
        calls(&[(
            "edit",
            json!({"path":"0.txt","edits":[{"oldText":"file 0","newText":"edited"}]}),
        )]),
        answer(),
        calls(&[
            ("read", json!({"path":"3.txt"})),
            ("read", json!({"path":"missing.txt"})),
        ]),
        answer(),
        answer(),
        answer(),
    ]);
    let fixture = Fixture::new(replies).await;
    for _ in 0..8 {
        fixture.send().await;
    }
    for _ in 0..2 {
        let card = fixture.compact().await;
        assert_eq!(
            card["restoredFiles"],
            json!([
                {"path":"3.txt","lines":1}, {"path":"0.txt","lines":1},
                {"path":"5.txt","lines":1}, {"path":"4.txt","lines":1}, {"path":"2.txt","lines":1}
            ])
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn five_near_limit_files_are_restored_in_full_including_over_2000_lines() {
    let reads: Vec<_> = (0..5)
        .map(|i| ("read", json!({"path":format!("{i}.txt")})))
        .collect();
    let fixture = Fixture::new(vec![calls(&reads), answer(), answer(), answer()]).await;
    for i in 0..4 {
        fixture.write(&format!("{i}.txt"), "x".repeat(14_800));
    }
    fixture.write("4.txt", "\n".repeat(2_100));
    fixture.send().await;
    let card = fixture.compact().await;
    assert_eq!(
        card["restoredFiles"],
        json!([
            {"path":"4.txt","lines":2100}, {"path":"3.txt","lines":1},
            {"path":"2.txt","lines":1}, {"path":"1.txt","lines":1}, {"path":"0.txt","lines":1}
        ])
    );
    fixture.send().await;
    let messages = fixture.messages();
    let restored: Vec<_> = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .filter(|s| s.starts_with("[Automatically reread after compaction:"))
        .collect();
    assert_eq!(restored.len(), 5);
    for text in &restored {
        assert!(
            (4_500..=5_000).contains(&text.len().div_ceil(3)),
            "{}",
            text.len()
        );
    }
    assert!(
        restored
            .iter()
            .map(|text| text.len().div_ceil(3))
            .sum::<usize>()
            <= 50_000
    );
    assert!(restored[0].ends_with("2100: \n"));
    fixture.close().await;
}

#[tokio::test]
async fn over_single_limit_references_only_the_name_counting_line_numbers_and_header() {
    let fixture = Fixture::new(vec![
        calls(&[
            ("read", json!({"path":"large.txt"})),
            ("read", json!({"path":"edge.txt"})),
        ]),
        answer(),
        answer(),
        answer(),
    ])
    .await;
    fixture.write("large.txt", "L".repeat(70_000));
    // The raw text fits 5,000 tokens, but the numbered message does not.
    fixture.write("edge.txt", "E".repeat(14_996));
    fixture.send().await;
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([
            {"path":"edge.txt"}, {"path":"large.txt"}
        ])
    );
    fixture.send().await;
    let text = fixture
        .messages()
        .iter()
        .map(|m| m["content"].to_string())
        .collect::<String>();
    assert!(text.contains("Referenced file after compaction: edge.txt"));
    assert!(!text.contains(&"E".repeat(100)));
    assert!(!text.contains(&"L".repeat(100)));
    fixture.close().await;
}

#[tokio::test]
async fn current_file_rules_hide_forbidden_paths_and_only_reference_prompt_paths_in_allow_all() {
    let fixture = Fixture::new(vec![
        calls(&[
            ("read", json!({"path":"forbidden.txt"})),
            ("read", json!({"path":"prompt.txt"})),
            ("read", json!({"path":"normal.txt"})),
        ]),
        answer(),
        answer(),
        answer(),
    ])
    .await;
    for name in ["forbidden", "prompt", "normal"] {
        fixture.write(&format!("{name}.txt"), format!("{name} content"));
    }
    fixture.send().await;
    std::fs::write(
        fixture.root.path().join("state/rules.json"),
        json!({"fileRules":[
            {"pattern":"forbidden.txt","decision":"forbidden"},
            {"pattern":"prompt.txt","decision":"prompt"}
        ]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([
            {"path":"normal.txt","lines":1}, {"path":"prompt.txt"}
        ])
    );
    fixture.send().await;
    let text = fixture
        .messages()
        .iter()
        .map(|m| m["content"].to_string())
        .collect::<String>();
    assert!(!text.contains("forbidden.txt"));
    assert!(!text.contains("prompt content"));
    assert!(text.contains("Referenced file after compaction: prompt.txt"));
    fixture.close().await;
}

#[tokio::test]
async fn excludes_deleted_unreadable_detected_nontext_and_system_instruction_files() {
    let fixture = Fixture::new(vec![
        calls(&[
            ("read", json!({"path":"keep.txt"})),
            ("read", json!({"path":"deleted.txt"})),
            ("read", json!({"path":"directory.txt"})),
            ("read", json!({"path":"binary.txt"})),
            ("read", json!({"path":"image.txt"})),
            ("read", json!({"path":"AGENTS.md"})),
            ("read", json!({"path":"instructions-link"})),
            ("read", json!({"path":"../state/AGENTS.md"})),
        ]),
        answer(),
        answer(),
    ])
    .await;
    for name in ["keep", "deleted", "directory", "binary", "image"] {
        fixture.write(&format!("{name}.txt"), "text\n");
    }
    std::os::unix::fs::symlink(
        "AGENTS.md",
        fixture.root.path().join("project/instructions-link"),
    )
    .unwrap();
    fixture.send().await;
    for name in ["deleted", "directory"] {
        std::fs::remove_file(fixture.root.path().join(format!("project/{name}.txt"))).unwrap();
    }
    std::fs::create_dir(fixture.root.path().join("project/directory.txt")).unwrap();
    fixture.write("binary.txt", [vec![b'x'; 70_000], vec![0, 0xff]].concat());
    fixture.write("image.txt", b"GIF89aabcdef");
    // The binary suffix is beyond the bounded read. Only its name is restored.
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([{"path":"binary.txt"}, {"path":"keep.txt","lines":1}])
    );
    fixture.close().await;
}

#[tokio::test]
async fn restored_paths_are_deduplicated_and_relative_only_inside_the_session_directory() {
    let fixture = Fixture::new(vec![
        calls(&[
            ("read", json!({"path":"./note.txt"})),
            ("read", json!({"path":"note-link"})),
            ("read", json!({"path":"../outside.txt"})),
        ]),
        answer(),
        answer(),
    ])
    .await;
    fixture.write("note.txt", "text\n");
    fixture.write("../outside.txt", "outside\n");
    std::os::unix::fs::symlink("note.txt", fixture.root.path().join("project/note-link")).unwrap();
    fixture.send().await;
    let outside = fixture
        .root
        .path()
        .join("outside.txt")
        .canonicalize()
        .unwrap();
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([
            {"path":outside,"lines":1}, {"path":"note.txt","lines":1}
        ])
    );
    fixture.close().await;
}

#[tokio::test]
async fn rewound_uses_do_not_replace_the_previous_file_order() {
    let fixture = Fixture::new(vec![
        calls(&[
            ("read", json!({"path":"a.txt"})),
            ("read", json!({"path":"b.txt"})),
        ]),
        answer(),
        calls(&[
            ("read", json!({"path":"a.txt"})),
            ("write", json!({"path":"gone.txt","content":"gone"})),
        ]),
        answer(),
        answer(),
    ])
    .await;
    for name in ["a.txt", "b.txt"] {
        fixture.write(name, "text\n");
    }
    fixture.send().await;
    let turn = fixture.send().await;
    let mut params: SessionRewindParams = serde_json::from_value(json!({
        "beforeTurnId":turn,"rewindFiles":false,"rewindConversation":true
    }))
    .unwrap();
    params.last_event_seq = Some(
        fixture
            .session
            .rewind_preview(params.clone())
            .await
            .unwrap()
            .last_event_seq,
    );
    fixture.session.rewind(params).await.unwrap();
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([
            {"path":"b.txt","lines":1}, {"path":"a.txt","lines":1}
        ])
    );
    fixture.close().await;
}

#[tokio::test]
async fn reopen_replays_the_exact_restored_context_and_next_compaction_labels_it_as_tool_data() {
    let mut fixture = Fixture::new(vec![
        calls(&[("read", json!({"path":"note.txt"}))]),
        answer(),
        answer(),
        answer(),
        answer(),
        answer(),
    ])
    .await;
    fixture.write("note.txt", "ORIGINAL_FILE_TEXT\n");
    fixture.send().await;
    let card = fixture.compact().await;
    fixture.send().await;
    let before = fixture.messages();
    fixture.write("note.txt", "CHANGED_AFTER_COMPACTION\n");
    fixture.reopen().await;
    assert_eq!(fixture.card().await, card);
    fixture.send().await;
    let after = fixture.messages();
    assert_eq!(&after[..before.len()], &before);
    assert!(after.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|s| s.contains("1: ORIGINAL_FILE_TEXT"))
    }));
    fixture.compact().await;
    let messages = fixture.messages();
    let restored = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .find(|s| s.starts_with("[Automatically reread after compaction:"))
        .unwrap();
    assert!(restored.contains("1: ORIGINAL_FILE_TEXT"));
    let prompt = messages.last().unwrap()["content"].as_str().unwrap();
    assert!(prompt.contains("automatically reread files ([Automatically reread after compaction:"));
    assert!(prompt.contains("not the user's own words"));
    fixture.close().await;
}

#[tokio::test]
async fn retract_before_model_output_preserves_only_the_retained_file_uses() {
    // The public API only permits retraction before model output, hence before
    // any tool calls in that turn. It must preserve earlier successful uses.
    let fixture = Fixture::new(vec![
        calls(&[("read", json!({"path":"keep.txt"}))]),
        answer(),
        Reply {
            hang: true,
            ..Reply::sse("")
        },
        answer(),
    ])
    .await;
    fixture.write("keep.txt", "keep\n");
    fixture.send().await;
    let response = fixture
        .session
        .input(
            serde_json::from_value(json!({
                "input":[{"type":"text","text":"Withdraw this request"}],"clientName":"test"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.server.arrivals.lock().unwrap().len() < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = fixture
        .session
        .interrupt(
            serde_json::from_value(json!({
                "turnId":response.turn_id,"retract":true
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.retracted, Some(true));
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([{"path":"keep.txt","lines":1}])
    );
    fixture.close().await;
}

#[tokio::test]
async fn failed_and_interrupted_compactions_do_not_restore_files() {
    for interrupted in [false, true] {
        let stop = || {
            if interrupted {
                Reply {
                    hang: true,
                    ..Reply::sse("")
                }
            } else {
                Reply {
                    status: axum::http::StatusCode::BAD_REQUEST,
                    ..Reply::sse(
                        r#"{"error":{"message":"Fixture summary failure","type":"invalid_request_error"}}"#,
                    )
                }
            }
        };
        let mut replies = vec![
            calls(&[("read", json!({"path":"note.txt"}))]),
            answer(),
            stop(),
        ];
        if !interrupted {
            // Fail both the prefix request and its transcript fallback.
            replies.push(stop());
        }
        replies.push(answer());
        let fixture = Fixture::new(replies).await;
        fixture.write("note.txt", "before\n");
        fixture.send().await;
        fixture.write("note.txt", "after\n");
        let mut events = fixture.events().await;
        let response = fixture.session.compact().await.unwrap();
        if interrupted {
            tokio::time::timeout(Duration::from_secs(10), async {
                while fixture.server.arrivals.lock().unwrap().len() < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fixture
                .session
                .interrupt(serde_json::from_value(json!({"turnId":response.turn_id})).unwrap())
                .await
                .unwrap();
        }
        assert_eq!(
            finished(&mut events).await,
            if interrupted {
                TurnStatus::Interrupted
            } else {
                TurnStatus::Failed
            }
        );
        if !interrupted {
            let requests = fixture.server.requests.lock().unwrap();
            assert_eq!(requests.len(), 4);
            assert!(requests[2].get("tools").is_some());
            assert!(requests[3].get("tools").is_none());
        }
        assert!(fixture.card().await.get("restoredFiles").is_none());
        fixture.send().await;
        let text = serde_json::to_string(&fixture.messages()).unwrap();
        assert!(text.contains("1: before"));
        assert!(!text.contains("1: after"));
        assert!(!text.contains("Automatically reread after compaction"));
        fixture.close().await;
    }
}

#[tokio::test]
async fn telemetry_counts_only_model_reads_after_compaction() {
    let fixture = Fixture::new(vec![
        calls(&[("read", json!({"path":"note.txt"}))]),
        answer(),
        answer(),
        calls(&[("read", json!({"path":"note.txt"}))]),
        answer(),
    ])
    .await;
    fixture.write("note.txt", "text\n");
    fixture.send().await;
    assert_eq!(
        fixture.compact().await["restoredFiles"],
        json!([{"path":"note.txt","lines":1}])
    );
    let report = || {
        canter_core::stats::report(
            &fixture.store,
            "files",
            &canter_core::usage::builtin_prices(),
        )
        .unwrap()
    };
    assert_eq!(
        report().compactions[0].signals.reread_files,
        Some(Vec::new())
    );
    fixture.send().await;
    assert_eq!(
        report().compactions[0].signals.reread_files,
        Some(vec![
            fixture
                .root
                .path()
                .join("project/note.txt")
                .display()
                .to_string()
        ])
    );
    fixture.close().await;
}
