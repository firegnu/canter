use super::*;
use crate::chatgpt_auth::test_support::{Fake, account_router};
use serde_json::{Value, json};

struct Fixture {
    root: tempfile::TempDir,
    fake: Fake,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-account-")
            .tempdir_in("/tmp")
            .unwrap();
        let fake = Fake::new().await;
        let app = account_router(&root.path().join("state"), &fake);
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            root,
            fake,
            client,
            server,
        }
    }
    async fn post(&self, method: &str, params: Value) -> Value {
        let response = self
            .client
            .post(format!("http://localhost/v1/account/{method}"))
            .json(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method}");
        response.json().await.unwrap()
    }
    async fn login(&self, background: bool) -> Value {
        let start = self
            .post(
                "login/start",
                json!({"provider":"chatgpt","backgroundConsent":background}),
            )
            .await;
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(start["authUrl"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        self.post(
            "login/wait",
            json!({"provider":"chatgpt","loginId":start["loginId"]}),
        )
        .await
    }
}

#[tokio::test]
async fn account_login_completion_selects_account_and_saves_consent() {
    let test = Fixture::new().await;
    let done = test.login(true).await;
    assert_eq!(done["success"], true);
    assert_eq!(done["showPlanNotice"], true);
    let read = test.post("read", json!({"provider":"chatgpt"})).await;
    assert_eq!(read["accounts"][0]["email"], "same@example.test");
    assert_eq!(read["accounts"][0]["backgroundConsent"], true);
    assert_eq!(read["currentAccountId"], done["account"]["id"]);
    assert!(
        test.root
            .path()
            .join("state/auth/chatgpt/host.json")
            .exists()
    );
}

#[tokio::test]
async fn account_selection_background_and_three_logout_results() {
    let test = Fixture::new().await;
    let first = test.login(true).await;
    test.fake.state.lock().unwrap().client_id = "oaiapp_test-two".into();
    let second = test.login(false).await;
    assert_eq!(second["showPlanNotice"], false);
    let first_id = &first["account"]["id"];
    let read = test.post("read", json!({"provider":"chatgpt"})).await;
    assert_eq!(read["currentAccountId"], second["account"]["id"]);
    let number = read["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .position(|a| a["id"] == *first_id)
        .unwrap()
        + 1;
    let selected = test
        .post(
            "select",
            json!({"provider":"chatgpt","account":number.to_string()}),
        )
        .await;
    assert_eq!(selected["currentAccountId"], *first_id);
    test.post("background", json!({"provider":"chatgpt","enabled":false}))
        .await;
    let read = test.post("read", json!({"provider":"chatgpt"})).await;
    assert_eq!(read["accounts"][number - 1]["backgroundConsent"], false);
    test.fake.state.lock().unwrap().revoke_status = 400;
    let pending = test.post("logout", json!({"provider":"chatgpt"})).await;
    assert_eq!(pending["result"], "pending");
    assert_eq!(pending["account"]["pendingRevocation"], true);
    assert_eq!(pending["account"]["status"], "signedOut");
    test.fake.state.lock().unwrap().revoke_status = 200;
    assert_eq!(
        test.post("logout", json!({"provider":"chatgpt"})).await["result"],
        "revoked"
    );
    assert_eq!(
        test.post("logout", json!({"provider":"chatgpt"})).await["result"],
        "noToken"
    );
}

#[tokio::test]
async fn account_cancel_completes_wait_and_closes_callback() {
    let test = Fixture::new().await;
    let start = test
        .post("login/start", json!({"provider":"chatgpt"}))
        .await;
    let params = json!({"provider":"chatgpt","loginId":start["loginId"]});
    assert_eq!(
        test.post("login/cancel", params.clone()).await["status"],
        "canceled"
    );
    let done = test.post("login/wait", params.clone()).await;
    assert_eq!(done["success"], false);
    assert!(done["error"].as_str().unwrap().contains("Cancelled"));
    assert_eq!(
        test.post("login/cancel", params).await["status"],
        "notFound"
    );
    let url = reqwest::Url::parse(start["authUrl"].as_str().unwrap()).unwrap();
    let callback = url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    assert!(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(callback)
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        test.post("read", json!({"provider":"chatgpt"})).await["accounts"],
        json!([])
    );
}

#[tokio::test]
async fn account_preferences_survive_restart_and_first_plan_notice_is_not_repeated() {
    let mut test = Fixture::new().await;
    let first = test.login(true).await;
    test.post("logout", json!({"provider":"chatgpt"})).await;
    test.server.abort();
    let _ = (&mut test.server).await;
    let socket = test.root.path().join("restarted.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let app = account_router(&test.root.path().join("state"), &test.fake);
    test.server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    test.client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let read = test.post("read", json!({"provider":"chatgpt"})).await;
    assert_eq!(read["currentAccountId"], first["account"]["id"]);
    assert_eq!(read["accounts"][0]["backgroundConsent"], true);
    let again = test.login(false).await;
    assert_eq!(again["showPlanNotice"], false);
    assert_eq!(again["account"]["id"], first["account"]["id"]);
    assert_eq!(again["account"]["backgroundConsent"], false);
}

#[tokio::test]
async fn all_account_endpoints_require_supported_provider_and_email_must_be_unique() {
    let test = Fixture::new().await;
    for method in [
        "read",
        "login/start",
        "login/wait",
        "login/cancel",
        "select",
        "background",
        "logout",
    ] {
        let response = test
            .client
            .post(format!("http://localhost/v1/account/{method}"))
            .json(&json!({"provider":"other","account":"1","loginId":"unknown","enabled":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("Providers: chatgpt")
        );
    }
    test.login(false).await;
    let selected = test
        .post(
            "select",
            json!({"provider":"chatgpt","account":"same@example.test"}),
        )
        .await;
    assert_eq!(selected["accounts"].as_array().unwrap().len(), 1);
    test.fake.state.lock().unwrap().client_id = "oaiapp_test-two".into();
    test.login(false).await;
    let response = test
        .client
        .post("http://localhost/v1/account/select")
        .json(&json!({"provider":"chatgpt","account":"same@example.test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("Multiple accounts"));
}
