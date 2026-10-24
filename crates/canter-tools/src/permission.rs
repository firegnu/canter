use crate::{PreparedCall, Risk, Tool};
use canter_protocol::{ApprovalDecision, ApprovalRequest, PermissionMode};
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum PermissionCheck {
    Execute,
    Ask(ApprovalRequest),
    Deny { cancel_turn: bool },
}

/// Own one gate per session. Grants are in-memory only and must not be restored from disk.
#[derive(Default)]
pub struct PermissionGate {
    grants: HashSet<Tool>,
}

impl PermissionGate {
    /// The caller must first correlate the answer with this session's pending request.
    pub fn resolve(
        &mut self,
        call: &PreparedCall,
        decision: ApprovalDecision,
    ) -> Result<PermissionCheck, &'static str> {
        match decision {
            ApprovalDecision::Accept => Ok(PermissionCheck::Execute),
            ApprovalDecision::AcceptForSession => {
                self.grants.insert(call.tool);
                Ok(PermissionCheck::Execute)
            }
            ApprovalDecision::Decline => Ok(PermissionCheck::Deny { cancel_turn: false }),
            ApprovalDecision::Cancel => Ok(PermissionCheck::Deny { cancel_turn: true }),
            ApprovalDecision::Unknown => Err("invalid_decision: unknown approval answer"),
        }
    }

    pub fn evaluate(&self, call: &PreparedCall, mode: PermissionMode) -> PermissionCheck {
        if self.grants.contains(&call.tool) {
            return PermissionCheck::Execute;
        }
        let ask = match mode {
            PermissionMode::AlwaysAsk | PermissionMode::Unknown => call.tool.risk() != Risk::Read,
            PermissionMode::AskForExecution => call.tool.risk() == Risk::Execute,
            PermissionMode::AllowAll => false,
        };
        if ask {
            PermissionCheck::Ask(ApprovalRequest {
                item_id: call.id.clone(),
                tool: call.tool.name().into(),
                arguments: call.arguments.clone(),
                available_decisions: vec![
                    ApprovalDecision::Accept,
                    ApprovalDecision::AcceptForSession,
                    ApprovalDecision::Decline,
                    ApprovalDecision::Cancel,
                ],
            })
        } else {
            PermissionCheck::Execute
        }
    }
}
