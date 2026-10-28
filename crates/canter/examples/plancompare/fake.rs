//! Loopback-only scripted model, following promptcompare/services.rs's SSE fixture.
use super::{DATA, Result};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Script {
    pub replies: VecDeque<Value>,
    pub requests: usize,
    pub checked_d: bool,
    enabled: bool,
    scenario: String,
}
pub async fn start() -> Result<(String, Arc<Mutex<Script>>, tokio::task::JoinHandle<()>)> {
    let script = Arc::new(Mutex::new(Script::default()));
    let router = Router::new()
        .route("/chat/completions", post(reply))
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Ok((url, script, task))
}
async fn reply(
    State(script): State<Arc<Mutex<Script>>>,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let mut script = script.lock().unwrap();
    script.requests += 1;
    assert_eq!(request["model"], "deepseek-flash");
    assert_eq!(request["reasoning_effort"], "high");
    let tools = request["tools"].as_array().unwrap();
    assert_eq!(
        tools.iter().any(|t| t["function"]["name"] == "update_plan"),
        script.enabled
    );
    assert!(
        tools
            .iter()
            .all(|t| !t["function"]["name"].as_str().unwrap().starts_with("mcp__"))
    );
    if script.scenario == "D" {
        assert!(request["messages"].as_array().unwrap().iter().any(|m| {
            m["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.iter().any(|c| c["function"]["name"] == "update_plan"))
        }));
        script.checked_d = true;
    }
    let delta = script.replies.pop_front().expect("unexpected fake request");
    let finish = if delta.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    let output = if script.scenario == "Stop" {
        10_000_000
    } else {
        20
    };
    let usage = json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":output,"total_tokens":100+output,"prompt_cache_hit_tokens":50,"prompt_cache_miss_tokens":50}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
    )
}
fn text(text: &str) -> Value {
    json!({"content":text})
}
fn calls(calls: Vec<(&str, Value)>) -> Value {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let batch = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    json!({"tool_calls":calls.into_iter().enumerate().map(|(i,(name,args))| json!({
        "index":i,"id":format!("call-{batch}-{i}"),"type":"function",
        "function":{"name":name,"arguments":args.to_string()}
    })).collect::<Vec<_>>()})
}
fn call(name: &str, args: Value) -> Value {
    calls(vec![(name, args)])
}
fn plan(stage: u8) -> Value {
    call(
        "update_plan",
        json!({"plan":[
            {"step":"Read the project", "status":if stage == 0 {"in_progress"} else {"completed"}},
            {"step":"Implement the change and tests", "status":if stage == 0 {"pending"} else {"completed"}},
            {"step":"Run tests and finish", "status":match stage {0=>"pending",1=>"in_progress",_=>"completed"}}
        ]}),
    )
}
pub fn enqueue(script: &Arc<Mutex<Script>>, scenario: &str, enabled: bool) -> Result<()> {
    let mut replies = Vec::new();
    if enabled && matches!(scenario, "A" | "B") {
        replies.push(plan(0));
    }
    match scenario {
        "A" => {
            let source = include_str!("data/A/src/lib.rs");
            let implementation = format!(
                "{source}\npub fn count_inclusive(start: u32, end: u32) -> u64 {{\n    if start > end {{ 0 }} else {{ u64::from(end - start) }}\n}}\n\n#[test]\nfn counts_ranges() {{\n    assert_eq!(count_inclusive(2, 4), 3);\n    assert_eq!(count_inclusive(7, 7), 1);\n    assert_eq!(count_inclusive(9, 2), 0);\n    assert_eq!(count_inclusive(0, u32::MAX), 4294967296);\n}}\n"
            );
            replies.extend([
                call("read", json!({"path":"src/lib.rs"})),
                call("write", json!({"path":"src/lib.rs","content":implementation})),
                call("shell", json!({"command":"cargo test"})),
                call("edit", json!({"path":"src/lib.rs","edits":[{"oldText":"u64::from(end - start)","newText":"u64::from(end) - u64::from(start) + 1"}]})),
            ]);
            if enabled {
                replies.push(plan(1));
            }
            replies.push(call("shell", json!({"command":"cargo test"})));
        }
        "B" => {
            let paths = [
                "src/lib.rs",
                "src/one.rs",
                "src/two.rs",
                "tests/integration.rs",
            ];
            replies.push(calls(
                paths
                    .iter()
                    .map(|path| ("read", json!({"path":path})))
                    .collect(),
            ));
            let mut edits = Vec::new();
            for path in paths {
                let source = fs::read_to_string(format!("{DATA}/B/{path}"))?;
                edits.push(("edit", json!({"path":path,"edits":[{"oldText":source,"newText":source.replace("old_name","greet")}]})));
            }
            replies.push(calls(edits));
            replies.push(call("shell", json!({"command":"cargo test"})));
        }
        "C" => replies.push(call("read", json!({"path":"src/format.rs"}))),
        "D" => {}
        "Stop" => replies.push(call("shell", json!({"command":"sleep 30"}))),
        _ => unreachable!(),
    }
    if enabled && matches!(scenario, "A" | "B") {
        replies.push(plan(2));
    }
    if scenario != "Stop" {
        replies.push(text(if scenario == "D" {
            "收到"
        } else {
            "假模型场景完成。"
        }));
    }
    let mut script = script.lock().unwrap();
    assert!(script.replies.is_empty());
    script.replies.extend(replies);
    script.enabled = enabled;
    script.scenario = scenario.into();
    Ok(())
}
