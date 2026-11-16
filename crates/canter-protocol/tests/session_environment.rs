use canter_protocol::{InfoResponse, SessionCreateParams, SessionResumeParams};
use serde_json::{Value, json};

#[test]
fn session_environment_can_be_omitted_without_changing_explicit_maps() {
    for environment in [None, Some(json!({})), Some(json!({"HOME":"/client"}))] {
        let mut create = json!({"cwd":"/tmp/project","model":"fake","approvalPolicy":"allowAll","clientName":"test"});
        let mut resume = json!({});
        if let Some(env) = &environment {
            create["env"] = env.clone();
            resume["env"] = env.clone();
        }
        let create: SessionCreateParams = serde_json::from_value(create).unwrap();
        let resume: SessionResumeParams = serde_json::from_value(resume).unwrap();
        for serialized in [
            serde_json::to_value(create).unwrap(),
            serde_json::to_value(resume).unwrap(),
        ] {
            assert_eq!(serialized.get("env"), environment.as_ref());
        }
    }
    let invalid: Result<SessionResumeParams, _> =
        serde_json::from_value(json!({"env":Value::String("invalid".into())}));
    assert!(invalid.is_err());
}

#[test]
fn info_from_older_servers_remains_readable() {
    let value = json!({"serverVersion":"old","protocolVersion":1,"sessions":[]});
    let info: InfoResponse = serde_json::from_value(value.clone()).unwrap();
    assert!(info.home.is_none());
    assert!(info.instance.is_none());
    assert_eq!(serde_json::to_value(info).unwrap(), value);
}
