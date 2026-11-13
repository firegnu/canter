use super::retract_tests::Harness;
use serde_json::json;

#[tokio::test]
async fn sandbox_client_checks_feature_and_sends_confirmation_only_for_off() {
    for supported in [false, true] {
        let mut test = Harness::new().await;
        for enabled in [true, false] {
            let client = test.chat.client.clone();
            let request = tokio::spawn(async move { client.set_sandbox("test", enabled).await });
            let info = test.request().await;
            assert_eq!(info.path, "/v1/info");
            info.json(
                json!({"features":if supported {vec!["sessionSandboxSwitch"]} else {vec![]},
                "serverVersion":"test","protocolVersion":1,"sessions":[]}),
            );
            if supported {
                let change = test.request().await;
                assert_eq!(change.path, "/v1/sessions/test/sandbox");
                assert_eq!(change.body, json!({"enabled":enabled,"confirmed":!enabled}));
                change.json(json!({"eventSeq":3}));
                assert_eq!(request.await.unwrap().unwrap().event_seq, 3);
            } else {
                let error = request.await.unwrap().unwrap_err().to_string();
                assert!(error.contains("canter serve --replace"));
                assert!(test.requests.try_recv().is_err());
            }
        }
        test.stop().await;
    }
}
