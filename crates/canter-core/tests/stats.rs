use canter_core::{stats, usage};
use canter_store::Store;
use serde_json::{Value, json};
use std::{fs, io::Write};

struct Saved {
    root: tempfile::TempDir,
    store: Store,
    events: Vec<Value>,
    history: Vec<Value>,
}
impl Saved {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path()).unwrap();
        fs::create_dir(root.path().join("s1")).unwrap();
        let mut saved = Self {
            root,
            store,
            events: vec![],
            history: vec![],
        };
        saved.event(json!({"type":"session.updated", "settings": {
            "cwd":"/work", "model":"deepseek-flash", "name":null,
            "approvalPolicy":"allowAll", "tags":{"task":"fixture"}
        }}));
        saved.event(json!({"type":"turn.started", "turn": {
            "id":"t1","items":[],"status":"inProgress","error":null,
            "input":[{"type":"text","text":"Inspect telemetry"}],"clientName":"test","usage":null
        }}));
        saved
            .history
            .push(json!({"type":"turn_boundary", "turn_id":"t1"}));
        saved
    }
    fn event(&mut self, mut payload: Value) {
        let seq = self.events.len() + 1;
        payload["sessionId"] = json!("s1");
        payload["seq"] = json!(seq);
        payload["timestamp"] = json!(1_000 + seq * 100);
        self.events.push(payload);
    }
    fn item(&mut self, item: Value) {
        self.event(json!({"type":"item.completed", "turnId":"t1", "item":item}));
    }
    fn tool(&mut self, id: &str, name: &str, args: Value, text: &str, item: Value) {
        self.history.push(
            json!({"type":"message", "message":{"type":"assistant", "content":[
                {"type":"tool_call","id":id,"name":name,"arguments":args.to_string()}
            ]}}),
        );
        self.history.push(json!({"type":"message", "message":{"type":"tool_result","tool_call_id":id,"text":text}}));
        self.item(item);
    }
    fn save(&self) {
        for (name, records) in [
            ("events.jsonl", &self.events),
            ("model-history.jsonl", &self.history),
        ] {
            let mut file = fs::File::create(self.root.path().join("s1").join(name)).unwrap();
            for record in records {
                writeln!(file, "{record}").unwrap();
            }
        }
    }
    fn report(&self) -> canter_protocol::StatsResponse {
        self.save();
        stats::report(&self.store, "s1", &usage::builtin_prices()).unwrap()
    }
}
fn request(id: &str, purpose: &str, retries: u32) -> Value {
    json!({"requestId":id, "turnId": if purpose == "conversation" {Some("t1")} else {None},
        "purpose":purpose,"outcome":"completed","retryCount":retries,"startedAt":1200,
        "durationMs":800,"firstResponseMs":120,
        "settings":{"model":"deepseek-flash","approvalPolicy":"allowAll","planMode":false},
        "context":{"estimated":true,"systemTokens":10,"toolDefinitionTokens":20,"conversationTokens":30,"toolResultTokens":40},
        "toolCallIds":[]})
}
fn tokens() -> Value {
    json!({"inputTokens":100,"cachedInputTokens":40,"outputTokens":20,"reasoningOutputTokens":5,"totalTokens":120})
}
fn usage_event(id: Option<&str>, private: bool) -> Value {
    let mut value = json!({"type":if private {"session.btwUsage"} else {"session.usage"},
        "tokenUsage":{"last":tokens(),"total":tokens(),"contextTokens":100,"modelContextWindow":1000}});
    if let Some(id) = id {
        value["tokenUsage"]["request"] =
            json!({"requestId":id,"model":"deepseek-flash","startedAt":1200,"costUsd":0.01});
    }
    value
}
fn read_item(id: &str, path: &str, status: &str) -> Value {
    json!({"type":"fileRead","id":id,"path":path,"startLine":1,"content":"abc","status":status})
}

#[test]
fn saved_telemetry_joins_usage_tools_private_requests_and_approvals_without_writing() {
    let mut saved = Saved::new();
    saved.event(usage_event(Some("r1"), false));
    let mut r = request("r1", "conversation", 2);
    r["type"] = json!("telemetry.modelRequest");
    r["toolCallIds"] = json!(["c1", "c2"]);
    saved.event(r);
    for (id, item, status, size, truncated) in [
        ("c1", "i1", "completed", 30, false),
        ("c2", "i2", "failed", 90, true),
    ] {
        saved.tool(
            id,
            "read",
            json!({"path":"a.rs"}),
            "abc",
            read_item(item, "/work/a.rs", status),
        );
        saved.event(
            json!({"type":"telemetry.toolResult", "turnId":"t1","toolCallId":id,"itemId":item,
            "estimated":true,"resultTokens":size,"truncated":truncated}),
        );
    }
    saved.event(json!({"type":"telemetry.approval", "turnId":"t1", "requestId":"a1", "durationMs":700,"outcome":"approved"}));
    saved.event(usage_event(Some("r2"), true));
    let private = saved.root.path().join("s1/private-telemetry.jsonl");
    let r2 = request("r2", "handoff", 3);
    let mut bytes = format!("bad JSON\n{r2}\n").into_bytes();
    bytes.extend_from_slice(b"\xff\n{\"requestId\":\"torn");
    fs::write(&private, &bytes).unwrap();
    saved.save();
    let events_before = fs::read(saved.root.path().join("s1/events.jsonl")).unwrap();
    let lock = fs::File::open(saved.root.path().join("s1/events.jsonl")).unwrap();
    lock.try_lock().unwrap();
    let report = stats::report(&saved.store, "s1", &usage::builtin_prices()).unwrap();
    assert_eq!(report.session_id, "s1");
    assert_eq!(report.overview.turn_count, 1);
    assert_eq!(report.overview.request_count, 2);
    assert_eq!(report.overview.retry_count, Some(5));
    let usage = report.overview.usage.as_ref().unwrap();
    assert_eq!(usage.total.cached_input_tokens, 80);
    assert_eq!(usage.total.uncached_input_tokens, 120);
    assert_eq!(usage.total.output_tokens, 40);
    assert!((usage.total.cost.usd - 0.02).abs() < 1e-12);
    assert_eq!(report.requests[0].context_total_tokens, Some(100));
    assert_eq!(report.requests[0].first_response_ms, Some(120));
    assert_eq!(
        report.requests[1].purpose,
        canter_protocol::RequestPurpose::Handoff
    );
    assert_eq!(report.tools[0].name, "read");
    assert_eq!(report.tools[0].call_count, 2);
    assert_eq!(report.tools[0].failure_count, Some(1));
    assert_eq!(report.tools[0].truncated_count, Some(1));
    assert_eq!(report.tools[0].average_result_tokens, Some(60.0));
    assert_eq!(report.tools[0].max_result_tokens, Some(90));
    assert_eq!(report.approvals.total_duration_ms, Some(700));
    assert_eq!(report.approvals.max_duration_ms, Some(700));
    assert_eq!(fs::read(private).unwrap(), bytes);
    assert_eq!(
        fs::read(saved.root.path().join("s1/events.jsonl")).unwrap(),
        events_before
    );
}

#[test]
fn old_and_disabled_telemetry_keep_observable_counts_and_unknown_measurements() {
    for priced in [false, true] {
        let mut saved = Saved::new();
        let mut usage = usage_event(priced.then_some("unused"), false);
        if priced {
            usage["tokenUsage"]["request"]
                .as_object_mut()
                .unwrap()
                .remove("requestId");
        }
        saved.event(usage);
        saved.tool(
            "c1",
            "read",
            json!({"path":"a.rs"}),
            "abcdefg",
            read_item("i1", "/work/a.rs", "failed"),
        );
        let retry = json!({"type":"modelRetry","id":"retry1","error":{"message":"temporary","additionalDetails":null},"willRetry":true,"attempt":1,"maxRetries":2,"delaySeconds":0.1});
        saved.event(json!({"type":"item.started","turnId":"t1","item":retry}));
        saved.item(retry);
        saved.item(json!({"type":"contextCompaction", "id":"compact1", "summary":"Remember a.rs"}));
        let report = saved.report();
        assert_eq!(report.overview.request_count, 1);
        assert_eq!(report.overview.recorded_retry_count, 1);
        assert_eq!(report.overview.retry_count, Some(1));
        assert_eq!(report.requests[0].request_id, None);
        assert_eq!(report.requests[0].duration_ms, None);
        assert_eq!(report.requests[0].retry_count, None);
        assert_eq!(report.requests[0].context, None);
        assert_eq!(report.requests[0].usage.as_ref().unwrap().total_tokens, 120);
        assert_eq!(report.tools[0].failure_count, Some(1));
        assert_eq!(report.tools[0].average_result_tokens, Some(3.0));
        assert_eq!(report.tools[0].truncated_count, Some(0));
        assert_eq!(report.tools[0].unknown_status_count, 0);
        assert_eq!(report.tools[0].unknown_truncated_count, 1);
        assert_eq!(report.tools[0].unknown_result_size_count, 0);
        assert_eq!(report.compactions[0].before_tokens, None);
        assert_eq!(report.compactions[0].succeeded, None);
        assert_eq!(
            report.compactions[0].summary.as_deref(),
            Some("Remember a.rs")
        );
        assert!(!report.overview.usage.as_ref().unwrap().total.cost.unknown);
        let json = serde_json::to_value(report).unwrap();
        assert!(json["requests"][0]["firstResponseMs"].is_null());
        assert_eq!(json["tools"][0]["truncatedCount"], 0);
        assert_eq!(json["tools"][0]["unknownTruncatedCount"], 1);
    }
}

#[test]
fn compaction_signals_compare_files_and_plans_only_until_the_next_compaction() {
    let mut saved = Saved::new();
    saved.tool(
        "r1",
        "read",
        json!({"path":"src/a.rs"}),
        "abc",
        read_item("i1", "src/./a.rs", "completed"),
    );
    saved.item(
        json!({"type":"fileChange","id":"edit1","status":"completed","changes":[
            {"path":"src/a.rs","kind":{"type":"update","movePath":null},"diff":""},
            {"path":"src/b.rs","kind":{"type":"add"},"diff":""},
            {"path":"src/c.rs","kind":{"type":"add"},"diff":""}
        ]}),
    );
    saved.item(json!({"type":"plan","id":"p1","status":"completed","plan":[
        {"step":"Build","status":"inProgress"},{"step":"Test","status":"pending"}
    ]}));
    saved.item(
        json!({"type":"contextCompaction","id":"compact1","summary":"Changed src/a.rs and bc.rs"}),
    );
    saved.event(
        json!({"type":"telemetry.compaction","turnId":"t1","itemId":"compact1","trigger":"auto",
        "triggerOccupancyTokens":900,"estimated":true,"beforeTokens":1000,"afterTokens":200,
        "beforeMessages":12,"afterMessages":3,"removedMessages":10,"succeeded":true}),
    );
    saved.tool(
        "r2",
        "read",
        json!({"path":"a.rs"}),
        "abc",
        read_item("i2", "/work/src/../src/a.rs", "completed"),
    );
    saved.item(json!({"type":"plan","id":"p2","status":"failed","plan":[]}));
    saved.item(json!({"type":"plan","id":"p3","status":"completed","plan":[{"step":" Build ","status":"completed"}]}));
    saved.item(json!({"type":"contextCompaction","id":"compact2","summary":"Second summary"}));
    saved.tool(
        "r3",
        "read",
        json!({"path":"b.rs"}),
        "abc",
        read_item("i3", "/work/src/b.rs", "completed"),
    );
    let report = saved.report();
    assert_eq!(report.compactions.len(), 2);
    let first = &report.compactions[0];
    assert_eq!(first.before_tokens, Some(1000));
    assert_eq!(first.removed_messages, Some(10));
    assert_eq!(
        first.signals.reread_files.as_ref().unwrap(),
        &["/work/src/a.rs"]
    );
    assert_eq!(
        first.signals.unmentioned_files.as_ref().unwrap(),
        &["/work/src/b.rs", "/work/src/c.rs"]
    );
    assert_eq!(first.signals.plan.removed.as_ref().unwrap()[0].step, "Test");
    assert_eq!(
        first.signals.plan.status_changed.as_ref().unwrap()[0].before,
        canter_protocol::PlanStepStatus::InProgress
    );
    assert_eq!(
        first.signals.plan.status_changed.as_ref().unwrap()[0].after,
        canter_protocol::PlanStepStatus::Completed
    );
    assert!(report.compactions[1].signals.plan.after.is_none());
    assert!(report.compactions[1].signals.plan.removed.is_none());
}

#[test]
fn missing_usage_and_unmeasured_plan_approval_remain_unknown() {
    let mut saved = Saved::new();
    saved.event(json!({"type":"request.opened", "request":{"id":"plan-approval", "turnId":"t1", "createdAt":1300,
        "kind":{"type":"planApproval","itemId":"p1","plan":"Do the work","approvalPolicy":"allowAll"}}}));
    saved.event(
        json!({"type":"request.opened", "request":{"id":"question", "turnId":"t1", "createdAt":1400,
        "kind":{"type":"userInput","itemId":"q1","question":"Which file?","options":[]}}}),
    );
    let report = saved.report();
    let json = serde_json::to_value(&report).unwrap();
    assert!(
        json["overview"]["usage"].is_null(),
        "no recorded usage is not zero usage"
    );
    assert_eq!(report.approvals.count, 1);
    assert_eq!(report.approvals.total_duration_ms, None);
    assert_eq!(report.approvals.max_duration_ms, None);
}

#[test]
fn compaction_still_in_progress_does_not_report_zero_effects() {
    let mut saved = Saved::new();
    saved.event(json!({"type":"item.started","turnId":"t1","item":{
        "type":"contextCompaction","id":"pending-compaction","summary":""
    }}));
    let report = saved.report();
    let pending = &report.compactions[0];
    assert_eq!(pending.summary, None);
    assert_eq!(pending.succeeded, None);
    assert_eq!(pending.signals.reread_files, None);
    assert_eq!(pending.signals.unmentioned_files, None);
}

#[test]
fn chinese_summary_mentions_adjacent_paths_and_names_without_matching_ascii_suffixes() {
    let mut saved = Saved::new();
    saved.item(
        json!({"type":"fileChange","id":"edit-cn","status":"completed","changes":[
            {"path":"src/b.rs","kind":{"type":"add"},"diff":""},
            {"path":"src/c.rs","kind":{"type":"add"},"diff":""},
            {"path":"src/d.rs","kind":{"type":"add"},"diff":""},
            {"path":"src/a.rs","kind":{"type":"add"},"diff":""}
        ]}),
    );
    saved.item(json!({"type":"contextCompaction","id":"compact-cn",
        "summary":"已修改src/b.rs和c.rs，另外 `src/d.rs` 也改了。还提到ba.rs。"}));
    assert_eq!(
        saved.report().compactions[0]
            .signals
            .unmentioned_files
            .as_ref()
            .unwrap(),
        &["/work/src/a.rs"]
    );
}

#[test]
fn legacy_failed_compaction_cards_report_false_and_no_signals_without_telemetry() {
    for summary in [
        "",
        "Compaction did not finish; the original context is kept.",
    ] {
        let mut saved = Saved::new();
        saved.tool(
            "read-before",
            "read",
            json!({"path":"src/b.rs"}),
            "abc",
            read_item("read-before", "src/b.rs", "completed"),
        );
        saved.item(
            json!({"type":"fileChange","id":"edit-before","status":"completed","changes":[
                {"path":"src/b.rs","kind":{"type":"add"},"diff":""}
            ]}),
        );
        saved.item(json!({"type":"plan","id":"plan-before","status":"completed","plan":[{"step":"Build","status":"inProgress"}]}));
        saved.item(json!({"type":"contextCompaction","id":"failed-compact","summary":summary}));
        saved.tool(
            "read-after",
            "read",
            json!({"path":"src/b.rs"}),
            "abc",
            read_item("read-after", "src/b.rs", "completed"),
        );
        saved.item(json!({"type":"plan","id":"plan-after","status":"completed","plan":[]}));
        let report = saved.report();
        assert_eq!(report.compactions[0].succeeded, Some(false), "{summary:?}");
        assert_eq!(
            report.compactions[0].signals,
            canter_protocol::CompactionSignals::default()
        );
    }
}

#[test]
fn unfinished_compaction_does_not_label_later_turns_as_compaction() {
    for (complete_previous_turn, start_next_turn) in [(false, true), (true, false), (true, true)] {
        let mut saved = Saved::new();
        saved.event(json!({"type":"item.started","turnId":"t1","item":{
            "type":"contextCompaction","id":"unfinished","summary":""
        }}));
        saved.event(usage_event(None, false));
        let mut turn = saved.events[1]["turn"].clone();
        if complete_previous_turn {
            turn["status"] = json!("interrupted");
            saved.event(json!({"type":"turn.completed","turn":turn}));
        }
        if start_next_turn {
            turn["id"] = json!("t2");
            turn["status"] = json!("inProgress");
            saved.event(json!({"type":"turn.started","turn":turn}));
        }
        saved.event(usage_event(None, false));
        let report = saved.report();
        assert_eq!(
            report.requests[0].purpose,
            canter_protocol::RequestPurpose::Compaction
        );
        assert_eq!(
            report.requests[1].purpose,
            canter_protocol::RequestPurpose::Conversation
        );
        assert_eq!(
            report.requests[1].turn_id.as_deref(),
            start_next_turn.then_some("t2")
        );
    }
}

#[test]
fn partial_tool_statistics_keep_known_values_and_count_unknown_calls() {
    for known_calls in [0, 3] {
        let mut saved = Saved::new();
        for index in 0..known_calls {
            let call_id = format!("c{index}");
            let item_id = format!("i{index}");
            saved.tool(
                &call_id,
                "read",
                json!({"path":"a.rs"}),
                "abc",
                read_item(&item_id, "a.rs", "failed"),
            );
            saved.event(json!({"type":"telemetry.toolResult", "turnId":"t1","toolCallId":call_id,"itemId":item_id,
                "estimated":true,"resultTokens":(index + 1) * 3,"truncated":true}));
        }
        saved.history.push(json!({"type":"message","message":{"type":"assistant","content":[
            {"type":"tool_call","id":"unobserved","name":"read","arguments":"{\"path\":\"a.rs\"}"}
        ]}}));
        let report = saved.report();
        let tool = &report.tools[0];
        assert_eq!(tool.call_count, known_calls + 1);
        assert_eq!(tool.failure_count, Some(known_calls));
        assert_eq!(tool.truncated_count, Some(known_calls));
        assert_eq!(tool.average_result_tokens, (known_calls > 0).then_some(6.0));
        assert_eq!(tool.max_result_tokens, (known_calls > 0).then_some(9));
        let json = serde_json::to_value(tool).unwrap();
        assert_eq!(json["unknownStatusCount"], 1);
        assert_eq!(json["unknownTruncatedCount"], 1);
        assert_eq!(json["unknownResultSizeCount"], 1);
    }
}

#[test]
fn failed_and_unfinished_compactions_do_not_shorten_successful_observation_windows() {
    for telemetry in [false, true] {
        let mut saved = Saved::new();
        saved.item(read_item("read-a-before", "a.rs", "completed"));
        saved.item(read_item("read-b-before", "b.rs", "completed"));
        saved.item(
            json!({"type":"plan","id":"plan-before","status":"completed","plan":[
                {"step":"Build","status":"inProgress"},{"step":"Test","status":"pending"}
            ]}),
        );
        for (id, summary) in [
            ("first", "Remember a.rs and b.rs"),
            (
                "failed",
                if telemetry {
                    "Kept the context"
                } else {
                    "Compaction did not finish; the original context is kept."
                },
            ),
        ] {
            saved.item(json!({"type":"contextCompaction","id":id,"summary":summary}));
            if telemetry {
                saved.event(json!({"type":"telemetry.compaction","turnId":"t1","itemId":id,
                    "trigger":"manual","estimated":true,"beforeTokens":100,"afterTokens":50,
                    "beforeMessages":4,"afterMessages":2,"removedMessages":if id == "first" { 3 } else { 0 },
                    "succeeded":id == "first"}));
            }
        }
        saved.event(json!({"type":"item.started","turnId":"t1","item":{
            "type":"contextCompaction","id":"unfinished","summary":""
        }}));
        saved.item(read_item("read-a-after", "a.rs", "completed"));
        saved.item(
            json!({"type":"plan","id":"plan-after","status":"completed","plan":[
                {"step":"Build","status":"completed"}
            ]}),
        );
        saved.item(json!({"type":"contextCompaction","id":"next","summary":"Another summary"}));
        if telemetry {
            saved.event(
                json!({"type":"telemetry.compaction","turnId":"t1","itemId":"next",
                "trigger":"manual","estimated":true,"beforeTokens":100,"afterTokens":50,
                "beforeMessages":4,"afterMessages":2,"removedMessages":3,"succeeded":true}),
            );
        }
        saved.item(read_item("read-b-after-next", "b.rs", "completed"));
        saved.item(json!({"type":"plan","id":"plan-after-next","status":"completed","plan":[]}));
        let report = saved.report();
        assert_eq!(report.compactions.len(), 4);
        let first = &report.compactions[0].signals;
        assert_eq!(first.reread_files.as_ref().unwrap(), &["/work/a.rs"]);
        assert_eq!(first.plan.after.as_ref().unwrap()[0].step, "Build");
        assert_eq!(first.plan.removed.as_ref().unwrap()[0].step, "Test");
        assert_eq!(
            first.plan.status_changed.as_ref().unwrap()[0].after,
            canter_protocol::PlanStepStatus::Completed
        );
        for row in &report.compactions[1..3] {
            assert_eq!(row.signals, canter_protocol::CompactionSignals::default());
        }
        assert_eq!(
            report.compactions[3].signals.reread_files.as_ref().unwrap(),
            &["/work/b.rs"]
        );
    }
}
