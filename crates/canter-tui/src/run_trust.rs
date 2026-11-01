use super::*;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

#[derive(Default)]
pub(super) struct Opening {
    pub restricted: bool,
    pub warning: Option<String>,
}

impl Opening {
    pub fn report(&self, app: &mut App) {
        if let Some(warning) = &self.warning {
            app.warning(warning);
        }
    }
}

/// Before creating or resuming: neither project services nor a first input run
/// until the user has selected how to open this folder.
pub(super) async fn confirm<B: FrameSync>(
    client: &Client,
    cwd: &str,
    terminal: &mut Terminal<B>,
    input: &mut Input,
    shutdown: &mut Shutdown,
) -> Result<Option<Opening>, RunError>
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
    let trust = match client.folder_trust(cwd).await {
        Ok(trust) => trust,
        Err(error) => {
            return Ok(Some(Opening {
                restricted: true,
                warning: Some(format!(
                    "Could not check folder trust: {error}. Opening in restricted mode; LSP is disabled."
                )),
            }));
        }
    };
    if trust.warning.is_some() {
        // The server also appends this warning after the opening snapshot.
        return Ok(Some(Opening {
            restricted: true,
            warning: None,
        }));
    }
    if trust.trusted {
        return Ok(Some(Opening::default()));
    }
    let mut restricted = false;
    let mut first_frame = None;
    let ready_at = now_ms() + crate::ANSWER_GUARD_MS;
    loop {
        if shutdown.requested() {
            return Ok(None);
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
            _ = shutdown.wait() => return Ok(None),
            event = input.recv() => event,
        };
        let Some((frame, at, TermEvent::Key(key))) = event else {
            if event.is_none() {
                return Ok(None);
            }
            continue;
        };
        if key.kind != KeyEventKind::Press || frame < first {
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(None);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab => restricted = !restricted,
            KeyCode::Char('1') => restricted = false,
            KeyCode::Char('2') => restricted = true,
            KeyCode::Enter if at >= ready_at => {
                if !restricted {
                    client.trust_folder(cwd).await?;
                }
                return Ok(Some(Opening {
                    restricted,
                    warning: None,
                }));
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
) -> Option<Opening>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match confirm(client, cwd, ui.terminal_mut(), input, shutdown).await {
        Ok(open) => open,
        Err(error) => {
            ui.app_mut()
                .warning(&format!("Could not check folder trust: {error}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_folder_trust_query_opens_restricted_with_warning() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let router = axum::Router::new()
            .route("/v1/info", axum::routing::get(|| async {
                axum::Json(serde_json::json!({"features":["folderTrust"],"serverVersion":"test","protocolVersion":1,"sessions":[]}))
            }))
            .route("/v1/folderTrust", axum::routing::get(|| async {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = Client::new(&socket).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let (_keys, mut input) = input();
        let mut shutdown = Shutdown::listen().unwrap();
        let opening = confirm(
            &client,
            root.path().to_str().unwrap(),
            &mut terminal,
            &mut input,
            &mut shutdown,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(opening.restricted);
        let warning = opening.warning.unwrap();
        assert!(
            warning.contains("Could not check folder trust"),
            "{warning}"
        );
        assert!(warning.contains("500"), "{warning}");
        server.abort();
        let _ = server.await;
    }
}
