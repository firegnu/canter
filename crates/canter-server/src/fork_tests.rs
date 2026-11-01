use super::*;
use canter_model::Message;

#[derive(Default)]
struct Replay(std::sync::Mutex<Vec<Request>>);
impl Model for Replay {
    fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
        self.0.lock().unwrap().push(request);
        Box::pin(futures_util::stream::iter([
            Ok(ModelEvent::TextDelta {
                item_id: "answer".into(),
                delta: "Saved answer\n  with spacing.".into(),
            }),
            Ok(ModelEvent::Usage(canter_model::Usage {
                input_tokens: 100,
                output_tokens: 10,
                cache_read_tokens: Some(60),
                ..Default::default()
            })),
            Ok(ModelEvent::Finished(FinishReason::Stop)),
        ]))
    }
}

async fn copy(test: &Harness, url: &str) -> SessionCreateResponse {
    test.client
        .post(format!("{url}/fork"))
        .json(&json!({"env":environment(test)}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn environment(test: &Harness) -> BTreeMap<String, String> {
    let home = test.root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "CANTER_HOME".into(),
            test.app.home.to_string_lossy().into_owned(),
        ),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ])
}

async fn create(test: &Harness) -> String {
    test.client
        .post("http://localhost/v1/folderTrust")
        .json(&json!({"cwd":test.root.path()}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let response: SessionCreateResponse = test.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":test.root.path(),"env":environment(test),"model":"fake-model","approvalPolicy":"alwaysAsk","tags":{"purpose":"fork-test"},"clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    format!("http://localhost/v1/sessions/{}", response.session.id)
}

async fn rewind(test: &Harness, url: &str, turn: &str) {
    let mut params = json!({"beforeTurnId":turn,"rewindConversation":true,"rewindFiles":false});
    let preview: serde_json::Value = test
        .client
        .post(format!("{url}/rewind/preview"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    params["lastEventSeq"] = preview["lastEventSeq"].clone();
    test.client
        .post(format!("{url}/rewind"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn send(test: &Harness, url: &str, text: &str) -> SessionReadResponse {
    test.client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":text}],"clientName":"fork-test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    idle(test, url).await
}

async fn idle(test: &Harness, url: &str) -> SessionReadResponse {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = test.read(url).await;
            if snapshot.session.status == SessionStatus::Idle {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn saved_files(test: &Harness, id: &str) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(test.app.store.session_dir(id).unwrap())
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn fork_copies_completed_conversation_and_settings_without_touching_source() {
    let test = Harness::new().await;
    let source = create(&test).await;
    test.patch(
        &source,
        json!({"name":"Refactor login","reasoningEffort":"max"}),
    )
    .await;
    let before = send(&test, &source, "Keep this conversation").await;
    let files = saved_files(&test, &before.session.id);
    let response = test
        .client
        .post(format!("{source}/fork"))
        .json(&json!({"env":environment(&test)}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fork: SessionCreateResponse = response.json().await.unwrap();
    assert_ne!(fork.session.id, before.session.id);
    assert_eq!(
        fork.session.settings.name.as_deref(),
        Some("Refactor login (fork)")
    );
    assert_eq!(fork.session.settings.reasoning_effort, ReasoningEffort::Max);
    let mut expected_settings = before.session.settings.clone();
    expected_settings.name = Some("Refactor login (fork)".into());
    assert_eq!(fork.session.settings, expected_settings);
    assert_eq!(fork.session.turns.len(), 1);
    assert_eq!(fork.session.turns[0].items, before.session.turns[0].items);
    assert_eq!(fork.session.status, SessionStatus::Idle);
    assert_eq!(test.read(&source).await, before);
    assert_eq!(saved_files(&test, &before.session.id), files);
    let list: SessionListResponse = test
        .client
        .get("http://localhost/v1/sessions")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.data.len(), 2);
    let rewind: RewindListResponse = test
        .client
        .get(format!(
            "http://localhost/v1/sessions/{}/rewind",
            fork.session.id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rewind.data.is_empty());
    test.stop().await;
}

#[tokio::test]
async fn fork_reopen_refreshes_project_instructions_and_skills() {
    let model = Arc::new(Replay::default());
    let test = Harness::with_model(model.clone()).await;
    let instructions = test.root.path().join("AGENTS.md");
    std::fs::write(&instructions, "Original fork instruction").unwrap();
    let source = create(&test).await;
    send(&test, &source, "Original conversation").await;
    let fork = copy(&test, &source).await;
    let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    send(&test, &url, "Before reopening").await;
    let before = model.0.lock().unwrap().last().unwrap().clone();
    assert!(
        matches!(&before.messages[0], Message::System { text } if text.contains("Original fork instruction"))
    );
    let notice = before.messages.iter().find(|message| matches!(message, Message::User { text } if text.starts_with("This session was forked"))).unwrap().clone();
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&fork.session.id)
        .unwrap();
    let contexts = history
        .iter()
        .filter(|entry| entry["type"] == "context")
        .count();
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    std::fs::write(
        &instructions,
        "Refreshed project instruction after reopening",
    )
    .unwrap();
    let skill = test.root.path().join(".agents/skills/after-fork/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(skill, "---\nname: after-fork\ndescription: Skill added after the fork\n---\nFollow the refreshed project instructions.\n").unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":environment(&test)}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    send(&test, &url, "After reopening").await;
    let actual = model.0.lock().unwrap().last().unwrap().clone();
    let Message::System { text } = &actual.messages[0] else {
        panic!("missing system prefix")
    };
    assert!(
        text.contains("Refreshed project instruction after reopening"),
        "reopening must refresh AGENTS.md"
    );
    assert!(!text.contains("Original fork instruction"));
    assert!(text.contains("after-fork") && text.contains("Skill added after the fork"));
    let skills: SkillListResponse = test
        .client
        .get(format!("{url}/skills"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(skills.data.iter().any(|skill| skill.name == "after-fork"));
    assert_eq!(
        actual
            .messages
            .iter()
            .filter(|message| *message == &notice)
            .count(),
        1
    );
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&fork.session.id)
        .unwrap();
    let updated: Vec<_> = history
        .iter()
        .filter(|entry| entry["type"] == "context")
        .collect();
    assert_eq!(updated.len(), contexts + 1);
    assert_eq!(
        updated.last().unwrap()["messages"],
        json!([actual.messages[0]])
    );
    test.stop().await;
}

#[tokio::test]
async fn fork_appends_goal_cleared_after_its_notice_and_restates_on_reopen() {
    let model = Arc::new(Replay::default());
    let test = Harness::with_model(model.clone()).await;
    let source = create(&test).await;
    test.client
        .post(format!("{source}/goal"))
        .json(&json!({"objective":"Original session goal","budgetUsd":0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{source}/goal/pause"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    send(&test, &source, "Keep the paused goal history").await;
    let before = test.read(&source).await;
    let files = saved_files(&test, &before.session.id);
    let fork = copy(&test, &source).await;
    let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    assert!(fork.session.goal.is_none());
    assert_eq!(test.read(&source).await, before);
    assert_eq!(saved_files(&test, &before.session.id), files);
    send(&test, &source, "Continue here").await;
    let expected = model.0.lock().unwrap().last().unwrap().clone();
    assert!(
        expected
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text.starts_with("[Goal]")))
    );
    send(&test, &url, "Continue here").await;
    let actual = model.0.lock().unwrap().last().unwrap().clone();
    let notice = actual.messages.iter().find(|message| matches!(message, Message::User { text } if text.starts_with("This session was forked"))).unwrap().clone();
    let cleared = Message::User {
        text:
            "[Goal cleared] There is no current goal. Do not pursue a previous goal automatically."
                .into(),
    };
    let mut messages = expected.messages;
    let index = messages.len() - 1;
    messages.splice(index..index, [notice, cleared.clone()]);
    assert_eq!(
        serde_json::to_vec(&actual.messages).unwrap(),
        serde_json::to_vec(&messages).unwrap(),
        "fork must append the existing goal-cleared notice immediately after its scratch notice"
    );
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":environment(&test)}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    send(&test, &url, "Continue after reopening").await;
    let actual = model.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(actual.messages[actual.messages.len() - 2], cleared);
    assert_eq!(
        actual
            .messages
            .iter()
            .filter(|message| *message == &cleared)
            .count(),
        2
    );
    test.stop().await;
}

#[tokio::test]
async fn fork_preserves_request_bytes_after_compaction_rewind_and_plan() {
    for scenario in ["compaction", "rewind", "plan"] {
        let model = Arc::new(Replay::default());
        let test = Harness::with_options(
            model.clone(),
            CoreOptions {
                sandbox: true,
                ..Default::default()
            },
        )
        .await;
        let source = create(&test).await;
        send(&test, &source, "First message\n  keep spaces").await;
        match scenario {
            "compaction" => {
                test.client
                    .post(format!("{source}/compact"))
                    .json(&json!({}))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                idle(&test, &source).await;
            }
            "rewind" => {
                let next = send(&test, &source, "Remove this message").await;
                rewind(&test, &source, &next.session.turns.last().unwrap().id).await;
            }
            "plan" => {
                test.patch(&source, json!({"planMode":true})).await;
            }
            _ => unreachable!(),
        }
        let fork = copy(&test, &source).await;
        let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
        assert_eq!(fork.session.settings.plan_mode, scenario == "plan");
        let sandbox: SessionSandboxResponse = test
            .client
            .get(format!("{url}/sandbox"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let scratch = sandbox
            .writable
            .iter()
            .find(|path| path.contains("canter-shell-"))
            .unwrap();
        let original_sandbox: SessionSandboxResponse = test
            .client
            .get(format!("{source}/sandbox"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let original_scratch = original_sandbox
            .writable
            .iter()
            .find(|path| path.contains("canter-shell-"))
            .unwrap();
        assert_ne!(scratch, original_scratch);
        assert!(!sandbox.writable.contains(original_scratch));
        assert!(!original_sandbox.writable.contains(scratch));
        let notice = Message::User {
            text: format!(
                "This session was forked from another session. Its scratch directory is now {scratch}; use it instead of the scratch directory named earlier."
            ),
        };
        // Only the fresh fork keeps the source's system prefix byte-for-byte.
        send(&test, &source, "Continue here").await;
        let expected = model.0.lock().unwrap().last().unwrap().clone();
        let before = test.read(&source).await;
        let files = saved_files(&test, &before.session.id);
        send(&test, &url, "Continue here").await;
        let actual = model.0.lock().unwrap().last().unwrap().clone();
        let mut messages = expected.messages.clone();
        messages.insert(messages.len() - 1, notice);
        assert_eq!(
            serde_json::to_vec(&actual.messages).unwrap(),
            serde_json::to_vec(&messages).unwrap(),
            "{scenario}"
        );
        assert_eq!(
            serde_json::to_vec(&actual.tools).unwrap(),
            serde_json::to_vec(&expected.tools).unwrap(),
            "{scenario}"
        );
        assert_eq!(test.read(&source).await, before);
        assert_eq!(saved_files(&test, &before.session.id), files);
        // New turns can be rewound exactly to the inherited conversation.
        let list: RewindListResponse = test
            .client
            .get(format!("{url}/rewind"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list.data.len(), 1);
        rewind(&test, &url, &list.data[0].turn_id).await;
        let list: RewindListResponse = test
            .client
            .get(format!("{url}/rewind"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(list.data.is_empty());
        test.stop().await;
    }
}

#[tokio::test]
async fn fork_of_closed_source_is_read_only_and_only_starts_the_new_session() {
    let options = CoreOptions {
        hooks: serde_json::from_value(json!({"SessionStart":[{"hooks":[{"type":"command","command":"(cat; printf '\\n') >> starts.jsonl"}]}]})).unwrap(),
        ..Default::default()
    };
    let test = Harness::with_options(Arc::new(Replay::default()), options).await;
    let source = create(&test).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.root.path().join("starts.jsonl").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let before = send(&test, &source, "Persisted conversation").await;
    test.client
        .delete(&source)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Persisted sessions can name a model no longer offered for new sessions.
    // Dormant goals and commands are saved state, not work to start in the fork.
    {
        let mut writer = test.app.store.open(&before.session.id).unwrap();
        let mut settings = before.session.settings.clone();
        settings.model = "archived-model".into();
        writer
            .append(EventPayload::SessionUpdated { settings }, 1)
            .unwrap();
        writer
            .append(
                EventPayload::SessionGoalUpdated {
                    goal: Some(SessionGoal {
                        id: "old-goal".into(),
                        objective: "Original only".into(),
                        status: GoalStatus::Paused,
                        budget_usd: 5.0,
                        cost: Default::default(),
                        created_at: 1,
                        updated_at: 1,
                    }),
                },
                1,
            )
            .unwrap();
        writer
            .append(
                EventPayload::BackgroundCommandUpdated {
                    command: BackgroundCommand {
                        id: 1,
                        item_id: "old-command".into(),
                        command: "printf original".into(),
                        started_at: 1,
                        completed_at: Some(2),
                        running: false,
                        exit_code: Some(0),
                    },
                },
                2,
            )
            .unwrap();
    }
    let files = saved_files(&test, &before.session.id);
    let fork = copy(&test, &source).await;
    assert_eq!(fork.session.settings.model, "archived-model");
    assert!(fork.session.goal.is_none());
    assert!(fork.session.background_commands.is_empty());
    assert!(test.app.opened_session(&before.session.id).await.is_none());
    assert_eq!(saved_files(&test, &before.session.id), files);
    assert_eq!(fork.session.turns[0].items, before.session.turns[0].items);
    let starts: Vec<serde_json::Value> = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let contents =
                std::fs::read_to_string(test.root.path().join("starts.jsonl")).unwrap_or_default();
            let starts: Vec<serde_json::Value> = contents
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            if starts.len() == 2 {
                return starts;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(starts[0]["session_id"], before.session.id);
    assert_eq!(starts[1]["session_id"], fork.session.id);
    assert!(starts.iter().all(|start| start["source"] == "startup"));
    test.stop().await;
}

#[tokio::test]
async fn fork_waits_for_its_source_without_blocking_other_sessions() {
    let test = Harness::new().await;
    let source = create(&test).await;
    let other = create(&test).await;
    {
        let operation = test
            .app
            .session_locks
            .lock(source.rsplit('/').next().unwrap())
            .await;
        let copying = copy(&test, &source);
        tokio::pin!(copying);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut copying)
                .await
                .is_err()
        );
        tokio::time::timeout(
            Duration::from_secs(2),
            test.patch(&other, json!({"name":"Independent"})),
        )
        .await
        .unwrap();
        drop(operation);
        tokio::time::timeout(Duration::from_secs(5), copying)
            .await
            .unwrap();
    }
    test.stop().await;
}

#[tokio::test]
async fn fork_copies_image_bytes_but_not_backups_telemetry_or_scratch() {
    let model = Arc::new(Replay::default());
    let test = Harness::with_options(
        model.clone(),
        CoreOptions {
            sandbox: true,
            image_models: vec!["fake-model".into()],
            ..Default::default()
        },
    )
    .await;
    let source = create(&test).await;
    // Image validation uses the PNG signature, without decoding or changing bytes.
    let picture = test.root.path().join("sample.png");
    let bytes = b"\x89PNG\r\n\x1a\nimage fixture";
    std::fs::write(&picture, bytes).unwrap();
    test.client.post(format!("{source}/input"))
        .json(&json!({"input":[{"type":"text","text":"Keep this picture"},{"type":"localImage","path":picture,"name":"sample.png"}],"clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap();
    let before = idle(&test, &source).await;
    let directory = test.app.store.session_dir(&before.session.id).unwrap();
    std::fs::write(
        directory.join("private-telemetry.jsonl"),
        b"private fixture",
    )
    .unwrap();
    let backup = test
        .app
        .store
        .save_rewind(&before.session.id, b"file backup")
        .unwrap();
    let sandbox: SessionSandboxResponse = test
        .client
        .get(format!("{source}/sandbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let scratch = std::path::Path::new(
        sandbox
            .writable
            .iter()
            .find(|path| path.contains("canter-shell-"))
            .unwrap(),
    );
    std::fs::write(scratch.join("original-only"), b"scratch").unwrap();
    let fork = copy(&test, &source).await;
    let target = test.app.store.session_dir(&fork.session.id).unwrap();
    assert!(!target.join("private-telemetry.jsonl").exists());
    assert!(!target.join(&backup).exists());
    assert_eq!(
        std::fs::read(directory.join("private-telemetry.jsonl")).unwrap(),
        b"private fixture"
    );
    assert_eq!(
        std::fs::read(directory.join(&backup)).unwrap(),
        b"file backup"
    );
    assert_eq!(
        std::fs::read(scratch.join("original-only")).unwrap(),
        b"scratch"
    );
    let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    let sandbox: SessionSandboxResponse = test
        .client
        .get(format!("{url}/sandbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let scratch = std::path::Path::new(
        sandbox
            .writable
            .iter()
            .find(|path| path.contains("canter-shell-"))
            .unwrap(),
    );
    assert!(!scratch.join("original-only").exists());
    let image = std::fs::read_dir(&target)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("image-")
        })
        .unwrap();
    assert_eq!(std::fs::read(&image).unwrap(), bytes);
    std::fs::remove_file(&picture).unwrap();
    // The fork remains self-contained even after the original is closed and trashed.
    test.client
        .delete(&source)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{source}/trash"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":environment(&test)}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    send(&test, &url, "Describe it").await;
    {
        let requests = model.0.lock().unwrap();
        let images = |request: &Request| {
            request
                .messages
                .iter()
                .flat_map(|message| match message {
                    Message::UserParts { content } => content
                        .iter()
                        .filter_map(|part| match part {
                            canter_model::UserContent::Image { data_url, .. } => {
                                Some(data_url.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(images(requests.last().unwrap()), images(&requests[0]));
        assert_eq!(images(&requests[0]).len(), 1);
    }
    test.stop().await;
}

#[tokio::test]
async fn fork_does_not_recount_tokens_or_costs_in_usage_or_stats() {
    let model = Arc::new(Replay::default());
    let mut options = CoreOptions::default();
    options.prices.insert(
        "fake-model".into(),
        canter_core::usage::ModelPrice {
            peak: canter_core::usage::Prices {
                cache_hit: 1.0,
                cache_miss: 2.0,
                output: 3.0,
            },
            off_peak: None,
        },
    );
    let test = Harness::with_options(model, options).await;
    let source = create(&test).await;
    let before = send(&test, &source, "Account for this once").await;
    let total: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(total.total.output_tokens, 10);
    assert!((total.total.cost.usd - 0.00017).abs() < 1e-12);
    let fork = copy(&test, &source).await;
    let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    assert!(fork.session.token_usage.is_none());
    assert!(fork.session.turns.iter().all(|turn| turn.usage.is_none()));
    let after: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, total);
    let own: UsageResponse = test
        .client
        .get(format!("{url}/usage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(own.total, UsageRow::default());
    let stats: StatsResponse = test
        .client
        .get(format!("{url}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stats.overview.usage.is_none());
    send(&test, &url, "New cost").await;
    let own: UsageResponse = test
        .client
        .get(format!("{url}/usage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(own.total, total.total);
    let stats: StatsResponse = test
        .client
        .get(format!("{url}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.overview.usage.unwrap().total, total.total);
    assert_eq!(test.read(&source).await, before);
    test.stop().await;
}

#[tokio::test]
async fn fork_keeps_its_scratch_notice_after_its_own_compaction() {
    let model = Arc::new(Replay::default());
    let test = Harness::with_model(model.clone()).await;
    let source = create(&test).await;
    send(&test, &source, "Original history").await;
    let fork = copy(&test, &source).await;
    let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    send(&test, &url, "Branch history").await;
    let notice = model.0.lock().unwrap().last().unwrap().messages.iter()
        .find(|message| matches!(message, Message::User { text } if text.starts_with("This session was forked"))).unwrap().clone();
    test.client
        .post(format!("{url}/compact"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    idle(&test, &url).await;
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":environment(&test)}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    send(&test, &url, "Continue after compaction").await;
    assert!(
        model
            .0
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .contains(&notice)
    );
    test.stop().await;
}

#[derive(Default)]
struct Running {
    entered: Notify,
    release: Notify,
    requests: std::sync::Mutex<Vec<Request>>,
}
impl Model for Running {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        let input = match request.messages.last() {
            Some(Message::User { text }) => text.clone(),
            _ => String::new(),
        };
        self.requests.lock().unwrap().push(request);
        Box::pin(async_stream::stream! {
            if input == "unfinished-work" {
                yield Ok(ModelEvent::TextDelta { item_id: "partial".into(), delta: "Unfinished answer".into() });
                self.entered.notify_one();
                tokio::select! { _ = self.release.notified() => {}, _ = cancel.cancelled() => return }
            } else if input == "pending-approval" || input == "pending-answer" {
                yield Ok(ModelEvent::ToolCall(canter_model::ToolCall {
                    id: "pending-tool".into(),
                    name: if input == "pending-approval" { "write" } else { "ask_user" }.into(),
                    arguments: if input == "pending-approval" {
                        json!({"path":"approved.txt","content":"original finished"})
                    } else {
                        json!({"question":"Which direction?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]})
                    }.to_string(),
                }));
                yield Ok(ModelEvent::Finished(FinishReason::ToolCalls));
                return;
            }
            yield Ok(ModelEvent::TextDelta { item_id: "answer".into(), delta: "Completed answer".into() });
            yield Ok(ModelEvent::Finished(FinishReason::Stop));
        })
    }
}

#[tokio::test]
async fn fork_excludes_running_turn_queue_and_pending_requests_and_original_finishes() {
    for kind in ["unfinished-work", "pending-approval", "pending-answer"] {
        let model = Arc::new(Running::default());
        let test = Harness::with_model(model.clone()).await;
        let source = create(&test).await;
        let completed = send(&test, &source, "Completed original input").await;
        test.client
            .post(format!("{source}/input"))
            .json(&json!({"input":[{"type":"text","text":kind}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        if kind == "unfinished-work" {
            tokio::time::timeout(Duration::from_secs(5), model.entered.notified())
                .await
                .unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(5), async {
                while test.read(&source).await.session.pending_requests.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        test.client.post(format!("{source}/input"))
            .json(&json!({"input":[{"type":"text","text":"queued-original-only"}],"clientName":"test","mode":"queue"}))
            .send().await.unwrap().error_for_status().unwrap();
        let before = test.read(&source).await;
        let files = saved_files(&test, &before.session.id);
        let fork = copy(&test, &source).await;
        assert_eq!(saved_files(&test, &before.session.id), files, "{kind}");
        assert_eq!(test.read(&source).await, before, "{kind}");
        assert_eq!(fork.session.turns, completed.session.turns);
        assert!(fork.session.queued_inputs.is_empty() && fork.session.pending_requests.is_empty());
        assert_eq!(fork.session.settings.name, None);
        let url = format!("http://localhost/v1/sessions/{}", fork.session.id);
        send(&test, &url, "Branch only").await;
        let history =
            serde_json::to_string(&model.requests.lock().unwrap().last().unwrap().messages)
                .unwrap();
        assert!(history.contains("Completed original input"));
        for excluded in [
            kind,
            "Unfinished answer",
            "queued-original-only",
            "pending-tool",
        ] {
            assert!(!history.contains(excluded), "{history}");
        }
        if kind == "unfinished-work" {
            model.release.notify_one();
        } else {
            let request = &before.session.pending_requests[0];
            let answer = if kind == "pending-approval" {
                json!({"decision":"accept","clientName":"test"})
            } else {
                json!({"answer":{"type":"text","text":"A"},"clientName":"test"})
            };
            test.client
                .post(format!("{source}/requests/{}", request.id))
                .json(&answer)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let after = idle(&test, &source).await;
        assert_eq!(after.session.turns.len(), 3);
        assert!(
            after
                .session
                .turns
                .iter()
                .all(|turn| turn.status == TurnStatus::Completed)
        );
        if kind == "pending-approval" {
            assert_eq!(
                std::fs::read_to_string(test.root.path().join("approved.txt")).unwrap(),
                "original finished"
            );
        }
        test.stop().await;
    }
}
