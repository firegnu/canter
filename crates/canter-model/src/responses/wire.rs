use serde_json::{Value, json};

use crate::{AssistantContent, Message, Replay, Request, UserContent};

pub(super) fn namespace(name: &str) -> &'static str {
    if name.starts_with("mcp_") {
        "mcp"
    } else {
        "canter"
    }
}

/// SIWC accepts a smaller contract than the general Responses API.
pub(super) fn request_body(request: &Request, account: &str, allow_replay: bool) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        append_input(&mut input, message, &request.model, account, allow_replay);
    }
    let mut body = json!({
        "model":request.model, "input":input, "store":false, "stream":true,
        "reasoning":{"summary":"auto"},
    });
    if let Some(effort) = &request.options.reasoning_effort {
        body["reasoning"]["effort"] = json!(effort);
    }
    if let Some(session_id) = session_id(request) {
        body["prompt_cache_key"] = json!(session_id.chars().take(64).collect::<String>());
    }
    let tools: Vec<_> = [
        ("canter", "Tools in the canter namespace."),
        ("mcp", "Tools in the mcp namespace."),
    ]
    .into_iter()
    .filter_map(|(group, description)| {
        let functions: Vec<_> = request
            .tools
            .iter()
            .filter(|tool| namespace(&tool.name) == group)
            .map(|tool| {
                json!({
                    "type":"function", "name":tool.name, "description":tool.description,
                    "parameters":tool.parameters, "strict":false,
                })
            })
            .collect();
        (!functions.is_empty()).then(
            || json!({"type":"namespace","name":group,"description":description,"tools":functions}),
        )
    })
    .collect();
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    body
}

fn append_input(
    input: &mut Vec<Value>,
    message: &Message,
    model: &str,
    account: &str,
    allow_replay: bool,
) {
    match message {
        Message::System { text } => input.push(text_message("developer", text)),
        Message::User { text } => input.push(text_message("user", text)),
        Message::UserParts { content } | Message::ToolImages { content } => {
            input.push(json!({"role":"user", "content":content.iter().map(|part| match part {
                UserContent::Text { text } => json!({"type":"input_text","text":text}),
                UserContent::Image { data_url, .. } => json!({"type":"input_image","image_url":data_url}),
            }).collect::<Vec<_>>()}));
        }
        Message::Assistant { content, replay } => {
            if allow_replay
                && let Some(Replay::Responses { output, .. }) = replay
                    .as_ref()
                    .filter(|replay| replay.belongs_to(model, account))
            {
                input.extend(output.iter().cloned());
                return;
            }
            for part in content {
                match part {
                    AssistantContent::Text { text } => input.push(json!({
                        "type":"message", "role":"assistant",
                        "content":[{"type":"output_text","text":text}],
                    })),
                    AssistantContent::ToolCall(call) => input.push(json!({
                        "type":"function_call", "call_id":call.id,
                        "namespace":namespace(&call.name), "name":call.name,
                        "arguments":call.arguments,
                    })),
                    AssistantContent::Reasoning { .. } | AssistantContent::Unknown => {}
                }
            }
        }
        Message::ToolResult { tool_call_id, text } => input.push(json!({
            "type":"function_call_output", "call_id":tool_call_id, "output":text,
        })),
        Message::Unknown => {}
    }
}

/// The conversation id sent as the cache key and the `session-id` header.
pub(super) fn session_id(request: &Request) -> Option<&str> {
    request.session_id.as_deref().filter(|id| !id.is_empty())
}

pub(super) fn replay_scopes(request: &Request, account: &str) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant {
                replay: Some(replay),
                ..
            } if replay.belongs_to(&request.model, account) => {
                Some(replay.scope().unwrap_or_default().to_owned())
            }
            _ => None,
        })
        .collect()
}

pub(super) fn has_replay(request: &Request, account: &str) -> bool {
    request.messages.iter().any(|message| {
        matches!(message, Message::Assistant { replay:Some(replay), .. } if replay.belongs_to(&request.model, account))
    })
}

fn text_message(role: &str, text: &str) -> Value {
    json!({"role":role,"content":[{"type":"input_text","text":text}]})
}

/// Keep only replay fields after the whole output has passed stream validation.
pub(super) fn replay_output(output: &Value) -> Vec<Value> {
    output
        .as_array()
        .expect("validated output")
        .iter()
        .map(|item| {
            let (keys, parts, part_keys): (&[&str], Option<&str>, &[&str]) =
                match item["type"].as_str() {
                    Some("message") => (
                        &["type", "id", "role", "status", "phase", "content"],
                        Some("content"),
                        &["type", "text", "refusal", "annotations"],
                    ),
                    Some("reasoning") => (
                        &["type", "id", "status", "summary", "encrypted_content"],
                        Some("summary"),
                        &["type", "text"],
                    ),
                    Some("function_call") => (
                        &[
                            "type",
                            "id",
                            "call_id",
                            "namespace",
                            "name",
                            "arguments",
                            "status",
                        ],
                        None,
                        &[],
                    ),
                    _ => unreachable!("validated output"),
                };
            let mut saved = fields(item, keys);
            if let Some(parts) = parts {
                saved[parts] = item[parts]
                    .as_array()
                    .expect("validated content")
                    .iter()
                    .map(|part| fields(part, part_keys))
                    .collect();
            }
            saved
        })
        .collect()
}

fn fields(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}
