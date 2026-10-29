//! Chat Completions wire types stay inside this adapter.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AssistantContent, Message, Request, ToolCall, Usage, UserContent};

pub(crate) fn request_body(request: &Request) -> Value {
    let messages = normalize_tool_pairs(request.messages.iter().filter_map(message).collect());
    let mut body = json!({
        "model": request.model, "messages": messages,
        "stream": true, "stream_options": {"include_usage": true}
    });
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(|tool| json!({
            "type": "function", "function": {
                "name": tool.name, "description": tool.description, "parameters": tool.parameters
            }
        })).collect();
    }
    if let Some(thinking) = request.options.thinking {
        body["thinking"] = json!({"type": if thinking { "enabled" } else { "disabled" }});
    }
    if let Some(limit) = request.options.max_output_tokens {
        body["max_tokens"] = json!(limit);
    }
    if request.options.thinking != Some(false)
        && let Some(effort) = &request.options.reasoning_effort
    {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

fn normalize_tool_pairs(messages: Vec<Value>) -> Vec<Value> {
    let mut messages = messages.into_iter().peekable();
    let mut normalized = Vec::new();
    while let Some(message) = messages.next() {
        if message["role"] == "tool" {
            continue;
        }
        let ids: Vec<_> = message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|call| call["id"].as_str())
            .map(str::to_owned)
            .collect();
        let mut missing: HashSet<_> = ids.iter().map(String::as_str).collect();
        normalized.push(message);
        // Tool results must immediately follow their assistant's call batch.
        // Unknown messages were already skipped; keep only matching results.
        while let Some(result) = messages.next_if(|message| message["role"] == "tool") {
            if result["tool_call_id"]
                .as_str()
                .is_some_and(|id| missing.remove(id))
            {
                normalized.push(result);
            }
        }
        for id in &ids {
            if missing.remove(id.as_str()) {
                normalized.push(json!({
                    "role": "tool", "tool_call_id": id,
                    "content": "No result: interrupted, or not understood by this version"
                }));
            }
        }
    }
    normalized
}

fn message(message: &Message) -> Option<Value> {
    Some(match message {
        Message::System { text } => json!({"role": "system", "content": text}),
        Message::User { text } => json!({"role": "user", "content": text}),
        Message::UserParts { content } | Message::ToolImages { content } => {
            json!({"role": "user", "content": content.iter().map(|part| match part {
            UserContent::Text { text } => json!({"type": "text", "text": text}),
            UserContent::Image { data_url, .. } => json!({"type": "image_url", "image_url": {"url": data_url}}),
        }).collect::<Vec<_>>()})
        }
        Message::ToolResult { tool_call_id, text } => {
            json!({"role": "tool", "tool_call_id": tool_call_id, "content": text})
        }
        Message::Assistant { content } => {
            let mut text = String::new();
            let mut reasoning = None::<String>;
            let mut tools = Vec::new();
            if content
                .iter()
                .all(|part| matches!(part, AssistantContent::Unknown))
            {
                return None;
            }
            for part in content {
                match part {
                    AssistantContent::Text { text: delta } => text.push_str(delta),
                    AssistantContent::Reasoning { text } => {
                        reasoning.get_or_insert_default().push_str(text)
                    }
                    AssistantContent::ToolCall(call) => tools.push(tool_call(call)),
                    AssistantContent::Unknown => {}
                }
            }
            let mut message = json!({"role": "assistant", "content": text});
            // DeepSeek requires the full history's reasoning when tools are present.
            // Without tools the provider ignores this field, so preserving it is safe.
            if let Some(reasoning) = reasoning {
                message["reasoning_content"] = json!(reasoning);
            }
            if !tools.is_empty() {
                message["tool_calls"] = json!(tools);
            }
            message
        }
        Message::Unknown => return None,
    })
}

fn tool_call(call: &ToolCall) -> Value {
    json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}})
}

#[derive(Deserialize)]
pub(crate) struct Chunk {
    pub choices: Vec<Choice>,
    pub usage: Option<WireUsage>,
}

#[derive(Deserialize)]
pub(crate) struct Choice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Delta {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<CallDelta>>,
}

#[derive(Deserialize)]
pub(crate) struct CallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
pub(crate) struct FunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct WireUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_cache_hit_tokens: Option<u64>,
    prompt_cache_miss_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptDetails>,
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct CompletionDetails {
    reasoning_tokens: Option<u64>,
}

impl From<WireUsage> for Usage {
    fn from(usage: WireUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cache_read_tokens: usage.prompt_cache_hit_tokens.or_else(|| {
                usage
                    .prompt_tokens_details
                    .and_then(|details| details.cached_tokens)
            }),
            cache_miss_tokens: usage.prompt_cache_miss_tokens,
            reasoning_tokens: usage
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens),
        }
    }
}
