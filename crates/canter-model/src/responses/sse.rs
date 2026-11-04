use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    AssistantContent, ErrorKind, FinishReason, ModelError, ModelEvent, ToolCall, ToolDefinition,
    Usage,
};

#[derive(Default)]
pub(super) struct State {
    items: BTreeMap<u64, Item>,
}

struct Item {
    id: String,
    kind: String,
    arguments: String,
}

pub(super) fn has_output(event: &Value) -> bool {
    match event["type"].as_str() {
        Some(
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta",
        ) => event["delta"].as_str().is_some_and(|text| !text.is_empty()),
        Some(
            "response.output_item.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.function_call_arguments.done",
        ) => true,
        Some("response.output_item.added") => {
            let item = &event["item"];
            ["content", "summary"]
                .iter()
                .any(|key| item[key].as_array().is_some_and(|parts| !parts.is_empty()))
                || ["arguments", "encrypted_content"]
                    .iter()
                    .any(|key| item[key].as_str().is_some_and(|text| !text.is_empty()))
        }
        _ => false,
    }
}

impl State {
    pub(super) fn accept(
        &mut self,
        event: &Value,
        tools: &[ToolDefinition],
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = Vec::new();
        match string(event, "type")? {
            "response.output_item.added" | "response.output_item.done" => {
                let index = number(event, "output_index")?;
                let item = &event["item"];
                let id = string(item, "id")?;
                let kind = output_kind(item)?;
                if let Some(previous) = self.items.get(&index) {
                    if previous.id != id || previous.kind != kind {
                        return Err(invalid());
                    }
                } else if self.items.values().any(|item| item.id == id) {
                    return Err(invalid());
                }
                self.items.insert(
                    index,
                    Item {
                        id: id.into(),
                        kind: kind.into(),
                        arguments: item["arguments"].as_str().unwrap_or_default().into(),
                    },
                );
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta" => {
                let summary = event["type"] == "response.reasoning_summary_text.delta";
                let item = self.item(event)?;
                if item.kind != if summary { "reasoning" } else { "message" } {
                    return Err(invalid());
                }
                let part = number(
                    event,
                    if summary {
                        "summary_index"
                    } else {
                        "content_index"
                    },
                )?;
                let id = format!("{}:{part}", item.id);
                let delta = string(event, "delta")?.to_owned();
                if !delta.is_empty() {
                    events.push(if summary {
                        ModelEvent::ReasoningDelta { item_id: id, delta }
                    } else {
                        ModelEvent::TextDelta { item_id: id, delta }
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let delta = string(event, "delta")?;
                let item = self.item(event)?;
                if item.kind != "function_call" {
                    return Err(invalid());
                }
                item.arguments.push_str(delta);
            }
            "response.function_call_arguments.done" => {
                let arguments = string(event, "arguments")?;
                let item = self.item(event)?;
                if item.kind != "function_call" {
                    return Err(invalid());
                }
                item.arguments = arguments.into();
            }
            "response.completed" => {
                let response = &event["response"];
                if response["status"] != "completed" {
                    return Err(invalid());
                }
                let output = response["output"].as_array().ok_or_else(invalid)?;
                for (index, item) in &self.items {
                    let index = usize::try_from(*index).map_err(|_| invalid())?;
                    if output.get(index).is_none_or(|final_item| {
                        final_item["id"] != item.id || final_item["type"] != item.kind
                    }) {
                        return Err(invalid());
                    }
                }
                let mut ids = HashSet::new();
                let mut calls = HashSet::new();
                // Validate the entire batch before releasing any complete item.
                for item in output {
                    let id = string(item, "id")?;
                    if !ids.insert(id) {
                        return Err(invalid());
                    }
                    if item
                        .get("status")
                        .is_some_and(|status| status != "completed")
                    {
                        return Err(invalid());
                    }
                    match output_kind(item)? {
                        "message" => {
                            if item["role"] != "assistant" || item["status"] != "completed" {
                                return Err(invalid());
                            }
                            for (index, part) in item["content"]
                                .as_array()
                                .ok_or_else(invalid)?
                                .iter()
                                .enumerate()
                            {
                                let text = match string(part, "type")? {
                                    "output_text" => string(part, "text")?,
                                    "refusal" => string(part, "refusal")?,
                                    _ => return Err(ModelError::new(ErrorKind::UnsupportedOutput)),
                                };
                                events.push(ModelEvent::ItemCompleted {
                                    item_id: format!("{id}:{index}"),
                                    content: AssistantContent::Text { text: text.into() },
                                });
                            }
                        }
                        "reasoning" => {
                            for (index, part) in item["summary"]
                                .as_array()
                                .ok_or_else(invalid)?
                                .iter()
                                .enumerate()
                            {
                                if part["type"] != "summary_text" {
                                    return Err(ModelError::new(ErrorKind::UnsupportedOutput));
                                }
                                events.push(ModelEvent::ItemCompleted {
                                    item_id: format!("{id}:{index}"),
                                    content: AssistantContent::Reasoning {
                                        text: string(part, "text")?.into(),
                                    },
                                });
                            }
                        }
                        "function_call" => {
                            let name = string(item, "name")?;
                            if item["namespace"] != super::wire::namespace(name)
                                || !tools.iter().any(|tool| tool.name == name)
                            {
                                return Err(ModelError::new(ErrorKind::UnsupportedOutput));
                            }
                            let call_id = string(item, "call_id")?;
                            if !calls.insert(call_id) {
                                return Err(invalid());
                            }
                            events.push(ModelEvent::ItemCompleted {
                                item_id: id.into(),
                                content: AssistantContent::ToolCall(ToolCall {
                                    id: call_id.into(),
                                    name: name.into(),
                                    arguments: string(item, "arguments")?.into(),
                                }),
                            });
                        }
                        _ => unreachable!(),
                    }
                }
                events.push(ModelEvent::Finished(if calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                }));
            }
            "response.failed" => return Err(super::error::classified(&event["response"], None)),
            "error" => return Err(super::error::classified(event, None)),
            "response.incomplete" => return Err(ModelError::new(ErrorKind::IncompleteResponse)),
            _ => {}
        }
        Ok(events)
    }

    fn item(&mut self, event: &Value) -> Result<&mut Item, ModelError> {
        let item = self
            .items
            .get_mut(&number(event, "output_index")?)
            .ok_or_else(invalid)?;
        if string(event, "item_id")? != item.id {
            return Err(invalid());
        }
        Ok(item)
    }
}

fn invalid() -> ModelError {
    ModelError::new(ErrorKind::Other)
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ModelError> {
    value[field].as_str().ok_or_else(invalid)
}

fn number(value: &Value, field: &str) -> Result<u64, ModelError> {
    value[field].as_u64().ok_or_else(invalid)
}

fn output_kind(item: &Value) -> Result<&str, ModelError> {
    match string(item, "type")? {
        kind @ ("message" | "reasoning" | "function_call") => Ok(kind),
        _ => Err(ModelError::new(ErrorKind::UnsupportedOutput)),
    }
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    input_tokens_details: Option<InputDetails>,
    output_tokens_details: Option<OutputDetails>,
}
#[derive(Deserialize)]
struct InputDetails {
    cached_tokens: Option<u64>,
}
#[derive(Deserialize)]
struct OutputDetails {
    reasoning_tokens: Option<u64>,
}

pub(super) fn usage(response: &Value) -> Result<Option<Usage>, ModelError> {
    let Some(value) = response.get("usage").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let wire: WireUsage = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    let cache_read_tokens = wire
        .input_tokens_details
        .and_then(|details| details.cached_tokens);
    Ok(Some(Usage {
        input_tokens: wire.input_tokens,
        output_tokens: wire.output_tokens,
        total_tokens: wire.total_tokens,
        cache_read_tokens,
        cache_miss_tokens: cache_read_tokens.map(|cached| wire.input_tokens.saturating_sub(cached)),
        reasoning_tokens: wire
            .output_tokens_details
            .and_then(|details| details.reasoning_tokens),
    }))
}
