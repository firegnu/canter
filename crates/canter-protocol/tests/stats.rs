use canter_protocol::StatsResponse;

#[test]
fn old_stats_reports_default_to_no_subagents_without_changing_usage() {
    let report = StatsResponse::default();
    let mut old = serde_json::to_value(&report).unwrap();
    old.as_object_mut().unwrap().remove("parent");
    old.as_object_mut().unwrap().remove("subAgents");
    old["overview"]
        .as_object_mut()
        .unwrap()
        .remove("usageWithSubAgents");
    let decoded: StatsResponse = serde_json::from_value(old).unwrap();
    assert_eq!(decoded, report);
}
