use crate::{
    CoreError, Session, runner,
    session::{Shared, id, owned},
};
use canter_model::Message;
use canter_protocol::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

impl Session {
    pub async fn shell_command(
        &self,
        params: SessionShellCommandParams,
    ) -> Result<SessionShellCommandResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.shell_command_inner(params).await }).await
    }

    async fn shell_command_inner(
        &self,
        mut params: SessionShellCommandParams,
    ) -> Result<SessionShellCommandResponse, CoreError> {
        params.command = params.command.trim().to_owned();
        if canter_tools::split_background_command(&params.command)
            .0
            .is_empty()
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "command must not be empty",
            ));
        }
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state.active.is_some() || state.journal.snapshot.status != SessionStatus::Idle {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "wait for this turn to finish before running a user command",
            ));
        }
        state.background_events.user_input();
        let (response, done) = self
            .shared
            .start(
                &mut state,
                SessionInputParams {
                    non_interactive: false,
                    approval_policy: None,
                    review: None,
                    input: Vec::new(),
                    mode: InputMode::Queue,
                    client_name: "userShell".into(),
                },
                None,
                Vec::new(),
            )
            .await?;
        state
            .active
            .as_mut()
            .expect("started user shell")
            .user_shell = Some(params);
        let shared = self.shared.clone();
        tokio::spawn(async move {
            runner::run(shared, done).await;
        });
        Ok(SessionShellCommandResponse {
            event_seq: response.event_seq,
            turn_id: response.turn_id.expect("standalone turn"),
        })
    }
}

pub(crate) async fn run(
    shared: &Shared,
    turn_id: &str,
    params: &SessionShellCommandParams,
    cancel: &CancellationToken,
) -> Result<(), CoreError> {
    let item_id = id("command");
    {
        let mut state = shared.state.lock().await;
        let item = Item::CommandExecution {
            id: item_id.clone(),
            source: ExecCommandSource::UserShell,
            command: canter_tools::split_background_command(&params.command)
                .0
                .into(),
            cwd: state.journal.snapshot.settings.cwd.clone(),
            status: ItemStatus::InProgress,
            aggregated_output: String::new(),
            exit_code: None,
            duration_ms: None,
        };
        state
            .journal
            .append(vec![EventPayload::ItemStarted {
                turn_id: turn_id.into(),
                item,
            }])
            .await?;
    }
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut on_output = |text: &str| {
        let _ = sender.send(text.to_owned());
    };
    let execute = shared
        .tools
        .user_shell(item_id.clone(), params, cancel, &mut on_output);
    tokio::pin!(execute);
    let mut pending = String::new();
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(20));
    let mut write_error = None;
    let output = loop {
        tokio::select! {
            output = &mut execute => break output,
            Some(delta) = receiver.recv() => pending.push_str(&delta),
            _ = timer.tick(), if !pending.is_empty() && write_error.is_none() => {
                if let Err(error) = shared.state.lock().await.journal.append(vec![EventPayload::ItemDelta {
                    turn_id: turn_id.into(), item_id: item_id.clone(),
                    delta: ItemDelta::CommandExecution { delta: std::mem::take(&mut pending) },
                }]).await {
                    cancel.cancel();
                    write_error = Some(error);
                }
            }
        }
    };
    if let Some(error) = write_error {
        return Err(error);
    }
    // ItemCompleted carries all bounded output, including any buffered tail.
    let Item::CommandExecution {
        exit_code,
        duration_ms,
        ..
    } = &output.item
    else {
        unreachable!("shell result");
    };
    let text = if shared
        .tools
        .background()
        .list()
        .iter()
        .any(|command| command.item_id == item_id)
    {
        format!(
            "<user_shell_command>\n<command>\n{}\n</command>\n<result>\n{}\n</result>\n</user_shell_command>",
            params.command, output.text
        )
    } else {
        format!(
            "<user_shell_command>\n<command>\n{}\n</command>\n<result>\nExit code: {}\nDuration: {:.4} seconds\nOutput:\n{}\n</result>\n</user_shell_command>",
            params.command,
            exit_code.unwrap_or(-1),
            duration_ms.unwrap_or_default() as f64 / 1000.0,
            output.text,
        )
    };
    shared.sync_background().await?;
    let mut state = shared.state.lock().await;
    state.message(Message::User { text }).await?;
    state
        .journal
        .append(vec![EventPayload::ItemCompleted {
            turn_id: turn_id.into(),
            item: output.item,
        }])
        .await?;
    Ok(())
}
