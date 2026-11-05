use super::*;

use crate::model_test_server as chat;

const SUMMARY: &str = "Summary from Responses only";
const CIPHER: &str = "private-switch-ciphertext";
const PATCH: &str = "*** Begin Patch\n*** Add File: switched.txt\n+created\n*** End Patch";

fn answer(text: &str) -> (u16, String) {
    completed(
        json!([{"type":"message","id":"answer","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":text,"annotations":[]}]}]),
    )
}

fn patch_turn() -> (u16, String) {
    completed(json!([
        {"type":"reasoning","id":"reason","summary":[{"type":"summary_text","text":SUMMARY}],"encrypted_content":CIPHER},
        {"type":"message","id":"before-patch","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Editing now","annotations":[]}]},
        {"type":"custom_tool_call","id":"patch-item","call_id":"patch-call","namespace":"canter","name":"apply_patch","input":PATCH,"status":"completed"},
        {"type":"function_call","id":"shell-item","call_id":"shell-call","namespace":"canter","name":"shell","arguments":"{\"command\":\"printf ordinary-output\"}","status":"completed"}
    ]))
}

fn chat_answer(text: &str) -> chat::Reply {
    chat::Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{
            "index":0,"delta":{"content":text},"finish_reason":"stop"
        }]})
    ))
}

struct Fixture {
    root: tempfile::TempDir,
    home: std::path::PathBuf,
    fake: Fake,
    auth: ChatgptAuth,
    config: Config,
    options: canter_core::CoreOptions,
    runtime: RuntimeContext,
    store: Arc<canter_store::Store>,
}

impl Fixture {
    async fn new(inference: &Inference, chat: &chat::Server) -> (Self, Session) {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let home = root.path().join("state");
        let fake = Fake::new().await;
        let (auth, _) = login(&home, &fake, true).await;
        std::fs::write(home.join("config.json"), json!({
            "model":"responses/patch-model","prompt_suggestions":false,"sandbox":false,"model_max_retries":0,
            "providers":{
                "responses":{"api":"responses","auth":"chatgpt","models":{
                    "patch-model":{"context_window":200000},"plain-model":{"context_window":200000}
                }},
                "chat":{"api":"chat_completions","base_url":chat.base_url,"api_key":chat::KEY,"models":{"chat-model":{"context_window":200000}}}
            }
        }).to_string()).unwrap();
        let config = Config::load(&home).unwrap();
        let source = ModelAuth::for_test(home.clone(), auth.clone(), inference.endpoint.clone());
        source.catalog().account_changed(auth.clone()).await;
        let options = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, options) = source.catalog().apply(
                    config.catalog_provider(),
                    config.models.clone(),
                    config.options(),
                );
                if options.apply_patch_models.contains("responses/patch-model") {
                    break options;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let model = config.model_client(Some(source)).unwrap();
        let store = Arc::new(canter_store::Store::new(home.join("sessions")).unwrap());
        let runtime = RuntimeContext {
            environment: BTreeMap::from([
                (
                    "HOME".into(),
                    root.path().join("home").to_string_lossy().into_owned(),
                ),
                ("CANTER_HOME".into(), home.to_string_lossy().into_owned()),
            ]),
            ..Default::default()
        };
        let settings = serde_json::from_value(json!({"cwd":root.path(),"model":"responses/patch-model","approvalPolicy":"allowAll","reasoningEffort":"high"})).unwrap();
        let (session, _) = Session::create(
            store.clone(),
            "switch".into(),
            settings,
            runtime.clone(),
            model,
            options.clone(),
        )
        .await
        .unwrap();
        (
            Self {
                root,
                home,
                fake,
                auth,
                config,
                options,
                runtime,
                store,
            },
            session,
        )
    }

    fn model(&self, inference: &Inference) -> Arc<dyn canter_model::Model> {
        self.config
            .model_client(Some(ModelAuth::for_test(
                self.home.clone(),
                self.auth.clone(),
                inference.endpoint.clone(),
            )))
            .unwrap()
    }

    async fn reopen(&self, inference: &Inference) -> Session {
        let store = Arc::new(canter_store::Store::new(self.home.join("sessions")).unwrap());
        Session::open(
            store,
            "switch".into(),
            self.runtime.clone(),
            self.model(inference),
            self.options.clone(),
        )
        .await
        .unwrap()
        .0
    }
}

fn catalog() -> Value {
    json!({"models":[
        {"slug":"patch-model","visibility":"list","apply_patch_tool_type":"freeform"},
        {"slug":"plain-model","visibility":"list","apply_patch_tool_type":"function"}
    ]})
}

async fn switch(session: &Session, model: &str) {
    session
        .update(serde_json::from_value(json!({"model":model})).unwrap())
        .await
        .unwrap();
}

async fn turn(session: &Session, events: &mut canter_core::Subscription) -> Vec<Event> {
    session.input(input()).await.unwrap();
    let seen = finished(events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    seen
}

fn assert_chat_history(body: &Value) {
    assert!(!body.to_string().contains(SUMMARY));
    assert!(!body.to_string().contains(CIPHER));
    let messages = body["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| {
        message["role"] == "assistant"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains(PATCH) && text.contains("Historical apply_patch"))
    }));
    assert!(
        messages.iter().any(
            |message| message["content"].as_str().is_some_and(|text| text
                .contains("Historical tool output")
                && text.contains("switched.txt"))
        )
    );
    assert!(
        messages
            .iter()
            .all(|message| message["tool_call_id"] != "patch-call"
                && message["tool_calls"].as_array().is_none_or(|calls| calls
                    .iter()
                    .all(|call| call["function"]["name"] != "apply_patch")))
    );
    let assistant = messages
        .iter()
        .find(|message| {
            message["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.iter().any(|call| call["id"] == "shell-call"))
        })
        .unwrap();
    assert_eq!(
        assistant["tool_calls"],
        json!([{"id":"shell-call","type":"function","function":{"name":"shell","arguments":"{\"command\":\"printf ordinary-output\"}"}}])
    );
    // Every remaining function batch is followed by exactly its own results.
    for (index, message) in messages.iter().enumerate() {
        if let Some(calls) = message["tool_calls"].as_array() {
            let results = &messages[index + 1..index + 1 + calls.len()];
            for call in calls {
                assert!(
                    results
                        .iter()
                        .any(|result| result["role"] == "tool"
                            && result["tool_call_id"] == call["id"])
                );
            }
        }
    }
}

#[tokio::test]
async fn model_switch_translates_history_and_preserves_replay_after_reopen() {
    let mut inference = Inference::with_catalog(
        vec![
            patch_turn(),
            answer("Patch done"),
            answer("Back on Responses"),
            answer("Plain model"),
        ],
        catalog(),
    )
    .await;
    let chat = chat::Server::start(vec![
        chat::Reply::sse(format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{
            "reasoning_content":"Chat native reasoning","tool_calls":[{"index":0,"id":"chat-shell","function":{"name":"shell","arguments":"{\"command\":\"printf chat-output\"}"}}]
        },"finish_reason":"tool_calls"}]}))),
        chat_answer("Chat done"), chat_answer("Chat again"),
        chat_answer("Side answer"), chat_answer("Side answer"), chat_answer("A complete summary"),
    ]).await;
    let (fixture, session) = Fixture::new(&inference, &chat).await;
    let log = tempfile::NamedTempFile::new_in(fixture.root.path()).unwrap();
    let writer = log.reopen().unwrap();
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.try_clone().unwrap())
            .finish(),
    );
    let mut events = session.subscribe(0).await.unwrap();
    let mut public = serde_json::to_string(&turn(&session, &mut events).await).unwrap();
    let (_, token) = inference.next().await;
    let (native, _) = inference.next().await;
    assert!(native.to_string().contains(CIPHER));
    assert!(
        native["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "custom_tool_call")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("switched.txt")).unwrap(),
        "created\n"
    );

    switch(&session, "chat/chat-model").await;
    public.push_str(&serde_json::to_string(&turn(&session, &mut events).await).unwrap());
    {
        let bodies = chat.requests.lock().unwrap();
        assert_chat_history(&bodies[0]);
        assert_chat_history(&bodies[1]);
        assert!(bodies[1]["messages"].as_array().unwrap().iter().any(
            |message| message["reasoning_content"] == "Chat native reasoning"
                && message["tool_calls"][0]["id"] == "chat-shell"
        ));
    }
    switch(&session, "responses/patch-model").await;
    public.push_str(&serde_json::to_string(&turn(&session, &mut events).await).unwrap());
    let (back, _) = inference.next().await;
    assert!(back.to_string().contains(CIPHER));
    assert!(!back.to_string().contains("Chat native reasoning"));
    for item in native["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item.get("id").is_some() || item["type"] == "custom_tool_call_output")
    {
        assert!(back["input"].as_array().unwrap().contains(item));
    }
    switch(&session, "chat/chat-model").await;
    public.push_str(&serde_json::to_string(&turn(&session, &mut events).await).unwrap());
    assert_chat_history(&chat.requests.lock().unwrap()[2]);
    let mut side = session.btw("Repeatable question".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    session.close().await.unwrap();
    drop(session);
    let session = fixture.reopen(&inference).await;
    let mut side = session.btw("Repeatable question".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    {
        let bodies = chat.bodies.lock().unwrap();
        assert_eq!(bodies[3], bodies[4]);
    }
    switch(&session, "responses/plain-model").await;
    let mut events = session
        .subscribe(session.snapshot().await.session.last_event_seq)
        .await
        .unwrap();
    public.push_str(&serde_json::to_string(&turn(&session, &mut events).await).unwrap());
    let (plain, _) = inference.next().await;
    assert!(!plain.to_string().contains(CIPHER));
    assert!(
        plain["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != "apply_patch" && item["call_id"] != "patch-call")
    );
    assert!(plain.to_string().contains("Historical apply_patch"));
    assert!(plain.to_string().contains("Historical tool output"));
    switch(&session, "chat/chat-model").await;
    session.compact().await.unwrap();
    public.push_str(&serde_json::to_string(&finished(&mut events).await).unwrap());
    assert_chat_history(&chat.requests.lock().unwrap()[5]);
    public.push_str(&std::fs::read_to_string(log.path()).unwrap());
    assert!(
        !public.contains(CIPHER)
            && !public.contains(token.strip_prefix("Bearer ").unwrap())
            && !public.contains(chat::KEY)
    );
    session.close().await.unwrap();
    fixture.fake.stop().await;
    chat.stop().await;
}

#[tokio::test]
async fn rejected_responses_summary_stays_out_of_chat_after_restore_and_fork() {
    let mut inference = Inference::with_catalog(vec![
        patch_turn(),
        (400, json!({"error":{"code":"invalid_encrypted_content","message":format!("{CIPHER} private-rejection-detail")}}).to_string()),
        answer("Recovered without replay"),
    ], catalog()).await;
    let chat = chat::Server::start((0..3).map(|_| chat_answer("Side answer")).collect()).await;
    let (fixture, session) = Fixture::new(&inference, &chat).await;
    let mut events = session.subscribe(0).await.unwrap();
    let public = serde_json::to_string(&turn(&session, &mut events).await).unwrap();
    let (_, token) = inference.next().await;
    let (rejected, _) = inference.next().await;
    let (fallback, _) = inference.next().await;
    assert!(rejected.to_string().contains(CIPHER));
    assert!(!fallback.to_string().contains(CIPHER));
    assert!(inference.requests.try_recv().is_err());
    assert!(
        !public.contains(CIPHER)
            && !public.contains("private-rejection-detail")
            && !public.contains(token.strip_prefix("Bearer ").unwrap())
    );
    let path = fixture.home.join("sessions/switch/model-history.jsonl");
    let original = std::fs::read_to_string(&path).unwrap();
    assert!(
        original.contains(SUMMARY)
            && original.contains(CIPHER)
            && original.contains("replay_rejected")
    );
    switch(&session, "chat/chat-model").await;
    let mut side = session.btw("After rejection".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    assert_chat_history(&chat.requests.lock().unwrap()[0]);
    session.close().await.unwrap();
    drop(session);

    let session = fixture.reopen(&inference).await;
    let mut side = session.btw("After rejection".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    assert_chat_history(&chat.requests.lock().unwrap()[1]);
    {
        let bodies = chat.bodies.lock().unwrap();
        assert_eq!(bodies[0], bodies[1]);
    }
    let (fork, _) = session
        .fork_snapshot()
        .await
        .unwrap()
        .create(
            fixture.store.clone(),
            "fork-switch".into(),
            fixture.runtime.clone(),
            fixture.model(&inference),
            fixture.options.clone(),
        )
        .await
        .unwrap();
    let mut side = fork.btw("After rejection".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    assert_chat_history(&chat.requests.lock().unwrap()[2]);
    let fork_history = std::fs::read_to_string(
        fixture
            .home
            .join("sessions/fork-switch/model-history.jsonl"),
    )
    .unwrap();
    assert!(
        fork_history.contains(SUMMARY)
            && fork_history.contains(CIPHER)
            && fork_history.contains("replay_rejected")
    );
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .starts_with(&original)
    );
    fork.close().await.unwrap();
    session.close().await.unwrap();
    fixture.fake.stop().await;
    chat.stop().await;
}

async fn incomplete_origin_survives_switch(ending: &str) {
    let mut partial: String = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"partial-reasoning","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"partial-reasoning","summary_index":0,"delta":SUMMARY}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"partial-message","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":1,"item_id":"partial-message","content_index":0,"delta":"Partial answer"}),
    ].into_iter().map(|value| format!("data: {value}\n\n")).collect();
    if ending == "error" {
        partial.push_str(&format!("data: {}\n\n", json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"private-terminal-error"}}})));
    }
    let mut inference =
        Inference::with_streaming_catalog(vec![(200, partial)], catalog(), ending == "esc").await;
    let chat =
        chat::Server::start(vec![chat_answer("Side answer"), chat_answer("Side answer")]).await;
    let (fixture, session) = Fixture::new(&inference, &chat).await;
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session.input(input()).await.unwrap().turn_id.unwrap();
    inference.next().await;
    if ending == "esc" {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.unwrap().unwrap();
                if matches!(&event.payload, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { delta }, .. } if delta == "Partial answer") {
                    break;
                }
            }
        }).await.unwrap();
        session
            .interrupt(serde_json::from_value(json!({"turnId":turn})).unwrap())
            .await
            .unwrap();
    }
    let public = serde_json::to_string(&finished(&mut events).await).unwrap();
    assert!(!public.contains("private-terminal-error"));
    let saved =
        std::fs::read_to_string(fixture.home.join("sessions/switch/model-history.jsonl")).unwrap();
    let message = saved
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry["type"] == "message" && entry["message"]["type"] == "assistant")
        .unwrap()["message"]
        .clone();
    assert_eq!(
        message["replay"],
        json!({"api":"responses","model":"patch-model","scope":turn,"output":[]})
    );
    assert!(
        message["content"]
            .as_array()
            .unwrap()
            .contains(&json!({"type":"reasoning","text":SUMMARY}))
    );
    assert!(
        message["content"]
            .as_array()
            .unwrap()
            .contains(&json!({"type":"text","text":"Partial answer"}))
    );
    switch(&session, "chat/chat-model").await;
    let mut side = session.btw("Continue".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    session.close().await.unwrap();
    drop(session);
    let session = fixture.reopen(&inference).await;
    let mut side = session.btw("Continue".into()).await.unwrap();
    super::replay::side_finished(&mut side).await;
    {
        let bodies = chat.bodies.lock().unwrap();
        assert_eq!(bodies[0], bodies[1]);
        let text = std::str::from_utf8(&bodies[0]).unwrap();
        assert!(!text.contains(SUMMARY));
        assert!(text.contains("Partial answer"));
    }
    session.close().await.unwrap();
    fixture.fake.stop().await;
    chat.stop().await;
}

#[tokio::test]
async fn interrupted_responses_keep_origin_without_replay() {
    incomplete_origin_survives_switch("esc").await;
}

#[tokio::test]
async fn disconnected_responses_keep_origin_without_replay() {
    incomplete_origin_survives_switch("eof").await;
}

#[tokio::test]
async fn failed_responses_keep_origin_without_replay() {
    incomplete_origin_survives_switch("error").await;
}
