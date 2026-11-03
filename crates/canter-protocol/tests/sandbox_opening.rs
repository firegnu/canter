use canter_protocol::{SessionCreateParams, SessionResumeParams};
use serde_json::json;

#[test]
fn sandbox_opening_is_optional_and_uses_the_same_wire_field_for_create_and_resume() {
    for disabled in [None, Some(false), Some(true)] {
        let mut create = json!({"cwd":"/tmp/project","env":{},"model":"fake","approvalPolicy":"allowAll","clientName":"test"});
        let mut resume = json!({"env":{}});
        if let Some(disabled) = disabled {
            create["sandboxDisabled"] = json!(disabled);
            resume["sandboxDisabled"] = json!(disabled);
        }
        let create: SessionCreateParams = serde_json::from_value(create).unwrap();
        let resume: SessionResumeParams = serde_json::from_value(resume).unwrap();
        assert_eq!(create.sandbox_disabled, disabled == Some(true));
        assert_eq!(resume.sandbox_disabled, disabled == Some(true));
        for serialized in [
            serde_json::to_value(create).unwrap(),
            serde_json::to_value(resume).unwrap(),
        ] {
            if disabled == Some(true) {
                assert_eq!(serialized["sandboxDisabled"], true);
            } else {
                assert!(serialized.get("sandboxDisabled").is_none());
            }
        }
    }
}
