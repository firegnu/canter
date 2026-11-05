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
    let mut replaying_batch = false;
    for (message, historical_output) in crate::history::messages_with_patch_outputs(request) {
        if let Message::Assistant { replay, .. } = message {
            replaying_batch = allow_replay
                && replay
                    .as_ref()
                    .is_some_and(|replay| replay.belongs_to(&request.model, account));
        }
        // Keep P2-210's native replay and its results together, even when an
        // auxiliary/read-only request has no current patch tool definition.
        append_input(
            &mut input,
            message,
            request,
            account,
            allow_replay,
            historical_output && !replaying_batch,
        );
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
        let tools: Vec<_> = request
            .tools
            .iter()
            .filter(|tool| namespace(&tool.name) == group)
            .map(|tool| {
                if let Some(grammar) = request.custom_tools.get(&tool.name) {
                    json!({
                        "type":"custom", "name":tool.name, "description":tool.description,
                        "format":{"type":"grammar","syntax":"lark","definition":grammar},
                    })
                } else {
                    json!({
                        "type":"function", "name":tool.name, "description":tool.description,
                        "parameters":tool.parameters, "strict":false,
                    })
                }
            })
            .collect();
        (!tools.is_empty()).then(
            || json!({"type":"namespace","name":group,"description":description,"tools":tools}),
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
    request: &Request,
    account: &str,
    allow_replay: bool,
    historical_output: bool,
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
                    .filter(|replay| replay.belongs_to(&request.model, account))
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
                    AssistantContent::ToolCall(call)
                        if call.name == "apply_patch"
                            && !request.custom_tools.contains_key("apply_patch") =>
                    {
                        input.push(json!({
                            "type":"message", "role":"assistant",
                            "content":[{"type":"output_text","text":crate::history::patch_call_text(call)}],
                        }));
                    }
                    AssistantContent::ToolCall(call)
                        if request.custom_tools.contains_key(&call.name) =>
                    {
                        input.push(json!({
                            "type":"custom_tool_call", "call_id":call.id,
                            "namespace":namespace(&call.name), "name":call.name,
                            "input":call.arguments,
                        }))
                    }
                    AssistantContent::ToolCall(call) => input.push(json!({
                        "type":"function_call", "call_id":call.id,
                        "namespace":namespace(&call.name), "name":call.name,
                        "arguments":call.arguments,
                    })),
                    AssistantContent::Reasoning { .. } | AssistantContent::Unknown => {}
                }
            }
        }
        Message::ToolResult { tool_call_id, text } if historical_output => {
            input.push(text_message(
                "user",
                &crate::history::patch_output_text(tool_call_id, text),
            ));
        }
        Message::ToolResult { tool_call_id, text } => {
            let custom = request.messages.iter().any(|message| {
                matches!(message, Message::Assistant { content, .. } if content.iter().any(|part| matches!(part, AssistantContent::ToolCall(call) if &call.id == tool_call_id && request.custom_tools.contains_key(&call.name))))
            });
            input.push(json!({
                "type":if custom { "custom_tool_call_output" } else { "function_call_output" },
                "call_id":tool_call_id, "output":text,
            }));
        }
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
                    Some("custom_tool_call") => (
                        &[
                            "type",
                            "id",
                            "call_id",
                            "namespace",
                            "name",
                            "input",
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
