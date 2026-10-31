use super::*;

#[tokio::test]
async fn non_interactive_background_completion_does_not_start_a_turn() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"nonInteractive":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.say(&url, "wake-background").await;
    fixture.release("release-wake");
    fixture
        .until(&url, |s| !s.background_commands[0].running)
        .await;
    fixture.no_extra_turn(&url, 1).await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"nonInteractive":false}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .say(&url, "Inspect the saved completion notice.")
        .await;
    std::fs::remove_file(fixture.root.path().join("project/release-wake")).unwrap();
    fixture.say(&url, "wake-background").await;
    fixture.release("release-wake");
    fixture.turns(&url, 4).await;
    fixture.close().await;
}

impl Fixture {
    fn release(&self, name: &str) {
        std::fs::write(self.root.path().join("project").join(name), "").unwrap();
    }

    async fn turns(&self, url: &str, count: usize) -> SessionSnapshot {
        self.until(url, |s| {
            s.turns.len() == count && s.status == SessionStatus::Idle
        })
        .await
    }

    async fn output_contains(&self, url: &str, needle: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let output: BackgroundCommandOutput = self
                    .client
                    .get(format!("{url}/backgroundCommands/1"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if output.output.contains(needle) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command did not produce expected output");
    }

    async fn no_extra_turn(&self, url: &str, count: usize) {
        tokio::time::sleep(Duration::from_millis(2300)).await;
        let snapshot: SessionReadResponse = self
            .client
            .get(url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(snapshot.session.turns.len(), count);
    }

    async fn interrupt_turn(&self, url: &str, id: &str, retract: bool) -> SessionInterruptResponse {
        self.client
            .post(format!("{url}/interrupt"))
            .json(&json!({"turnId":id,"retract":retract}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn cards(snapshot: &SessionSnapshot) -> Vec<&str> {
    snapshot
        .turns
        .iter()
        .flat_map(|turn| &turn.items)
        .filter_map(|item| {
            if let Item::BackgroundEvent { text, .. } = item {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

async fn completion_during_turn(action: &str, interrupt: bool) {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    match action {
        "reply" => {
            fixture.input(&url, "finish-later").await;
            fixture
                .until(&url, |s| {
                    s.turns.last().unwrap().items.iter().any(
                        |item| matches!(item, Item::AgentMessage { text, .. } if text == "working"),
                    )
                })
                .await;
        }
        "shell" => {
            fixture.client.post(format!("{url}/shellCommand"))
                .json(&json!({"command":"printf ready; while [ ! -e release-shell ]; do sleep 0.01; done"}))
                .send().await.unwrap().error_for_status().unwrap();
            fixture.until(&url, |s| s.turns.last().unwrap().items.iter().any(|item| {
                matches!(item, Item::CommandExecution { aggregated_output, .. } if aggregated_output == "ready")
            })).await;
        }
        "compact" => {
            fixture.pause_compaction.store(true, Ordering::Relaxed);
            fixture
                .client
                .post(format!("{url}/compact"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            fixture
                .until(&url, |_| {
                    fixture
                        .requests
                        .lock()
                        .unwrap()
                        .last()
                        .unwrap()
                        .get("tools")
                        .is_none()
                })
                .await;
        }
        _ => unreachable!(),
    }
    fixture.release("release-wake");
    let busy = fixture
        .until(&url, |s| !s.background_commands[0].running)
        .await;
    fixture.no_extra_turn(&url, 2).await;
    assert!(cards(&busy).is_empty());
    if interrupt {
        fixture.interrupt_turn(&url, &busy.turns[1].id, false).await;
        fixture.no_extra_turn(&url, 2).await;
        assert_eq!(fixture.requests.lock().unwrap().len(), 3);
    } else {
        match action {
            "reply" => fixture.release_reply.notify_one(),
            "shell" => fixture.release("release-shell"),
            "compact" => fixture.release_compaction.notify_one(),
            _ => unreachable!(),
        }
        let snapshot = fixture.turns(&url, 3).await;
        let wake = &snapshot.turns[2];
        assert!(wake.input.is_empty());
        assert!(
            !wake
                .items
                .iter()
                .any(|item| matches!(item, Item::UserMessage { .. }))
        );
        assert_eq!(cards(&snapshot).len(), 1);
        assert!(cards(&snapshot)[0].contains("Exit code: 7"));
        let requests = fixture.requests.lock().unwrap().clone();
        assert_eq!(
            requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| {
                    m["content"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("[Background command 1 finished:"))
                })
                .count(),
            1
        );
        fixture.no_extra_turn(&url, 3).await;
    }
    fixture.close().await;
}

#[tokio::test]
async fn completion_during_last_response_wakes_after_turn_finishes() {
    completion_during_turn("reply", false).await;
}

#[tokio::test]
async fn completion_during_last_response_does_not_wake_after_interrupt() {
    completion_during_turn("reply", true).await;
}

#[tokio::test]
async fn completion_during_user_shell_wakes_after_turn_finishes() {
    completion_during_turn("shell", false).await;
}

#[tokio::test]
async fn completion_during_manual_compaction_wakes_after_turn_finishes() {
    completion_during_turn("compact", false).await;
}

#[tokio::test]
async fn matches_after_wake_limit_stay_in_one_bounded_notice() {
    let fixture = Fixture::with_wake_limit(Some(1)).await;
    let url = fixture.create("allowAll").await;
    fixture.shell(&url, "while [ ! -e first ]; do sleep 0.01; done; printf 'error first\n'; batch=1; while [ $batch -le 3 ]; do while [ ! -e batch-$batch ]; do sleep 0.01; done; line=1; while [ $line -le 15 ]; do printf 'error queued %s-%s\n' $batch $line; line=$((line+1)); done; batch=$((batch+1)); done; exec sleep 30 &").await;
    fixture.say(&url, "monitor-errors").await;
    fixture.release("first");
    fixture.turns(&url, 3).await;
    for batch in 1..=3 {
        fixture.release(&format!("batch-{batch}"));
        fixture
            .output_contains(&url, &format!("error queued {batch}-15"))
            .await;
        fixture.no_extra_turn(&url, 3).await;
    }
    let snapshot = fixture.say(&url, "collect pending matches").await.session;
    let requests = fixture.requests.lock().unwrap().clone();
    let notices: Vec<_> = requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .filter(|text| {
            text.starts_with("[Background command 1 output matched")
                && text.contains("error queued")
        })
        .collect();
    assert_eq!(
        notices.len(),
        1,
        "pending matches must stay in one bounded notice"
    );
    assert!(notices[0].contains("Skipped 25 matching lines"));
    assert_eq!(
        notices[0]
            .lines()
            .filter(|line| line.starts_with("error queued"))
            .count(),
        20
    );
    assert!(notices[0].contains("error queued 2-11\n"));
    assert!(notices[0].contains("error queued 3-15\n"));
    assert_eq!(
        cards(&snapshot)
            .iter()
            .filter(|text| text.contains("error queued"))
            .count(),
        1
    );
    fixture.no_extra_turn(&url, 4).await;
    fixture.close().await;
}

#[tokio::test]
async fn promoted_foreground_preserves_model_or_user_origin_for_waking() {
    for model in [true, false] {
        let fixture = Fixture::new().await;
        let url = fixture.create("allowAll").await;
        if model {
            fixture.input(&url, "wake-foreground").await;
        } else {
            fixture.client.post(format!("{url}/shellCommand"))
                .json(&json!({"command":"printf ready; while [ ! -e release-wake ]; do sleep 0.01; done; exit 7"}))
                .send().await.unwrap().error_for_status().unwrap();
        }
        let running = fixture.until(&url, |s| s.turns.iter().flat_map(|turn| &turn.items).any(|item|
            matches!(item, Item::CommandExecution { aggregated_output, .. } if aggregated_output.contains("ready")))).await;
        let id = running.turns[0]
            .items
            .iter()
            .find_map(|item| {
                if let Item::CommandExecution { id, .. } = item {
                    Some(id)
                } else {
                    None
                }
            })
            .unwrap();
        fixture
            .client
            .post(format!("{url}/shellBackground"))
            .json(&json!({"itemId":id}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fixture.idle(&url).await;
        fixture.release("release-wake");
        fixture
            .until(&url, |s| !s.background_commands[0].running)
            .await;
        if model {
            fixture.turns(&url, 2).await;
        } else {
            fixture.no_extra_turn(&url, 1).await;
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn two_completions_within_two_seconds_share_one_wake() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    fixture.say(&url, "wake-background").await;
    fixture.release("release-wake");
    let snapshot = fixture.turns(&url, 3).await;
    assert_eq!(
        snapshot.turns[2]
            .items
            .iter()
            .filter(|item| matches!(item, Item::BackgroundEvent { .. }))
            .count(),
        2
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 5);
    fixture.no_extra_turn(&url, 3).await;
    fixture.close().await;
}

#[tokio::test]
async fn user_input_takes_priority_over_pending_wake_and_carries_the_event() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    fixture.release("release-wake");
    fixture
        .until(&url, |s| !s.background_commands[0].running)
        .await;
    let snapshot = fixture.say(&url, "user takes priority").await;
    assert_eq!(snapshot.session.turns.len(), 2);
    assert_eq!(
        snapshot.session.turns[1].input,
        vec![UserInput::Text {
            text: "user takes priority".into()
        }]
    );
    assert_eq!(cards(&snapshot.session).len(), 1);
    let requests = fixture.requests.lock().unwrap().clone();
    assert!(
        requests.last().unwrap()["messages"]
            .to_string()
            .contains("Exit code: 7")
    );
    fixture.no_extra_turn(&url, 2).await;
    fixture.close().await;
}

#[tokio::test]
async fn completion_while_streaming_or_waiting_does_not_start_another_turn() {
    for action in ["wait", "approve", "question"] {
        let fixture = Fixture::new().await;
        let url = fixture.create("allowAll").await;
        fixture.say(&url, "wake-background").await;
        if action == "approve" {
            fixture
                .client
                .patch(&url)
                .json(&json!({"approvalPolicy":"alwaysAsk"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        fixture.input(&url, action).await;
        let waiting = fixture
            .until(&url, |s| {
                s.turns.len() == 2
                    && if action == "wait" {
                        fixture.requests.lock().unwrap().len() == 3
                    } else {
                        !s.pending_requests.is_empty()
                    }
            })
            .await;
        fixture.release("release-wake");
        fixture
            .until(&url, |s| !s.background_commands[0].running)
            .await;
        fixture.no_extra_turn(&url, 2).await;
        assert_eq!(fixture.requests.lock().unwrap().len(), 3);
        if action == "wait" {
            fixture
                .interrupt_turn(&url, &waiting.turns[1].id, false)
                .await;
            fixture.say(&url, "after interrupt").await;
        } else {
            let answer = if action == "approve" {
                json!({"decision":"accept","clientName":"test"})
            } else {
                json!({"answer":{"type":"text","text":"A"},"clientName":"test"})
            };
            fixture
                .client
                .post(format!("{url}/requests/{}", waiting.pending_requests[0].id))
                .json(&answer)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            fixture.idle(&url).await;
        }
        let requests = fixture.requests.lock().unwrap().clone();
        assert_eq!(
            requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("[Background command 1 finished:")))
                .count(),
            1
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn monitor_replaces_cancels_and_keeps_its_condition_after_invalid_regex() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.shell(&url, "while [ ! -e stage1 ]; do sleep 0.01; done; printf 'error first\n'; while [ ! -e stage2 ]; do sleep 0.01; done; printf 'error ignored\nwarning second\n'; while [ ! -e stage3 ]; do sleep 0.01; done; printf 'warning cancelled\n' &").await;
    fixture.say(&url, "monitor-errors").await;
    fixture.say(&url, "monitor-invalid").await;
    fixture.release("stage1");
    let first = fixture.turns(&url, 4).await;
    assert!(
        cards(&first)
            .iter()
            .any(|text| text.contains("error first"))
    );
    fixture.say(&url, "monitor-warnings").await;
    fixture.release("stage2");
    let second = fixture.turns(&url, 6).await;
    let matched = cards(&second).last().unwrap().to_string();
    assert!(
        matched.contains("warning second") && !matched.contains("error ignored"),
        "{matched}"
    );
    fixture.say(&url, "monitor-off").await;
    fixture.release("stage3");
    fixture
        .until(&url, |s| !s.background_commands[0].running)
        .await;
    fixture.no_extra_turn(&url, 7).await;
    fixture.close().await;
}

#[tokio::test]
async fn monitored_user_exit_wakes_once_but_explicit_stops_do_not() {
    for stop in ["natural", "tool", "one", "all"] {
        let fixture = Fixture::new().await;
        let url = fixture.create("allowAll").await;
        fixture
            .shell(
                &url,
                "while [ ! -e release-wake ]; do sleep 0.01; done; exit 0 &",
            )
            .await;
        fixture.say(&url, "monitor-errors").await;
        match stop {
            "natural" => {
                fixture.release("release-wake");
                fixture.turns(&url, 3).await;
            }
            "tool" => {
                fixture.say(&url, "stop").await;
            }
            _ => {
                let path = if stop == "one" {
                    "backgroundCommands/1/stop"
                } else {
                    "backgroundCommands/stop"
                };
                fixture
                    .client
                    .post(format!("{url}/{path}"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
            }
        }
        fixture
            .until(&url, |s| !s.background_commands[0].running)
            .await;
        fixture
            .no_extra_turn(
                &url,
                if matches!(stop, "natural" | "tool") {
                    3
                } else {
                    2
                },
            )
            .await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn ten_consecutive_wakes_pause_and_user_input_resets_the_limit() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.shell(&url, "i=1; while [ $i -le 12 ]; do while [ ! -e tick-$i ]; do sleep 0.01; done; printf 'error tick-%s\n' $i; i=$((i+1)); done; exec sleep 30 &").await;
    fixture.say(&url, "monitor-errors").await;
    for count in 1..=10 {
        fixture.release(&format!("tick-{count}"));
        fixture.turns(&url, 2 + count).await;
    }
    let paused = fixture.idle(&url).await.session;
    assert_eq!(
        cards(&paused)
            .iter()
            .filter(|text| text.contains("Auto wake paused"))
            .count(),
        1
    );
    assert!(
        cards(&paused).last().unwrap().contains("10 consecutive")
            && cards(&paused).last().unwrap().contains("send a message")
    );
    fixture.release("tick-11");
    fixture.output_contains(&url, "error tick-11").await;
    fixture.no_extra_turn(&url, 12).await;
    fixture.say(&url, "reset").await;
    assert!(
        fixture.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("error tick-11")
    );
    fixture.release("tick-12");
    fixture.turns(&url, 14).await;
    fixture.close().await;
}

#[tokio::test]
async fn wake_can_be_interrupted_without_retracting_or_repeating_its_event() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    fixture.pause_wakes.store(true, Ordering::Relaxed);
    fixture.release("release-wake");
    let working = fixture
        .until(&url, |s| {
            s.turns.len() == 2 && fixture.requests.lock().unwrap().len() == 3
        })
        .await;
    let interrupted = fixture
        .interrupt_turn(&url, &working.turns[1].id, true)
        .await;
    assert_eq!(interrupted.retracted, Some(false));
    let snapshot = fixture.idle(&url).await.session;
    assert_eq!(snapshot.turns[1].status, TurnStatus::Interrupted);
    assert_eq!(cards(&snapshot).len(), 1);
    fixture.no_extra_turn(&url, 2).await;
    fixture.pause_wakes.store(false, Ordering::Relaxed);
    fixture.assert_background_notice(&url, &snapshot.id).await;
    fixture.close().await;
}

#[tokio::test]
async fn plan_mode_allows_monitor_and_restricts_tools_in_the_wake_turn() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.shell(&url, "while [ ! -e release-match ]; do sleep 0.01; done; printf 'error plan\n'; exec sleep 30 &").await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"planMode":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.say(&url, "monitor-errors").await;
    *fixture.wake_tool.lock().unwrap() = Some((
        "write".into(),
        json!({"path":"forbidden","content":"must not write"}),
    ));
    fixture.release("release-match");
    let snapshot = fixture.turns(&url, 3).await;
    assert!(snapshot.pending_requests.is_empty());
    assert!(!fixture.root.path().join("project/forbidden").exists());
    assert!(snapshot.turns[2].items.iter().any(|item| matches!(
        item,
        Item::FileChange {
            status: ItemStatus::Blocked,
            ..
        }
    )));
    fixture.close().await;
}

#[tokio::test]
async fn shutdown_during_debounce_never_starts_a_model_turn_or_restores_a_wake() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    let started = fixture.say(&url, "wake-background").await.session;
    fixture.release("release-wake");
    fixture
        .until(&url, |s| !s.background_commands[0].running)
        .await;
    let session = fixture.app.sessions.lock().await[&started.id].clone();
    session.drain().await;
    fixture.no_extra_turn(&url, 1).await;
    fixture.reopen(&url, &started.id).await;
    fixture.no_extra_turn(&url, 1).await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    fixture.close().await;
}

#[tokio::test]
async fn matching_batch_keeps_twenty_lines_and_four_kib_without_consuming_output() {
    for contents in [
        (1..=25).map(|i| format!("error {i}\n")).collect::<String>(),
        format!("error {}\n", "字".repeat(3000)),
    ] {
        let fixture = Fixture::new().await;
        let url = fixture.create("allowAll").await;
        std::fs::write(fixture.root.path().join("project/matches"), &contents).unwrap();
        fixture
            .shell(
                &url,
                "while [ ! -e release-match ]; do sleep 0.01; done; cat matches; exec sleep 30 &",
            )
            .await;
        fixture.say(&url, "monitor-errors").await;
        fixture.release("release-match");
        let snapshot = fixture.turns(&url, 3).await;
        let text = cards(&snapshot).last().unwrap().to_string();
        assert!(text.contains("Skipped"), "{text}");
        let (_, output) = text.split_once("showing the tail.\n").unwrap();
        let output = output.split_once("\nUse shell_output").unwrap().0;
        assert!(output.len() <= 4096 && output.lines().count() <= 20);
        if contents.len() < 4096 {
            assert!(
                text.contains("Skipped 5 matching lines")
                    && output.starts_with("error 6\n")
                    && output.ends_with("error 25\n"),
                "{text}"
            );
        } else {
            assert!(output.contains('字') && !output.contains('\u{fffd}'));
        }
        fixture.say(&url, "output").await;
        let requests = fixture.requests.lock().unwrap().clone();
        assert!(
            requests.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.ends_with(&contents)))
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn matching_event_during_compaction_survives_and_is_not_user_words() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.shell(&url, "while [ ! -e release-match ]; do sleep 0.01; done; printf 'error during compact\n'; exec sleep 30 &").await;
    fixture.say(&url, "monitor-errors").await;
    fixture.pause_compaction.store(true, Ordering::Relaxed);
    fixture
        .client
        .post(format!("{url}/compact"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .until(&url, |_| {
            fixture
                .requests
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .get("tools")
                .is_none()
        })
        .await;
    fixture.release("release-match");
    fixture.output_contains(&url, "error during compact").await;
    // The batch must stay pending while the summary is being generated.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    fixture.release_compaction.notify_one();
    let woken = fixture.turns(&url, 4).await;
    assert!(woken.turns[3].input.is_empty());
    assert_eq!(cards(&woken).len(), 1);
    fixture.say(&url, "after compaction").await;
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(
        requests.last().unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("[Background command 1 output matched")))
            .count(),
        1
    );
    fixture.pause_compaction.store(false, Ordering::Relaxed);
    fixture
        .client
        .post(format!("{url}/compact"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let compacted = fixture.idle(&url).await;
    let requests = fixture.requests.lock().unwrap().clone();
    let transcript = requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        transcript.contains("Background event: [Background command 1 output matched"),
        "{transcript}"
    );
    assert!(
        !transcript.contains("User: [Background command"),
        "{transcript}"
    );
    fixture.reopen(&url, &compacted.session.id).await;
    assert_eq!(
        fixture.idle(&url).await.session.turns,
        compacted.session.turns
    );
    fixture.close().await;
}
