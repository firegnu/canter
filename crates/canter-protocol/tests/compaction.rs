use canter_protocol::{Event, Item, ItemDelta, SessionSnapshot, apply_event_mut};
use serde_json::json;

#[test]
fn compaction_numbers_are_optional_and_legacy_clients_ignore_them() {
    #[derive(serde::Deserialize)]
    struct LegacyItem {
        id: String,
        summary: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum LegacyDelta {
        #[serde(other)]
        Unknown,
    }
    let old = json!({"type":"contextCompaction","id":"c1","summary":"Summary."});
    let parsed: Item = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let mut current = old;
    current["beforeTokens"] = json!(152_000);
    current["afterTokens"] = json!(9_000);
    current["part"] = json!(3);
    let parsed: Item = serde_json::from_value(current.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), current);
    let legacy: LegacyItem = serde_json::from_value(current).unwrap();
    assert_eq!(legacy.id, "c1");
    assert_eq!(legacy.summary, "Summary.");
    let delta = json!({"type":"contextCompaction","part":2});
    let parsed: ItemDelta = serde_json::from_value(delta.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), delta);
    assert!(matches!(
        serde_json::from_value::<LegacyDelta>(delta).unwrap(),
        LegacyDelta::Unknown
    ));
}

#[test]
fn saved_compaction_progress_replays_and_preserves_its_token_estimate() {
    let mut snapshot = SessionSnapshot::empty("s1");
    let payloads = [
        json!({"type":"turn.started","turn":{"id":"t1","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null}}),
        json!({"type":"item.started","turnId":"t1","item":{"type":"contextCompaction","id":"c1","summary":"","beforeTokens":152000}}),
        json!({"type":"item.delta","turnId":"t1","itemId":"c1","delta":{"type":"contextCompaction","part":2}}),
        json!({"type":"item.delta","turnId":"t1","itemId":"c1","delta":{"type":"contextCompaction","part":3}}),
    ];
    for (index, mut payload) in payloads.into_iter().enumerate() {
        payload["seq"] = json!(index + 1);
        payload["sessionId"] = json!("s1");
        payload["timestamp"] = json!(index + 1);
        let event: Event = serde_json::from_value(payload).unwrap();
        apply_event_mut(&mut snapshot, &event).unwrap();
        if index >= 2 {
            let item = serde_json::to_value(&snapshot.turns[0].items[0]).unwrap();
            assert_eq!(item["part"], index);
            assert_eq!(item["beforeTokens"], 152_000);
            assert!(item.get("afterTokens").is_none());
            let saved = serde_json::to_string(&snapshot).unwrap();
            snapshot = serde_json::from_str(&saved).unwrap();
        }
    }
}
