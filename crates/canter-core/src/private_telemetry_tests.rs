use super::*;
use crate::{CoreOptions, RuntimeContext, Session};
use canter_model::{FinishReason, Model, ModelEvent, ModelStream, Request};
use tokio_util::sync::CancellationToken;

struct Reply;
impl Model for Reply {
    fn stream(&self, _: Request, _: CancellationToken) -> ModelStream<'_> {
        Box::pin(futures_util::stream::iter([
            Ok(ModelEvent::TextDelta {
                item_id: "answer".into(),
                delta: "private answer".into(),
            }),
            Ok(ModelEvent::Finished(FinishReason::Stop)),
        ]))
    }
}

#[tokio::test]
async fn private_completion_does_not_wait_for_the_telemetry_writer() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let settings = serde_json::from_value(serde_json::json!({
        "cwd":temp.path(), "model":"fake", "approvalPolicy":"allowAll"
    }))
    .unwrap();
    let (session, _) = Session::create(
        store,
        "private".into(),
        settings,
        RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    temp.path().join("home").display().to_string(),
                ),
                (
                    "CANTER_HOME".into(),
                    temp.path().join("state").display().to_string(),
                ),
            ]
            .into(),
            ..Default::default()
        },
        Arc::new(Reply),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let writer = session.shared.state.lock().await.journal.writer.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        let _guard = writer.lock().unwrap();
        locked.send(()).unwrap();
        let _ = held.recv();
    });
    ready.await.unwrap();
    let mut stream = session.btw("question".into()).await.unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Some(event) = stream.recv().await {
            if matches!(event, BtwEvent::Completed) {
                return true;
            }
        }
        false
    })
    .await;
    release.send(()).unwrap();
    blocker.join().unwrap();
    assert!(
        completed.unwrap(),
        "the final event must precede the telemetry commit"
    );
    session.close().await.unwrap();
    let stored =
        std::fs::read_to_string(temp.path().join("sessions/private/private-telemetry.jsonl"))
            .unwrap();
    let records: Vec<ModelRequestTelemetry> = stored
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1, "close must wait for the telemetry commit");
    assert_eq!(records[0].purpose, RequestPurpose::Btw);
    assert!(!stored.contains("private answer"));
}
