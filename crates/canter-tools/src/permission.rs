use crate::{Action, PreparedCall, Risk, Tool};
use canter_protocol::{ApprovalDecision, ApprovalRequest, PermissionMode};
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum PermissionCheck<'a> {
    Execute(ExecutionPermit<'a>),
    Ask(ApprovalRequest),
    Deny { cancel_turn: bool },
}

/// An unforgeable, single-use permission for this exact prepared call.
pub struct ExecutionPermit<'a> {
    call: &'a PreparedCall,
}

impl<'a> ExecutionPermit<'a> {
    pub(crate) fn into_call(self) -> &'a PreparedCall {
        self.call
    }
}
impl std::fmt::Debug for ExecutionPermit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPermit").finish_non_exhaustive()
    }
}
impl PartialEq for ExecutionPermit<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.call, other.call)
    }
}

/// Own one gate per session. Grants are in-memory only and must not be restored from disk.
#[derive(Default)]
pub struct PermissionGate {
    grants: HashSet<Tool>,
}

impl PermissionGate {
    /// Review restrictions precede both permission mode and session grants.
    pub fn evaluate_review<'a>(&self, call: &'a PreparedCall) -> PermissionCheck<'a> {
        match call.tool {
            Tool::Read => PermissionCheck::Execute(ExecutionPermit { call }),
            Tool::Shell if matches!(&call.action, Action::Shell(args) if call.executor.is_readonly_shell(&args.command)) => {
                PermissionCheck::Execute(ExecutionPermit { call })
            }
            Tool::Write | Tool::Edit | Tool::Shell | Tool::AskUser => {
                PermissionCheck::Deny { cancel_turn: false }
            }
        }
    }
    /// The caller must first correlate the answer with this session's pending request.
    pub fn resolve<'a>(
        &mut self,
        call: &'a PreparedCall,
        decision: ApprovalDecision,
    ) -> Result<PermissionCheck<'a>, &'static str> {
        match decision {
            ApprovalDecision::Accept => Ok(PermissionCheck::Execute(ExecutionPermit { call })),
            ApprovalDecision::AcceptForSession => {
                self.grants.insert(call.tool);
                Ok(PermissionCheck::Execute(ExecutionPermit { call }))
            }
            ApprovalDecision::Decline => Ok(PermissionCheck::Deny { cancel_turn: false }),
            ApprovalDecision::Cancel => Ok(PermissionCheck::Deny { cancel_turn: true }),
            ApprovalDecision::Unknown => Err("invalid_decision: unknown approval answer"),
        }
    }

    pub fn evaluate<'a>(
        &self,
        call: &'a PreparedCall,
        mode: PermissionMode,
    ) -> PermissionCheck<'a> {
        if self.grants.contains(&call.tool) {
            return PermissionCheck::Execute(ExecutionPermit { call });
        }
        let ask = match mode {
            PermissionMode::AlwaysAsk | PermissionMode::Unknown => call.tool.risk() != Risk::Read,
            PermissionMode::AskForExecution => call.tool.risk() == Risk::Execute,
            PermissionMode::AllowAll => false,
        };
        let readonly_shell = ask
            && matches!(&call.action, Action::Shell(args) if call.executor.is_readonly_shell(&args.command));
        if ask && !readonly_shell {
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
            PermissionCheck::Execute(ExecutionPermit { call })
        }
    }
}
