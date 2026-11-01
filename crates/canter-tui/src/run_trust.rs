use super::*;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

/// Before creating or resuming: neither project services nor a first input run
/// until the user has selected how to open this folder.
pub(super) async fn confirm<B: FrameSync>(
    client: &Client,
    cwd: &str,
    terminal: &mut Terminal<B>,
    input: &mut Input,
    shutdown: &mut Shutdown,
) -> Result<bool, RunError>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if !client
        .info()
        .await?
        .features
        .unwrap_or_default()
        .iter()
        .any(|f| f == "folderTrust")
    {
        return Err(ClientError::Protocol(
            "This server does not support folder trust; run canter serve --replace and retry"
                .into(),
        )
        .into());
    }
    let trust = client.folder_trust(cwd).await?;
    if trust.trusted {
        return Ok(true);
    }
    let mut restricted = false;
    let mut first_frame = None;
    let ready_at = now_ms() + crate::ANSWER_GUARD_MS;
    loop {
        if shutdown.requested() {
            return Ok(false);
        }
        terminal
            .backend_mut()
            .begin_frame()
            .map_err(terminal_error)?;
        let drawn = terminal
            .draw(|frame| {
                let area = frame.area();
                let width = area.width.min(86);
                let height = area.height.min(17);
                let area = Rect::new(
                    (area.width - width) / 2,
                    (area.height - height) / 2,
                    width,
                    height,
                );
                let block = Block::bordered().title(" Folder trust ");
                let inner = block.inner(area);
                frame.render_widget(block, area);
                // Keep both decisions visible when the explanation wraps.
                let choices_height = inner.height.min(4);
                let body = Rect {
                    height: inner.height.saturating_sub(choices_height),
                    ..inner
                };
                let choices = Rect {
                    y: body.bottom(),
                    height: choices_height,
                    ..inner
                };
                let option = |label: &'static str, selected: bool| {
                    if selected {
                        Line::from(Span::styled(format!(" › {label}"), theme::accent().bold()))
                    } else {
                        Line::from(format!("   {label}"))
                    }
                };
                let rows = vec![
                    Line::from("Trust this folder?".bold()),
                    Line::default(),
                    Line::from(crate::text::visible(
                        &trust.path.replace('\n', "\\n").replace('\t', "\\t"),
                    )),
                    Line::default(),
                    Line::from(
                        "Language servers can run code from this folder, including build scripts.",
                    ),
                    Line::from(
                        "Trust is saved for the repository root, or this folder outside Git.",
                    ),
                    Line::from("Restricted mode disables LSP. AGENTS.md and skills still load."),
                ];
                frame.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }), body);
                let rows = vec![
                    option("Trust and continue", !restricted),
                    option("Open restricted", restricted),
                    Line::default(),
                    Line::from(Span::styled(
                        " ↑/↓ select · Enter confirm · Esc cancel",
                        theme::secondary(),
                    )),
                ];
                frame.render_widget(Paragraph::new(rows), choices);
            })
            .map(drop);
        terminal.backend_mut().end_frame().map_err(terminal_error)?;
        drawn.map_err(terminal_error)?;
        let frame = input.frame_on_screen();
        let first = *first_frame.get_or_insert(frame);
        let event = tokio::select! {
            biased;
            _ = shutdown.wait() => return Ok(false),
            event = input.recv() => event,
        };
        let Some((frame, at, TermEvent::Key(key))) = event else {
            if event.is_none() {
                return Ok(false);
            }
            continue;
        };
        if key.kind != KeyEventKind::Press || frame < first {
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(false);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab => restricted = !restricted,
            KeyCode::Char('1') => restricted = false,
            KeyCode::Char('2') => restricted = true,
            KeyCode::Enter if at >= ready_at => {
                if !restricted {
                    client.trust_folder(cwd).await?;
                }
                return Ok(true);
            }
            _ => {}
        }
    }
}

pub(super) async fn in_ui<B: FrameSync>(
    client: &Client,
    cwd: &str,
    ui: &mut Ui<B>,
    input: &mut Input,
    shutdown: &mut Shutdown,
) -> bool
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match confirm(client, cwd, ui.terminal_mut(), input, shutdown).await {
        Ok(open) => open,
        Err(error) => {
            ui.app_mut()
                .warning(&format!("Could not check folder trust: {error}"));
            false
        }
    }
}
