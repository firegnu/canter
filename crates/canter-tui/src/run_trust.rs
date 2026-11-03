use super::*;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph},
};

use crate::approval::shorten_home;
use crate::short_path::middle;
use crate::text::{LogicalLine, str_width, visible, wrap};

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
    home: Option<&str>,
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
                    "Could not check folder trust: {error}. Opening in restricted mode; LSP and hooks are disabled."
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
            .draw(|frame| draw_box(frame, &trust.path, home, restricted))
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

/// The question, centred: a column of room inside each side of the border,
/// and as tall as its rows, or the screen.
fn draw_box(frame: &mut Frame, path: &str, home: Option<&str>, restricted: bool) {
    let screen = frame.area();
    let width = screen.width.min(86);
    // The border and a column of room on each side.
    let room = width.saturating_sub(4);
    let mut rows = vec![Line::from("Trust this folder?".bold()), Line::default()];
    rows.extend(path_rows(path, home, usize::from(room)));
    rows.push(Line::default());
    for text in [
        "Language servers can run code from this folder, including build scripts.",
        "Trust is saved for the repository root, or this folder outside Git.",
        "Restricted mode disables LSP. AGENTS.md and skills still load.",
    ] {
        rows.extend(wrap(
            &LogicalLine::new(Vec::new(), vec![Span::raw(text)]),
            room,
        ));
    }
    rows.push(Line::default());
    let option = |label: &'static str, selected: bool| {
        if selected {
            Line::from(Span::styled(format!("› {label}"), theme::accent().bold()))
        } else {
            Line::from(format!("  {label}"))
        }
    };
    let choices = vec![
        option("Trust and continue", !restricted),
        option("Open restricted", restricted),
        Line::default(),
        Line::from(Span::styled(
            "↑/↓ select · Enter confirm · Esc cancel",
            theme::secondary(),
        )),
    ];
    let height = u16::try_from(rows.len() + choices.len() + 2)
        .unwrap_or(u16::MAX)
        .min(screen.height);
    let area = Rect::new(
        (screen.width - width) / 2,
        (screen.height - height) / 2,
        width,
        height,
    );
    let block = Block::bordered()
        .title(" Folder trust ")
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Keep both decisions visible when the screen is short.
    let choices_height = inner.height.min(4);
    let body = Rect {
        height: inner.height.saturating_sub(choices_height),
        ..inner
    };
    let choices_area = Rect {
        y: body.bottom(),
        height: choices_height,
        ..inner
    };
    frame.render_widget(Paragraph::new(rows), body);
    frame.render_widget(Paragraph::new(choices), choices_area);
}

/// The folder, the home directory as `~`, broken after a `/` where it does
/// not fit in `room` columns; a part longer than a row loses its middle.
fn path_rows(path: &str, home: Option<&str>, room: usize) -> Vec<Line<'static>> {
    let path = shorten_home(path, home);
    let path = visible(&path.replace('\n', "\\n").replace('\t', "\\t"));
    let mut rows = Vec::new();
    let mut row = String::new();
    for part in path.split_inclusive('/') {
        if !row.is_empty() && str_width(&row) + str_width(part) > room {
            rows.push(std::mem::take(&mut row));
        }
        if row.is_empty() && str_width(part) > room {
            row = middle(part, room);
        } else {
            row.push_str(part);
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.into_iter().map(Line::from).collect()
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
    let home = ui.app().home().map(str::to_owned);
    match confirm(
        client,
        cwd,
        home.as_deref(),
        ui.terminal_mut(),
        input,
        shutdown,
    )
    .await
    {
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

    /// The screen's rows, each cell's symbol.
    fn screen_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// The rows inside the box's border.
    fn box_rows(rows: &[String]) -> Vec<String> {
        let top = rows.iter().position(|row| row.contains('┌')).unwrap();
        let bottom = rows.iter().position(|row| row.contains('└')).unwrap();
        rows[top + 1..bottom]
            .iter()
            .map(|row| {
                let start = row.find('│').unwrap() + '│'.len_utf8();
                let end = row.rfind('│').unwrap();
                row[start..end].to_owned()
            })
            .collect()
    }

    #[test]
    fn trust_box_pads_its_sides_breaks_the_path_at_slashes_and_fits_its_rows() {
        let path = "/Users/test/Developer/personal_projs/canter-worktrees/55677df5-b7be-47c6-8d1e-0123456789ab/crates";
        let mut terminal = Terminal::new(TestBackend::new(60, 40)).unwrap();
        terminal
            .draw(|frame| draw_box(frame, path, Some("/Users/test"), false))
            .unwrap();
        let rows = box_rows(&screen_rows(&terminal));
        let shown = rows.join("\n");
        for row in &rows {
            assert!(
                row.starts_with(' ') && row.ends_with(' '),
                "a column inside each side:\n{shown}"
            );
        }
        assert_eq!(rows[0].trim(), "Trust this folder?", "{shown}");
        assert!(
            rows.last().unwrap().contains("Esc cancel"),
            "no rows under the keys:\n{shown}"
        );
        assert!(rows[rows.len() - 2].trim().is_empty(), "{shown}");
        assert!(rows[rows.len() - 3].contains("Open restricted"), "{shown}");
        let path_rows: Vec<&str> = rows[2..]
            .iter()
            .map(|row| row.trim())
            .take_while(|row| !row.is_empty())
            .collect();
        assert!(path_rows.len() > 1, "{shown}");
        assert!(path_rows[0].starts_with("~/"), "{shown}");
        for row in &path_rows[..path_rows.len() - 1] {
            assert!(row.ends_with('/'), "broken after a slash:\n{shown}");
        }
        assert_eq!(path_rows.concat(), path.replacen("/Users/test", "~", 1));

        // A part longer than a row loses its middle rather than breaking.
        let long = format!("/srv/{}", "x".repeat(80));
        terminal
            .draw(|frame| draw_box(frame, &long, None, true))
            .unwrap();
        let rows = box_rows(&screen_rows(&terminal));
        let shown = rows.join("\n");
        assert_eq!(rows[2].trim(), "/srv/", "{shown}");
        assert!(
            rows[3].trim().starts_with("xxx") && rows[3].contains('…'),
            "{shown}"
        );
        assert!(rows[4].trim().is_empty(), "{shown}");
    }

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
            None,
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
