use super::*;

const DAY: i64 = 86_400_000;

fn save(test: &Harness, id: &str, updated_at: i64) -> SessionSummary {
    let settings = SessionSnapshot::empty(id).settings;
    let writer = test.app.store.create(id, settings, updated_at).unwrap();
    writer.snapshot().summary()
}

#[tokio::test]
async fn cleanup_deletes_an_expired_session() {
    let test = Harness::new().await;
    let summary = save(&test, "expired", 0);

    crate::http::cleanup::cleanup_at(&test.app, 30, 31 * DAY).await;

    assert!(!test.app.store.session_dir(&summary.id).unwrap().exists());
    assert!(test.app.home.join("usage-archive.jsonl").exists());
    test.stop().await;
}

#[tokio::test]
async fn usage_totals_survive_cleanup_without_changing_daily_rows() {
    let test = Harness::new().await;
    let settings = SessionSnapshot::empty("old-usage").settings;
    let mut writer = test.app.store.create("old-usage", settings, 0).unwrap();
    for (timestamp, model, cost_usd, billing_kind) in [
        (1, "priced", Some(0.25), BillingKind::TokenMetered),
        (DAY + 1, "unknown", None, BillingKind::TokenMetered),
        (DAY + 2, "chatgpt/model", None, BillingKind::ChatgptPlan),
    ] {
        let tokens = TokenUsageBreakdown {
            input_tokens: 100,
            cached_input_tokens: 40,
            output_tokens: 10,
            total_tokens: 110,
            ..Default::default()
        };
        writer
            .append(
                EventPayload::SessionUsage {
                    token_usage: TokenUsage {
                        request: Some(RequestCharge {
                            request_id: None,
                            model: model.into(),
                            started_at: timestamp,
                            cost_usd,
                            billing_kind: Some(billing_kind),
                        }),
                        total: tokens.clone(),
                        last: tokens,
                        cost: None,
                        context_tokens: 0,
                        model_context_window: None,
                        auto_compact_tokens: None,
                    },
                },
                timestamp,
            )
            .unwrap();
    }
    drop(writer);
    let before: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before.data.len(), 3);
    assert!(before.total.cost.unknown);
    assert!(before.total.cost.chatgpt_plan);

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(!test.app.store.session_dir("old-usage").unwrap().exists());
    let after: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, before);
    test.stop().await;
}

#[tokio::test]
async fn cleanup_treats_a_parent_and_children_as_one_family() {
    let test = Harness::new().await;
    save(&test, "protected-parent", 0);
    let parent = SessionParent {
        session_id: "protected-parent".into(),
        turn_id: "turn".into(),
        task: "child task".into(),
        agent_type: AgentType::General,
        depth: None,
    };
    let settings = SessionSnapshot::empty("recent-child").settings;
    drop(
        test.app
            .store
            .create_child("recent-child", settings, parent, 39 * DAY)
            .unwrap(),
    );

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(
        test.app
            .store
            .session_dir("protected-parent")
            .unwrap()
            .exists()
    );
    assert!(test.app.store.session_dir("recent-child").unwrap().exists());
    save(&test, "old-parent", 0);
    let parent = SessionParent {
        session_id: "old-parent".into(),
        turn_id: "turn".into(),
        task: "old child task".into(),
        agent_type: AgentType::General,
        depth: None,
    };
    let settings = SessionSnapshot::empty("old-child").settings;
    drop(
        test.app
            .store
            .create_child("old-child", settings, parent, 0)
            .unwrap(),
    );

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(!test.app.store.session_dir("old-parent").unwrap().exists());
    assert!(!test.app.store.session_dir("old-child").unwrap().exists());
    test.stop().await;
}

#[tokio::test]
async fn cleanup_keeps_sessions_used_within_the_retention_period() {
    let test = Harness::new().await;
    save(&test, "recent", 39 * DAY);

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(test.app.store.session_dir("recent").unwrap().exists());
    assert!(!test.app.home.join("usage-archive.jsonl").exists());
    test.stop().await;
}

#[tokio::test]
async fn zero_retention_disables_cleanup() {
    let test = Harness::new().await;
    save(&test, "forever", 0);

    crate::http::cleanup::cleanup_at(&test.app, 0, 1_000 * DAY).await;

    assert!(test.app.store.session_dir("forever").unwrap().exists());
    assert!(!test.app.home.join("usage-archive.jsonl").exists());
    test.stop().await;
}

#[tokio::test]
async fn cleanup_deletes_expired_sessions_from_trash() {
    let test = Harness::new().await;
    save(&test, "trashed", 0);
    let trash = test.app.home.join("trash");
    std::fs::create_dir(&trash).unwrap();
    std::fs::rename(
        test.app.store.session_dir("trashed").unwrap(),
        trash.join("trashed"),
    )
    .unwrap();

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(!trash.join("trashed").exists());
    assert!(test.app.home.join("usage-archive.jsonl").exists());
    test.stop().await;
}

#[tokio::test]
async fn cleanup_keeps_open_sessions_even_without_attached_clients() {
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    crate::http::cleanup::cleanup_at(&test.app, 30, now + 40 * DAY).await;

    assert!(test.app.store.session_dir(id).unwrap().exists());
    assert!(test.app.opened_session(id).await.is_some());
    test.stop().await;
}

#[tokio::test]
async fn archive_failure_preserves_the_session() {
    let test = Harness::new().await;
    save(&test, "unarchived", 0);
    std::fs::create_dir(test.app.home.join("usage-archive.jsonl")).unwrap();

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(test.app.store.session_dir("unarchived").unwrap().exists());
    test.stop().await;
}

#[tokio::test]
async fn repeated_cleanup_does_not_append_duplicate_archives() {
    let test = Harness::new().await;
    save(&test, "once", 0);
    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;
    let path = test.app.home.join("usage-archive.jsonl");
    let before = std::fs::read(&path).unwrap();

    crate::http::cleanup::cleanup_at(&test.app, 30, 80 * DAY).await;

    assert_eq!(std::fs::read(&path).unwrap(), before);
    test.stop().await;
}

fn future_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 40 * DAY
}

#[tokio::test]
async fn cleanup_and_resume_have_one_winner() {
    let test = Harness::new().await;
    for _ in 0..6 {
        let url = test.create().await;
        let id = url.rsplit('/').next().unwrap().to_owned();
        test.client
            .delete(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let ((), response) = tokio::join!(
            crate::http::cleanup::cleanup_at(&test.app, 30, future_now()),
            test.client
                .post(format!("{url}/resume"))
                .json(&json!({"env":{}}))
                .send()
        );
        let response = response.unwrap();
        let exists = test.app.store.session_dir(&id).unwrap().exists();
        if exists {
            assert_eq!(response.status(), StatusCode::OK);
            assert!(test.app.opened_session(&id).await.is_some());
        } else {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(test.app.opened_session(&id).await.is_none());
        }
    }
    test.stop().await;
}

#[tokio::test]
async fn cleanup_and_manual_trash_leave_one_consistent_outcome() {
    let test = Harness::new().await;
    for _ in 0..6 {
        let url = test.create().await;
        let id = url.rsplit('/').next().unwrap().to_owned();
        test.client
            .delete(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let ((), response) = tokio::join!(
            crate::http::cleanup::cleanup_at(&test.app, 30, future_now()),
            test.client.post(format!("{url}/trash")).send()
        );
        let response = response.unwrap();
        assert!(
            response.status() == StatusCode::NO_CONTENT
                || response.status() == StatusCode::NOT_FOUND
        );
        assert!(!test.app.store.session_dir(&id).unwrap().exists());
        assert!(!test.app.home.join("trash").join(&id).exists());
        assert!(test.app.opened_session(&id).await.is_none());
    }
    let archive = std::fs::read_to_string(test.app.home.join("usage-archive.jsonl")).unwrap();
    assert_eq!(archive.lines().count(), 6);
    test.stop().await;
}

#[tokio::test]
async fn failed_delete_never_double_counts_the_write_ahead_archive() {
    use std::os::unix::fs::PermissionsExt;
    let test = Harness::new().await;
    let settings = SessionSnapshot::empty("write-ahead").settings;
    let mut writer = test.app.store.create("write-ahead", settings, 0).unwrap();
    let tokens = TokenUsageBreakdown {
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    writer
        .append(
            EventPayload::SessionUsage {
                token_usage: TokenUsage {
                    request: Some(RequestCharge {
                        request_id: None,
                        model: "priced".into(),
                        started_at: 1,
                        cost_usd: Some(0.5),
                        billing_kind: Some(BillingKind::TokenMetered),
                    }),
                    total: tokens.clone(),
                    last: tokens,
                    cost: None,
                    context_tokens: 0,
                    model_context_window: None,
                    auto_compact_tokens: None,
                },
            },
            1,
        )
        .unwrap();
    drop(writer);
    let before: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sessions = test.app.home.join("sessions");
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o500)).unwrap();

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(test.app.store.session_dir("write-ahead").unwrap().exists());
    let during: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(during, before);
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;
    let after: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, before);
    let archive = std::fs::read_to_string(test.app.home.join("usage-archive.jsonl")).unwrap();
    assert_eq!(archive.lines().count(), 1);
    test.stop().await;
}

fn usage_event(timestamp: i64) -> EventPayload {
    let tokens = TokenUsageBreakdown {
        input_tokens: 100,
        cached_input_tokens: 40,
        output_tokens: 10,
        total_tokens: 110,
        ..Default::default()
    };
    EventPayload::SessionUsage {
        token_usage: TokenUsage {
            request: Some(RequestCharge {
                request_id: None,
                model: "priced".into(),
                started_at: timestamp,
                cost_usd: Some(0.25),
                billing_kind: Some(BillingKind::TokenMetered),
            }),
            total: tokens.clone(),
            last: tokens,
            cost: None,
            context_tokens: 0,
            model_context_window: None,
            auto_compact_tokens: None,
        },
    }
}

#[tokio::test]
async fn cleanup_deletes_a_rewound_child_whose_parent_is_not_in_trash() {
    let (test, fake) = super::agents::harness(false).await;
    let parent = super::agents::parent(&test).await;
    test.client
        .post(format!("{parent}/input"))
        .json(&json!({"input":[{"type":"text","text":"Second parent turn"}],"mode":"queue","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let second_turn = super::agents::idle(&test, &parent)
        .await
        .turns
        .last()
        .unwrap()
        .id
        .clone();
    let child = super::agents::spawn(&test, &parent, "Remove this task").await;
    super::agents::idle(&test, &child).await;
    let child_id = child.rsplit('/').next().unwrap();
    let mut params =
        json!({"beforeTurnId":second_turn,"rewindFiles":false,"rewindConversation":true});
    let preview: RewindPreviewResponse = test
        .client
        .post(format!("{parent}/rewind/preview"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    params["lastEventSeq"] = json!(preview.last_event_seq);
    test.client
        .post(format!("{parent}/rewind"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(test.app.home.join("trash").join(child_id).exists());

    crate::http::cleanup::cleanup_at(&test.app, 30, future_now()).await;

    assert!(!test.app.home.join("trash").join(child_id).exists());
    assert!(
        test.app
            .store
            .session_dir(parent.rsplit('/').next().unwrap())
            .unwrap()
            .exists()
    );
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn trash_usage_totals_survive_cleanup_without_changing_daily_rows() {
    let test = Harness::new().await;
    let settings = SessionSnapshot::empty("trashed-usage").settings;
    let mut writer = test.app.store.create("trashed-usage", settings, 0).unwrap();
    writer.append(usage_event(1), 1).unwrap();
    drop(writer);
    let trash = test.app.home.join("trash");
    std::fs::create_dir(&trash).unwrap();
    std::fs::rename(
        test.app.store.session_dir("trashed-usage").unwrap(),
        trash.join("trashed-usage"),
    )
    .unwrap();
    let before: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before.total.uncached_input_tokens, 60);
    assert_eq!(before.total.cached_input_tokens, 40);
    assert_eq!(before.total.output_tokens, 10);

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(!trash.join("trashed-usage").exists());
    let after: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, before);
    test.stop().await;
}

#[tokio::test]
async fn cleanup_rereads_summary_after_locking_the_family() {
    let test = Harness::new().await;
    save(&test, "refreshed", 0);
    let block = test.app.pauses.block("refreshed", "cleanup_locked");
    let app = test.app.clone();
    let cleaning = tokio::spawn(async move {
        crate::http::cleanup::cleanup_at(&app, 30, 40 * DAY).await;
    });
    block.reached().await;
    let mut writer = test.app.store.open("refreshed").unwrap();
    writer.append(usage_event(39 * DAY), 39 * DAY).unwrap();
    drop(writer);
    drop(block);
    cleaning.await.unwrap();

    assert!(test.app.store.session_dir("refreshed").unwrap().exists());
    assert!(!test.app.home.join("usage-archive.jsonl").exists());
    test.stop().await;
}

#[tokio::test]
async fn failed_child_delete_preserves_its_parent() {
    use std::os::unix::fs::PermissionsExt;
    let test = Harness::new().await;
    save(&test, "delete-parent", 0);
    let parent = SessionParent {
        session_id: "delete-parent".into(),
        turn_id: "turn".into(),
        task: "child task".into(),
        agent_type: AgentType::General,
        depth: None,
    };
    let settings = SessionSnapshot::empty("delete-child").settings;
    drop(
        test.app
            .store
            .create_child("delete-child", settings, parent, 0)
            .unwrap(),
    );
    let child = test.app.store.session_dir("delete-child").unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o500)).unwrap();

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    assert!(child.exists());
    assert!(
        test.app
            .store
            .session_dir("delete-parent")
            .unwrap()
            .exists()
    );
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
    test.stop().await;
}

#[tokio::test]
async fn archive_skips_bad_lines_separates_a_partial_line_and_uses_camel_case() {
    use std::io::Write;
    let test = Harness::new().await;
    save(&test, "archived-first", 0);
    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;
    let path = test.app.home.join("usage-archive.jsonl");
    let first = std::fs::read_to_string(&path).unwrap();
    let record: serde_json::Value = serde_json::from_str(first.trim_end()).unwrap();
    assert_eq!(record["sessionId"], "archived-first");
    assert!(record.get("eventSeq").is_some());
    assert!(record.get("session_id").is_none());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"sessionId\":")
        .unwrap();
    save(&test, "archived-second", 0);

    crate::http::cleanup::cleanup_at(&test.app, 30, 40 * DAY).await;

    let archive = std::fs::read_to_string(&path).unwrap();
    assert!(archive.ends_with('\n'));
    let records = archive
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["sessionId"], "archived-second");
    test.client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.stop().await;
}
