//! Rows for each kind of item: the user's prompt, the model's text, thinking,
//! and the tool cards (read a file, change files with a diff, run a command,
//! call an MCP tool, fetch a web page).

use std::ops::Range;

use canter_protocol::{
    FileUpdateChange, Item, ItemStatus, PatchChangeKind, ToolContentItem, Turn, TurnStatus,
    UserInput,
};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::format_elapsed;
use crate::diff::Diff;
use crate::highlight;
use crate::links::{self, Target};
use crate::markdown;
use crate::short_path::{PATH_MIN, fit_paths};
use crate::text::{
    LogicalLine, Row, char_width, drawn_width, sanitize, spans_width, str_width, truncate, visible,
    wrap_rows,
};
use crate::theme;

/// Output lines a finished command keeps.
const FINISHED_TAIL: usize = 3;
/// Output lines shown while a command runs.
const LIVE_TAIL: usize = 5;
/// Output lines a failed command keeps: the error is what matters then.
const FAILED_TAIL: usize = 10;
/// Result lines an MCP call keeps, from the start.
const MCP_HEAD: usize = 3;
/// Output lines one item shows at most with details (Ctrl+O): the first
/// and last half of them.
const DETAILS_LINES: usize = 500;
const GUTTER: &str = "    ┃ ";
/// Rows a command takes at most in a folded card, counting the rows
/// wrapping adds.
const COMMAND_ROWS: usize = 5;
/// Where a card's title text starts: after `  ● Run `.
const TITLE_INDENT: &str = "        ";
/// Diff lines a changed file shows in the history; the approval box shows all.
const EDIT_LINES: usize = 12;
/// Between a moved file's old and new paths.
const ARROW: &str = " → ";
/// Diff lines a new file shows in the history. A deleted file shows none
/// until details are shown.
const ADD_LINES: usize = 6;
/// Where a card's diff lines start: under the text of its command output.
const DIFF_INDENT: &str = "      ";

/// A blank row, `› ` and the text, then another blank row. `source` names the
/// client that sent it when that was not this terminal.
pub(crate) fn prompt(input: &[UserInput], source: Option<&str>, width: u16) -> Vec<Row> {
    let rows = prompt_rows(input, source, width);
    if rows.is_empty() {
        return rows;
    }
    let mut spaced = vec![Row::blank()];
    spaced.extend(rows);
    spaced.push(Row::blank());
    spaced
}

/// A message the user sent: [`prompt`], on the user's background when the
/// terminal can show it ([`on_background`]).
pub(crate) fn sent(input: &[UserInput], source: Option<&str>, width: u16) -> Vec<Row> {
    match theme::user_bg() {
        Some(color) => on_background(prompt_rows(input, source, width), color),
        None => prompt(input, source, width),
    }
}

/// `rows` on `color` to the edge of the window, with a row of it above and
/// below, and a blank row outside those. A copy leaves those two rows out.
pub(crate) fn on_background(rows: Vec<Row>, color: Color) -> Vec<Row> {
    if rows.is_empty() {
        return rows;
    }
    let edge = Row {
        fill: Some(color),
        ..Row::marks(Line::default())
    };
    let mut out = vec![Row::blank(), edge.clone()];
    out.extend(rows.into_iter().map(|row| Row {
        fill: Some(color),
        ..row
    }));
    out.extend([edge, Row::blank()]);
    out
}

/// `› ` and the text, each line wrapped under the first row's text.
fn prompt_rows(input: &[UserInput], source: Option<&str>, width: u16) -> Vec<Row> {
    let mut image_number = 0;
    let texts: Vec<String> = input
        .iter()
        .filter_map(|part| match part {
            UserInput::Skill { name } => Some(format!("Used skill `{name}`")),
            UserInput::Text { text } => Some(text.clone()),
            UserInput::LocalImage { name, .. } => {
                image_number += 1;
                Some(format!(
                    "[Image #{image_number} {}]",
                    name.as_deref().unwrap_or("image")
                ))
            }
            UserInput::Unknown => None,
        })
        .collect();
    if texts.is_empty() {
        return Vec::new();
    }
    let text = sanitize(&texts.join("\n"));
    let mut rows = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let indent = if i == 0 {
            Span::styled("› ", theme::accent().bold())
        } else {
            Span::raw("  ")
        };
        let mut body = vec![Span::raw(line.to_owned())];
        if let Some(source) = source.filter(|_| i == 0) {
            body.push(Span::styled(
                format!("  (from {})", sanitize(source)),
                theme::secondary(),
            ));
        }
        rows.extend(wrap_rows(&LogicalLine::new(vec![indent], body), width));
    }
    rows
}

/// The model's text as markdown, indented two columns, for a screen `width`
/// columns wide.
pub(crate) fn agent_message(text: &str, width: u16) -> Vec<LogicalLine> {
    markdown::render(&sanitize(text), width.saturating_sub(2))
        .into_iter()
        .map(|line| line.prefixed(Span::raw("  "), Span::raw("  ")))
        .collect()
}

/// Rows for an item; `finished` picks its final form over the in-progress one.
/// With `details` (Ctrl+O), nothing is folded: whole thinking, all output, all
/// of a diff.
pub(crate) fn item(
    item: &Item,
    finished: bool,
    details: bool,
    cwd: &str,
    width: u16,
    feedback: Option<&str>,
) -> Vec<Row> {
    let mut rows = match item {
        Item::ModelRetry { .. } => {
            wrap_all(&agent_message(&item.retry_message().unwrap(), width), width)
        }
        Item::BackgroundEvent {
            command_id, text, ..
        } => {
            let label =
                command_id.map_or_else(|| "Auto wake".into(), |id| format!("Background #{id}"));
            let mut rows = vec![title(
                dot(theme::accent()),
                "Event ",
                vec![Span::raw(label)],
                Vec::new(),
                None,
                width,
            )];
            rows.extend(output_rows(
                text,
                if details {
                    Fold::Details
                } else {
                    Fold::Head(20)
                },
                theme::secondary(),
                theme::accent(),
                width,
            ));
            rows
        }
        Item::BackgroundCommandAction {
            command_id,
            action,
            result,
            status,
            ..
        } => mcp_call(
            &format!(
                "{} #{command_id}",
                if action == "monitor" {
                    action.clone()
                } else {
                    format!("shell_{action}")
                }
            ),
            &serde_json::Value::Null,
            *status,
            result,
            details,
            width,
        ),
        Item::PlanProposal {
            plan,
            answer,
            status,
            ..
        } => {
            let mut rows = wrap_all(
                &agent_message(
                    &format!(
                        "**{}**",
                        crate::plan_mode::result_label(answer.as_ref(), *status)
                    ),
                    width,
                ),
                width,
            );
            let mut body = wrap_all(&agent_message(plan, width), width);
            if !details && body.len() > 8 {
                let count = body.len() - 8;
                body.truncate(8);
                body.extend(wrap_all(
                    &agent_message(&format!("… {count} more lines · Ctrl+O to expand"), width),
                    width,
                ));
            }
            rows.extend(body);
            if let Some(feedback) = answer
                .as_ref()
                .and_then(|answer| answer.feedback.as_deref())
            {
                rows.extend(wrap_all(
                    &agent_message(&format!("Feedback: {feedback}"), width),
                    width,
                ));
            }
            rows
        }
        Item::UserMessage { content, .. } => sent(content, None, width),
        Item::AgentMessage { text, status, .. } => {
            let text = if *status == Some(ItemStatus::Interrupted) {
                format!("{text}\n\n[Interrupted]")
            } else {
                text.clone()
            };
            wrap_all(&agent_message(&text, width), width)
        }
        Item::Reasoning {
            summary,
            content,
            duration_ms,
            status,
            ..
        } => {
            let mut rows = reasoning(summary, content, *duration_ms, finished, details, width);
            if *status == Some(ItemStatus::Interrupted) {
                rows.extend(wrap_all(&agent_message("[Interrupted]", width), width));
            }
            rows
        }
        Item::WebFetch {
            url,
            resolved_url,
            result,
            status,
            ..
        } => web_fetch(
            resolved_url.as_deref().unwrap_or(url),
            result,
            *status,
            details,
            width,
        ),
        Item::McpToolCall {
            server,
            tool,
            arguments,
            status,
            result,
            content_items,
            ..
        } => {
            let mut rows = mcp_call(
                &format!("{server}/{tool}"),
                arguments,
                *status,
                result,
                details,
                width,
            );
            rows.extend(tool_images(content_items, "Image ", width));
            rows
        }
        Item::CommandExecution {
            command,
            source,
            status,
            aggregated_output,
            exit_code,
            duration_ms,
            ..
        } => command_card(
            (command, *source),
            *status,
            *exit_code,
            *duration_ms,
            aggregated_output,
            details,
            width,
        ),
        Item::FileChange {
            changes, status, ..
        } => changes
            .iter()
            .flat_map(|change| file_change(change, *status, details, cwd, width))
            .collect(),
        Item::FileRead {
            path,
            start_line,
            content,
            status,
            content_items,
            ..
        } => {
            let rows = tool_images(content_items, "Read ", width);
            if rows.is_empty() {
                vec![file_read(path, *start_line, content, *status, cwd, width)]
            } else {
                rows
            }
        }
        Item::ContextCompaction { summary, .. } => vec![Row::text_from(
            Line::from(Span::styled(
                compaction_line(summary, finished),
                theme::secondary(),
            )),
            2,
        )],
        Item::UserInput {
            question,
            answer,
            status,
            ..
        } => {
            let answer = match answer {
                Some(canter_protocol::UserInputAnswer::Option { label }) => label.clone(),
                Some(canter_protocol::UserInputAnswer::Text { text }) => {
                    format!("Own answer: {text}")
                }
                _ if *status == ItemStatus::InProgress => "Waiting for your answer".into(),
                _ => "No answer".into(),
            };
            wrap_rows(
                &notice(
                    "? ",
                    &format!("{} → {}", visible(question), visible(&answer)),
                    theme::secondary(),
                ),
                width,
            )
        }
        Item::Plan {
            plan,
            explanation,
            status,
            ..
        } => crate::plan::card(plan, explanation.as_deref(), *status, width),
        Item::Unknown => Vec::new(),
    };
    if let Some(feedback) = feedback
        && matches!(
            item,
            Item::CommandExecution {
                status: ItemStatus::Declined,
                ..
            } | Item::FileChange {
                status: ItemStatus::Declined,
                ..
            } | Item::FileRead {
                status: ItemStatus::Declined,
                ..
            } | Item::WebFetch {
                status: ItemStatus::Declined,
                ..
            } | Item::McpToolCall {
                status: ItemStatus::Declined,
                ..
            }
        )
    {
        for line in sanitize(feedback).lines() {
            rows.extend(wrap_rows(
                &LogicalLine::new(
                    vec![Span::raw("    ")],
                    vec![Span::styled(line.to_owned(), theme::secondary())],
                ),
                width,
            ));
        }
    }
    rows
}

/// What a turn leaves behind once it has ended: `─ Worked for 12s ─`, or
/// why it did not end normally, with ` · 12s` when its time is known.
fn turn_footer(turn: &Turn, error_shown: bool) -> Option<LogicalLine> {
    let time = turn.duration_ms.map(took);
    let mut line = match turn.status {
        TurnStatus::Completed => {
            return time
                .map(|time| notice("─ ", &format!("Worked for {time} ─"), theme::secondary()));
        }
        TurnStatus::Interrupted => notice("■ ", "Turn interrupted", theme::secondary()),
        TurnStatus::Failed if !error_shown => {
            let message = turn
                .error
                .as_ref()
                .map_or("no reason given", |error| error.message.as_str());
            notice("✗ ", &format!("Turn failed: {message}"), theme::failure())
        }
        _ => return None,
    };
    if let Some(time) = time {
        line.body
            .push(Span::styled(format!(" · {time}"), theme::secondary()));
    }
    Some(line)
}

/// Files a turn's end lists at most; the rest are counted.
const CHANGED_FILES: usize = 8;

/// The rows a turn leaves once it has ended: what files canter's tools
/// changed in it ([`changed_files`]), then [`turn_footer`].
pub(crate) fn turn_end(turn: &Turn, error_shown: bool, cwd: &str, width: u16) -> Vec<Row> {
    let mut rows = changed_files(turn, cwd, width);
    rows.extend(
        turn_footer(turn, error_shown).map_or_else(Vec::new, |footer| wrap_rows(&footer, width)),
    );
    rows
}

/// `● Changed 3 files  +40 −12` and a row for each file the turn's
/// file-changing tools changed, in the order first changed, with its lines
/// added and removed in all; `new` and `deleted` after them. Nothing when
/// they changed none. Changes that failed or were declined do not count.
fn changed_files(turn: &Turn, cwd: &str, width: u16) -> Vec<Row> {
    struct File {
        /// The path, then where it was moved to, each time.
        names: Vec<String>,
        added: usize,
        removed: usize,
        created: bool,
        deleted: bool,
    }
    let mut files: Vec<File> = Vec::new();
    let mut at = std::collections::HashMap::new();
    let key = |path: &str| std::path::Path::new(cwd).join(path);
    for item in &turn.items {
        let Item::FileChange {
            changes,
            status: ItemStatus::Completed,
            ..
        } = item
        else {
            continue;
        };
        for change in changes {
            let diff = Diff::parse(&sanitize(&change.diff), &change.kind, &change.path);
            let index = *at.entry(key(&change.path)).or_insert_with(|| {
                files.push(File {
                    names: vec![display_path(&change.path, cwd)],
                    added: 0,
                    removed: 0,
                    created: false,
                    deleted: false,
                });
                files.len() - 1
            });
            let file = &mut files[index];
            file.added += diff.added();
            file.removed += diff.removed();
            match &change.kind {
                PatchChangeKind::Add => (file.created, file.deleted) = (true, false),
                PatchChangeKind::Delete => file.deleted = true,
                PatchChangeKind::Update {
                    move_path: Some(to),
                } => {
                    file.names.push(display_path(to, cwd));
                    at.insert(key(to), index);
                }
                _ => {}
            }
        }
    }
    if files.is_empty() {
        return Vec::new();
    }
    // `+a` unless only lines were removed (or none from a deleted file),
    // `−r` unless none were.
    let counts = |added: usize, removed: usize, deleted: bool| {
        (
            (added > 0 || (removed == 0 && !deleted)).then(|| format!("+{added}")),
            (removed > 0).then(|| format!("−{removed}")),
        )
    };
    let count_spans = |(added, removed): (Option<String>, Option<String>), added_width: usize| {
        let mut spans = Vec::new();
        let added = added.unwrap_or_default();
        spans.push(Span::raw(
            " ".repeat(added_width.saturating_sub(str_width(&added))),
        ));
        spans.push(Span::styled(added, theme::success()));
        if let Some(removed) = removed {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(removed, theme::failure()));
        }
        spans
    };
    let (added, removed) = files
        .iter()
        .fold((0, 0), |(a, r), file| (a + file.added, r + file.removed));
    let count = files.len();
    let mut header = vec![
        Span::raw("  "),
        dot(theme::secondary()),
        Span::styled(
            format!(
                "Changed {count} file{}  ",
                if count == 1 { "" } else { "s" }
            ),
            theme::secondary(),
        ),
    ];
    header.extend(count_spans(counts(added, removed, false), 0));
    let cut = |spans: Vec<Span<'static>>, from: usize| {
        Row::text_from(Line::from(truncate(spans, usize::from(width))), from)
    };
    let mut rows = vec![cut(header, 4)];
    let shown = &files[..count.min(CHANGED_FILES)];
    let file_counts: Vec<_> = shown
        .iter()
        .map(|file| counts(file.added, file.removed, file.deleted))
        .collect();
    let full_width = |file: &File| {
        let names: usize = file.names.iter().map(|name| str_width(name)).sum();
        names + str_width(ARROW) * (file.names.len() - 1)
    };
    let name_width = shown.iter().map(full_width).max().unwrap_or(0);
    let added_width = file_counts
        .iter()
        .map(|(added, _)| added.as_deref().map_or(0, str_width))
        .max()
        .unwrap_or(0);
    let counts_width = |(_, removed): &(Option<String>, Option<String>)| {
        added_width
            + removed
                .as_deref()
                .map_or(0, |removed| 1 + str_width(removed))
    };
    let all_counts_width = file_counts.iter().map(counts_width).max().unwrap_or(0);
    let word = |file: &File| {
        if file.deleted {
            Some("deleted")
        } else if file.created {
            Some("new")
        } else {
            None
        }
    };
    // The names take what the widest row leaves of the width, all alike so
    // the counts stay in a column; paths lose directories to fit
    // ([`fit_paths`]), never into fewer than [`PATH_MIN`] columns.
    let after_names = shown
        .iter()
        .zip(&file_counts)
        .map(|(file, counts)| match word(file) {
            Some(word) => 2 + all_counts_width + 2 + str_width(word),
            None => 2 + counts_width(counts),
        })
        .max()
        .unwrap_or(0);
    let name_width = usize::from(width)
        .saturating_sub(6 + after_names)
        .max(PATH_MIN)
        .min(name_width);
    for (file, counts) in shown.iter().zip(file_counts) {
        let names: Vec<&str> = file.names.iter().map(String::as_str).collect();
        let arrows = str_width(ARROW) * (names.len() - 1);
        let name = fit_paths(&names, name_width.saturating_sub(arrows)).join(ARROW);
        let pad = name_width.saturating_sub(str_width(&name)) + 2;
        let after = all_counts_width - counts_width(&counts) + 2;
        let mut spans = vec![
            Span::raw("      "),
            Span::styled(name, theme::secondary()),
            Span::raw(" ".repeat(pad)),
        ];
        spans.extend(count_spans(counts, added_width));
        if let Some(word) = word(file) {
            spans.push(Span::raw(" ".repeat(after)));
            spans.push(Span::styled(word, theme::secondary()));
        }
        rows.push(cut(spans, 6));
    }
    if count > CHANGED_FILES {
        rows.push(cut(
            vec![Span::styled(
                format!("      … and {} more", count - CHANGED_FILES),
                theme::secondary(),
            )],
            6,
        ));
    }
    rows
}

/// How long a turn or its thinking took: `<1s` under a second, otherwise
/// as the status line counts (`12s`, `3m 05s`).
fn took(ms: u64) -> String {
    if ms < 1000 {
        "<1s".to_owned()
    } else {
        format_elapsed(i64::try_from(ms).unwrap_or(i64::MAX))
    }
}

/// The summary the core gives a compaction that did not finish
/// (`canter-core/src/compaction.rs`).
const COMPACTION_KEPT: &str = "Compaction did not finish; the original context is kept.";

/// The line for a compaction: still going, compacted, or not finished. A
/// finished one with no summary never got completed (its turn ended
/// first); a summary that compacted is never empty.
fn compaction_line(summary: &str, finished: bool) -> &'static str {
    if !finished {
        "  ── Compacting context… ──"
    } else if summary.is_empty() || summary == COMPACTION_KEPT {
        "  ── Compaction did not finish; the original context is kept ──"
    } else {
        "  ── Context compacted ──"
    }
}

/// A one-line message from the client or server (warning, error, send failure).
pub(crate) fn notice(mark: &str, text: &str, style: Style) -> LogicalLine {
    LogicalLine::new(
        vec![Span::raw("  "), Span::styled(mark.to_owned(), style)],
        vec![Span::styled(sanitize(text), style)],
    )
}

pub(crate) fn wrap_all(lines: &[LogicalLine], width: u16) -> Vec<Row> {
    lines
        .iter()
        .flat_map(|line| wrap_rows(line, width))
        .collect()
}

/// Thinking, grey and italic, from the summary when there is one. Folded it
/// is one row: `Thinking… <the latest line>` while it runs, `Thought: <the
/// first line>` once done, `Thought for 4s: …` when its time is known. With
/// `details`, a `Thought` (`Thought for 4s`) row and then the whole text as
/// markdown, each row after a grey `┊`.
fn reasoning(
    summary: &[String],
    content: &[String],
    duration_ms: Option<u64>,
    finished: bool,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let style = theme::thinking();
    let summarized = summary.iter().any(|part| !part.trim().is_empty());
    let parts = if summarized { summary } else { content };
    let text = sanitize(&parts.join("\n\n"));
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let thought = match duration_ms {
        Some(ms) => format!("  Thought for {}", took(ms)),
        None => "  Thought".to_owned(),
    };
    if details {
        if finished && text.trim().is_empty() {
            return Vec::new();
        }
        let heading = if finished {
            thought
        } else {
            "  Thinking…".to_owned()
        };
        let mut rows = vec![Row::text_from(Line::from(Span::styled(heading, style)), 2)];
        let bar = || Span::styled("  ┊ ", theme::secondary());
        let thinking = |spans: Vec<Span<'static>>| {
            spans
                .into_iter()
                .map(|span| span.patch_style(style))
                .collect()
        };
        for line in markdown::render(&text, width.saturating_sub(4)) {
            let line = LogicalLine {
                indent: thinking(line.indent),
                hang: thinking(line.hang),
                body: thinking(line.body),
                marker: line.marker,
                // Thinking is not the answer: its addresses are not links.
                links: Vec::new(),
            };
            rows.extend(wrap_rows(&line.prefixed(bar(), bar()), width));
        }
        return rows;
    }
    if !finished {
        let lead = "  Thinking…";
        let Some(latest) = lines.next_back() else {
            return vec![Row::text_from(Line::from(Span::styled(lead, style)), 2)];
        };
        let room = usize::from(width).saturating_sub(str_width(lead) + 1);
        let shown = if str_width(latest) <= room {
            latest.to_owned()
        } else {
            format!("…{}", tail(latest, room.saturating_sub(1)).trim_start())
        };
        let row = truncate(
            vec![Span::styled(format!("{lead} {shown}"), style)],
            usize::from(width),
        );
        return vec![Row::text_from(Line::from(row), 2)];
    }
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    // A summary starts with its title in bold: the title alone is the row.
    let (first, rest) = match first
        .strip_prefix("**")
        .and_then(|after| after.split_once("**"))
    {
        Some((title, rest)) if summarized && !title.trim().is_empty() => (title.trim(), rest),
        _ => (first, ""),
    };
    let more = if !rest.trim().is_empty() || lines.next().is_some() {
        "…"
    } else {
        ""
    };
    let row = truncate(
        vec![Span::styled(format!("{thought}: {first}{more}"), style)],
        usize::from(width),
    );
    vec![Row::text_from(Line::from(row), 2)]
}

/// The end of `text` that fits in `width` columns.
fn tail(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices().rev() {
        used += char_width(c);
        if used > width {
            return &text[at + c.len_utf8()..];
        }
    }
    text
}

fn tool_images(items: &[ToolContentItem], verb: &str, width: u16) -> Vec<Row> {
    items
        .iter()
        .filter_map(|item| {
            let ToolContentItem::LocalImage {
                name,
                mime_type,
                size_bytes,
                ..
            } = item
            else {
                return None;
            };
            let format = mime_type
                .strip_prefix("image/")
                .unwrap_or(mime_type)
                .to_uppercase();
            let mut spans = vec![
                Span::raw("  "),
                Span::styled("◇ ", theme::secondary()),
                Span::styled(verb.to_owned(), Style::new().bold()),
                Span::raw(visible(name)),
                Span::styled(
                    format!(
                        "  {} · {}",
                        visible(&format),
                        format_size(*size_bytes as usize)
                    ),
                    theme::secondary(),
                ),
            ];
            fit_path_spans(&mut spans, &[3], usize::from(width));
            Some(cut_row(spans, Vec::new(), width))
        })
        .collect()
}

fn file_read(
    path: &str,
    start_line: u32,
    content: &str,
    status: ItemStatus,
    cwd: &str,
    width: u16,
) -> Row {
    let mark = if status == ItemStatus::Failed {
        cross()
    } else {
        Span::styled("◇ ", theme::secondary())
    };
    let mut spans = vec![
        Span::raw("  "),
        mark,
        Span::styled("Read ", Style::new().bold()),
        Span::raw(display_path(path, cwd)),
    ];
    let lines = content.lines().count() as u32;
    let detail = match status {
        ItemStatus::InProgress => None,
        ItemStatus::Failed => Some(Span::styled("  failed", theme::failure())),
        ItemStatus::Declined => Some(Span::styled("  declined", theme::secondary())),
        _ if lines == 0 => Some(Span::styled("  empty", theme::secondary())),
        _ if lines == 1 => Some(Span::styled(
            format!("  line {start_line}"),
            theme::secondary(),
        )),
        _ => Some(Span::styled(
            format!("  lines {start_line}–{}", start_line + lines - 1),
            theme::secondary(),
        )),
    };
    spans.extend(detail);
    fit_path_spans(&mut spans, &[3], usize::from(width));
    let links: Vec<_> = links::path(path, cwd)
        .map(|target| span_link(&spans, 3, target))
        .into_iter()
        .collect();
    cut_row(spans, links, width)
}

/// Shortens the paths that are spans `paths` of `spans` ([`fit_paths`]) so
/// the row fits in `width` columns with what follows them; never into
/// fewer than [`PATH_MIN`] columns each, and past that the row is cut at
/// its end.
fn fit_path_spans(spans: &mut [Span<'static>], paths: &[usize], width: usize) {
    let texts: Vec<String> = paths
        .iter()
        .map(|&index| spans[index].content.to_string())
        .collect();
    let full: usize = texts.iter().map(|text| str_width(text)).sum();
    let room = width
        .saturating_sub(spans_width(spans) - full)
        .max(full.min(PATH_MIN * paths.len()));
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    for (&index, fitted) in paths.iter().zip(fit_paths(&texts, room)) {
        spans[index].content = fitted.into();
    }
}

/// A card's title row of `spans`, cut to `width`, with `links` on what is
/// left of them; a copy takes it from the verb.
fn cut_row(spans: Vec<Span<'static>>, links: Vec<(Range<usize>, Target)>, width: u16) -> Row {
    let line = Line::from(truncate(spans, usize::from(width)));
    let end = drawn_width(&line.spans);
    Row {
        links: clip(links, end),
        ..Row::text_from(line, 4)
    }
}

/// Where span `index` of `spans` is drawn, as a link to `target`.
fn span_link(spans: &[Span<'static>], index: usize, target: Target) -> (Range<usize>, Target) {
    (
        drawn_width(&spans[..index])..drawn_width(&spans[..=index]),
        target,
    )
}

/// `links` in the first `end` columns: what is left of them on a row cut to fit.
fn clip(links: Vec<(Range<usize>, Target)>, end: usize) -> Vec<(Range<usize>, Target)> {
    links
        .into_iter()
        .map(|(columns, target)| (columns.start..columns.end.min(end), target))
        .filter(|(columns, _)| !columns.is_empty())
        .collect()
}

/// An item waiting for an approval: only its title row, marked 「等你批准」.
/// The approval box below shows the whole command or diff.
pub(crate) fn waiting(item: &Item, details: bool, cwd: &str, width: u16) -> Vec<Row> {
    let mark = || vec![Span::styled("Waiting for approval", theme::warning())];
    match item {
        Item::CommandExecution { command, .. } => command_title(
            command,
            false,
            dot(theme::secondary()),
            mark(),
            false,
            width,
        ),
        Item::FileChange { changes, .. } => changes
            .iter()
            .map(|change| {
                let diff = Diff::parse(&sanitize(&change.diff), &change.kind, &change.path);
                let room = left_room(&mark(), usize::from(width));
                let (header, links) =
                    change_header(change, &diff, dot(theme::secondary()), None, cwd, room);
                card_row(right_aligned(header, mark(), usize::from(width)), links)
            })
            .collect(),
        _ => self::item(item, false, details, cwd, width, None),
    }
}

fn file_change(
    change: &FileUpdateChange,
    status: ItemStatus,
    details: bool,
    cwd: &str,
    width: u16,
) -> Vec<Row> {
    let diff = Diff::parse(&sanitize(&change.diff), &change.kind, &change.path);
    let (bullet, suffix) = match status {
        ItemStatus::Blocked => (
            dot(theme::warning()),
            Some(Span::styled("  blocked (Plan mode)", theme::warning())),
        ),
        ItemStatus::InProgress => (dot(theme::secondary()), None),
        ItemStatus::Failed => (cross(), Some(Span::styled("  failed", theme::failure()))),
        ItemStatus::Declined => (
            dot(theme::secondary()),
            Some(Span::styled("  declined", theme::secondary())),
        ),
        _ => (dot(theme::success()), None),
    };
    let (header, links) = change_header(change, &diff, bullet, suffix, cwd, usize::from(width));
    let mut rows = vec![cut_row(header, links, width)];
    let limit = match &change.kind {
        _ if details => None,
        PatchChangeKind::Add => Some(ADD_LINES),
        PatchChangeKind::Delete => return rows,
        _ => Some(EDIT_LINES),
    };
    rows.extend(diff.rows(DIFF_INDENT, width, limit));
    rows
}

/// `● 改 path  +a −r` and `suffix`: a file change's title, its paths
/// shortened to fit in `width` columns ([`fit_path_spans`]), and the link
/// on its path as drawn: the file as it is after the change, so none for a
/// deleted file, and the new path of a moved one.
fn change_header(
    change: &FileUpdateChange,
    diff: &Diff,
    bullet: Span<'static>,
    suffix: Option<Span<'static>>,
    cwd: &str,
    width: usize,
) -> (Vec<Span<'static>>, Vec<(Range<usize>, Target)>) {
    let (added, removed) = (diff.added(), diff.removed());
    let verb = match &change.kind {
        PatchChangeKind::Add => "Add ",
        PatchChangeKind::Delete => "Delete ",
        _ => "Edit ",
    };
    let mut header = vec![
        Span::raw("  "),
        bullet,
        Span::styled(verb, Style::new().bold()),
        Span::raw(display_path(&change.path, cwd)),
    ];
    let mut paths = vec![3];
    let mut link = match &change.kind {
        PatchChangeKind::Delete => None,
        _ => Some((3, change.path.as_str())),
    };
    if let PatchChangeKind::Update {
        move_path: Some(to),
    } = &change.kind
    {
        header.push(Span::raw(ARROW));
        header.push(Span::raw(display_path(to, cwd)));
        paths.push(header.len() - 1);
        link = Some((header.len() - 1, to.as_str()));
    }
    header.push(Span::raw("  "));
    match &change.kind {
        PatchChangeKind::Add => header.push(Span::styled(format!("+{added}"), theme::success())),
        PatchChangeKind::Delete => {
            header.push(Span::styled(format!("−{removed}"), theme::failure()))
        }
        _ => header.extend([
            Span::styled(format!("+{added}"), theme::success()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), theme::failure()),
        ]),
    }
    header.extend(suffix);
    fit_path_spans(&mut header, &paths, width);
    let links = link
        .and_then(|(index, path)| Some(span_link(&header, index, links::path(path, cwd)?)))
        .into_iter()
        .collect();
    (header, links)
}

/// Whether a command's exit code 1 only means its search found nothing: the
/// whole command is one `rg` or `grep`, it printed nothing, and it exited 1.
/// Other commands' exit code 1 still means failure.
pub(crate) fn no_matches(command: &str, exit_code: Option<i32>, output: &str) -> bool {
    exit_code == Some(1)
        && output.trim().is_empty()
        && matches!(single_program(command).as_deref(), Some("rg" | "grep"))
}

/// The name of the one program a command runs, without its directory, when
/// the shell would run exactly that simple command. `None` when it would do
/// more: pipes, `&&`, `;`, redirection, substitutions, several lines.
fn single_program(command: &str) -> Option<String> {
    let mut lines = command.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if open == '"' && matches!(c, '$' | '`') {
                // Substitutions still run inside double quotes.
                return None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                started = true;
            }
            '\\' => {
                escaped = true;
                started = true;
            }
            // A comment starts at the beginning of a word and runs to the line's end.
            '#' if !started => break,
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '$' => return None,
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if started {
        words.push(word);
    }
    let first = words.first()?;
    let name = first.rsplit('/').next()?;
    Some(name.strip_suffix(".exe").unwrap_or(name).to_owned())
}

fn command_card(
    command: (&str, canter_protocol::ExecCommandSource),
    status: ItemStatus,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: &str,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let (command, source) = command;
    let dim = theme::secondary();
    let took = duration_ms.map(|ms| Span::styled(format!(" · {}", format_duration(ms)), dim));
    let searched_nothing = no_matches(command, exit_code, output);
    let (bullet, mut right) = match (status, exit_code) {
        (ItemStatus::InProgress, _) => (dot(dim), vec![Span::styled("running", dim)]),
        (ItemStatus::Declined, _) => (dot(dim), vec![Span::styled("declined", dim)]),
        (ItemStatus::Blocked, _) => (
            dot(theme::warning()),
            vec![Span::styled("blocked (Plan mode)", theme::warning())],
        ),
        _ if searched_nothing => (dot(dim), vec![Span::styled("No matches", dim)]),
        (_, Some(0)) => (dot(theme::success()), vec![Span::styled("exit 0", dim)]),
        (_, Some(code)) => (
            cross(),
            vec![Span::styled(format!("exit {code}"), theme::failure())],
        ),
        (ItemStatus::Failed, None) => (cross(), vec![Span::styled("failed", theme::failure())]),
        (_, None) => (dot(theme::success()), Vec::new()),
    };
    right.extend(took);
    // The same cases as the `✗` above.
    let failed = !searched_nothing
        && !matches!(status, ItemStatus::InProgress | ItemStatus::Declined)
        && exit_code.map_or(status == ItemStatus::Failed, |code| code != 0);
    let user = source == canter_protocol::ExecCommandSource::UserShell;
    let mut rows = command_title(command, user, bullet, right, details, width);
    let (fold, style) = if details {
        (Fold::Details, if failed { Style::new() } else { dim })
    } else if status == ItemStatus::InProgress {
        (Fold::Tail(LIVE_TAIL), dim)
    } else if failed {
        (Fold::Tail(FAILED_TAIL), Style::new())
    } else {
        (Fold::Tail(FINISHED_TAIL), dim)
    };
    rows.extend(output_rows(output, fold, style, bar(failed), width));
    rows
}

/// The bar before a card's output: red when the card failed, else grey.
fn bar(failed: bool) -> Style {
    if failed {
        theme::failure()
    } else {
        theme::secondary()
    }
}

/// A command on one row: its first line, then ` …` when it has more.
pub(crate) fn command_line(command: &str) -> String {
    let clean = visible(command);
    let mut lines = clean.lines();
    let first = lines.next().unwrap_or_default().to_owned();
    if lines.next().is_some() {
        first + " …"
    } else {
        first
    }
}

/// `● Call server/tool  {arguments}`, the arguments as JSON on one row, then
/// the first lines of the result: an MCP result's start matters most.
fn mcp_call(
    name: &str,
    arguments: &serde_json::Value,
    status: ItemStatus,
    result: &str,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let (bullet, right) = status_marks(status);
    let mut object = vec![Span::raw(first_line(name))];
    let empty = arguments.is_null() || arguments.as_object().is_some_and(|args| args.is_empty());
    if !empty {
        object.push(Span::raw(format!(
            "  {}",
            first_line(&arguments.to_string())
        )));
    }
    let mut rows = vec![title(bullet, "Call ", object, right, None, width)];
    let fold = if details {
        Fold::Details
    } else {
        Fold::Head(MCP_HEAD)
    };
    let failed = status == ItemStatus::Failed;
    let style = if failed {
        Style::new()
    } else {
        theme::secondary()
    };
    rows.extend(output_rows(result, fold, style, bar(failed), width));
    rows
}

/// `● Fetch <url>` and the size fetched. The page is for the model, so it
/// shows only with details; a one-line note (a redirect to another host,
/// content left out) shows as it is, and a failure's reason in red.
fn web_fetch(url: &str, result: &str, status: ItemStatus, details: bool, width: u16) -> Vec<Row> {
    let dim = theme::secondary();
    let (bullet, mut right) = status_marks(status);
    let one_line = !result.trim_end().contains('\n');
    let redirect = one_line && result.starts_with("Redirected to ");
    let note = one_line
        && (result.starts_with("[Content omitted: ")
            || (result.starts_with("Download exceeds ")
                && result.trim_end().ends_with("; content omitted.")));
    let page = status == ItemStatus::Completed && !redirect && !note;
    if status == ItemStatus::Completed {
        if redirect {
            right = vec![Span::styled("redirect", dim)];
        } else if page {
            right = vec![Span::styled(format_size(result.len()), dim)];
        }
    }
    let mut rows = vec![title(
        bullet,
        "Fetch ",
        vec![Span::raw(first_line(url))],
        right,
        links::web(url),
        width,
    )];
    if page && !details {
        return rows;
    }
    let fold = if details {
        Fold::Details
    } else {
        Fold::Head(FINISHED_TAIL)
    };
    let failed = status == ItemStatus::Failed;
    let style = if failed { theme::failure() } else { dim };
    rows.extend(output_rows(result, fold, style, bar(failed), width));
    rows
}

/// The mark and right-hand status of a card that has no exit code.
fn status_marks(status: ItemStatus) -> (Span<'static>, Vec<Span<'static>>) {
    let dim = theme::secondary();
    match status {
        ItemStatus::Blocked => (
            dot(theme::warning()),
            vec![Span::styled("blocked (Plan mode)", theme::warning())],
        ),
        ItemStatus::InProgress => (dot(dim), vec![Span::styled("running", dim)]),
        ItemStatus::Declined => (dot(dim), vec![Span::styled("declined", dim)]),
        ItemStatus::Failed => (cross(), vec![Span::styled("failed", theme::failure())]),
        _ => (dot(theme::success()), Vec::new()),
    }
}

/// Which output lines a card keeps.
enum Fold {
    /// The first lines, then `… N more lines · Ctrl+O`.
    Head(usize),
    /// `… N more lines · Ctrl+O`, then the last lines.
    Tail(usize),
    /// All of them, up to `DETAILS_LINES`: past that the first and last half
    /// with one row between saying how many are left out.
    Details,
}

/// Output rows, each after `    ┃ ` in `bar` and cut to `width`, in `style`.
fn output_rows(output: &str, fold: Fold, style: Style, bar: Style, width: u16) -> Vec<Row> {
    let dim = theme::secondary();
    let output = sanitize(output);
    let mut lines: Vec<&str> = output.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let all = lines.len();
    // Output line `source`.
    let row = |source: usize| {
        let spans = truncate(
            vec![
                Span::styled(GUTTER, bar),
                Span::styled(lines[source].to_owned(), style),
            ],
            usize::from(width),
        );
        Row {
            source: Some(source),
            ..Row::text_from(Line::from(spans), str_width(GUTTER))
        }
    };
    let note = |text: String| {
        Row::marks(Line::from(vec![
            Span::styled(GUTTER, bar),
            Span::styled(text, dim),
        ]))
    };
    let more = |hidden: usize| note(format!("… {hidden} more lines · Ctrl+O"));
    let mut rows = Vec::new();
    match fold {
        Fold::Head(keep) => {
            rows.extend((0..all.min(keep)).map(row));
            if all > keep {
                rows.push(more(all - keep));
            }
        }
        Fold::Tail(keep) => {
            if all > keep {
                rows.push(more(all - keep));
            }
            rows.extend((all.saturating_sub(keep)..all).map(row));
        }
        Fold::Details if all > DETAILS_LINES => {
            let half = DETAILS_LINES / 2;
            rows.extend((0..half).map(row));
            rows.push(note(format!("… {} lines not shown", all - DETAILS_LINES)));
            rows.extend((all - half..all).map(row));
        }
        Fold::Details => rows.extend((0..all).map(row)),
    }
    rows
}

/// `● Run <command>` with `right` flush with the right edge of the first
/// row. The command is coloured as Bash, as in the approval box, and each of
/// its lines wraps by words under where it starts. Unless `whole`, it takes
/// at most `COMMAND_ROWS` rows, then a row saying how many more there are.
/// Control characters in the command show as escapes.
/// With `bang`, the user ran it: `! ` before it, the `!` red.
fn command_title(
    command: &str,
    bang: bool,
    bullet: Span<'static>,
    right: Vec<Span<'static>>,
    whole: bool,
    width: u16,
) -> Vec<Row> {
    let clean = visible(command);
    let mut lines: Vec<&str> = clean.lines().collect();
    if lines.is_empty() {
        lines.push("");
    }
    let mut bodies = highlight::bash(&lines).unwrap_or_else(|| {
        lines
            .iter()
            .map(|line| vec![Span::raw((*line).to_owned())])
            .collect()
    });
    if bang {
        bodies[0].splice(0..0, [Span::styled("!", theme::shell()), Span::raw(" ")]);
    }
    // The first row leaves room for `right` where `right_aligned` puts it.
    let right_width = spans_width(&right);
    let reserve = if right.is_empty() || usize::from(width) < right_width + 12 {
        0
    } else {
        right_width + 2
    };
    let hang = || vec![Span::raw(TITLE_INDENT)];
    let mut rows = Vec::new();
    for (index, body) in bodies.into_iter().enumerate() {
        let indent = if index == 0 {
            vec![Span::raw(" ".repeat(TITLE_INDENT.len() + reserve))]
        } else {
            hang()
        };
        let line = LogicalLine {
            indent,
            hang: hang(),
            body,
            marker: 0,
            links: Vec::new(),
        };
        rows.extend(wrap_rows(&line, width));
    }
    // The mark and verb in place of the first row's indent, the status after it.
    let mut left = vec![
        Span::raw("  "),
        bullet,
        Span::styled("Run ", Style::new().bold()),
    ];
    left.extend(rows[0].line.spans.drain(1..));
    rows[0] = card_row(right_aligned(left, right, usize::from(width)), Vec::new());
    if !whole && rows.len() > COMMAND_ROWS {
        let hidden = rows.len() - COMMAND_ROWS;
        rows.truncate(COMMAND_ROWS);
        rows.push(Row::marks(Line::from(vec![
            Span::raw(TITLE_INDENT),
            Span::styled(
                format!("… {hidden} more lines · Ctrl+O"),
                theme::secondary(),
            ),
        ])));
    }
    rows
}

/// A card's first row: `● Verb object` with `right` flush with the right
/// edge; the object a link to `link` when there is one.
fn title(
    bullet: Span<'static>,
    verb: &'static str,
    object: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    link: Option<Target>,
    width: u16,
) -> Row {
    let mut left = vec![
        Span::raw("  "),
        bullet,
        Span::styled(verb, Style::new().bold()),
    ];
    let start = drawn_width(&left);
    let links = link
        .map(|target| (start..start + drawn_width(&object), target))
        .into_iter()
        .collect();
    left.extend(object);
    card_row(right_aligned(left, right, usize::from(width)), links)
}

/// A card's title row: a copy takes it from the verb to the end of the
/// left side, leaving out the mark and the status on the right. `links`
/// are on the left side.
fn card_row((line, left): (Line<'static>, usize), links: Vec<(Range<usize>, Target)>) -> Row {
    let end = left.min(drawn_width(&line.spans));
    Row {
        text: Some(4..left),
        links: clip(links, end),
        ..Row::from(line)
    }
}

/// The first line of `text` with control characters as escapes, ending with
/// ` …` when more lines follow.
fn first_line(text: &str) -> String {
    let clean = visible(text);
    let mut lines = clean.lines();
    let mut shown = lines.next().unwrap_or_default().to_owned();
    if lines.next().is_some() {
        shown.push_str(" …");
    }
    shown
}

/// `12 B`, `18.4 KB`, `1.2 MB`.
fn format_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let size = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if size < KB * KB {
        format!("{:.1} KB", size / KB)
    } else {
        format!("{:.1} MB", size / KB / KB)
    }
}

/// A card's mark: `●` in `style`.
fn dot(style: Style) -> Span<'static> {
    Span::styled("● ", style)
}

/// A failed card's mark: a red `✗`.
fn cross() -> Span<'static> {
    Span::styled("✗ ", theme::failure())
}

/// `left` then `right` flush with the right edge; the left side is cut to fit.
/// Also returns where the left side ends as drawn (`usize::MAX` when `right`
/// was left out): the gap is counted by character, but an emoji of several
/// characters is drawn narrower, so the right side starts sooner.
fn right_aligned(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> (Line<'static>, usize) {
    let right_width = spans_width(&right);
    let room = left_room(&right, width);
    if room == width {
        return (Line::from(truncate(left, width)), usize::MAX);
    }
    let mut spans = truncate(left, room);
    let left_width = spans_width(&spans);
    let left_end = drawn_width(&spans);
    let gap = width - right_width - left_width;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    (Line::from(spans), left_end)
}

/// The columns [`right_aligned`] leaves the left side: all of `width` when
/// `right` is left out, otherwise up to two columns before it.
fn left_room(right: &[Span<'static>], width: usize) -> usize {
    let right_width = spans_width(right);
    if right.is_empty() || width < right_width + 12 {
        width
    } else {
        width - right_width - 2
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// Paths inside the session's working directory are shown relative to it.
/// Control characters show as escapes.
pub(crate) fn display_path(path: &str, cwd: &str) -> String {
    let path = visible(path);
    if cwd.is_empty() {
        return path;
    }
    match path.strip_prefix(cwd.trim_end_matches('/')) {
        Some(rest) if rest.starts_with('/') && rest.len() > 1 => rest[1..].to_owned(),
        _ => path,
    }
}
