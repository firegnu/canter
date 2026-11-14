use super::*;

fn subscription(journal: &Journal) -> Subscription {
    Subscription {
        disk_reader: journal.store.event_reader("s1").unwrap(),
        receiver: journal.events.as_ref().unwrap().subscribe(),
        committed: journal.committed.subscribe(),
        sandbox_event: journal.sandbox_event.clone(),
        sandbox_seen: None,
        pending: VecDeque::new(),
        cursor: 0,
        ended: false,
    }
}

#[tokio::test]
async fn repeated_lag_and_writes_during_pending_catch_up_match_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path()).unwrap());
    let writer = store
        .create("s1", SessionSnapshot::empty("s1").settings, 1)
        .unwrap();
    let mut journal = Journal::new(store.clone(), writer, false);
    let mut subscription = subscription(&journal);
    subscription.catch_up().await.unwrap();
    let mut actual = vec![subscription.recv().await.unwrap().unwrap()];
    for round in 0..3 {
        journal
            .append(
                (0..300)
                    .map(|index| EventPayload::Warning {
                        message: format!("{round}-{index}"),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        // Leave most of the disk catch-up pending when the next batch arrives.
        for _ in 0..2 {
            actual.push(subscription.recv().await.unwrap().unwrap());
        }
    }
    journal.release().await.unwrap();
    while let Some(event) = subscription.recv().await.unwrap() {
        actual.push(event);
    }
    assert_eq!(actual, store.events_after("s1", 0).unwrap());
}

#[tokio::test]
async fn catch_up_does_not_skip_disk_events_ahead_of_publication_or_broadcast_gaps() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path()).unwrap());
    let mut writer = store
        .create("s1", SessionSnapshot::empty("s1").settings, 1)
        .unwrap();
    let (sender, receiver) = broadcast::channel(2);
    let (committed, progress) = watch::channel(1);
    let mut subscription = Subscription {
        disk_reader: store.event_reader("s1").unwrap(),
        receiver,
        committed: progress,
        sandbox_event: Default::default(),
        sandbox_seen: None,
        pending: VecDeque::new(),
        cursor: 0,
        ended: false,
    };
    let events = writer
        .append_batch((2..=5).map(|time| {
            (
                EventPayload::Warning {
                    message: time.to_string(),
                },
                time,
            )
        }))
        .unwrap();
    subscription.catch_up().await.unwrap();
    assert_eq!(subscription.recv().await.unwrap().unwrap().seq, 1);
    committed.send_replace(5);
    // A sequence gap, rather than Lagged, also has to recover the missing suffix.
    sender.send(events[3].clone()).unwrap();
    for expected in &events {
        assert_eq!(&subscription.recv().await.unwrap().unwrap(), expected);
    }
    drop(sender);
    assert!(subscription.recv().await.unwrap().is_none());
}

#[test]
fn cancelling_a_disk_read_does_not_advance_the_subscription() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path()).unwrap());
        let writer = store
            .create("s1", SessionSnapshot::empty("s1").settings, 1)
            .unwrap();
        let mut journal = Journal::new(store.clone(), writer, false);
        let mut subscription = subscription(&journal);
        subscription.catch_up().await.unwrap();
        assert_eq!(subscription.recv().await.unwrap().unwrap().seq, 1);
        journal
            .append(vec![EventPayload::Warning {
                message: "next".into(),
            }])
            .await
            .unwrap();

        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let occupied = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        });
        ready.await.unwrap();
        let mut reading = Box::pin(subscription.catch_up());
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(reading.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(reading);
        drop(release);
        occupied.await.unwrap();
        // The abandoned read runs before this barrier in the single blocking worker.
        tokio::task::spawn_blocking(|| {}).await.unwrap();
        subscription.catch_up().await.unwrap();
        assert_eq!(
            subscription.pending.iter().cloned().collect::<Vec<_>>(),
            store.events_after("s1", 1).unwrap()
        );
    });
}
