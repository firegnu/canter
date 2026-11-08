use crate::{CoreError, session::State};
use canter_model::{Message, ToolCall};
use canter_protocol::{EventPayload, GoalStatus, PlanStepStatus};

#[derive(Default)]
pub(crate) struct Guards {
    continuations: u8,
    plan_reminders: u8,
    repeated: Option<RepeatedCall>,
}

struct RepeatedCall {
    name: String,
    arguments: String,
    result: String,
    count: u8,
}

impl State {
    pub(crate) fn repeated_result(&mut self, call: &ToolCall, text: &mut String) {
        let enabled = self.long_run_enabled();
        let Some(active) = &mut self.active else {
            return;
        };
        if !enabled {
            active.long_run.repeated = None;
            return;
        }
        let previous = &mut active.long_run.repeated;
        if let Some(previous) = previous.as_mut()
            && previous.name == call.name
            && previous.arguments == call.arguments
            && previous.result == *text
        {
            previous.count += 1;
        } else {
            *previous = Some(RepeatedCall {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: text.clone(),
                count: 1,
            });
        }
        if previous
            .as_ref()
            .is_some_and(|previous| previous.count == 3)
        {
            text.push_str("\n\n[Repeated call] This identical call and result have occurred 3 consecutive times. Try a different approach.");
        }
    }

    pub(crate) async fn stop_repeated_call(&mut self, call: &ToolCall) -> Result<bool, CoreError> {
        if !self.long_run_enabled()
            || !self
                .active
                .as_ref()
                .expect("active turn")
                .long_run
                .repeated
                .as_ref()
                .is_some_and(|previous| {
                    previous.count >= 4
                        && previous.name == call.name
                        && previous.arguments == call.arguments
                })
        {
            return Ok(false);
        }
        if self
            .journal
            .snapshot
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            self.goal_status(GoalStatus::Paused).await?;
        }
        self.journal.append(vec![EventPayload::Warning {
            message: "Stopped before the fifth repeated tool call: the previous four calls and results were identical. Send a message to continue with a different approach.".into(),
        }]).await?;
        self.active.as_ref().expect("active turn").cancel.cancel();
        Ok(true)
    }

    pub(crate) async fn remind_unfinished_plan(&mut self) -> Result<bool, CoreError> {
        if !self.long_run_enabled()
            || self.journal.snapshot.settings.plan_enabled == Some(false)
            || self
                .journal
                .snapshot
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == GoalStatus::Active)
            || self
                .active
                .as_ref()
                .expect("active turn")
                .long_run
                .plan_reminders
                == 3
        {
            return Ok(false);
        }
        let steps: Vec<_> = self
            .journal
            .snapshot
            .current_plan()
            .into_iter()
            .flatten()
            .filter(|step| {
                matches!(
                    step.status,
                    PlanStepStatus::Pending | PlanStepStatus::InProgress
                )
            })
            .map(|step| step.step.as_str())
            .collect();
        if steps.is_empty() {
            return Ok(false);
        }
        let text = format!(
            "[Unfinished plan] There are {} unfinished plan steps:\n- {}\nContinue working. If you are blocked or need a user decision, explain why and stop. First use update_plan to mark any finished steps completed.",
            steps.len(),
            steps.join("\n- ")
        );
        self.active
            .as_mut()
            .expect("active turn")
            .long_run
            .plan_reminders += 1;
        self.message(Message::User { text }).await?;
        self.journal
            .append(vec![EventPayload::Warning {
                message: "The model stopped with unfinished plan steps; reminding it to continue."
                    .into(),
            }])
            .await?;
        Ok(true)
    }

    pub(crate) fn long_run_enabled(&self) -> bool {
        !self.journal.snapshot.settings.plan_mode
            && self.goal_summary.is_none()
            && self.steers.is_empty()
            && self.active.as_ref().is_some_and(|active| {
                active.turn.review.is_none()
                    && !active.plan_handoff
                    && !active.cancel.is_cancelled()
            })
    }

    pub(crate) async fn continue_truncated(&mut self, has_calls: bool) -> Result<bool, CoreError> {
        if !self.long_run_enabled() {
            return Ok(false);
        }
        let guards = &mut self.active.as_mut().expect("active turn").long_run;
        if guards.continuations == 3 {
            return Ok(false);
        }
        guards.continuations += 1;
        let text = if has_calls {
            "[Reply truncated] Your tool calls were truncated and were not executed. Split them into smaller steps and try again."
        } else {
            "[Reply truncated] Continue from where your reply was cut off. Do not repeat the text already written."
        };
        self.message(Message::User { text: text.into() }).await?;
        self.journal
            .append(vec![EventPayload::Warning {
                message: "The model reply was truncated; continuing automatically.".into(),
            }])
            .await?;
        Ok(true)
    }
}
