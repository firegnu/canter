use super::*;

#[tokio::test]
async fn session_start_context_precedes_the_first_user_prompt() {
    let mut fixture = Fixture::new(json!({"hooks":{"SessionStart":command(
        r#"cat > start.json; printf '%s' '{"hookSpecificOutput":{"additionalContext":"Remember the unfinished migration."}}'"#
    )}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    let hook = fixture.hook_file("start.json").await;
    assert_transcript(&fixture, &url, &hook);
    fixture.input(&url).await;
    let request = fixture.request().await;
    let messages = request.body["messages"].as_array().unwrap();
    let context = messages.iter().position(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.contains("Remember the unfinished migration."))
    });
    let user = messages
        .iter()
        .position(|message| message["content"] == "hello")
        .unwrap();
    request.stop();
    fixture.idle(&url).await;
    let snapshot = serde_json::to_value(fixture.snapshot(&url).await).unwrap();
    assert_eq!(
        snapshot["session"]["turns"][0]["items"][0]["type"],
        "hookContext"
    );
    assert_eq!(
        snapshot["session"]["turns"][0]["items"][0]["event"],
        "SessionStart"
    );
    assert_eq!(snapshot["session"]["turns"][0]["items"][0]["charCount"], 34);
    assert!(!snapshot.to_string().contains("unfinished migration"));
    fixture.shutdown().await;
    assert!(
        context.is_some_and(|index| index < user),
        "SessionStart context must precede the user prompt"
    );
}

pub(super) fn assert_transcript(fixture: &Fixture, url: &str, hook: &Value) {
    let id = url.rsplit('/').next().unwrap();
    let expected = fixture
        .root
        .path()
        .join("state/sessions")
        .join(id)
        .join("events.jsonl");
    let actual = std::path::Path::new(hook["transcript_path"].as_str().unwrap());
    assert!(actual.is_absolute());
    assert_eq!(
        actual.canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
    let first: Value =
        serde_json::from_str(fs::read_to_string(actual).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(first["sessionId"], id);
}

fn contexts(request: &Request, event: &str) -> Vec<String> {
    request.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .filter(|text| text.starts_with(&format!("[Hook context: {event}]")))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn prompt_context_follows_configuration_order_and_precedes_each_queued_or_steered_input() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":[{"hooks":[
        {"type":"command","command":r#"cat >/dev/null; sleep 0.05; printf '%s' '{"hookSpecificOutput":{"additionalContext":"first 钩子"}}'"#},
        {"type":"command","command":r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"second hook"}}'"#}
    ]}]}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    let prefix = first.body["messages"].as_array().unwrap().clone();
    assert!(contexts(&first, "UserPromptSubmit")[0].ends_with("first 钩子\nsecond hook"));
    fixture.send(&url, "steered one", "steer").await;
    fixture.send(&url, "steered two", "steer").await;
    fixture.send(&url, "queued next", "queue").await;
    first.tool("read", json!({"path":"missing.txt"}));
    let next = fixture.request().await;
    let messages = next.body["messages"].as_array().unwrap();
    assert_eq!(
        serde_json::to_vec(&messages[..prefix.len()]).unwrap(),
        serde_json::to_vec(&prefix).unwrap()
    );
    for text in ["steered one", "steered two"] {
        let index = messages
            .iter()
            .position(|message| message["content"] == text)
            .unwrap();
        assert!(
            messages[index - 1]["content"]
                .as_str()
                .unwrap()
                .ends_with("first 钩子\nsecond hook")
        );
    }
    next.stop();
    let queued = fixture.request().await;
    let messages = queued.body["messages"].as_array().unwrap();
    let index = messages
        .iter()
        .position(|message| message["content"] == "queued next")
        .unwrap();
    assert!(
        messages[index - 1]["content"]
            .as_str()
            .unwrap()
            .ends_with("first 钩子\nsecond hook")
    );
    assert_eq!(contexts(&queued, "UserPromptSubmit").len(), 4);
    queued.stop();
    fixture.idle(&url).await;
    let snapshot = fixture.snapshot(&url).await;
    let items = &snapshot.session.turns[0].items;
    for (index, item) in items.iter().enumerate() {
        if matches!(item, canter_protocol::Item::UserMessage { .. }) {
            assert!(matches!(
                items[index - 1],
                canter_protocol::Item::HookContext { char_count: 20, .. }
            ));
        }
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn invalid_context_output_is_not_injected_and_failures_warn() {
    for (output, warning) in [
        ("plain hook log", true),
        ("{broken", true),
        (r#"{"hookSpecificOutput":{"additionalContext":""}}"#, false),
        (
            r#"{"hookSpecificOutput":{"additionalContext":"  "}}"#,
            false,
        ),
        (r#"{"hookSpecificOutput":{"additionalContext":42}}"#, false),
    ] {
        let script = format!("cat >/dev/null; printf '%s' '{output}'");
        let mut fixture = Fixture::new(json!({"hooks":{
            "SessionStart":command(&script), "UserPromptSubmit":command(&script)
        }}))
        .await;
        fixture.trust(fixture.root.path()).await;
        let url = fixture.create().await;
        let mut warnings = fixture.warnings_after(&url, 0).await;
        fixture.input(&url).await;
        let request = fixture.request().await;
        assert!(contexts(&request, "SessionStart").is_empty());
        assert!(contexts(&request, "UserPromptSubmit").is_empty());
        if warning {
            assert!(warnings.next().await.contains("invalid JSON output"));
            assert!(warnings.next().await.contains("invalid JSON output"));
        }
        request.stop();
        fixture.idle(&url).await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn blocked_prompt_discards_all_command_context_and_keeps_start_context_for_the_next_input() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"opening context"}}'"#),
        "UserPromptSubmit":[{"hooks":[
            {"type":"command","command":r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"blocked context"}}'"#},
            {"type":"command","command":r#"cat >/dev/null; printf '%s' '{"decision":"block","reason":"rejected"}'"#}
        ]}]
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.idle(&url).await;
    assert!(fixture.requests.try_recv().is_err());
    assert!(
        !serde_json::to_string(&fixture.snapshot(&url).await)
            .unwrap()
            .contains("hookContext")
    );
    fixture.config["hooks"]["UserPromptSubmit"] = json!([]);
    fixture.applied("prompt-allowed").await;
    fixture.send(&url, "allowed", "queue").await;
    let request = fixture.request().await;
    assert_eq!(contexts(&request, "SessionStart").len(), 1);
    assert!(!request.body.to_string().contains("blocked context"));
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn cancelling_the_first_wait_retains_start_context_without_waking_the_model() {
    let mut fixture = Fixture::new(json!({"hooks":{"SessionStart":command(
        r#"cat > start.json; while [ ! -f release ]; do sleep 0.02; done; printf '%s' '{"hookSpecificOutput":{"additionalContext":"delayed opening context"}}'"#
    )}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.hook_file("start.json").await;
    fixture.input(&url).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.requests.recv())
            .await
            .is_err()
    );
    fixture.interrupt_hook_turn(&url).await;
    fs::write(fixture.root.path().join("release"), "").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.requests.recv())
            .await
            .is_err()
    );
    fixture.send(&url, "after cancellation", "queue").await;
    let request = fixture.request().await;
    assert_eq!(contexts(&request, "SessionStart").len(), 1);
    let messages = request.body["messages"].as_array().unwrap();
    let index = messages
        .iter()
        .position(|message| message["content"] == "after cancellation")
        .unwrap();
    assert!(
        messages[index - 1]["content"]
            .as_str()
            .unwrap()
            .contains("delayed opening context")
    );
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn reopening_and_forking_replay_context_and_append_one_new_start_context() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"opening context"}}'"#),
        "UserPromptSubmit":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"input context"}}'"#)
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    let prefix = first.body["messages"].as_array().unwrap().clone();
    first.stop();
    fixture.idle(&url).await;
    fixture.close(&url).await;
    fixture.resume(&url).await;
    fixture.send(&url, "resumed input", "queue").await;
    let resumed = fixture.request().await;
    assert_eq!(contexts(&resumed, "SessionStart").len(), 2);
    assert_eq!(contexts(&resumed, "UserPromptSubmit").len(), 2);
    assert_eq!(
        serde_json::to_vec(&resumed.body["messages"].as_array().unwrap()[..prefix.len()]).unwrap(),
        serde_json::to_vec(&prefix).unwrap()
    );
    let inherited = contexts(&resumed, "SessionStart");
    resumed.stop();
    fixture.idle(&url).await;
    let fork: SessionCreateResponse = fixture.client.post(format!("{url}/fork"))
        .json(&json!({"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")}}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let fork_url = format!("http://localhost/v1/sessions/{}", fork.session.id);
    fixture.send(&fork_url, "forked input", "queue").await;
    let forked = fixture.request().await;
    let all = contexts(&forked, "SessionStart");
    assert_eq!(&all[..inherited.len()], inherited);
    assert_eq!(all.len(), 3);
    assert_eq!(contexts(&forked, "UserPromptSubmit").len(), 3);
    forked.stop();
    fixture.idle(&fork_url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn retracted_session_start_is_reinjected_once_even_after_reopening() {
    for reopen_before_retry in [false, true] {
        let mut fixture = Fixture::new(json!({"hooks":{
            "SessionStart":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"opening context to retry"}}'"#),
            "UserPromptSubmit":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"discard this input context"}}'"#)
        }})).await;
        fixture.trust(fixture.root.path()).await;
        let url = fixture.create().await;
        fixture.input(&url).await;
        let first = fixture.request().await;
        let opening = contexts(&first, "SessionStart");
        assert_eq!(opening.len(), 1);
        let turn_id = fixture.snapshot(&url).await.session.turns[0].id.clone();
        let response: Value = fixture
            .client
            .post(format!("{url}/interrupt"))
            .json(&json!({"turnId":turn_id,"retract":true}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["retracted"], true);
        drop(first);
        fixture.config["hooks"] = json!({});
        fixture.applied("without-hooks").await;
        if reopen_before_retry {
            fixture.close(&url).await;
            fixture.resume(&url).await;
        }
        fixture.send(&url, "corrected input", "queue").await;
        let retry = fixture.request().await;
        assert_eq!(
            contexts(&retry, "SessionStart"),
            opening,
            "retracted SessionStart context must be reinjected exactly once"
        );
        assert!(contexts(&retry, "UserPromptSubmit").is_empty());
        let messages = retry.body["messages"].as_array().unwrap();
        let user = messages
            .iter()
            .position(|message| message["content"] == "corrected input")
            .unwrap();
        assert_eq!(messages[user - 1]["content"], opening[0]);
        retry.stop();
        fixture.idle(&url).await;
        let snapshot = fixture.snapshot(&url).await;
        assert_eq!(snapshot.session.turns.iter().flat_map(|turn| &turn.items)
            .filter(|item| matches!(item, canter_protocol::Item::HookContext { event, .. } if event == "SessionStart")).count(), 1);
        // Replaying the retry must keep its one saved injection, not requeue it again.
        fixture.close(&url).await;
        fixture.resume(&url).await;
        fixture.send(&url, "continue", "queue").await;
        let restored = fixture.request().await;
        assert_eq!(contexts(&restored, "SessionStart"), opening);
        assert!(contexts(&restored, "UserPromptSubmit").is_empty());
        restored.stop();
        fixture.idle(&url).await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn rewind_drops_start_context_while_retract_reinjects_it_in_memory_and_after_replay() {
    for retract in [false, true] {
        let mut fixture = Fixture::new(json!({"hooks":{
            "SessionStart":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"remove opening context"}}'"#),
            "UserPromptSubmit":command(r#"cat >/dev/null; printf '%s' '{"hookSpecificOutput":{"additionalContext":"remove input context"}}'"#)
        }})).await;
        fixture.trust(fixture.root.path()).await;
        let url = fixture.create().await;
        fixture.input(&url).await;
        let request = fixture.request().await;
        assert_eq!(contexts(&request, "SessionStart").len(), 1);
        let opening = if retract {
            contexts(&request, "SessionStart")
        } else {
            Vec::new()
        };
        let turn_id = fixture.snapshot(&url).await.session.turns[0].id.clone();
        if retract {
            let response: Value = fixture
                .client
                .post(format!("{url}/interrupt"))
                .json(&json!({"turnId":turn_id,"retract":true}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response["retracted"], true);
            drop(request);
        } else {
            request.stop();
            fixture.idle(&url).await;
            let mut params =
                json!({"beforeTurnId":turn_id,"rewindConversation":true,"rewindFiles":false});
            let preview: Value = fixture
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
            fixture
                .client
                .post(format!("{url}/rewind"))
                .json(&params)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        assert!(
            !serde_json::to_string(&fixture.snapshot(&url).await)
                .unwrap()
                .contains("hookContext")
        );
        fixture.config["hooks"] = json!({});
        fixture.applied("without-hooks").await;
        fixture.send(&url, "after removal", "queue").await;
        let request = fixture.request().await;
        assert_eq!(contexts(&request, "SessionStart"), opening);
        assert!(!request.body.to_string().contains("remove input context"));
        request.stop();
        fixture.idle(&url).await;
        fixture.close(&url).await;
        fixture.resume(&url).await;
        fixture.input(&url).await;
        let request = fixture.request().await;
        assert_eq!(contexts(&request, "SessionStart"), opening);
        assert!(contexts(&request, "UserPromptSubmit").is_empty());
        request.stop();
        fixture.idle(&url).await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn session_start_timeout_releases_the_first_request_with_a_warning() {
    let mut fixture = Fixture::new(json!({"hooks":{"SessionStart":[{"hooks":[{
        "type":"command","command":"cat > start.json; sleep 30", "timeout":0.2
    }]}]}}))
    .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings_after(&url, 0).await;
    fixture.input(&url).await;
    let request = fixture.request().await;
    assert!(contexts(&request, "SessionStart").is_empty());
    assert!(warnings.next().await.contains("timed out"));
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}
