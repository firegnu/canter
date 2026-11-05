use canter_model::{AssistantContent, Message, ToolCall};

#[test]
fn source_only_marker_uses_existing_history_format_and_never_matches_an_account() {
    // This fixture uses only fields already readable before P2-207 (and omits
    // account, as P2-204 records did). Also run this test against the base commit.
    let value = serde_json::json!({
        "type":"assistant","content":[
            {"type":"reasoning","text":"Partial summary"},
            {"type":"text","text":"Partial answer"}
        ],
        "replay":{"api":"responses","model":"test-model","scope":"turn-1","output":[]}
    });
    let message: Message = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&message).unwrap(), value);
    let Message::Assistant {
        replay: Some(replay),
        ..
    } = message
    else {
        panic!("missing origin")
    };
    for model in ["test-model", "other-model", ""] {
        for account in ["test-account-fingerprint", "other-account", ""] {
            assert!(!replay.belongs_to(model, account));
        }
    }
}

#[test]
fn private_responses_output_roundtrips_without_debug_disclosure() {
    let value = serde_json::json!({
        "type":"assistant","content":[{"type":"text","text":"Checking"}],
        "replay":{"api":"responses","model":"test-model","scope":"turn-1","output":[
            {"type":"reasoning","id":"r1","summary":[],"encrypted_content":"test-only-encrypted-history"},
            {"type":"message","id":"m1","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"Checking","annotations":[]}]},
            {"type":"function_call","id":"fc1","call_id":"call1","namespace":"canter","name":"read","arguments":"{ \"path\":\"a\" }"}
        ]}
    });
    let message: Message = serde_json::from_value(value.clone()).unwrap();
    assert!(serde_json::to_value(&message).unwrap() == value);
    assert!(!format!("{message:?}").contains("test-only-encrypted-history"));
}

#[test]
fn history_preserves_raw_content_and_reads_unknown_kinds_without_writing_them() {
    let history = r#"[
        {"type":"system","text":"Be helpful."},
        {"type":"user","text":"Read the file."},
        {"type":"future_message","payload":{"nested":[1,2]}},
        {"type":"assistant","content":[
            {"type":"reasoning","text":"  Think\n"},
            {"type":"future_content","payload":true},
            {"type":"text","text":"Reading."},
            {"type":"tool_call","id":"call_a","name":"read","arguments":"{broken: ["}
        ]},
        {"type":"tool_result","tool_call_id":"call_a","text":"Invalid arguments."}
    ]"#;
    let messages: Vec<Message> = serde_json::from_str(history).unwrap();
    assert_eq!(messages[2], Message::Unknown);
    assert_eq!(
        messages[3],
        Message::Assistant {
            replay: None,
            content: vec![
                AssistantContent::Reasoning {
                    text: "  Think\n".into()
                },
                AssistantContent::Unknown,
                AssistantContent::Text {
                    text: "Reading.".into()
                },
                AssistantContent::ToolCall(ToolCall {
                    id: "call_a".into(),
                    name: "read".into(),
                    arguments: "{broken: [".into()
                }),
            ]
        }
    );
    assert!(serde_json::to_string(&messages[2]).is_err());
    assert!(serde_json::to_string(&messages[3]).is_err());
    assert!(serde_json::to_string(&messages).is_err());
}

#[test]
fn known_history_ignores_new_fields_and_preserves_existing_fields_on_roundtrip() {
    let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
        {"type":"system","text":"System","new_field":true},
        {"type":"user","text":"User","new_field":{}},
        {"type":"assistant","new_field":[],"content":[
            {"type":"reasoning","text":"  Think\n","new_field":1},
            {"type":"text","text":"Reading.","new_field":2},
            {"type":"tool_call","id":"call_a","name":"read","arguments":"{broken: [","new_field":3}
        ]},
        {"type":"tool_result","tool_call_id":"call_a","text":"Invalid arguments.","new_field":null}
    ]))
    .unwrap();
    let saved = serde_json::to_value(&messages).unwrap();
    assert_eq!(
        saved,
        serde_json::json!([
            {"type":"system","text":"System"},
            {"type":"user","text":"User"},
            {"type":"assistant","content":[
                {"type":"reasoning","text":"  Think\n"},
                {"type":"text","text":"Reading."},
                {"type":"tool_call","id":"call_a","name":"read","arguments":"{broken: ["}
            ]},
            {"type":"tool_result","tool_call_id":"call_a","text":"Invalid arguments."}
        ])
    );
    assert_eq!(
        serde_json::from_value::<Vec<Message>>(saved).unwrap(),
        messages
    );
}
