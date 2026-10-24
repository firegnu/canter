use super::*;
use canter_model::{FinishReason, ModelEvent, ModelStream, Request};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

// A failed model task is a real Core close-error path, without modifying Core or the store.
struct PanicOnce {
    panicked: AtomicBool,
    reached: Notify,
}
impl Model for PanicOnce {
    fn stream(&self, _request: Request, _cancel: CancellationToken) -> ModelStream<'_> {
        if !self.panicked.swap(true, Ordering::Relaxed) {
            self.reached.notify_one();
            panic!("test-only failed model task");
        }
        Box::pin(futures_util::stream::iter([
            Ok(ModelEvent::TextDelta("recovered".into())),
            Ok(ModelEvent::Finished(FinishReason::Stop)),
        ]))
    }
}

#[tokio::test]
async fn failed_close_can_be_resumed_through_http() {
    let root = tempfile::Builder::new()
        .prefix("canter-close-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("state");
    let directory = home.clone();
    let store = tokio::task::spawn_blocking(move || Store::new(directory.join("sessions")))
        .await
        .unwrap()
        .unwrap();
    let model = Arc::new(PanicOnce {
        panicked: AtomicBool::new(false),
        reached: Notify::new(),
    });
    let app = Arc::new(App {
        home,
        store: Arc::new(store),
        model: model.clone(),
        default_model: "fake-model".into(),
        options: CoreOptions::default(),
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(app))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let created = client.post("http://localhost/v1/sessions").json(&json!({
        "cwd":root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"allowAll", "clientName":"test"
    })).send().await.unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created: SessionCreateResponse = created.json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let input = json!({"input":[{"type":"text","text":"hello"}], "clientName":"test"});
    assert_eq!(
        client
            .post(format!("{url}/input"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), model.reached.notified())
        .await
        .unwrap();
    let failed = client.delete(&url).send().await.unwrap();
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        failed.json::<ErrorResponse>().await.unwrap().code,
        ErrorCode::Internal
    );
    let resumed = client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed: SessionResumeResponse = resumed.json().await.unwrap();
    assert_eq!(
        resumed.session.status,
        SessionStatus::Idle,
        "resume must reopen the failed session instead of returning its stale snapshot"
    );
    assert_eq!(resumed.session.turns[0].status, TurnStatus::Interrupted);
    assert_eq!(
        client
            .post(format!("{url}/input"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot: SessionReadResponse =
                client.get(&url).send().await.unwrap().json().await.unwrap();
            if snapshot.session.status == SessionStatus::Idle {
                assert_eq!(
                    snapshot.session.turns.last().unwrap().status,
                    TurnStatus::Completed
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.delete(url).send().await.unwrap().status(),
        StatusCode::OK
    );
    stop.cancel();
    server.await.unwrap();
}
