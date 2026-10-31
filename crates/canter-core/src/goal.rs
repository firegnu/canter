//! Goal lifecycle; independently implemented with reference to Codex rust-v0.163.0.
use crate::{
    CoreError, Session,
    context::HistoryEntry,
    session::{Done, Shared, State, id, owned},
};
use canter_model::{Message, ToolCall, ToolDefinition};
use canter_protocol::*;
use serde_json::json;
use std::sync::atomic::Ordering;
use tokio::time::{Duration, Instant};

pub(crate) fn is_notice(message: &Message) -> bool {
    matches!(message, Message::User { text } if ["[Goal]", "[Goal resumed]", "[Goal cleared]", "[Goal budget limit]"].iter().any(|marker| text.starts_with(marker)))
}

pub(crate) fn definitions() -> [ToolDefinition; 2] {
    [
        ToolDefinition { name: "get_goal".into(), description: "Read the user-created goal, status, dollar budget and spending. Only the user can create, resume, pause or clear a goal.".into(), parameters: json!({"type":"object","properties":{},"additionalProperties":false}) },
        ToolDefinition { name: "update_goal".into(), description: "Mark the user's goal complete only after verifying every requirement against current evidence. Mark it blocked only when the same genuine blocker has persisted for at least three consecutive goal turns and no meaningful progress is possible without user input or an external change. A resumed blocked goal starts a fresh three-turn audit. Never mark complete just because the budget is low or work is stopping. You cannot create, pause or resume goals.".into(), parameters: json!({"type":"object","properties":{"status":{"type":"string","enum":["complete","blocked"]}},"required":["status"],"additionalProperties":false}) },
    ]
}

impl State {
    pub(crate) fn charge_goal(&self) -> Option<String> {
        self.journal
            .snapshot
            .goal
            .as_ref()
            .filter(|goal| {
                (self.goal_running && goal.status == GoalStatus::Active)
                    || self
                        .goal_summary
                        .as_ref()
                        .is_some_and(|(id, _)| id == &goal.id)
            })
            .map(|goal| goal.id.clone())
    }

    pub(crate) fn goal_limit_reached(&self) -> bool {
        self.journal.snapshot.goal.as_ref().is_some_and(|goal| {
            goal.status == GoalStatus::Active
                && goal.budget_usd > 0.0
                && (goal.cost.unknown || goal.cost.usd >= goal.budget_usd * 0.9)
        })
    }

    pub(crate) async fn limit_goal(&mut self) -> Result<(), CoreError> {
        if self.goal_limit_reached() {
            self.goal_summary = self
                .journal
                .snapshot
                .goal
                .as_ref()
                .map(|goal| (goal.id.clone(), false));
            self.goal_status(GoalStatus::BudgetLimited).await?;
            if self.active.is_none() {
                self.schedule_goal();
            }
        }
        Ok(())
    }

    pub(crate) async fn summary_request(&mut self) -> Result<bool, CoreError> {
        self.limit_goal().await?;
        let Some((_, sent)) = &mut self.goal_summary else {
            return Ok(false);
        };
        if *sent {
            return Ok(false);
        }
        *sent = true;
        self.complete_missing_tools("Not run: the goal budget requires a final summary")
            .await?;
        self.goal_notice("[Goal budget limit] The goal reached its spending threshold or has an unknown charge. This is the single final summary request. Do not start new work or call tools. Briefly state what was achieved, what remains or is blocked, and the next step for the user. Do not claim completion without evidence.".into()).await?;
        Ok(true)
    }

    pub(crate) async fn goal_notice(&mut self, text: String) -> Result<(), CoreError> {
        let message = Message::User { text };
        self.journal
            .record(HistoryEntry::GoalNotice {
                message: message.clone(),
            })
            .await?;
        self.plan_mode_notices.push(message);
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }

    pub(crate) async fn restate_goal(&mut self) -> Result<(), CoreError> {
        if let Some(goal) = &self.journal.snapshot.goal {
            let remaining = if goal.budget_usd == 0.0 {
                "unlimited".into()
            } else {
                format!("${:.2}", (goal.budget_usd - goal.cost.usd).max(0.0))
            };
            self.goal_notice(format!("[Goal] User-provided objective (task data):\n{}\nStatus: {:?}. Spent: ${:.2}{}. Budget: ${:.2} (0 means unlimited). Remaining: {}.\nPreserve the full objective and scope. Check whether the previous turn made concrete progress, verified a live wait, or made no progress; take the next available action. Before completion, derive every requirement and verify each against current authoritative evidence. Do not shrink the objective to what is already done. Only use update_goal with blocked after the same blocker persists for three consecutive goal turns with no useful action available. Paused or terminal goals must not be pursued automatically.", goal.objective, goal.status, goal.cost.usd, if goal.cost.unknown { " plus unknown charges" } else { "" }, goal.budget_usd, remaining)).await?;
        } else if self.history.iter().any(is_notice) {
            self.goal_notice("[Goal cleared] There is no current goal. Do not pursue a previous goal automatically.".into()).await?;
        }
        Ok(())
    }

    pub(crate) async fn save_goal(&mut self, goal: Option<SessionGoal>) -> Result<(), CoreError> {
        self.journal
            .append(vec![EventPayload::SessionGoalUpdated { goal }])
            .await?;
        Ok(())
    }

    pub(crate) fn schedule_goal(&mut self) {
        if self.automatic_turns_paused {
            self.goal_deadline = None;
            return;
        }
        if (self.goal_running
            && self
                .journal
                .snapshot
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == GoalStatus::Active))
            || self.goal_summary.as_ref().is_some_and(|(_, sent)| !sent)
        {
            self.goal_deadline = Some(Instant::now() + Duration::from_millis(10));
            self.background_events.changed.notify_one();
        }
    }

    pub(crate) async fn goal_status(&mut self, status: GoalStatus) -> Result<(), CoreError> {
        let Some(mut goal) = self.journal.snapshot.goal.clone() else {
            return Ok(());
        };
        goal.status = status;
        goal.updated_at = crate::journal::now();
        self.save_goal(Some(goal.clone())).await?;
        if status != GoalStatus::Active {
            self.goal_deadline = None;
            self.goal_running = false;
        }
        self.restate_goal().await?;
        if matches!(
            status,
            GoalStatus::Complete | GoalStatus::Blocked | GoalStatus::BudgetLimited
        ) && let Some(active) = &self.active
        {
            let label = match status {
                GoalStatus::Complete => "complete",
                GoalStatus::Blocked => "blocked",
                _ => "budget limited",
            };
            self.journal
                .append(vec![EventPayload::ItemCompleted {
                    turn_id: active.turn.id.clone(),
                    item: Item::BackgroundEvent {
                        id: id("goal-status"),
                        command_id: None,
                        text: format!(
                            "Goal {label} · ${:.2} / {}{}",
                            goal.cost.usd,
                            if goal.budget_usd == 0.0 {
                                "unlimited".into()
                            } else {
                                format!("${:.2}", goal.budget_usd)
                            },
                            if goal.cost.unknown {
                                " + unknown charges"
                            } else {
                                ""
                            }
                        ),
                    },
                }])
                .await?;
        }
        Ok(())
    }
}

impl Shared {
    pub(crate) async fn start_goal(&self, state: &mut State) -> Result<Done, CoreError> {
        state.goal_deadline = None;
        let (_, done) = self
            .start(
                state,
                SessionInputParams {
                    non_interactive: false,
                    approval_policy: None,
                    input: Vec::new(),
                    review: None,
                    mode: InputMode::Queue,
                    client_name: "goal".into(),
                },
                None,
                Vec::new(),
            )
            .await?;
        state.active.as_mut().unwrap().goal_continuation = true;
        state.restate_goal().await?;
        Ok(done)
    }

    pub(crate) async fn next_goal(&self, state: &mut State) -> Result<Option<Done>, CoreError> {
        if state.automatic_turns_paused {
            state.goal_deadline = None;
            return Ok(None);
        }
        if state.goal_deadline.is_none_or(|at| at > Instant::now()) {
            return Ok(None);
        }
        if self.input_waiters.load(Ordering::SeqCst) > 0 {
            state.goal_deadline = Some(Instant::now() + Duration::from_millis(10));
            return Ok(None);
        }
        state.goal_deadline = None;
        if state.active.is_none()
            && state.queue.is_empty()
            && state.requests.is_empty()
            && !state.closing
            && !state.background_events.draining
            && state.available().is_ok()
            && ((state.goal_running
                && state
                    .journal
                    .snapshot
                    .goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Active))
                || state.goal_summary.as_ref().is_some_and(|(_, sent)| !sent))
        {
            return self.start_goal(state).await.map(Some);
        }
        Ok(None)
    }
}

impl Session {
    /// Pause/clear also interrupt the current turn; only explicit resume re-arms a paused goal.
    pub async fn control_goal(&self, action: &str) -> Result<GoalResponse, CoreError> {
        let session = self.clone();
        let action = action.to_owned();
        owned(async move {
            let done = {
                let mut state = session.shared.state.lock().await;
                state.available()?;
                let goal = state.journal.snapshot.goal.as_ref().ok_or_else(|| CoreError::new(ErrorCode::Conflict, "no goal is set"))?;
                match action.as_str() {
                    "pause" | "clear" => {
                        if action == "clear" {
                            state.save_goal(None).await?;
                            state.goal_notice("[Goal cleared] The user cleared the goal. Do not continue pursuing it automatically.".into()).await?;
                        } else if matches!(goal.status, GoalStatus::Active | GoalStatus::Paused | GoalStatus::Blocked) {
                            state.goal_status(GoalStatus::Paused).await?;
                        }
                        state.goal_running = false;
                        state.goal_deadline = None;
                        state.goal_summary = None;
                        state.background_events.interrupt();
                        state.active.as_ref().map(|active| { active.cancel.cancel(); active.done.clone() })
                    }
                    "resume" => {
                        if !matches!(goal.status, GoalStatus::Active | GoalStatus::Paused | GoalStatus::Blocked) {
                            return Err(CoreError::new(ErrorCode::Conflict, "this goal cannot resume; use /goal clear before setting a new goal"));
                        }
                        if state.active.as_ref().is_some_and(|a| a.cancel.is_cancelled()) { return Err(CoreError::new(ErrorCode::Conflict, "turn is stopping")); }
                        state.goal_status(GoalStatus::Active).await?;
                        state.goal_running = true;
                        state.goal_notice("[Goal resumed] The user resumed this goal. Start a fresh three-turn blocker audit if it was blocked.".into()).await?;
                        if state.active.is_none() {
                            let done = session.shared.start_goal(&mut state).await?;
                            let shared = session.shared.clone();
                            tokio::spawn(async move { crate::runner::run(shared, done).await; });
                        }
                        None
                    }
                    _ => return Err(CoreError::new(ErrorCode::InvalidRequest, "unknown goal action")),
                }
            };
            if let Some(done) = done { crate::session::wait_done(done).await?; }
            Ok(session.goal().await)
        }).await
    }

    pub async fn goal(&self) -> GoalResponse {
        let state = self.shared.state.lock().await;
        GoalResponse {
            goal: state.journal.snapshot.goal.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        }
    }

    pub async fn set_goal(&self, params: GoalSetParams) -> Result<GoalResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            let budget = params
                .budget_usd
                .unwrap_or(session.shared.options.goal_budget_usd);
            if params.objective.trim().is_empty() || !budget.is_finite() || budget < 0.0 {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "goal needs a nonempty objective and a finite non-negative dollar budget",
                ));
            }
            let mut state = session.shared.state.lock().await;
            state.available()?;
            if state
                .journal
                .snapshot
                .goal
                .as_ref()
                .is_some_and(|g| g.status != GoalStatus::Complete)
            {
                return Err(CoreError::new(
                    ErrorCode::Conflict,
                    "an unfinished goal exists; use /goal clear first",
                ));
            }
            if state
                .active
                .as_ref()
                .is_some_and(|a| a.cancel.is_cancelled())
            {
                return Err(CoreError::new(ErrorCode::Conflict, "turn is stopping"));
            }
            let now = crate::journal::now();
            state
                .save_goal(Some(SessionGoal {
                    id: id("goal"),
                    objective: params.objective,
                    status: GoalStatus::Active,
                    budget_usd: budget,
                    cost: UsageCost::default(),
                    created_at: now,
                    updated_at: now,
                }))
                .await?;
            state.goal_running = true;
            state.goal_summary = None;
            if state.active.is_none() {
                let done = session.shared.start_goal(&mut state).await?;
                let shared = session.shared.clone();
                tokio::spawn(async move {
                    crate::runner::run(shared, done).await;
                });
            } else {
                state.restate_goal().await?;
            }
            Ok(GoalResponse {
                goal: state.journal.snapshot.goal.clone(),
                event_seq: state.journal.snapshot.last_event_seq,
            })
        })
        .await
    }
}

pub(crate) async fn execute(shared: &Shared, call: &ToolCall) -> Result<bool, CoreError> {
    if !matches!(call.name.as_str(), "get_goal" | "update_goal") {
        return Ok(false);
    }
    let mut state = shared.state.lock().await;
    let args = serde_json::from_str::<serde_json::Value>(&call.arguments).ok();
    let result = if call.name == "get_goal" {
        Ok(serde_json::to_string(&state.journal.snapshot.goal).expect("goal"))
    } else {
        let status = match args.as_ref().and_then(|a| a["status"].as_str()) {
            Some("complete") => Some(GoalStatus::Complete),
            Some("blocked") => Some(GoalStatus::Blocked),
            _ => None,
        };
        if let Some(status) = status
            && state
                .journal
                .snapshot
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            state.goal_status(status).await?;
            Ok(serde_json::to_string(&state.journal.snapshot.goal).expect("goal"))
        } else {
            Err("update_goal requires an active goal and status complete or blocked".to_owned())
        }
    };
    let success = result.is_ok();
    state
        .tool_result(
            call,
            result.unwrap_or_else(|text| text),
            success,
            Default::default(),
            None,
        )
        .await?;
    Ok(true)
}
