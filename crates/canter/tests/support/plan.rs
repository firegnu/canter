use super::*;
use canter_protocol::{SessionSnapshot, TurnStatus};
use canter_tui::{App, Export, Ui};
use ratatui::backend::TestBackend;

const REMINDER: &str = "[Plan reminder] If any plan steps are complete, call update_plan to mark them completed before continuing.";

fn plan(id: &str, status: &str) -> Value {
    json!({"id":id,"type":"function","function":{
        "name":"update_plan","arguments":json!({"plan":[{"step":"Implement","status":status}]}).to_string()
    }})
}

fn reads(range: std::ops::RangeInclusive<usize>) -> Vec<Value> {
    range
        .map(|n| {
            json!({"id":format!("read-{n}"),"type":"function","function":{
                "name":"read","arguments":r#"{"path":"note.txt"}"#
            }})
        })
        .collect()
}

fn batch(mut calls: Vec<Value>) -> String {
    for (index, call) in calls.iter_mut().enumerate() {
        call["index"] = json!(index);
    }
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{
            "index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"
        }]})
    )
}

async fn turn(fixture: &Fixture, id: &str) -> SessionSnapshot {
    let path = format!("/v1/sessions/{id}");
    let before: SessionReadResponse = fixture
        .request(Method::GET, &path, json!({}))
        .await
        .json()
        .await
        .unwrap();
    fixture.input(id).await;
    fixture
        .events_until(id, before.event_seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let after: SessionReadResponse = fixture
        .request(Method::GET, &path, json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        after.session.turns.last().unwrap().status,
        TurnStatus::Completed
    );
    after.session
}

fn results(fixture: &Fixture) -> Vec<(String, String)> {
    fixture.script.lock().unwrap().requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            (
                message["tool_call_id"].as_str().unwrap().into(),
                message["content"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn reminded(fixture: &Fixture) -> Vec<String> {
    results(fixture)
        .into_iter()
        .filter(|(_, text)| text.contains(REMINDER))
        .map(|(id, _)| id)
        .collect()
}

#[tokio::test]
async fn plan_reminders_count_calls_across_turns_repeat_and_stay_out_of_ui_and_user_words() {
    let mut first = vec![plan("initial", "in_progress")];
    first.extend(reads(1..=9));
    let mut fixture = Fixture::start(vec![
        batch(first),
        text_reply(),
        batch(reads(10..=20)),
        text_reply(),
        text_reply(),
    ])
    .await;
    fs::write(
        fixture.root.path().join("work/note.txt"),
        "Tool output only\n",
    )
    .unwrap();
    let id = fixture.create().await.session.id;
    turn(&fixture, &id).await;
    assert!(
        reminded(&fixture).is_empty(),
        "the ninth call must not remind"
    );
    let snapshot = turn(&fixture, &id).await;
    assert_eq!(reminded(&fixture), ["read-10", "read-20"]);
    let results = results(&fixture);
    let plain = &results.iter().find(|(id, _)| id == "read-9").unwrap().1;
    assert!(plain.contains("Tool output only"));
    for n in [10, 20] {
        let text = &results
            .iter()
            .find(|(id, _)| id == &format!("read-{n}"))
            .unwrap()
            .1;
        assert_eq!(text, &format!("{plain}\n\n{REMINDER}"));
    }
    {
        let script = fixture.script.lock().unwrap();
        for pair in script.requests.windows(2) {
            let mut next = pair[1].clone();
            next["messages"]
                .as_array_mut()
                .unwrap()
                .truncate(pair[0]["messages"].as_array().unwrap().len());
            assert_eq!(
                next, pair[0],
                "old messages and tool definitions must stay unchanged"
            );
        }
    }
    let directory = fixture.root.path().join("state/sessions").join(&id);
    let history = fs::read_to_string(directory.join("model-history.jsonl")).unwrap();
    assert_eq!(history.matches(REMINDER).count(), 2);
    for line in history.lines().filter(|line| line.contains(REMINDER)) {
        let entry: Value = serde_json::from_str(line).unwrap();
        assert_eq!(entry["user_input"], false);
    }
    assert!(
        !fs::read_to_string(directory.join("events.jsonl"))
            .unwrap()
            .contains(REMINDER)
    );
    assert!(!serde_json::to_string(&snapshot).unwrap().contains(REMINDER));

    // Render and export the actual server snapshot, not a separate UI fixture.
    let mut ui = Ui::new(
        TestBackend::new(140, 200),
        App::new(snapshot.clone(), None, 0),
    )
    .unwrap();
    ui.draw(0).unwrap();
    let rendered: String = ui
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Updated Plan"));
    assert!(rendered.contains("note.txt"));
    assert!(!rendered.contains("[Plan reminder]"));
    let export = fixture.root.path().join("export.md");
    ui.app_mut()
        .export(Export::File(Some(export.to_string_lossy().into())), 0);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !export.exists() {
            ui.draw(0).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let markdown = fs::read_to_string(export).unwrap();
    assert!(markdown.contains("Updated Plan"));
    assert!(markdown.contains("note.txt"));
    assert!(!markdown.contains("[Plan reminder]"));

    assert_eq!(
        fixture
            .request(
                Method::POST,
                &format!("/v1/sessions/{id}/compact"),
                json!({})
            )
            .await
            .status(),
        200
    );
    fixture
        .events_until(&id, snapshot.last_event_seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let request = fixture
        .script
        .lock()
        .unwrap()
        .requests
        .last()
        .unwrap()
        .clone();
    let transcript = request["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        transcript.contains(&format!("Tool result read-10: {plain}\n\n{REMINDER}")),
        "reminders stay under tool results, not user speech"
    );
    let history = fs::read_to_string(directory.join("model-history.jsonl")).unwrap();
    let compacted: Value = history
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|entry| entry["type"] == "compaction")
        .unwrap();
    assert_eq!(
        compacted["user_messages"],
        json!(["private-user-input", "private-user-input"])
    );
    fixture.stop().await;
}

#[tokio::test]
async fn plan_reminders_reset_only_after_successful_updates() {
    let mut calls = vec![plan("initial", "in_progress")];
    calls.extend(reads(1..=9));
    calls.push(plan("update", "pending"));
    calls.extend(reads(10..=18));
    calls.push(plan("invalid", "invalid-status"));
    calls.extend(reads(19..=19));
    let mut fixture = Fixture::start(vec![batch(calls), text_reply()]).await;
    fs::write(
        fixture.root.path().join("work/note.txt"),
        "Tool output only\n",
    )
    .unwrap();
    let id = fixture.create().await.session.id;
    let snapshot = turn(&fixture, &id).await;
    assert_eq!(reminded(&fixture), ["read-19"]);
    assert_eq!(
        serde_json::to_value(snapshot.current_plan()).unwrap(),
        json!([{"step":"Implement","status":"pending"}])
    );
    fixture.stop().await;
}

#[tokio::test]
async fn plan_reminders_require_an_enabled_unfinished_plan() {
    for status in [None, Some("completed")] {
        let mut calls = status
            .map(|status| vec![plan("initial", status)])
            .unwrap_or_default();
        calls.extend(reads(1..=20));
        let mut fixture = Fixture::start(vec![batch(calls), text_reply()]).await;
        fs::write(
            fixture.root.path().join("work/note.txt"),
            "Tool output only\n",
        )
        .unwrap();
        let id = fixture.create().await.session.id;
        turn(&fixture, &id).await;
        assert!(reminded(&fixture).is_empty());
        fixture.stop().await;
    }
    // An unfinished plan remains in history when the session is reopened disabled.
    let mut fixture = Fixture::start(vec![
        batch(vec![plan("initial", "in_progress")]),
        text_reply(),
        batch(reads(1..=20)),
        text_reply(),
    ])
    .await;
    fs::write(
        fixture.root.path().join("work/note.txt"),
        "Tool output only\n",
    )
    .unwrap();
    let id = fixture.create().await.session.id;
    turn(&fixture, &id).await;
    let config_path = fixture.root.path().join("state/config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["plan_enabled"] = json!(false);
    fs::write(config_path, config.to_string()).unwrap();
    let path = format!("/v1/sessions/{id}");
    assert_eq!(
        fixture
            .request(Method::DELETE, &path, json!({}))
            .await
            .status(),
        200
    );
    assert_eq!(
        fixture
            .request(
                Method::POST,
                &format!("{path}/resume"),
                json!({"env":{"HOME":fixture.root.path().join("home")}}),
            )
            .await
            .status(),
        200
    );
    let snapshot = turn(&fixture, &id).await;
    assert_eq!(snapshot.settings.plan_enabled, Some(false));
    assert!(snapshot.current_plan().is_some());
    assert!(reminded(&fixture).is_empty());
    fixture.stop().await;
}
