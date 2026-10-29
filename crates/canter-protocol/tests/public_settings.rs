use canter_protocol::*;
use serde_json::json;

#[test]
fn additional_directories_are_public_and_legacy_settings_default_to_empty() {
    let mut settings =
        json!({"cwd":"/work/main","name":null,"model":"fake","approvalPolicy":"askForExecution"});
    let legacy: SessionSettings = serde_json::from_value(settings.clone()).unwrap();
    assert!(legacy.additional_directories.is_empty());
    assert!(
        serde_json::to_value(legacy)
            .unwrap()
            .get("additionalDirectories")
            .is_none()
    );
    settings["additionalDirectories"] = json!(["/work/service"]);
    let settings: SessionSettings = serde_json::from_value(settings).unwrap();
    let value = serde_json::to_value(settings).unwrap();
    assert_eq!(value["additionalDirectories"], json!(["/work/service"]));
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacySettings {
        cwd: String,
        model: String,
        approval_policy: PermissionMode,
    }
    let old_client: LegacySettings = serde_json::from_value(value).unwrap();
    assert_eq!(old_client.cwd, "/work/main");
    assert_eq!(old_client.model, "fake");
    assert_eq!(old_client.approval_policy, PermissionMode::AskForExecution);
    let update: SessionUpdateParams =
        serde_json::from_value(json!({"addDirectories":["../service"]})).unwrap();
    assert_eq!(
        serde_json::to_value(update).unwrap()["addDirectories"],
        json!(["../service"])
    );
}

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
