use canter_protocol::PermissionMode;
use canter_tools::{ExecutionPermit, PermissionCheck, PermissionGate, PreparedCall};

pub fn allow(call: &PreparedCall) -> ExecutionPermit<'_> {
    match PermissionGate::default().evaluate(call, PermissionMode::AllowAll) {
        PermissionCheck::Execute(permit) => permit,
        _ => panic!("allowAll must issue a permit"),
    }
}
