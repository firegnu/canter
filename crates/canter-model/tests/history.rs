use canter_model::{AssistantContent, Message, ToolCall};

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
