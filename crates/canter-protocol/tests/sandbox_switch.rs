use canter_protocol::*;
use serde_json::json;

#[test]
fn sandbox_selection_is_live_only_and_does_not_advance_the_replay_cursor() {
    let mut snapshot = SessionSnapshot::empty("sandbox");
    snapshot.last_event_seq = 12;
    snapshot.settings.sandbox_enabled = Some(true);
    for pending in [Some(false), Some(true), None] {
        let event: Event = serde_json::from_value(json!({
            "seq":0,"sessionId":"sandbox","timestamp":100,
            "type":"session.sandboxUpdated","pendingEnabled":pending
        }))
        .unwrap();
        assert!(event.payload.is_ephemeral());
        assert!(!sse_frame(&event).unwrap().contains("id: "));
        apply_event_mut(&mut snapshot, &event).unwrap();
        assert_eq!(snapshot.sandbox_pending, pending);
        assert_eq!(snapshot.settings.sandbox_enabled, Some(true));
        assert_eq!(snapshot.last_event_seq, 12);
        let mut wrong_session = event;
        wrong_session.session_id = "other".into();
        assert!(apply_event_mut(&mut snapshot, &wrong_session).is_err());
    }
    let params: SessionSandboxParams = serde_json::from_value(json!({"enabled":false})).unwrap();
    assert!(!params.confirmed);
}
