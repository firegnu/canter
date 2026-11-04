use std::{
    collections::{BTreeMap, hash_map::RandomState},
    hash::BuildHasher,
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use crate::{ModelError, ModelEvent};

// Process-wide, including separate adapters and concurrent sessions. Keep only
// the previous input, compare serialized item bytes (not hashes), never log it.
struct Previous {
    sequence: u64,
    input: Vec<Vec<u8>>,
    tools: Vec<u8>,
    developer: Option<Vec<u8>>,
}

static PREVIOUS: Mutex<Previous> = Mutex::new(Previous {
    sequence: 0,
    input: Vec::new(),
    tools: Vec::new(),
    developer: None,
});

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON value serialization")
}

// A process-random key prevents dictionary matching of low-entropy prompts or
// configured names. These short fingerprints are comparable only in this process.
fn fingerprint(value: &[u8]) -> String {
    static HASHER: OnceLock<RandomState> = OnceLock::new();
    format!(
        "{:012x}",
        HASHER.get_or_init(RandomState::new).hash_one(value) & 0xffff_ffff_ffff
    )
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) struct Diagnostic {
    started: Instant,
    row: Value,
    done: BTreeMap<u64, Value>,
}

impl Diagnostic {
    pub(super) fn new(body: &Value) -> Self {
        let input: Vec<_> = body["input"]
            .as_array()
            .expect("request input array")
            .iter()
            .map(bytes)
            .collect();
        let tools = bytes(&body["tools"]);
        let developer = body["input"]
            .as_array()
            .and_then(|input| input.first())
            .filter(|item| item["role"] == "developer")
            .map(bytes);
        let mut previous = PREVIOUS.lock().unwrap_or_else(|error| error.into_inner());
        let first_different = previous
            .input
            .iter()
            .enumerate()
            .find(|(index, item)| input.get(*index) != Some(item))
            .map(|(index, _)| index);
        let mut row = json!({
            "request":previous.sequence + 1,
            "started_unix_ms":unix_ms(), "sent_unix_ms":"missing",
            "model_hash":fingerprint(&bytes(&body["model"])),
            "prompt_cache_key_present":body.get("prompt_cache_key").is_some(),
            "input_count":input.len(), "previous_request":"missing",
            "input_prefix_equal":"missing", "first_different_item":"missing",
            "tools_hash":fingerprint(&tools), "tools_equal":"missing",
            "developer_hash":developer.as_ref().map(|value| fingerprint(value)), "developer_equal":"missing",
            "done":[], "completed":[], "completed_output":"missing",
            "outcome":"cancelled", "error_kind":"missing",
            "usage":usage(&Value::Null),
        });
        if previous.sequence > 0 {
            row["previous_request"] = json!(previous.sequence);
            row["input_prefix_equal"] = json!(first_different.is_none());
            if let Some(index) = first_different {
                row["first_different_item"] = json!(index);
            }
            row["tools_equal"] = json!(previous.tools == tools);
            row["developer_equal"] = json!(previous.developer == developer);
        }
        *previous = Previous {
            sequence: previous.sequence + 1,
            input,
            tools,
            developer,
        };
        Self {
            started: Instant::now(),
            row,
            done: BTreeMap::new(),
        }
    }

    pub(super) fn sent(&mut self) {
        self.row["sent_unix_ms"] = json!(unix_ms());
    }

    pub(super) fn observe(&mut self, event: &Value) {
        match event["type"].as_str() {
            Some("response.output_item.done") => {
                if let Some(index) = event["output_index"].as_u64() {
                    self.done.insert(index, event["item"].clone());
                }
            }
            Some("response.completed" | "response.failed" | "response.incomplete") => {
                self.row["usage"] = usage(&event["response"]["usage"]);
                if event["type"] == "response.completed" {
                    let output = event["response"].get("output");
                    self.row["completed_output"] = json!(match output {
                        None => "missing",
                        Some(Value::Array(items)) if items.is_empty() => "empty",
                        Some(Value::Array(items))
                            if items.len() == self.done.len()
                                && items
                                    .iter()
                                    .enumerate()
                                    .all(|(index, item)| self.done.get(&(index as u64))
                                        == Some(item)) =>
                            "equal",
                        Some(Value::Array(_)) => "different",
                        _ => "invalid",
                    });
                    if let Some(Value::Array(items)) = output {
                        self.row["completed"] = items
                            .iter()
                            .enumerate()
                            .map(|(index, item)| metadata(index as u64, item))
                            .collect();
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn finish(&mut self, terminal: Option<&Result<ModelEvent, ModelError>>) {
        match terminal {
            Some(Ok(ModelEvent::Finished(_))) => self.row["outcome"] = json!("success"),
            Some(Err(error)) => {
                self.row["outcome"] = json!("failure");
                // ErrorKind is a local enum, never an upstream error string.
                self.row["error_kind"] = json!(format!("{:?}", error.kind));
            }
            _ => {}
        }
    }
}

impl Drop for Diagnostic {
    fn drop(&mut self) {
        self.row["elapsed_ms"] = json!(self.started.elapsed().as_millis());
        self.row["done"] = self
            .done
            .iter()
            .map(|(index, item)| metadata(*index, item))
            .collect();
        eprintln!("Responses cache: {}", self.row);
    }
}

fn metadata(index: u64, item: &Value) -> Value {
    let kind = match item["type"].as_str() {
        Some("reasoning") => "reasoning",
        Some("message") => "message",
        Some("function_call") => "function_call",
        _ => "unknown",
    };
    let mut result = json!({"index":index,"type":kind,"encrypted_content":item.get("encrypted_content").is_some()});
    if kind == "function_call" {
        result["encrypted_function_args"] = json!(item.get("encrypted_function_args").is_some());
    }
    result
}

fn count(value: Option<&Value>) -> Value {
    match value {
        None => json!("missing"),
        Some(value) if value.is_u64() => value.clone(),
        Some(_) => json!("invalid"),
    }
}

fn usage(value: &Value) -> Value {
    let mut result = json!({
        "input_tokens":count(value.get("input_tokens")),
        "output_tokens":count(value.get("output_tokens")),
        "input_tokens_details.cached_tokens":count(value["input_tokens_details"].get("cached_tokens")),
    });
    // Only local field names are printed, including when a server sends an
    // unexpected string instead of a number. Usage accounting is untouched.
    for key in [
        "cache_write_tokens",
        "cache_creation_tokens",
        "cache_creation_input_tokens",
    ] {
        result[key] = count(value.get(key));
        result[format!("input_tokens_details.{key}")] =
            count(value["input_tokens_details"].get(key));
    }
    result
}
