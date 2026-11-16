use canter_protocol::UserInput;
use serde_json::json;

#[test]
fn image_data_round_trips_with_optional_name_and_keeps_local_images() {
    for value in [
        json!({"type":"image", "url":"data:image/png;base64,iVBORw0KGgo="}),
        json!({"type":"image", "url":"data:image/png;base64,iVBORw0KGgo=", "name":"shot.png"}),
        json!({"type":"localImage", "path":"/tmp/shot.png", "name":"shot.png"}),
    ] {
        let input: UserInput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), value);
    }
}
