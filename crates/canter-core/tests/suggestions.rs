#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn complete(text: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105}
        })
    ))
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
}
impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        Self::with_options(
            replies,
            CoreOptions {
                prompt_suggestions: true,
                ..Default::default()
            },
        )
        .await
    }
    async fn with_options(replies: Vec<Reply>, options: CoreOptions) -> Self {
        Self::with_restricted_mode(replies, options, false).await
    }
    async fn with_restricted_mode(
        replies: Vec<Reply>,
        options: CoreOptions,
        restricted_mode: bool,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(replies).await;
        let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
        let model = Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap());
        let (session, _) = Session::create(
            store.clone(),
            "suggestions".into(),
            serde_json::from_value(
                json!({"cwd":root.path(),"model":"fake","approvalPolicy":"allowAll"}),
            )
            .unwrap(),
            RuntimeContext {
                restricted_mode,
                environment: [
                    (
                        "HOME".into(),
                        root.path().join("home").display().to_string(),
                    ),
                    (
                        "PATH".into(),
                        format!("{}:/usr/bin:/bin", root.path().join("bin").display()),
                    ),
                    (
                        "CANTER_HOME".into(),
                        root.path().join("state").display().to_string(),
                    ),
                ]
                .into(),
                ..Default::default()
            },
            model,
            options,
        )
        .await
        .unwrap();
        Self {
            root,
            store,
            session,
            server,
        }
    }
}

fn tool(name: &str, arguments: Value) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]})
    ))
}

async fn no_suggestion(events: &mut Subscription) {
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            next_type(events, "session.promptSuggestion")
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn empty_tool_only_multiline_and_long_answers_are_not_suggestions() {
    for reply in [
        complete(""),
        complete("  "),
        complete("first\nsecond"),
        complete(&"界".repeat(201)),
        complete(&format!("{} trailing text", "x".repeat(200))),
        tool("shell", json!({"command":"touch must-not-exist"})),
    ] {
        let f = Fixture::new(vec![complete("Done."), reply]).await;
        let mut events = f.session.subscribe(0).await.unwrap();
        f.session.input(input()).await.unwrap();
        next_type(&mut events, "turn.completed").await;
        requests(&f.server, 2).await;
        no_suggestion(&mut events).await;
        f.session.close().await.unwrap();
        assert!(!f.root.path().join("must-not-exist").exists());
        assert_eq!(f.server.requests.lock().unwrap().len(), 2);
        f.server.stop().await;
    }
}

async fn excluded_turn(kind: &str) {
    let mut reply = complete("Done.");
    if kind == "failed" {
        reply.status = axum::http::StatusCode::BAD_REQUEST;
        reply.body = "invalid request".into();
    }
    if kind == "truncated" {
        reply.body = reply.body.replace("\"stop\"", "\"length\"");
    }
    let expected_requests = if kind == "truncated" { 4 } else { 1 };
    let mut replies = Vec::new();
    for _ in 1..expected_requests {
        replies.push(Reply::sse(reply.body.clone()));
    }
    replies.push(reply);
    let f = Fixture::with_options(
        replies,
        CoreOptions {
            prompt_suggestions: kind != "disabled",
            ..Default::default()
        },
    )
    .await;
    if kind == "plan" {
        f.session
            .update(serde_json::from_value(json!({"planMode":true})).unwrap())
            .await
            .unwrap();
    }
    let mut events = f.session.subscribe(0).await.unwrap();
    let mut params = input();
    params.non_interactive = kind == "exec";
    f.session.input(params).await.unwrap();
    next_type(&mut events, "turn.completed").await;
    no_suggestion(&mut events).await;
    f.session.close().await.unwrap();
    assert_eq!(
        f.server.requests.lock().unwrap().len(),
        expected_requests,
        "{kind}"
    );
    f.server.stop().await;
}
#[tokio::test]
async fn failed_turn_does_not_suggest() {
    excluded_turn("failed").await;
}
#[tokio::test]
async fn truncated_turn_does_not_suggest() {
    excluded_turn("truncated").await;
}
#[tokio::test]
async fn plan_mode_does_not_suggest() {
    excluded_turn("plan").await;
}
#[tokio::test]
async fn noninteractive_turn_does_not_suggest() {
    excluded_turn("exec").await;
}
#[tokio::test]
async fn disabled_suggestions_do_not_request_the_model() {
    excluded_turn("disabled").await;
}

#[tokio::test]
async fn interrupted_turn_does_not_suggest() {
    let mut reply = Reply::sse("");
    reply.hang = true;
    let f = Fixture::new(vec![reply]).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    let turn = f.session.input(input()).await.unwrap().turn_id.unwrap();
    requests(&f.server, 1).await;
    f.session
        .interrupt(serde_json::from_value(json!({"turnId":turn})).unwrap())
        .await
        .unwrap();
    next_type(&mut events, "turn.completed").await;
    no_suggestion(&mut events).await;
    assert_eq!(f.server.requests.lock().unwrap().len(), 1);
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn pending_questions_approvals_and_plan_approvals_do_not_suggest() {
    for (name, args, settings) in [
        (
            "ask_user",
            json!({"question":"Continue?","options":[{"label":"yes","description":"Proceed"},{"label":"no","description":"Stop"}]}),
            json!({}),
        ),
        (
            "shell",
            json!({"command":"touch approved-file"}),
            json!({"approvalPolicy":"alwaysAsk"}),
        ),
        (
            "submit_plan",
            json!({"plan":"Implement parser tests."}),
            json!({"planMode":true}),
        ),
    ] {
        let f = Fixture::new(vec![tool(name, args)]).await;
        f.session
            .update(serde_json::from_value(settings).unwrap())
            .await
            .unwrap();
        let mut events = f.session.subscribe(0).await.unwrap();
        f.session.input(input()).await.unwrap();
        next_type(&mut events, "request.opened").await;
        no_suggestion(&mut events).await;
        assert_eq!(f.server.requests.lock().unwrap().len(), 1, "{name}");
        f.session.close().await.unwrap();
        f.server.stop().await;
    }
}

#[tokio::test]
async fn queued_input_runs_before_any_suggestion() {
    let f = Fixture::new(vec![tool("ask_user", json!({"question":"Continue?","options":[{"label":"yes","description":"Proceed"},{"label":"no","description":"Stop"}]})), complete("First done."), complete("Second done."), complete("add tests")]).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    f.session.input(input()).await.unwrap();
    let request = next_type(&mut events, "request.opened").await;
    assert!(f.session.input(input()).await.unwrap().turn_id.is_none());
    f.session
        .resolve(
            request["request"]["id"].as_str().unwrap(),
            serde_json::from_value(
                json!({"answer":{"type":"option","label":"yes"},"clientName":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    next_type(&mut events, "turn.completed").await;
    let second = next_type(&mut events, "turn.completed").await;
    let suggestion = next_type(&mut events, "session.promptSuggestion").await;
    assert_eq!(suggestion["turnId"], second["turn"]["id"]);
    assert_eq!(f.server.requests.lock().unwrap().len(), 4);
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn child_sessions_do_not_suggest() {
    let f = Fixture::new(vec![complete("Parent done."), complete("Child done.")]).await;
    let mut parent_events = f.session.subscribe(0).await.unwrap();
    let mut params = input();
    params.non_interactive = true;
    f.session.input(params).await.unwrap();
    next_type(&mut parent_events, "turn.completed").await;
    let (child, _) = f
        .session
        .spawn_agent("child".into(), "Inspect parser".into())
        .await
        .unwrap();
    let mut events = child.subscribe(0).await.unwrap();
    next_type(&mut events, "turn.completed").await;
    no_suggestion(&mut events).await;
    assert_eq!(f.server.requests.lock().unwrap().len(), 2);
    child.close().await.unwrap();
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn goal_continuation_takes_precedence_over_suggestions() {
    let mut hanging = Reply::sse("");
    hanging.hang = true;
    let f = Fixture::new(vec![complete("Progress."), hanging]).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    f.session
        .set_goal(
            serde_json::from_value(json!({"objective":"Finish parser","budgetUsd":0})).unwrap(),
        )
        .await
        .unwrap();
    next_type(&mut events, "turn.completed").await;
    requests(&f.server, 2).await;
    assert!(
        !f.server.requests.lock().unwrap()[1]["messages"]
            .to_string()
            .contains("most likely next prompt")
    );
    no_suggestion(&mut events).await;
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn budget_stopped_turn_does_not_suggest() {
    let f = Fixture::new(vec![complete("Progress."), complete("Budget summary.")]).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    f.session
        .set_goal(
            serde_json::from_value(json!({"objective":"Finish parser","budgetUsd":1})).unwrap(),
        )
        .await
        .unwrap();
    next_type(&mut events, "turn.completed").await;
    assert_eq!(
        f.session.goal().await.goal.unwrap().status,
        GoalStatus::BudgetLimited
    );
    no_suggestion(&mut events).await;
    f.session.close().await.unwrap();
    assert_eq!(f.server.requests.lock().unwrap().len(), 2);
    f.server.stop().await;
}

#[tokio::test]
async fn no_example_for_non_git_empty_git_disabled_or_timed_out_git() {
    use std::os::unix::fs::PermissionsExt;
    for kind in ["non-git", "empty", "disabled", "slow", "failed"] {
        let f = Fixture::with_options(
            vec![],
            CoreOptions {
                prompt_suggestions: kind != "disabled",
                ..Default::default()
            },
        )
        .await;
        if kind == "empty" || kind == "disabled" {
            git(f.root.path(), &["init", "-q"]);
            if kind == "disabled" {
                std::fs::write(f.root.path().join("file.rs"), "data").unwrap();
                git(f.root.path(), &["add", "file.rs"]);
                git(f.root.path(), &["commit", "-qm", "Add file"]);
            }
        }
        if kind == "slow" || kind == "failed" {
            std::fs::create_dir(f.root.path().join("bin")).unwrap();
            let git = f.root.path().join("bin/git");
            std::fs::write(
                &git,
                if kind == "slow" {
                    "#!/bin/sh\nprintf '%s' \"$$\" > git-pid\nexec /bin/sleep 10\n"
                } else {
                    "#!/bin/sh\nexit 1\n"
                },
            )
            .unwrap();
            std::fs::set_permissions(git, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut events = tokio::time::timeout(Duration::from_secs(1), f.session.subscribe(0))
            .await
            .expect("open must not await git")
            .unwrap();
        if kind == "slow" {
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    // The script creates git-pid before writing to it; an empty read means not yet written.
                    if let Ok(pid) = std::fs::read_to_string(f.root.path().join("git-pid"))
                        && let Ok(pid) = pid.parse()
                        && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                            == Err(nix::errno::Errno::ESRCH)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("timed-out git must be stopped");
        }
        no_suggestion(&mut events).await;
        assert!(f.server.requests.lock().unwrap().is_empty());
        tokio::time::timeout(Duration::from_secs(1), f.session.close())
            .await
            .unwrap()
            .unwrap();
        f.server.stop().await;
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("/usr/bin/git")
        .current_dir(root)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[tokio::test]
async fn restricted_mode_does_not_provide_a_git_example() {
    let f = Fixture::with_restricted_mode(
        vec![],
        CoreOptions {
            prompt_suggestions: true,
            ..Default::default()
        },
        true,
    )
    .await;
    git(f.root.path(), &["init", "-q"]);
    std::fs::write(f.root.path().join("parser.rs"), "fn parse() {}\n").unwrap();
    git(f.root.path(), &["add", "parser.rs"]);
    git(f.root.path(), &["commit", "-qm", "Add parser"]);
    let mut events = f.session.subscribe(0).await.unwrap();
    no_suggestion(&mut events).await;
    assert!(f.server.requests.lock().unwrap().is_empty());
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn git_example_passes_hardened_arguments_and_environment() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(vec![]).await;
    std::fs::create_dir(f.root.path().join("bin")).unwrap();
    let git = f.root.path().join("bin/git");
    // Record the invocation directly; no repository configuration is involved.
    std::fs::write(
        &git,
        r#"#!/bin/sh
printf '%s\n' "$GIT_CONFIG_GLOBAL" "$GIT_CONFIG_NOSYSTEM" "$GIT_OPTIONAL_LOCKS" > git-env
printf '%s\n' "$@" > git-args
printf 'parser.rs\000'
"#,
    )
    .unwrap();
    std::fs::set_permissions(git, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(f.root.path().join("parser.rs"), "fn parse() {}\n").unwrap();
    let mut events = f.session.subscribe(0).await.unwrap();
    let example = next_type(&mut events, "session.promptSuggestion").await;
    assert_eq!(example["text"], "explain how parser.rs works");
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("git-env")).unwrap(),
        "/dev/null\n1\n0\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("git-args"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "log",
            "--no-show-signature",
            "-30",
            "--format=",
            "--name-only",
            "-z",
            "--relative",
            "--diff-filter=AM",
            "--",
            ".",
        ]
    );
    assert!(f.server.requests.lock().unwrap().is_empty());
    f.session.close().await.unwrap();
    f.server.stop().await;
}

#[tokio::test]
async fn opening_a_git_project_provides_a_live_example_without_a_model_request() {
    let f = Fixture::new(vec![]).await;
    git(f.root.path(), &["init", "-q"]);
    std::fs::write(f.root.path().join("parser.rs"), "fn parse() {}\n").unwrap();
    std::fs::write(f.root.path().join("removed.rs"), "old\n").unwrap();
    git(f.root.path(), &["add", "parser.rs", "removed.rs"]);
    git(f.root.path(), &["commit", "-qm", "Add parser"]);
    std::fs::write(f.root.path().join("removed.rs"), "changed\n").unwrap();
    git(f.root.path(), &["add", "removed.rs"]);
    git(
        f.root.path(),
        &["commit", "-qm", "Change removed file more often"],
    );
    std::fs::remove_file(f.root.path().join("removed.rs")).unwrap();
    let mut events = f.session.subscribe(0).await.unwrap();
    let example = next_type(&mut events, "session.promptSuggestion").await;
    assert!(example["turnId"].is_null());
    assert_eq!(example["text"], "explain how parser.rs works");
    assert!(f.server.requests.lock().unwrap().is_empty());
    let mut late = f.session.subscribe(0).await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            next_type(&mut late, "session.promptSuggestion")
        )
        .await
        .is_err()
    );
    f.session.close().await.unwrap();
    f.server.stop().await;
}
fn input() -> SessionInputParams {
    serde_json::from_value(
        json!({"input":[{"type":"text","text":"fix the parser"}],"clientName":"test"}),
    )
    .unwrap()
}
async fn next_type(events: &mut Subscription, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let value = serde_json::to_value(events.recv().await.unwrap().unwrap()).unwrap();
            if value["type"] == kind {
                return value;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing {kind}"))
}

#[tokio::test]
async fn completed_turn_broadcasts_a_private_suggestion_with_the_same_request_prefix() {
    let mut reply = complete("add tests for the parser");
    reply.body = reply
        .body
        .replace("\"prompt_tokens\":100", "\"prompt_tokens\":1200")
        .replace("\"total_tokens\":105", "\"total_tokens\":1205");
    let f = Fixture::new(vec![complete("Fixed the parser."), reply]).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    let mut other = f.session.subscribe(0).await.unwrap();
    let turn = f.session.input(input()).await.unwrap().turn_id.unwrap();
    let completed = next_type(&mut events, "turn.completed").await;
    assert_eq!(completed["turn"]["status"], "completed");
    let suggestion = next_type(&mut events, "session.promptSuggestion").await;
    assert_eq!(suggestion["turnId"], turn);
    assert_eq!(suggestion["text"], "add tests for the parser");
    assert_eq!(
        next_type(&mut other, "session.promptSuggestion").await,
        suggestion
    );
    let requests = f.server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(f.server.arrivals.lock().unwrap().len(), 2);
    let mut prefix = requests[1].clone();
    let messages = prefix["messages"].as_array_mut().unwrap();
    assert_eq!(messages.pop().unwrap()["role"], "user");
    assert_eq!(messages.pop().unwrap()["content"], "Fixed the parser.");
    assert_eq!(prefix, requests[0]);
    let snapshot = f.session.snapshot().await.session;
    assert_eq!(snapshot.token_usage.as_ref().unwrap().context_tokens, 105);
    assert_eq!(
        snapshot.token_usage.as_ref().unwrap().total.input_tokens,
        1300
    );
    assert_eq!(
        snapshot.turns[0].usage.as_ref().unwrap().total.input_tokens,
        100
    );
    let mut late = f.session.subscribe(snapshot.last_event_seq).await.unwrap();
    no_suggestion(&mut late).await;
    f.session.close().await.unwrap();
    let history =
        serde_json::to_string(&f.store.model_history::<Value>("suggestions").unwrap()).unwrap();
    assert!(!history.contains("add tests for the parser"));
    let disk =
        std::fs::read_to_string(f.root.path().join("sessions/suggestions/events.jsonl")).unwrap();
    assert!(!disk.contains("promptSuggestion"));
    assert!(disk.contains("session.btwUsage"));
    assert!(
        f.store
            .private_telemetry("suggestions")
            .unwrap()
            .iter()
            .any(|record| serde_json::to_value(record).unwrap()["purpose"] == "suggestion")
    );
    f.server.stop().await;
}

async fn requests(server: &Server, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn hanging_suggestion_is_cancelled_without_waiting_for_network() {
    for operation in [
        "input",
        "next-suggestion",
        "btw",
        "handoff",
        "compact",
        "review",
        "close",
    ] {
        let mut hanging = Reply::sse("");
        hanging.hang = true;
        let mut next_hanging = Reply::sse("");
        next_hanging.hang = true;
        let f = Fixture::new(vec![
            complete("Fixed."),
            hanging,
            complete("Next answer."),
            next_hanging,
        ])
        .await;
        let mut events = f.session.subscribe(0).await.unwrap();
        f.session.input(input()).await.unwrap();
        // The end event is available even though the suggestion never finishes.
        next_type(&mut events, "turn.completed").await;
        requests(&f.server, 2).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            match operation {
                "input" | "review" | "next-suggestion" => {
                    let mut params = input();
                    params.non_interactive = operation != "next-suggestion";
                    params.review = (operation == "review").then(|| "Review changes".into());
                    f.session.input(params).await.unwrap();
                    requests(&f.server, 3).await;
                    next_type(&mut events, "turn.completed").await;
                    if operation == "next-suggestion" {
                        requests(&f.server, 4).await;
                    }
                }
                "btw" | "handoff" => {
                    let mut response = if operation == "btw" {
                        f.session.btw("what changed?".into()).await.unwrap()
                    } else {
                        f.session.handoff(None).await.unwrap()
                    };
                    while let Some(event) = response.recv().await {
                        if matches!(event, BtwEvent::Completed) {
                            break;
                        }
                    }
                    requests(&f.server, 3).await;
                }
                "compact" => {
                    f.session.compact().await.unwrap();
                    requests(&f.server, 3).await;
                }
                "close" => {}
                _ => unreachable!(),
            }
            f.session.close().await.unwrap();
        })
        .await
        .unwrap_or_else(|_| panic!("{operation} waited for suggestion network"));
        let telemetry = f.store.private_telemetry("suggestions").unwrap();
        if operation == "next-suggestion" {
            assert_eq!(
                telemetry
                    .iter()
                    .filter(|record| record.purpose == RequestPurpose::Suggestion
                        && record.outcome == ModelRequestOutcome::Cancelled)
                    .count(),
                2
            );
        }
        assert!(
            telemetry.iter().any(|record| {
                let record = serde_json::to_value(record).unwrap();
                record["purpose"] == "suggestion" && record["outcome"] == "cancelled"
            }),
            "{operation} did not cancel the suggestion"
        );
        f.server.stop().await;
    }
}

#[tokio::test]
async fn twenty_unused_suggestions_reduce_frequency_and_using_one_restores_it() {
    let mut replies = Vec::new();
    for turn in 1..=26 {
        replies.push(complete("Done."));
        if turn <= 20 || turn >= 24 {
            replies.push(complete("add parser tests"));
        }
    }
    let f = Fixture::new(replies).await;
    let mut events = f.session.subscribe(0).await.unwrap();
    for turn in 1..=26 {
        let params = if turn == 25 {
            serde_json::from_value(json!({"input":[{"type":"text","text":"add more parser tests"}],"clientName":"test","fromSuggestion":true})).unwrap()
        } else {
            input()
        };
        f.session.input(params).await.unwrap();
        next_type(&mut events, "turn.completed").await;
        if turn == 21 {
            next_type(&mut events, "session.promptSuggestionsReduced").await;
        }
        if turn <= 20 || turn >= 24 {
            next_type(&mut events, "session.promptSuggestion").await;
        }
    }
    f.session.close().await.unwrap();
    assert_eq!(f.server.requests.lock().unwrap().len(), 49);
    f.server.stop().await;
}
