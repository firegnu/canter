use canter_model::{AssistantContent, Message, ToolCall};

#[test]
fn history_preserves_raw_content_and_skips_unknown_kinds() {
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
    let saved = serde_json::to_string(&messages).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<Message>>(&saved).unwrap(),
        messages
    );
}
