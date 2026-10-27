//! Local fixtures only. SSE replay follows crates/canter/tests/server.rs.
use super::{DATA, Result};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Default)]
pub struct Script {
    pub replies: VecDeque<Value>,
    pub requests: usize,
    pub compactions: usize,
}

pub async fn start(fake: bool) -> Result<(String, Arc<Mutex<Script>>, JoinHandle<()>)> {
    let state = Arc::new(Mutex::new(Script::default()));
    let mut router = Router::new().route(
        "/notes.html",
        get(|| async { axum::response::Html(include_str!("data/S4/notes.html")) }),
    );
    if fake {
        router = router.route("/chat/completions", post(reply));
    }
    let router = router.with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Ok((url, state, task))
}

async fn reply(
    State(state): State<Arc<Mutex<Script>>>,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let mut script = state.lock().unwrap();
    script.requests += 1;
    assert_eq!(body["model"], "deepseek-flash");
    let delta = if body
        .get("tools")
        .is_none_or(|t| t.as_array().is_some_and(Vec::is_empty))
    {
        script.compactions += 1;
        assert_eq!(body["thinking"]["type"], "disabled");
        text(
            "## Goal\n继续改名。\n## User's words\n把所有 .rs 文件里的 old_name 改名为 new_name。\n## Constraints and preferences\n运行 cargo test。\n## Decisions\n(none)\n## Files\n已读六个源文件。\n## Commands and results\n(none)\n## Where it stopped\n尚未改文件。\n## Open issues\n(none)\n## Next steps\n修改剩余文件并测试。",
        )
    } else {
        assert_eq!(body["reasoning_effort"], "high");
        script
            .replies
            .pop_front()
            .expect("unexpected fake model request")
    };
    let finish = if delta.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    let usage = json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,"prompt_cache_hit_tokens":50,"prompt_cache_miss_tokens":50}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n"),
    )
}

fn text(s: &str) -> Value {
    json!({"content":s})
}
fn calls(calls: Vec<(&str, Value)>) -> Value {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let batch = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    json!({"tool_calls":calls.into_iter().enumerate().map(|(i,(name,args))| json!({
        "index":i,"id":format!("call-{batch}-{i}"),"type":"function",
        "function":{"name":name,"arguments":args.to_string()}
    })).collect::<Vec<_>>()})
}
fn read(path: &str) -> Value {
    calls(vec![("read", json!({"path":path}))])
}
fn shell(command: &str) -> Value {
    calls(vec![("shell", json!({"command":command}))])
}
fn edit(path: &str, old: &str, new: &str) -> Value {
    calls(vec![(
        "edit",
        json!({"path":path,"edits":[{"oldText":old,"newText":new}]}),
    )])
}

pub fn enqueue(state: &Arc<Mutex<Script>>, scenario: u8, url: &str) -> Result<()> {
    let replies = match scenario {
        1 => vec![shell("ls /no-such-dir"), text("目录不存在，命令失败。")],
        2 => vec![
            read("src/greet.rs"),
            edit(
                "src/greet.rs",
                "format!(\"{}, {}!\", \"Hi\", name)",
                "String::from(\"Hello, canter\")",
            ),
            text("已修改问候语。"),
        ],
        3 => vec![
            shell("cargo test"),
            read("src/lib.rs"),
            edit("src/lib.rs", "end - start", "end - start + 1"),
            shell("cargo test"),
            text("修复了差一错误，cargo test 通过；改动未提交。"),
        ],
        4 => vec![
            calls(vec![(
                "web_fetch",
                json!({"url":format!("{url}/notes.html")}),
            )]),
            shell("touch /tmp/canter-p2-57-marker"),
            text("文章介绍 Result、问号运算符和错误信息。伪模型故意请求 shell，验证驱动拒绝流程。"),
        ],
        5 => vec![
            calls(
                ["Cargo.toml", "src/main.rs", "README.md"]
                    .into_iter()
                    .map(|path| ("read", json!({"path":path})))
                    .collect(),
            ),
            text("程序按命令行参数输出问候语，入口在 src/main.rs:2。"),
        ],
        6 => vec![
            read("README.md"),
            edit("README.md", "Version: 0.1.0", "Version: 0.2.0"),
            calls(vec![(
                "ask_user",
                json!({"question":"批准被拒，未修改。希望换什么做法？","options":[{"label":"保持原样","description":"不修改文件"},{"label":"说明原因","description":"说明拒绝原因后再决定"}]}),
            )]),
        ],
        7 => {
            let paths: Vec<String> = (0..6)
                .map(|n| {
                    if n == 0 {
                        "src/lib.rs".into()
                    } else {
                        format!("src/part{n}.rs")
                    }
                })
                .collect();
            let reads = calls(paths.iter().map(|p| ("read", json!({"path":p}))).collect());
            let mut edits = Vec::new();
            for path in &paths {
                let source = std::fs::read_to_string(format!("{DATA}/S7/{path}"))?;
                let changes: Vec<Value> = source
                    .lines()
                    .filter(|s| s.contains("old_name"))
                    .map(|s| json!({"oldText":s,"newText":s.replace("old_name","new_name")}))
                    .collect();
                edits.push(("edit", json!({"path":path,"edits":changes})));
            }
            vec![
                reads,
                calls(edits),
                shell("cargo test"),
                text("全部改名完成，测试通过。"),
            ]
        }
        8 => vec![
            read("src/math.rs"),
            edit(
                "src/math.rs",
                "    a + b\n}",
                "    a + b\n}\n\n#[test]\nfn adds_two_numbers() {\n    assert_eq!(add(2, 3), 5);\n}",
            ),
            shell("cargo test"),
            text("已加 add 的单元测试并运行通过，保留 sub 原样。"),
        ],
        9 => vec![text(
            "## Task\n继续修复计数项目。\n## Goal\n修好失败测试。\n## User's words\ncargo test 有一个测试挂了，修好它\n## Constraints and preferences\n(none)\n## Decisions\n补上端点。\n## Files\nsrc/lib.rs：差一错误。\n## Commands and results\ncargo test 通过。\n## Where it stopped\nsrc/lib.rs 修改未提交。\n## Open issues\n(none)\n## Next steps\n检查 git status 和测试结果。",
        )],
        _ => unreachable!(),
    };
    let mut script = state.lock().unwrap();
    assert!(script.replies.is_empty());
    script.replies.extend(replies);
    Ok(())
}
