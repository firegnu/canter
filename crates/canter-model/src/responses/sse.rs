use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    AssistantContent, ErrorKind, FinishReason, ModelError, ModelEvent, Request, ToolCall, Usage,
};

#[derive(Default)]
pub(super) struct State {
    items: BTreeMap<u64, Item>,
}

struct Item {
    id: String,
    kind: String,
    arguments: String,
    done: Option<Value>,
}

pub(super) fn has_output(event: &Value) -> bool {
    match event["type"].as_str() {
        Some(
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta",
        ) => event["delta"].as_str().is_some_and(|text| !text.is_empty()),
        Some(
            "response.output_item.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.done",
        ) => true,
        Some("response.output_item.added") => {
            let item = &event["item"];
            ["content", "summary"]
                .iter()
                .any(|key| item[key].as_array().is_some_and(|parts| !parts.is_empty()))
                || ["arguments", "input", "encrypted_content"]
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
        request: &Request,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = Vec::new();
        let check = Check {
            location: "event",
            index: event["output_index"].as_u64(),
        };
        match check.string(event, "type")? {
            "response.output_item.added" | "response.output_item.done" => {
                let check = Check {
                    location: if event["type"] == "response.output_item.done" {
                        "output_item.done"
                    } else {
                        "output_item.added"
                    },
                    ..check
                };
                let index = check.number(event, "output_index")?;
                let item = &event["item"];
                let id = check.string(item, "id")?;
                let kind = check.output_kind(item)?;
                if let Some(previous) = self.items.get(&index) {
                    if previous.id != id || previous.kind != kind {
                        return Err(check.invalid("item id or type changed"));
                    }
                } else if self.items.values().any(|item| item.id == id) {
                    return Err(check.invalid("item id is already used at another index"));
                }
                self.items.insert(
                    index,
                    Item {
                        id: id.into(),
                        kind: kind.into(),
                        arguments: item[if kind == "custom_tool_call" {
                            "input"
                        } else {
                            "arguments"
                        }]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                        done: (event["type"] == "response.output_item.done").then(|| item.clone()),
                    },
                );
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta" => {
                let summary = event["type"] == "response.reasoning_summary_text.delta";
                let item = self.item(event, check)?;
                if item.kind != if summary { "reasoning" } else { "message" } {
                    return Err(check.invalid("text delta has the wrong item type"));
                }
                let part = check.number(
                    event,
                    if summary {
                        "summary_index"
                    } else {
                        "content_index"
                    },
                )?;
                let id = format!("{}:{part}", item.id);
                let delta = check.string(event, "delta")?.to_owned();
                if !delta.is_empty() {
                    events.push(if summary {
                        ModelEvent::ReasoningDelta { item_id: id, delta }
                    } else {
                        ModelEvent::TextDelta { item_id: id, delta }
                    });
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let custom = event["type"] == "response.custom_tool_call_input.delta";
                let delta = check.string(event, "delta")?;
                let item = self.item(event, check)?;
                if item.kind
                    != if custom {
                        "custom_tool_call"
                    } else {
                        "function_call"
                    }
                {
                    return Err(check.invalid("arguments delta has the wrong item type"));
                }
                item.arguments.push_str(delta);
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let custom = event["type"] == "response.custom_tool_call_input.done";
                let arguments = check.string(event, if custom { "input" } else { "arguments" })?;
                let item = self.item(event, check)?;
                if item.kind
                    != if custom {
                        "custom_tool_call"
                    } else {
                        "function_call"
                    }
                {
                    return Err(check.invalid("arguments done has the wrong item type"));
                }
                item.arguments = arguments.into();
            }
            "response.completed" => {
                let response = &event["response"];
                if response["status"] != "completed" {
                    return Err(check.invalid("response.completed has an invalid status"));
                }
                let mut ids = HashSet::new();
                let mut calls = HashSet::new();
                // Validate the entire batch before releasing any complete item.
                for (index, item) in &self.items {
                    let check = Check {
                        location: "output_item.done",
                        index: Some(*index),
                    };
                    let item = item.done.as_ref().ok_or_else(|| {
                        check.invalid("was not received before response.completed")
                    })?;
                    let id = check.string(item, "id")?;
                    if !ids.insert(id) {
                        return Err(check.invalid("item id is duplicated"));
                    }
                    if item
                        .get("status")
                        .is_some_and(|status| status != "completed")
                    {
                        return Err(check.invalid("item status is not completed"));
                    }
                    match check.output_kind(item)? {
                        "message" => {
                            if item["role"] != "assistant" || item["status"] != "completed" {
                                return Err(check.invalid("message role or status is invalid"));
                            }
                            for (index, part) in item["content"]
                                .as_array()
                                .ok_or_else(|| check.invalid("has no content array"))?
                                .iter()
                                .enumerate()
                            {
                                let text = match check.string(part, "type")? {
                                    "output_text" => check.string(part, "text")?,
                                    "refusal" => check.string(part, "refusal")?,
                                    _ => {
                                        return Err(check
                                            .unsupported("message content type is unsupported"));
                                    }
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
                                .ok_or_else(|| check.invalid("has no summary array"))?
                                .iter()
                                .enumerate()
                            {
                                if part["type"] != "summary_text" {
                                    return Err(
                                        check.unsupported("reasoning summary type is unsupported")
                                    );
                                }
                                events.push(ModelEvent::ItemCompleted {
                                    item_id: format!("{id}:{index}"),
                                    content: AssistantContent::Reasoning {
                                        text: check.string(part, "text")?.into(),
                                    },
                                });
                            }
                        }
                        kind @ ("function_call" | "custom_tool_call") => {
                            let custom = kind == "custom_tool_call";
                            let name = check.string(item, "name")?;
                            let known = if custom {
                                request.custom_tools.contains_key(name)
                            } else {
                                request.tools.iter().any(|tool| tool.name == name)
                            };
                            if item["namespace"] != super::wire::namespace(name) || !known {
                                return Err(check.unsupported(
                                    "tool name or namespace is not in the request tools",
                                ));
                            }
                            let call_id = check.string(item, "call_id")?;
                            if !calls.insert(call_id) {
                                return Err(check.invalid("call_id is duplicated"));
                            }
                            events.push(ModelEvent::ItemCompleted {
                                item_id: id.into(),
                                content: AssistantContent::ToolCall(ToolCall {
                                    id: call_id.into(),
                                    name: name.into(),
                                    arguments: check
                                        .string(item, if custom { "input" } else { "arguments" })?
                                        .into(),
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

    pub(super) fn replay_output(&self) -> Vec<Value> {
        let output = Value::Array(
            self.items
                .values()
                .map(|item| {
                    item.done
                        .as_ref()
                        .expect("validated output_item.done")
                        .clone()
                })
                .collect(),
        );
        super::wire::replay_output(&output)
    }

    fn item(&mut self, event: &Value, check: Check) -> Result<&mut Item, ModelError> {
        let item = self
            .items
            .get_mut(&check.number(event, "output_index")?)
            .ok_or_else(|| check.invalid("has no preceding output item"))?;
        if check.string(event, "item_id")? != item.id {
            return Err(check.invalid("item_id does not match the output item"));
        }
        Ok(item)
    }
}

#[derive(Clone, Copy)]
struct Check {
    location: &'static str,
    index: Option<u64>,
}

impl Check {
    fn invalid(self, reason: &'static str) -> ModelError {
        self.error(ErrorKind::Other, format_args!("{reason}"))
    }

    fn unsupported(self, reason: &'static str) -> ModelError {
        self.error(ErrorKind::UnsupportedOutput, format_args!("{reason}"))
    }

    // Only local static descriptions/field names and numeric positions reach
    // stderr (the daemon's server.log). Never format upstream values here.
    fn error(self, kind: ErrorKind, reason: std::fmt::Arguments<'_>) -> ModelError {
        if let Some(index) = self.index {
            eprintln!(
                "Responses stream: {} at index {index} {reason}",
                self.location
            );
        } else {
            eprintln!("Responses stream: {} {reason}", self.location);
        }
        ModelError::new(kind)
    }

    fn missing(self, field: &'static str) -> ModelError {
        self.error(ErrorKind::Other, format_args!("has no {field}"))
    }

    fn string<'a>(self, value: &'a Value, field: &'static str) -> Result<&'a str, ModelError> {
        value[field].as_str().ok_or_else(|| self.missing(field))
    }

    fn number(self, value: &Value, field: &'static str) -> Result<u64, ModelError> {
        value[field].as_u64().ok_or_else(|| self.missing(field))
    }

    fn output_kind(self, item: &Value) -> Result<&str, ModelError> {
        match self.string(item, "type")? {
            kind @ ("message" | "reasoning" | "function_call" | "custom_tool_call") => Ok(kind),
            _ => Err(self.unsupported("output type is unsupported")),
        }
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
    let wire: WireUsage = serde_json::from_value(value.clone()).map_err(|_| {
        Check {
            location: "usage",
            index: None,
        }
        .invalid("has invalid token counts")
    })?;
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
