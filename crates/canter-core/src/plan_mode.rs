use crate::{
    CoreError,
    session::{Shared, State, id},
};
use canter_model::Message;
use canter_protocol::*;
use canter_tools::{PreparedCall, ToolOutcome, ToolOutput};
use tokio_util::sync::CancellationToken;

const ENTER: &str = "[Plan mode enabled] Investigate the task and the existing project before proposing changes. Use ask_user for requirements and decisions that investigation cannot resolve. Do not modify files, run commands outside the read-only whitelist, use MCP tools, or call update_plan. Produce a complete, actionable plan with implementation steps and verification, then call submit_plan with the full Markdown plan. Only the user can leave Plan mode or approve implementation.";
const EXIT: &str = "[Plan mode disabled] The user has left Plan mode. Continue under the current permission policy.";
pub(crate) const APPROVED: &str = "[Plan mode disabled] The user approved the plan for implementation under the selected permission policy.";

pub(crate) fn is_notice(message: &Message) -> bool {
    matches!(message, Message::User { text } if matches!(text.as_str(), ENTER | EXIT | APPROVED))
}

impl State {
    pub(crate) async fn restate_plan_mode(&mut self) -> Result<(), CoreError> {
        if self.journal.snapshot.settings.plan_mode {
            self.plan_mode_notice(ENTER).await?;
        }
        Ok(())
    }

    pub(crate) async fn plan_mode_notice(&mut self, text: &str) -> Result<(), CoreError> {
        let message = Message::User { text: text.into() };
        self.journal
            .record(crate::context::HistoryEntry::PlanModeNotice {
                message: message.clone(),
            })
            .await?;
        self.plan_mode_notices.push(message);
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }
    pub(crate) async fn set_plan_mode(
        &mut self,
        settings: &mut SessionSettings,
        enabled: bool,
    ) -> Result<(), CoreError> {
        if settings.plan_mode == enabled {
            return Ok(());
        }
        if enabled {
            settings.pre_plan_approval_policy = Some(settings.approval_policy);
        } else {
            settings.approval_policy = settings
                .pre_plan_approval_policy
                .take()
                .unwrap_or(settings.approval_policy);
            let requests: Vec<_> = self
                .journal
                .snapshot
                .pending_requests
                .iter()
                .filter(|r| matches!(r.kind, RequestKind::PlanApproval(_)))
                .map(|r| r.id.clone())
                .collect();
            for request_id in requests {
                let events = self
                    .journal
                    .append(vec![
                        EventPayload::RequestResolved {
                            request_id: request_id.clone(),
                            resolution: ApprovalResolution::Cancelled,
                            client_name: None,
                        },
                        EventPayload::SessionStatus {
                            status: SessionStatus::Working,
                            close_reason: None,
                        },
                    ])
                    .await?;
                self.resolved.insert(request_id.clone(), events[0].seq);
                if let Some(sender) = self.requests.remove(&request_id) {
                    let _ = sender.send(ApprovalResolution::Cancelled);
                }
            }
        }
        settings.plan_mode = enabled;
        self.plan_mode_notice(if enabled { ENTER } else { EXIT })
            .await
    }
}

pub(crate) async fn submit(
    shared: &Shared,
    turn_id: &str,
    call: &PreparedCall,
    cancel: &CancellationToken,
) -> Result<ToolOutput, CoreError> {
    let Item::PlanProposal {
        id: item_id, plan, ..
    } = call.started_item()
    else {
        unreachable!("submit_plan item")
    };
    let mut receiver = {
        let mut state = shared.state.lock().await;
        if !state.journal.snapshot.settings.plan_mode {
            return Ok(output(item_id, plan, None, ItemStatus::Blocked,
                "This session is not in Plan mode. Only the user can enable Plan mode; continue the task under the current permission policy.".into()));
        }
        let policy = state
            .journal
            .snapshot
            .settings
            .pre_plan_approval_policy
            .unwrap_or(state.journal.snapshot.settings.approval_policy);
        let policy = match policy {
            PermissionMode::AllowAll | PermissionMode::Unknown => PermissionMode::AlwaysAsk,
            other => other,
        };
        let request = PendingRequest {
            id: id("plan"),
            turn_id: turn_id.into(),
            created_at: crate::journal::now(),
            kind: RequestKind::PlanApproval(PlanApprovalRequest {
                item_id: item_id.clone(),
                plan: plan.clone(),
                approval_policy: policy,
            }),
        };
        if state.journal.snapshot.settings.non_interactive {
            state
                .journal
                .append(vec![EventPayload::ItemStarted {
                    turn_id: turn_id.into(),
                    item: call.started_item(),
                }])
                .await?;
            state
                .resolve_without_user(request, ApprovalResolution::Cancelled)
                .await?;
            return Ok(output(item_id, plan, None, ItemStatus::Declined,
                "No one is available to approve this plan. Stay in Plan mode; do not implement the proposal.".into()));
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state
            .journal
            .append(vec![
                EventPayload::ItemStarted {
                    turn_id: turn_id.into(),
                    item: call.started_item(),
                },
                EventPayload::RequestOpened {
                    request: request.clone(),
                },
                EventPayload::SessionStatus {
                    status: SessionStatus::WaitingApproval,
                    close_reason: None,
                },
            ])
            .await?;
        state.requests.insert(request.id, sender);
        receiver
    };
    let resolution = tokio::select! {
        biased;
        _ = cancel.cancelled() => match receiver.try_recv() {
            Ok(resolution) => resolution,
            Err(_) => { shared.state.lock().await.cancel_requests().await?; ApprovalResolution::Cancelled }
        },
        resolution = &mut receiver => resolution.map_err(|_| CoreError::internal())?,
    };
    let ApprovalResolution::Plan { answer } = resolution else {
        return Ok(output(item_id, plan, None, ItemStatus::Declined,
            "Plan approval was cancelled; do not implement this proposal on that approval. Follow the current session mode.".into()));
    };
    let text = match answer.decision {
        PlanDecision::Implement => {
            let mut text = "The user approved this plan. Implement it now under the selected permission policy.".to_owned();
            if shared.state.lock().await.journal.snapshot.settings.plan_enabled != Some(false) {
                text.push_str(" First call update_plan to turn the plan's steps into a checklist, then implement and update each step as it is completed.");
            }
            text
        },
        PlanDecision::ClearContext => "The user chose to implement this plan in a new session with a clear context. This turn is finished; do not continue implementation in this session.".into(),
        PlanDecision::KeepPlanning => {
            let mut text = "The user chose to keep planning. Stay in Plan mode, revise the plan, and submit the complete replacement with submit_plan.".to_owned();
            if let Some(feedback) = &answer.feedback { text.push_str(&format!(" The user says: {feedback}")); }
            text
        },
        PlanDecision::Unknown => return Err(CoreError::internal()),
    };
    let status = if answer.decision == PlanDecision::KeepPlanning {
        ItemStatus::Declined
    } else {
        ItemStatus::Completed
    };
    Ok(output(item_id, plan, Some(answer), status, text))
}

fn output(
    id: String,
    plan: String,
    answer: Option<PlanAnswer>,
    status: ItemStatus,
    text: String,
) -> ToolOutput {
    ToolOutput {
        output_size: Default::default(),
        change_ticket: None,
        images: Vec::new(),
        item: Item::PlanProposal {
            id,
            plan,
            answer,
            status,
        },
        text,
        outcome: if status == ItemStatus::Completed {
            ToolOutcome::Completed
        } else {
            ToolOutcome::Declined
        },
    }
}
