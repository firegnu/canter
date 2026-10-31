use canter_protocol::*;
use std::io::{IsTerminal, Read, Write};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub chat: crate::ChatArgs,
    pub json: bool,
}

pub fn parse(words: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut prompt = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--" => prompt.extend(words.by_ref().cloned()),
            "--json" => args.json = true,
            "--yolo" => args.chat.yolo = true,
            "--resume" => {
                args.chat.resume = Some(Some(
                    words
                        .next()
                        .filter(|id| !id.starts_with('-'))
                        .ok_or("--resume needs a session ID")?
                        .clone(),
                ))
            }
            "--last" => args.chat.continue_latest = true,
            "-m" | "--model" => {
                args.chat.model = Some(words.next().ok_or("--model needs a model name")?.clone())
            }
            "--add-dir" => args
                .chat
                .additional_directories
                .push(words.next().ok_or("--add-dir needs a path")?.clone()),
            "--tag" => {
                let (key, value) = words
                    .next()
                    .ok_or("--tag needs key=value")?
                    .split_once('=')
                    .ok_or("--tag needs key=value")?;
                if key.trim().is_empty() {
                    return Err("--tag needs a non-empty key".into());
                }
                args.chat.tags.insert(key.into(), value.into());
            }
            option if option.starts_with('-') && option != "-" => {
                return Err(format!("unknown exec option: {option}"));
            }
            _ => prompt.push(word.clone()),
        }
    }
    if !prompt.is_empty() {
        args.chat.first = Some(prompt.join(" "));
    }
    if args.chat.resume.is_some() && args.chat.continue_latest {
        return Err("use either --resume or --last".into());
    }
    if !args.chat.tags.is_empty() && (args.chat.resume.is_some() || args.chat.continue_latest) {
        return Err("--tag is only supported for new sessions".into());
    }
    Ok(args)
}

#[derive(Default)]
struct Output {
    json: bool,
    session_id: Option<String>,
    turn_id: Option<String>,
}
impl Output {
    fn json_line(&self, value: &serde_json::Value) -> Result<(), String> {
        if self.json {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{value}")
                .and_then(|()| stdout.flush())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    fn finish(
        &self,
        code: u8,
        outcome: &str,
        error: Option<&str>,
        usage: Option<&TokenUsage>,
    ) -> Result<u8, String> {
        if let Some(error) = error {
            eprintln!("canter exec: {error}");
        }
        self.json_line(&serde_json::json!({
            "type":"exec.completed", "sessionId":self.session_id, "turnId":self.turn_id,
            "outcome":outcome,"exitCode":code,"error":error,
            "usage":usage.map(|usage| &usage.total),"cost":usage.and_then(|usage| usage.cost.as_ref()),
        }))?;
        Ok(code)
    }
}

pub async fn run(args: Args, context: crate::Context) -> Result<u8, String> {
    let mut output = Output {
        json: args.json,
        ..Default::default()
    };
    match run_inner(args.chat, context, &mut output).await {
        Ok(code) => Ok(code),
        Err(error) => output.finish(1, "error", Some(&error), None),
    }
}

pub fn startup_error(json: bool, error: &str) -> Result<u8, String> {
    Output {
        json,
        ..Default::default()
    }
    .finish(1, "error", Some(error), None)
}

async fn run_inner(
    args: crate::ChatArgs,
    context: crate::Context,
    output: &mut Output,
) -> Result<u8, String> {
    let mut prompt = args.first.filter(|text| text != "-").unwrap_or_default();
    if prompt.is_empty() || !std::io::stdin().is_terminal() {
        eprintln!("Reading additional input from stdin...");
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        if prompt.is_empty() {
            prompt = input;
        } else if !input.is_empty() {
            prompt.push_str(&format!("\n\n<stdin>\n{input}\n</stdin>"));
        }
    }
    if prompt.trim().is_empty() {
        return Err("exec needs a non-empty task argument or stdin".into());
    }
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|error| error.to_string())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| error.to_string())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|error| error.to_string())?;
    let signal = async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
            _ = hangup.recv() => {},
        }
    };
    tokio::pin!(signal);
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(warning) = &connection.version_warning {
        eprintln!("{warning}");
    }
    let client = crate::connected(connection)?.client;
    if !client
        .info()
        .await
        .map_err(|error| error.to_string())?
        .features
        .unwrap_or_default()
        .iter()
        .any(|feature| feature == "nonInteractiveInput")
    {
        return Err(
            "This server does not support exec; run canter serve --replace and retry".into(),
        );
    }
    let cwd = context
        .cwd
        .to_str()
        .ok_or("the current directory's path is not UTF-8")?
        .to_owned();
    let resume = if args.continue_latest {
        Some(
            client
                .list_sessions()
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|session| session.cwd == cwd)
                .ok_or("no session found in the current directory")?
                .id,
        )
    } else {
        args.resume.flatten()
    };
    let policy = if args.yolo {
        PermissionMode::AllowAll
    } else {
        PermissionMode::AskForExecution
    };
    let snapshot = if let Some(id) = resume {
        client
            .resume(&id, &context.env)
            .await
            .map_err(|error| error.to_string())?;
        client
            .update(
                &id,
                &SessionUpdateParams {
                    model: args.model,
                    add_directories: args.additional_directories,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        client
            .read_session(&id)
            .await
            .map_err(|error| error.to_string())?
            .session
    } else {
        client
            .create_session(&SessionCreateParams {
                tags: args.tags,
                additional_directories: args.additional_directories,
                cwd,
                env: context.env,
                model: args.model.unwrap_or_default(),
                reasoning_effort: None,
                approval_policy: PermissionMode::AlwaysAsk,
                input: None,
                name: None,
                client_name: "canter-exec".into(),
            })
            .await
            .map_err(|error| error.to_string())?
            .session
    };
    let id = &snapshot.id;
    if snapshot.settings.sandbox_enabled == Some(false) {
        eprintln!(
            "Sandbox is off. Commands requiring approval will be denied in default exec mode; use --yolo to allow them under the configured rules."
        );
    }
    output.session_id = Some(id.clone());
    output.json_line(&serde_json::json!({"type":"exec.started","sessionId":id}))?;
    let mut events = client
        .subscribe(id, snapshot.last_event_seq)
        .await
        .map_err(|error| error.to_string())?;
    tokio::select! {
        biased;
        _ = &mut signal => return output.finish(130,"interrupted",Some("Interrupted before starting the turn."),None),
        _ = std::future::ready(()) => {},
    }
    let started = client
        .send_input(
            id,
            &SessionInputParams {
                non_interactive: true,
                approval_policy: Some(policy),
                review: None,
                input: vec![UserInput::Text { text: prompt }],
                mode: InputMode::Queue,
                client_name: "canter-exec".into(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    output.turn_id = started.turn_id;
    let mut final_answer = None;
    let mut denied = false;
    let mut budget_limited = false;
    let mut interrupted = false;
    loop {
        let event = tokio::select! {
            biased;
            _ = &mut signal, if !interrupted => {
                interrupted = true;
                let turn_id = output.turn_id.as_deref().ok_or("server did not start a turn")?;
                match client.interrupt(id,turn_id,false).await {
                    Ok(_) => {},
                    Err(canter_tui::client::ClientError::Server { code: ErrorCode::TurnMismatch, .. }) => {},
                    Err(error) => return Err(format!("could not confirm that the interrupted turn stopped: {error}")),
                }
                continue;
            }
            event = events.next() => event.ok_or("server event stream ended before the turn completed")?,
        };
        let event = event.map_err(|error| error.to_string())?;
        progress(&event.payload);
        if let EventPayload::SessionGoalUpdated { goal: Some(goal) } = &event.payload
            && goal.status == GoalStatus::BudgetLimited
        {
            budget_limited = true;
        }
        output.json_line(&serde_json::to_value(&event).map_err(|error| error.to_string())?)?;
        if let EventPayload::RequestResolved {
            client_name,
            resolution: ApprovalResolution::Answered { .. } | ApprovalResolution::Cancelled,
            ..
        } = &event.payload
            && client_name.as_deref() == Some("non-interactive")
        {
            denied = true;
            eprintln!("Action requires approval; no one is available to approve it.");
        }
        if let EventPayload::ItemCompleted {
            item: Item::AgentMessage { text, status, .. },
            ..
        } = &event.payload
            && *status != Some(ItemStatus::Interrupted)
        {
            final_answer = Some(text.clone());
        }
        if let EventPayload::TurnCompleted { turn } = event.payload {
            if !output.json
                && let Some(text) = final_answer
            {
                writeln!(std::io::stdout(), "{text}").map_err(|error| error.to_string())?;
            }
            return if interrupted || turn.status == TurnStatus::Interrupted {
                output.finish(
                    130,
                    "interrupted",
                    Some("The turn was interrupted."),
                    turn.usage.as_ref(),
                )
            } else if turn.status == TurnStatus::Failed {
                output.finish(
                    2,
                    "failed",
                    Some(
                        turn.error
                            .as_ref()
                            .map_or("The model turn failed.", |error| error.message.as_str()),
                    ),
                    turn.usage.as_ref(),
                )
            } else if turn.status == TurnStatus::Completed {
                if budget_limited {
                    output.finish(
                        4,
                        "budgetLimited",
                        Some("The goal spending limit was reached."),
                        turn.usage.as_ref(),
                    )
                } else if denied {
                    output.finish(
                        3,
                        "approvalDenied",
                        Some(
                            "One or more actions required approval and were automatically denied.",
                        ),
                        turn.usage.as_ref(),
                    )
                } else {
                    output.finish(0, "completed", None, turn.usage.as_ref())
                }
            } else {
                Err("turn did not complete".into())
            };
        }
    }
}

fn progress(event: &EventPayload) {
    let message = match event {
        EventPayload::Warning { message } => Some(format!("Warning: {message}")),
        EventPayload::Error { error, .. } => Some(format!("Error: {}", error.message)),
        EventPayload::ItemCompleted { item, .. } => match item {
            Item::CommandExecution {
                status: ItemStatus::Failed,
                exit_code,
                ..
            } => Some(format!("Tool failed: shell (exit code {exit_code:?})")),
            Item::FileRead { status, .. }
            | Item::FileChange { status, .. }
            | Item::McpToolCall { status, .. }
            | Item::McpToolSearch { status, .. }
            | Item::WebFetch { status, .. }
            | Item::WebSearch { status, .. }
            | Item::BackgroundCommandAction { status, .. }
            | Item::Plan { status, .. }
                if *status == ItemStatus::Failed =>
            {
                Some("Tool failed; details are saved in the session.".into())
            }
            _ => None,
        },
        EventPayload::ItemStarted { item, .. } => match item {
            Item::ModelRetry { .. } => item.retry_message(),
            Item::CommandExecution { command, .. } => Some(format!("shell: {command}")),
            Item::FileRead { path, .. } => Some(format!("read: {path}")),
            Item::FileChange { changes, .. } => Some(format!(
                "write/edit: {}",
                changes
                    .iter()
                    .map(|change| change.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Item::McpToolCall { server, tool, .. } => Some(format!("MCP: {server}/{tool}")),
            Item::McpToolSearch { query, .. } => Some(format!("mcp_search: {query}")),
            Item::WebFetch { url, .. } => Some(format!("web_fetch: {url}")),
            Item::WebSearch { query, .. } => Some(format!("web_search: {query}")),
            Item::UserInput { question, .. } => {
                Some(format!("ask_user: {question} (no one is available)"))
            }
            Item::PlanProposal { .. } => Some("submit_plan".into()),
            Item::Plan { .. } => Some("update_plan".into()),
            Item::BackgroundCommandAction {
                action, command_id, ..
            } => Some(format!("{action}: {command_id}")),
            Item::ContextCompaction { .. } => Some("Compacting context".into()),
            _ => None,
        },
        _ => None,
    };
    if let Some(message) = message {
        let brief: String = message
            .chars()
            .take(300)
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        eprintln!("{brief}");
    }
}
