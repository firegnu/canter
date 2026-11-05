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
