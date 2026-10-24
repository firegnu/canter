use canter_protocol::*;
use serde_json::json;

#[test]
fn working_directory_is_public_but_environment_is_not() {
    let event: Event = serde_json::from_value(json!({
        "seq":1,"sessionId":"s1","timestamp":1,"type":"session.updated",
        "settings":{"name":"work","model":"fake","approvalPolicy":"alwaysAsk","cwd":"/work/project","env":{"TEST_ONLY":"fixture-private"}}
    })).unwrap();
    let snapshot = apply_event(&SessionSnapshot::empty("s1"), &event).unwrap();
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["settings"]["cwd"], "/work/project");
    assert!(value["settings"].get("env").is_none());
    assert_eq!(
        serde_json::to_value(snapshot.summary()).unwrap()["cwd"],
        "/work/project"
    );
    assert!(
        !serde_json::to_string(&event)
            .unwrap()
            .contains("fixture-private")
    );
}
