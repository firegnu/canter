use canter_protocol::{Model, ReasoningEffort, SessionSettings};
use serde_json::json;

#[test]
fn reasoning_efforts_round_trip_and_old_clients_read_new_values_as_unknown() {
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum LegacyEffort {
        None,
        Low,
        High,
        Max,
        #[serde(other)]
        Unknown,
    }
    for effort in ["none", "low", "medium", "high", "xhigh", "max"] {
        let current: ReasoningEffort = serde_json::from_value(json!(effort)).unwrap();
        assert_eq!(current.as_str(), effort);
        assert_eq!(serde_json::to_value(current).unwrap(), json!(effort));
        let legacy: LegacyEffort = serde_json::from_value(json!(effort)).unwrap();
        assert_eq!(
            legacy == LegacyEffort::Unknown,
            ["medium", "xhigh"].contains(&effort)
        );
    }
}

#[test]
fn model_reasoning_metadata_round_trips_and_remains_optional() {
    let old = json!({"id":"fake","model":"fake","displayName":"fake","isDefault":true});
    let parsed: Model = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let mut current = old;
    current["supportedReasoningEfforts"] = json!(["low", "medium", "high", "xhigh", "max"]);
    current["defaultReasoningEffort"] = json!("medium");
    let parsed: Model = serde_json::from_value(current.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), current);
}

#[test]
fn legacy_saved_settings_keep_their_effort_and_serialized_shape() {
    for effort in [None, Some("none"), Some("low"), Some("high"), Some("max")] {
        let mut saved =
            json!({"cwd":"/work","name":null,"model":"fake","approvalPolicy":"alwaysAsk"});
        if let Some(effort) = effort {
            saved["reasoningEffort"] = json!(effort);
        }
        let parsed: SessionSettings = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(parsed.reasoning_effort.as_str(), effort.unwrap_or("high"));
        saved["reasoningEffort"] = json!(effort.unwrap_or("high"));
        assert_eq!(serde_json::to_value(parsed).unwrap(), saved);
    }
}
