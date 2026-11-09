use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[test]
fn plan_serialization_uses_tool_statuses_and_skips_unknown_steps() {
    let plan: Vec<canter_protocol::PlanStep> = serde_json::from_value(json!([
        {"step":"Read", "status":"completed"},
        {"step":"Future step", "status":"futureStatus"},
        {"step":"Implement", "status":"inProgress"},
        {"step":"Verify", "status":"pending"}
    ]))
    .unwrap();
    assert_eq!(
        canter_tools::serialize_plan(&plan),
        json!([
            {"step":"Read", "status":"completed"},
            {"step":"Implement", "status":"in_progress"},
            {"step":"Verify", "status":"pending"}
        ])
    );
    assert_eq!(canter_tools::serialize_plan(&[]), json!([]));
}

#[tokio::test]
async fn invalid_plan_steps_return_errors_instead_of_a_successful_update() {
    let root = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    for plan in [
        json!([{"step":"Read", "status":"in_progress"}, {"step":"Implement", "status":"in_progress"}]),
        json!([{"step":"", "status":"pending"}]),
        json!([{"step":" \n\t", "status":"pending"}]),
        json!([{"step":"Read", "status":"invented"}]),
        json!([{"step":"Read", "status":"inProgress"}]),
    ] {
        let result = tools
            .prepare(
                Tool::from_name("update_plan").unwrap(),
                "bad-plan",
                &json!({"plan":plan}).to_string(),
                &CancellationToken::new(),
            )
            .await;
        let Err(error) = result else {
            panic!("invalid plan was accepted: {plan}");
        };
        assert_eq!(error.outcome, ToolOutcome::Failed);
        assert!(!error.text.is_empty());
        assert_eq!(
            serde_json::to_value(error.item).unwrap()["status"],
            "failed"
        );
    }
}

#[tokio::test]
async fn update_plan_accepts_description_in_place_of_step() {
    let root = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let gate = PermissionGate::default();
    let tool = Tool::from_name("update_plan").expect("update_plan is a built-in tool");
    let mut items = Vec::new();
    for plan in [
        json!([
            {"step":"Read the code", "status":"completed"},
            {"step":"Implement", "status":"in_progress"}
        ]),
        json!([
            {"description":"Read the code", "status":"completed"},
            {"description":"Implement", "status":"in_progress"}
        ]),
    ] {
        let call = tools
            .prepare(tool, "plan", &json!({"plan":plan}).to_string(), &cancel)
            .await
            .unwrap();
        let PermissionCheck::Execute(permit) = gate.evaluate(&call, PermissionMode::AllowAll)
        else {
            panic!("update_plan must not ask for approval");
        };
        let result = tools.execute(permit, &cancel, &mut |_| {}).await;
        assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
        items.push(serde_json::to_value(result.item).unwrap());
    }
    assert_eq!(items[0], items[1]);

    let raw =
        json!({"plan":[{"step":"Read", "description":"Read", "status":"pending"}]}).to_string();
    let error = match tools.prepare(tool, "plan", &raw, &cancel).await {
        Err(error) => error,
        Ok(_) => panic!("a step with both step and description must be rejected"),
    };
    assert_eq!(error.outcome, ToolOutcome::Failed);
    assert!(
        error.text.contains("step") && error.text.contains("description"),
        "{}",
        error.text
    );
}

#[tokio::test]
async fn update_plan_returns_a_complete_checklist_without_approval_in_every_mode() {
    let root = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let tool = Tool::from_name("update_plan").expect("update_plan is a built-in tool");
    assert_eq!(
        tool.definition().parameters["properties"]["plan"]["items"]["properties"]["status"]["enum"],
        json!(["pending", "in_progress", "completed"])
    );
    let args = json!({"explanation":"Starting the work", "plan":[
        {"step":"Read the code", "status":"completed"},
        {"step":"Implement", "status":"in_progress"},
        {"step":"Verify", "status":"pending"}
    ]});
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(tool, "plan-1", &args.to_string(), &cancel)
        .await
        .unwrap();
    let gate = PermissionGate::default();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
        PermissionMode::Unknown,
    ] {
        let PermissionCheck::Execute(permit) = gate.evaluate(&call, mode) else {
            panic!("update_plan must not ask for approval in {mode:?}");
        };
        let result = tools.execute(permit, &cancel, &mut |_| {}).await;
        assert_eq!(result.outcome, ToolOutcome::Completed);
        assert_eq!(result.text, "Plan updated");
        let item = serde_json::to_value(result.item).unwrap();
        assert_eq!(item["type"], "plan");
        assert_eq!(
            item["plan"],
            json!([
                {"step":"Read the code", "status":"completed"},
                {"step":"Implement", "status":"inProgress"},
                {"step":"Verify", "status":"pending"}
            ])
        );
        assert_eq!(item["explanation"], args["explanation"]);
        assert_eq!(item["status"], "completed");
    }
}
