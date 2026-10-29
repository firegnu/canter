//! LSP in the terminal (P2-135): a diagnostics line under the file change
//! card it came with (`LSP: 2 errors, 1 warning`), a separate
//! `LSP update: path · …` for results that arrived later, and `/lsp`, the
//! language servers of this session.

use canter_protocol::{
    DiagnosticDelivery, DiagnosticFreshness, DiagnosticSeverity, DiagnosticStatus, Item,
    LanguageServer, LanguageServerState,
};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::cells::display_path;
use crate::report::{Part, Report, clean};
use crate::text::{LogicalLine, Row, str_width, wrap_rows};
use crate::theme;

/// Where a card's diagnostics line starts: under the card's title text.
const CARD_INDENT: &str = "    ";
/// Where an `LSP update` starts, as a card's title does.
const UPDATE_INDENT: &str = "  ";
/// Where the entries start with details (Ctrl+O): under the diff lines.
const DETAILS_INDENT: &str = "      ";

fn plural(count: u32, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// Whether the diagnostics item at `index` came with its file change's
/// tool result, so it goes under that card. Items saved before `delivery`
/// existed go by where they are: right after their file change, as one
/// pending or unavailable status or results for different files.
pub(crate) fn inline(items: &[Item], index: usize) -> bool {
    let Some(Item::Diagnostics {
        delivery,
        source_item_id,
        ..
    }) = items.get(index)
    else {
        return false;
    };
    match delivery {
        Some(DiagnosticDelivery::Inline) => return true,
        Some(DiagnosticDelivery::Late) => return false,
        _ => {}
    }
    let Some(source) = source_item_id else {
        return false;
    };
    let mut first = index;
    loop {
        let Some(before) = first.checked_sub(1).and_then(|at| items.get(at)) else {
            return false;
        };
        match before {
            Item::FileChange { id, .. } if id == source => break,
            Item::Diagnostics {
                source_item_id: Some(other),
                ..
            } if other == source => first -= 1,
            _ => return false,
        }
    }
    let group = &items[first..=index];
    let mut paths = Vec::new();
    group.iter().all(|item| match item {
        Item::Diagnostics { status, files, .. } => {
            let path = files.first().map(|file| file.path.as_str());
            let fresh = !paths.contains(&path);
            paths.push(path);
            match status {
                DiagnosticStatus::Pending | DiagnosticStatus::Unavailable => group.len() == 1,
                DiagnosticStatus::Available => fresh,
                _ => false,
            }
        }
        _ => false,
    })
}

/// The diagnostics line for the item at `index`, without its indent, and
/// whether it goes under its card; `None` for anything else.
fn line(items: &[Item], index: usize, cwd: &str) -> Option<(Vec<Span<'static>>, bool)> {
    let Item::Diagnostics {
        source_item_id,
        status,
        files,
        total_count,
        error_count,
        warning_count,
        ..
    } = items.get(index)?
    else {
        return None;
    };
    let grey = theme::secondary();
    let attached = inline(items, index);
    let path = files.first().map(|file| display_path(&file.path, cwd));
    // Another file than the card's: one of the project's.
    let changed: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            Item::FileChange { id, changes, .. } if Some(id) == source_item_id.as_ref() => {
                Some(changes)
            }
            _ => None,
        })
        .flatten()
        .map(|change| display_path(&change.path, cwd))
        .collect();
    let mut spans = vec![Span::styled(
        match (&path, attached) {
            (Some(path), true) if !changed.contains(path) => format!("LSP: {path} · "),
            (_, true) => "LSP: ".into(),
            (Some(path), false) => format!("LSP update: {path} · "),
            (None, false) => "LSP update: ".into(),
        },
        grey,
    )];
    let mut word = |text: &str| spans.push(Span::styled(text.to_owned(), grey));
    match status {
        DiagnosticStatus::Pending => word("pending"),
        DiagnosticStatus::Unavailable => word("unavailable"),
        DiagnosticStatus::TimedOut => word("timed out"),
        DiagnosticStatus::Unknown => word("unknown"),
        DiagnosticStatus::Available
            if !files.is_empty()
                && files
                    .iter()
                    .all(|file| file.freshness == DiagnosticFreshness::Deleted) =>
        {
            word("file deleted")
        }
        DiagnosticStatus::Available => {
            match (error_count, warning_count) {
                (Some(0), Some(0)) => word("no errors or warnings"),
                (Some(errors), Some(warnings)) => {
                    if *errors > 0 {
                        spans.push(Span::styled(plural(*errors, "error"), theme::failure()));
                    }
                    if *warnings > 0 {
                        let comma = if *errors > 0 { ", " } else { "" };
                        spans.push(Span::styled(
                            format!("{comma}{}", plural(*warnings, "warning")),
                            grey,
                        ));
                    }
                }
                // Saved before the counts existed: only the total is known.
                _ if *total_count == 0 => word("no errors or warnings"),
                _ if *total_count == 1 => word("1 error or warning"),
                _ => word(&format!("{total_count} errors or warnings")),
            }
            let has = |freshness| files.iter().any(|file| file.freshness == freshness);
            if has(DiagnosticFreshness::Stale) {
                spans.push(Span::styled(" · may be stale", grey));
            } else if has(DiagnosticFreshness::VersionUnconfirmed) {
                spans.push(Span::styled(" · version unconfirmed", grey));
            }
            if files.iter().any(|file| file.incomplete) {
                spans.push(Span::styled(" · incomplete", grey));
            }
        }
    }
    Some((spans, attached))
}

/// With details, each entry `path:line:column severity [source/code]
/// message`, one-based, then the reasons a result is unavailable,
/// incomplete or may be stale, and how many were not shown.
fn details(item: &Item, cwd: &str) -> Vec<String> {
    let Item::Diagnostics {
        status,
        files,
        omitted_count,
        ..
    } = item
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for file in files {
        let path = display_path(&file.path, cwd);
        for entry in &file.diagnostics {
            let tag = match (&entry.source, &entry.code) {
                (Some(source), Some(code)) => format!(" [{source}/{code}]"),
                (Some(one), None) | (None, Some(one)) => format!(" [{one}]"),
                (None, None) => String::new(),
            };
            lines.push(clean(&format!(
                "{path}:{}:{} {}{tag} {}",
                entry.range.start.line + 1,
                entry.range.start.character + 1,
                match entry.severity {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warning",
                    DiagnosticSeverity::Unknown => "unknown",
                },
                entry.message
            )));
        }
        let Some(reason) = &file.reason else {
            continue;
        };
        match status {
            DiagnosticStatus::Unavailable => lines.push(clean(reason)),
            DiagnosticStatus::Available if file.incomplete => {
                lines.push(clean(&format!("Incomplete: {reason}")))
            }
            DiagnosticStatus::Available if file.freshness == DiagnosticFreshness::Stale => {
                lines.push(clean(&format!("May be stale: {reason}")))
            }
            _ => {}
        }
    }
    if *omitted_count > 0 {
        lines.push(format!("{omitted_count} not shown"));
    }
    lines
}

/// The rows of the diagnostics item at `index` in `items`.
pub(crate) fn rows(items: &[Item], index: usize, show: bool, cwd: &str, width: u16) -> Vec<Row> {
    let Some((spans, attached)) = line(items, index, cwd) else {
        return Vec::new();
    };
    let indent = if attached { CARD_INDENT } else { UPDATE_INDENT };
    let mut rows = wrap_rows(&LogicalLine::new(vec![Span::raw(indent)], spans), width);
    if show {
        for text in details(&items[index], cwd) {
            rows.extend(wrap_rows(
                &LogicalLine::new(
                    vec![Span::raw(DETAILS_INDENT)],
                    vec![Span::styled(text, theme::secondary())],
                ),
                width,
            ));
        }
    }
    rows
}

/// The diagnostics line for the export.
pub(crate) fn text(items: &[Item], index: usize, cwd: &str) -> Option<String> {
    let (spans, _) = line(items, index, cwd)?;
    Some(spans.iter().map(|span| span.content.as_ref()).collect())
}

/// `/lsp`: the list, or one server's details (`detail`), with the error of
/// the last stop or retry.
pub(crate) struct View {
    pub servers: Vec<LanguageServer>,
    pub selected: usize,
    pub detail: bool,
    pub error: Option<String>,
}

impl View {
    pub fn new(servers: Vec<LanguageServer>) -> Self {
        Self {
            servers,
            selected: 0,
            detail: false,
            error: None,
        }
    }

    /// The server selected, or shown with details.
    pub fn current(&self) -> Option<&LanguageServer> {
        self.servers.get(self.selected)
    }

    /// Takes the list a stop or retry returned, keeping the server shown.
    pub fn replace(&mut self, servers: Vec<LanguageServer>) {
        let id = self.current().map(|server| server.id.clone());
        self.servers = servers;
        self.selected = id
            .and_then(|id| self.servers.iter().position(|server| server.id == id))
            .unwrap_or(0)
            .min(self.servers.len().saturating_sub(1));
        if self.servers.is_empty() {
            self.detail = false;
        }
        self.error = None;
    }

    /// One server as `languageServer.updated` says, keeping the memory read
    /// last when the update has none.
    pub fn update(&mut self, server: &LanguageServer) {
        if let Some(old) = self.servers.iter_mut().find(|old| old.id == server.id) {
            let rss = old.rss_bytes;
            *old = server.clone();
            old.rss_bytes = old.rss_bytes.or(rss);
        }
    }

    pub fn report(&self, home: Option<&str>) -> Report {
        let title = |text: String| {
            vec![
                Span::styled(" LSP", Style::new().bold()),
                Span::styled(text, theme::secondary()),
            ]
        };
        if self.detail
            && let Some(server) = self.current()
        {
            let mut body = server_rows(server, None, home, (0, 0));
            body.push(Part::blank());
            body.push(Part::Line(vec![Span::styled(
                " Stop applies to this session only; other sessions keep their own.",
                theme::secondary(),
            )]));
            if let Some(error) = &self.error {
                body.push(Part::Wrap {
                    indent: 1,
                    text: clean(error),
                    style: theme::failure(),
                    rows: 3,
                });
            }
            return Report::new(
                title(format!("  {}", clean(&server.name))),
                body,
                vec![vec![Span::styled(
                    " s stop · r retry · Backspace list",
                    theme::secondary(),
                )]],
            );
        }
        let count = self.servers.len() as u32;
        let title = title(format!("  {}", plural(count, "server")));
        if self.servers.is_empty() {
            let body = vec![
                Part::Line(vec![Span::raw(" No language servers for this session.")]),
                Part::Wrap {
                    indent: 1,
                    text: "LSP is off by default. To turn it on, set lsp.enabled in \
                           ~/.canter/config.json and run canter serve --replace."
                        .into(),
                    style: theme::secondary(),
                    rows: usize::MAX,
                },
            ];
            return Report::new(title, body, Vec::new());
        }
        let width = |text: fn(&LanguageServer, Option<&str>) -> String| {
            self.servers
                .iter()
                .map(|server| str_width(&text(server, home)))
                .max()
                .unwrap_or(0)
        };
        let columns = (width(|s, _| clean(&s.name)), width(root));
        let body = self
            .servers
            .iter()
            .enumerate()
            .flat_map(|(index, server)| {
                server_rows(server, Some(index == self.selected), home, columns)
            })
            .collect();
        Report::new(title, body, Vec::new()).keys(" Enter details · Esc close")
    }
}

/// `/lsp` when the server does not have the `lsp` feature.
pub(crate) fn unsupported() -> Report {
    Report::new(
        vec![Span::styled(" LSP", Style::new().bold())],
        vec![Part::Wrap {
            indent: 1,
            text: "This canter server does not support LSP. Run canter serve --replace.".into(),
            style: Style::default(),
            rows: usize::MAX,
        }],
        Vec::new(),
    )
}

fn root(server: &LanguageServer, home: Option<&str>) -> String {
    let root = clean(&server.root);
    match home.map(|home| home.trim_end_matches('/')) {
        Some(home) if !home.is_empty() && root == home => "~".into(),
        Some(home) if !home.is_empty() && root.starts_with(&format!("{home}/")) => {
            format!("~{}", &root[home.len()..])
        }
        _ => root,
    }
}

fn memory(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= 1024 * MB {
        format!("{:.1} GB", bytes as f64 / (1024 * MB) as f64)
    } else {
        format!("{} MB", (bytes + MB / 2) / MB)
    }
}

/// A server's row: whether it is `selected` in the list (`None` with
/// details), its state's dot, name, root, `state · memory · N sessions`,
/// then its reason under it. `columns` pads the name and root.
fn server_rows(
    server: &LanguageServer,
    selected: Option<bool>,
    home: Option<&str>,
    (name_width, root_width): (usize, usize),
) -> Vec<Part> {
    let (state, colour) = match server.state {
        LanguageServerState::Starting => ("starting", theme::warning()),
        LanguageServerState::Running => ("running", theme::success()),
        LanguageServerState::Stopped => ("stopped", theme::secondary()),
        LanguageServerState::Unavailable => ("unavailable", theme::failure()),
        LanguageServerState::Reconnecting => ("reconnecting", theme::warning()),
        LanguageServerState::Unknown => ("unknown", theme::secondary()),
    };
    let mut about = vec![state.to_owned()];
    about.extend(server.rss_bytes.map(memory));
    about.push(plural(server.shared_references, "session"));
    let name = clean(&server.name);
    let mut before = match selected {
        Some(true) => vec![Span::styled(" › ", theme::accent().bold())],
        Some(false) => vec![Span::raw("   ")],
        None => vec![Span::raw(" ")],
    };
    before.extend([
        Span::styled("●", colour),
        Span::raw(" "),
        Span::styled(name.clone(), Style::new().bold()),
        Span::raw(" ".repeat(name_width.saturating_sub(str_width(&name)) + 3)),
    ]);
    let mut rows = vec![Part::Path {
        before,
        path: root(server, home),
        column: root_width,
        after: vec![
            Span::raw("   "),
            Span::styled(about.join(" · "), theme::secondary()),
        ],
    }];
    if let Some(reason) = &server.reason {
        rows.push(Part::Wrap {
            indent: if selected.is_some() { 5 } else { 3 },
            text: clean(reason),
            style: theme::secondary(),
            rows: 3,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_under_home_and_memory_read_short() {
        let server: LanguageServer = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "n", "root": "/home/me/proj", "state": "running",
            "sharedReferences": 1, "updatedAt": 0,
        }))
        .unwrap();
        assert_eq!(root(&server, Some("/home/me")), "~/proj");
        assert_eq!(root(&server, Some("/home/m")), "/home/me/proj");
        assert_eq!(root(&server, None), "/home/me/proj");
        assert_eq!(memory(312 * 1024 * 1024), "312 MB");
        assert_eq!(memory(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
