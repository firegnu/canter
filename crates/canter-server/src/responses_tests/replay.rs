use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    home: std::path::PathBuf,
    fake: Fake,
    auth: ChatgptAuth,
    account: String,
}

fn answer(text: &str, ciphertext: &str) -> Value {
    json!([
        {"type":"reasoning","id":"reasoning-replay","summary":[],"encrypted_content":ciphertext},
        {"type":"message","id":"message-replay","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":text,"annotations":[]}]}
    ])
}

fn message(text: &str) -> Value {
    json!([{"type":"message","id":"message-reply","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]}])
}

async fn fixture(inference: &Inference, suggestions: bool, window: u64) -> (Fixture, Session) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, account) = login(&home, &fake, true).await;
    config(&home);
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    value["prompt_suggestions"] = json!(suggestions);
    value["providers"]["chatgpt"]["models"]["test-model"]["context_window"] = json!(window);
    std::fs::write(home.join("config.json"), value.to_string()).unwrap();
    let config = Config::load(&home).unwrap();
    let model = config
        .model_client(Some(ModelAuth::for_test(
            home.clone(),
            auth.clone(),
            inference.endpoint.clone(),
        )))
        .unwrap();
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
    let settings: SessionSettings = serde_json::from_value(json!({
        "cwd":root.path(),"model":"chatgpt/test-model","approvalPolicy":"allowAll","reasoningEffort":"low"
    }))
    .unwrap();
    let id = format!("replay-{}", std::process::id());
    let mut options = config.options();
    if window == 81921 {
        options.max_output_tokens = 40000;
    }
    let (session, _) = Session::create(
        store.clone(),
        id,
        settings,
        runtime.clone(),
        model.clone(),
        options,
    )
    .await
    .unwrap();
    (
        Fixture {
            _root: root,
            home,
            fake,
            auth,
            account,
        },
        session,
    )
}

async fn side_finished(stream: &mut canter_core::BtwStream) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = stream.recv().await {
            match event {
                BtwEvent::Completed => return,
                BtwEvent::Error { message } => panic!("side request failed: {message}"),
                _ => {}
            }
        }
        panic!("side request ended without completion");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn prefix_compaction_replays_native_history_then_drops_it() {
    let ciphertext = "private-prefix-ciphertext";
    let mut inference = Inference::new(vec![
        completed(answer("First answer", ciphertext)),
        completed(message("A complete summary.")),
        completed(message("After compaction")),
    ])
    .await;
    let (fixture, session) = fixture(&inference, false, 200000).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    let (first, _) = inference.next().await;
    finished(&mut events).await;

    session.compact().await.unwrap();
    let (compaction, _) = inference.next().await;
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::TelemetryCompaction { method:Some(method), .. } if method == "prefix")));
    let first_input = first["input"].as_array().unwrap();
    let compaction_input = compaction["input"].as_array().unwrap();
    assert_eq!(&compaction_input[..first_input.len()], first_input);
    assert!(compaction.to_string().contains(ciphertext));

    session.input(input()).await.unwrap();
    let (after, _) = inference.next().await;
    assert!(!after.to_string().contains(ciphertext));
    finished(&mut events).await;
    session.close().await.unwrap();
    fixture.fake.stop().await;
}

#[tokio::test]
async fn transcript_compaction_uses_only_portable_text() {
    let ciphertext = "private-transcript-ciphertext";
    let mut output = vec![
        json!({"type":"reasoning","id":"reasoning-replay","summary":[],"encrypted_content":ciphertext}),
    ];
    output.extend((0..3).map(|index| json!({
        "type":"function_call","id":format!("call-item-{index}"),"call_id":format!("call-{index}"),
        "namespace":"canter","name":"shell","arguments":json!({"command":"head -c 120000 /dev/zero | tr '\\0' 'a'"}).to_string(),"status":"completed"
    })));
    let mut inference = Inference::new(vec![
        completed(Value::Array(output)),
        completed(message("A complete transcript summary.")),
        completed(message("After transcript compaction")),
    ])
    .await;
    let (fixture, session) = fixture(&inference, false, 81921).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    inference.next().await;
    let (compaction, _) = inference.next().await;
    assert!(!compaction.to_string().contains(ciphertext));
    assert!(compaction.to_string().contains("<transcript>"));
    assert!(compaction["input"].as_array().unwrap().iter().all(|item| {
        item.get("role").is_some() && item["type"] != "reasoning" && item["type"] != "function_call"
    }));
    let (after, _) = inference.next().await;
    assert!(!after.to_string().contains(ciphertext));
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::TelemetryCompaction { method:Some(method), .. } if method == "transcript")));
    session.close().await.unwrap();
    fixture.fake.stop().await;
}

#[tokio::test]
async fn suggestions_btw_and_handoff_follow_model_and_account_scope() {
    let ciphertext = "private-auxiliary-ciphertext";
    let mut inference = Inference::new(vec![
        completed(answer("Main answer", ciphertext)),
        completed(message("Suggest the next step")),
        completed(message("Private answer")),
        completed(message("## Task\nContinue.")),
        completed(message("Other account answer")),
    ])
    .await;
    let (fixture, session) = fixture(&inference, true, 200000).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    inference.next().await;
    finished(&mut events).await;

    let (suggestion, _) = inference.next().await;
    assert!(suggestion.to_string().contains(ciphertext));
    let mut btw = session.btw("One private question".into()).await.unwrap();
    let (btw_request, _) = inference.next().await;
    assert!(btw_request.to_string().contains(ciphertext));
    side_finished(&mut btw).await;
    let mut handoff = session.handoff(None).await.unwrap();
    let (handoff_request, _) = inference.next().await;
    assert!(handoff_request.to_string().contains(ciphertext));
    side_finished(&mut handoff).await;

    fixture.fake.state.lock().unwrap().client_id = "second-test-client".into();
    let login = fixture.auth.start_login(None).await.unwrap();
    assert!(fixture.fake.callback(&login).await.status().is_success());
    let second = login.wait().await.unwrap();
    fixture.auth.complete_login(&second.id, true).await.unwrap();
    let mut other = session.btw("Another account".into()).await.unwrap();
    let (other_request, _) = inference.next().await;
    assert!(!other_request.to_string().contains(ciphertext));
    side_finished(&mut other).await;

    let saved = std::fs::read_to_string(fixture.home.join(format!(
        "sessions/{}/model-history.jsonl",
        session.snapshot().await.session.id
    )))
    .unwrap();
    assert!(!saved.contains(&fixture.account) && !saved.contains(&second.id));
    session.close().await.unwrap();
    fixture.fake.stop().await;
}
