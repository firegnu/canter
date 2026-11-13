//! Rows for each kind of item: the user's prompt, the model's text, thinking,
//! and the tool cards (read a file, change files with a diff, run a command,
//! call an MCP tool, fetch a web page).

use std::ops::Range;

use canter_protocol::{
    AgentStatus, CollabAgentTool, FileUpdateChange, Item, ItemStatus, PatchChangeKind,
    ToolContentItem, Turn, TurnStatus, UserInput,
};
use chrono::TimeZone;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::app::format_elapsed;
use crate::diff::Diff;
use crate::links::{self, Target};
use crate::markdown;
use crate::programs;
use crate::short_path::{PATH_MIN, fit_paths};
use crate::text::{
    LogicalLine, Row, char_width, drawn_width, push_linked, sanitize, spans_width, str_width,
    truncate, visible, wrap_rows,
};
use crate::theme;

/// Output lines a finished command keeps, from the start.
const FINISHED_LINES: usize = 3;
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
/// Before the first row of a command's output.
const BRANCH: &str = "  └ ";
/// Rows a command takes at most in a folded card, counting the rows
/// wrapping adds.
const COMMAND_ROWS: usize = 5;
/// Diff lines a changed file shows in the history; the approval box shows all.
const EDIT_LINES: usize = 12;
/// Between a moved file's old and new paths.
const ARROW: &str = " → ";
/// Diff lines a new file shows in the history. A deleted file shows none
/// until details are shown.
const ADD_LINES: usize = 6;
/// Where a card's diff lines start: under the text of its command output.
const DIFF_INDENT: &str = "    ";
/// Between a card's title and its status.
const STATUS_SEPARATOR: &str = " · ";

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

/// What a model retry in a turn shows: retries in a row are one row, on the
/// last of them. While it is the turn's latest item and the turn goes on,
/// the retry on its way; after that, how many retries there were and why.
pub(crate) enum Retry {
    /// Another retry comes right after it.
    Hidden,
    Waiting(String),
    Over(String),
}

/// `None` when the item at `index` is not a model retry.
pub(crate) fn retry(turn: &Turn, index: usize) -> Option<Retry> {
    let item = &turn.items[index];
    let Item::ModelRetry { error, attempt, .. } = item else {
        return None;
    };
    Some(match turn.items.get(index + 1) {
        Some(Item::ModelRetry { .. }) => Retry::Hidden,
        None if turn.status == TurnStatus::InProgress => Retry::Waiting(item.retry_message()?),
        _ => Retry::Over(canter_protocol::model_retry_summary(
            &error.message,
            *attempt,
        )),
    })
}

/// The row for [`retry`]: the summary dim.
pub(crate) fn retry_rows(retry: &Retry, width: u16) -> Vec<Row> {
    let (text, style) = match retry {
        Retry::Hidden => return Vec::new(),
        Retry::Waiting(text) => (text, Style::default()),
        Retry::Over(text) => (text, theme::secondary()),
    };
    wrap_rows(
        &LogicalLine::new(
            vec![Span::raw("  ")],
            vec![Span::styled(sanitize(text), style)],
        ),
        width,
    )
}

/// The model's text as markdown, indented two columns, for a screen `width`
/// columns wide.
pub(crate) fn agent_message(text: &str, width: u16) -> Vec<LogicalLine> {
    markdown::render(&sanitize(text), width.saturating_sub(2))
        .into_iter()
        .map(|line| line.prefixed(Span::raw("  "), Span::raw("  ")))
        .collect()
}

/// What the model said: [`agent_message`] with a bold `● ` in the default
/// colour in place of the first row's indent, apart from the cards' `•`.
fn said(text: &str, width: u16) -> Vec<LogicalLine> {
    let mut lines = agent_message(text, width);
    if let Some(first) = lines.first_mut() {
        first.indent[0] = Span::styled("● ", Style::new().bold());
    }
    lines
}

/// Rows for an item; `finished` picks its final form over the in-progress one.
/// With `details` (Ctrl+O), nothing is folded: whole thinking, all output, all
/// of a diff. With `sandbox` (the sandbox is on), cards for what runs outside
/// it say so.
pub(crate) fn item(
    item: &Item,
    finished: bool,
    details: bool,
    cwd: &str,
    width: u16,
    feedback: Option<&str>,
    sandbox: bool,
) -> Vec<Row> {
    let mut rows = match item {
        Item::HookContext { .. } => wrap_rows(
            &item_line(&item.hook_context_message().unwrap(), theme::secondary()),
            width,
        ),
        Item::ModelRetry { .. } => {
            wrap_all(&agent_message(&item.retry_message().unwrap(), width), width)
        }
        // A sub-agent's result the model was given without asking.
        Item::BackgroundEvent {
            agent: Some(agent), ..
        } => wrap_rows(
            &item_line(
                &format!(
                    "Sub-agent {} {} — result sent to the model",
                    visible(&agent.name),
                    crate::agents::status_label(
                        agent.status.unwrap_or(AgentStatus::Unknown),
                        false
                    )
                ),
                theme::secondary(),
            ),
            width,
        ),
        Item::CollabAgentToolCall { .. } => agent_tool(item, details, width),
        Item::BackgroundEvent {
            command_id, text, ..
        } => {
            let label =
                command_id.map_or_else(|| "Auto wake".into(), |id| format!("Background #{id}"));
            let mut rows = vec![title(
                (dot(theme::accent()), theme::accent().bold()),
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
            (details, false),
            width,
        ),
        Item::PlanProposal {
            plan,
            answer,
            status,
            ..
        } => {
            // Bold, but not in the colour of bold text in an answer.
            let mut rows = wrap_rows(
                &LogicalLine::new(
                    vec![Span::raw("  ")],
                    vec![Span::styled(
                        crate::plan_mode::result_label(answer.as_ref(), *status),
                        Style::new().bold(),
                    )],
                ),
                width,
            );
            let mut body = wrap_all(&agent_message(plan, width), width);
            if !details && body.len() > 8 {
                let count = body.len() - 8;
                body.truncate(8);
                // A copy keeps the note, without the spaces around it.
                let chip = more_lines(count);
                let start = 3;
                let end = 2 + str_width(&chip.content) - 1;
                body.push(Row {
                    text: Some(start..end),
                    ..Row::marks(Line::from(vec![Span::raw("  "), chip]))
                });
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
            wrap_all(&said(&text, width), width)
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
        Item::McpToolSearch {
            query,
            tools,
            status,
            ..
        } => {
            let (mark, mut right) = status_marks(*status, theme::explored());
            if *status == ItemStatus::Completed {
                right = vec![Span::styled(
                    format!("{} found", tools.len()),
                    theme::secondary(),
                )];
            }
            let mut rows = vec![title(
                mark,
                if *status == ItemStatus::InProgress {
                    "Searching MCP tools "
                } else {
                    "Searched MCP tools "
                },
                vec![Span::raw(format!("\"{}\"", first_line(query)))],
                right,
                None,
                width,
            )];
            rows.extend(output_rows(
                &tools.join("\n"),
                Fold::Details,
                theme::secondary(),
                bar(false),
                width,
            ));
            rows
        }

        Item::AstGrep {
            pattern,
            matches,
            status,
            error,
            ..
        } => ast_grep(pattern, matches, *status, error.as_deref(), details, width),
        Item::WebSearch {
            query,
            results,
            status,
            error,
            ..
        } => web_search(query, results, *status, error.as_deref(), details, width),
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
            (details, sandbox),
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
                (details, sandbox),
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
            (details, sandbox),
            width,
        ),
        Item::FileChange {
            changes, status, ..
        } => changes
            .iter()
            .flat_map(|change| file_change(change, *status, details, cwd, width))
            .collect(),
        // Reads in a row are drawn together: see [`explored`].
        Item::FileRead { .. } => vec![file_read(item, cwd, width)],
        Item::ContextCompaction {
            summary,
            restored_files,
            before_tokens,
            after_tokens,
            part,
            ..
        } => {
            let mut rows = vec![Row::text_from(
                Line::from(Span::styled(
                    compaction_line(summary, finished, *before_tokens, *after_tokens, *part),
                    theme::secondary(),
                )),
                2,
            )];
            for file in restored_files {
                let path = display_path(&file.path, cwd);
                let text = match file.lines {
                    Some(lines) => format!("  Read {path} ({lines} lines)"),
                    None => format!("  Referenced file {path}"),
                };
                rows.push(Row::text_from(
                    Line::from(truncate(
                        vec![Span::styled(text, theme::secondary())],
                        usize::from(width),
                    )),
                    2,
                ));
            }
            rows
        }
        Item::UserInput {
            question,
            answer,
            status,
            ..
        } => {
            // Still open: the question alone; the box below asks it.
            let answer = match answer {
                Some(canter_protocol::UserInputAnswer::Option { label }) => Some(label.clone()),
                Some(canter_protocol::UserInputAnswer::Text { text }) => {
                    Some(format!("Own answer: {text}"))
                }
                _ if *status == ItemStatus::InProgress => None,
                _ => Some("No answer".into()),
            };
            let text = match answer {
                Some(answer) => format!("{} → {}", visible(question), visible(&answer)),
                None => visible(question),
            };
            wrap_rows(&notice("? ", &text, theme::secondary()), width)
        }
        Item::Plan {
            plan,
            explanation,
            status,
            ..
        } => crate::plan::card(plan, explanation.as_deref(), *status, width),
        Item::Diagnostics { .. } | Item::Unknown => Vec::new(),
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
            } | Item::WebSearch {
                status: ItemStatus::Declined,
                ..
            } | Item::AstGrep {
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

/// What a turn leaves behind once it has ended: why it did not end
/// normally, if so; then, when its time is known,
/// `Worked for 3m 05s · ↑ 1.27M (98% cached) · ↓ 26.2k · 11:13`: the input
/// tokens of all its requests and how many of them were cached, its output
/// tokens (when its usage is known), and when it ended, in local time (if
/// known).
fn turn_footer(turn: &Turn, error_shown: bool) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    match turn.status {
        TurnStatus::Completed => {}
        TurnStatus::Interrupted => {
            lines.push(notice("■ ", "Turn interrupted", theme::secondary()));
        }
        TurnStatus::Failed if !error_shown => {
            let message = turn
                .error
                .as_ref()
                .map_or("no reason given", |error| error.message.as_str());
            lines.push(notice(
                "✗ ",
                &format!("Turn failed: {message}"),
                theme::failure(),
            ));
        }
        TurnStatus::Failed => {}
        _ => return lines,
    }
    let Some(ms) = turn.duration_ms else {
        return lines;
    };
    let mut text = format!("Worked for {}", took(ms));
    if let Some(usage) = &turn.usage {
        let total = &usage.total;
        text += &format!(" · ↑ {}", crate::app::token_count(total.input_tokens));
        if total.input_tokens > 0 {
            let cached = (u128::from(total.cached_input_tokens) * 100
                + u128::from(total.input_tokens) / 2)
                / u128::from(total.input_tokens);
            text += &format!(" ({cached}% cached)");
        }
        text += &format!(" · ↓ {}", crate::app::token_count(total.output_tokens));
    }
    if let Some(ended) = turn
        .completed_at
        .and_then(|secs| chrono::Local.timestamp_opt(secs, 0).single())
    {
        text += &ended.format(" · %H:%M").to_string();
    }
    lines.push(notice("", &text, theme::secondary()));
    lines
}

/// Files a turn's end lists at most; the rest are counted.
const CHANGED_FILES: usize = 8;

/// The rows a turn leaves once it has ended: what files canter's tools
/// changed in it ([`changed_files`]), then [`turn_footer`].
pub(crate) fn turn_end(turn: &Turn, error_shown: bool, cwd: &str, width: u16) -> Vec<Row> {
    let mut rows = changed_files(turn, cwd, width);
    for footer in turn_footer(turn, error_shown) {
        rows.extend(wrap_rows(&footer, width));
    }
    rows
}

/// `• Changed 3 files  +40 −12` and a row for each file the turn's
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
        // Only `+0` counts nothing here: `−0` is left out.
        let style = if added == "+0" {
            theme::secondary()
        } else {
            theme::success()
        };
        spans.push(Span::styled(added, style));
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
    let mut rows = vec![cut(header, 2)];
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
        .saturating_sub(4 + after_names)
        .max(PATH_MIN)
        .min(name_width);
    for (file, counts) in shown.iter().zip(file_counts) {
        let names: Vec<&str> = file.names.iter().map(String::as_str).collect();
        let arrows = str_width(ARROW) * (names.len() - 1);
        let name = fit_paths(&names, name_width.saturating_sub(arrows)).join(ARROW);
        let pad = name_width.saturating_sub(str_width(&name)) + 2;
        let after = all_counts_width - counts_width(&counts) + 2;
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(name, theme::secondary()),
            Span::raw(" ".repeat(pad)),
        ];
        spans.extend(count_spans(counts, added_width));
        if let Some(word) = word(file) {
            spans.push(Span::raw(" ".repeat(after)));
            spans.push(Span::styled(word, theme::secondary()));
        }
        rows.push(cut(spans, 4));
    }
    if count > CHANGED_FILES {
        rows.push(cut(
            vec![Span::styled(
                format!("    … and {} more", count - CHANGED_FILES),
                theme::secondary(),
            )],
            4,
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
fn compaction_line(
    summary: &str,
    finished: bool,
    before_tokens: Option<u64>,
    after_tokens: Option<u64>,
    part: Option<u32>,
) -> String {
    if !finished {
        format!("  ── {} ──", compacting(before_tokens, part))
    } else if summary.is_empty() || summary == COMPACTION_KEPT {
        "  ── Compaction did not finish; the original context is kept ──".into()
    } else if let (Some(before), Some(after)) = (before_tokens, after_tokens) {
        format!(
            "  ── Context compacted · {} → {} tokens ──",
            crate::app::tokens(before),
            crate::app::tokens(after)
        )
    } else {
        "  ── Context compacted ──".into()
    }
}

pub(crate) fn compacting(before_tokens: Option<u64>, part: Option<u32>) -> String {
    let mut text = "Compacting context…".to_owned();
    if let Some(before) = before_tokens {
        text += &format!(" {} tokens", crate::app::tokens(before));
    }
    if let Some(part) = part.filter(|part| *part > 1) {
        text += &format!(" · part {part}");
    }
    text
}

/// An item of one line of `text`, `• ` and all in `style`.
fn item_line(text: &str, style: Style) -> LogicalLine {
    LogicalLine::new(
        vec![Span::styled("• ", style)],
        vec![Span::styled(sanitize(text), style)],
    )
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

/// Thinking, from the summary when there is one. Folded, while it runs it is
/// `✻ Thinking… <the latest line>`; once done with a summary title (GPT's
/// `**title**`), `✻ Thought · <title>`, `✻ Thought for 4s · <title>` when
/// its time is known; the label grey while it runs and purple once done
/// ([`theme::thought`]), what follows it fainter and italic. A raw thought
/// done (DeepSeek's) has no title: the label alone on its row, then its text
/// indented 2, grey and upright, at most three rows, then
/// ` ⋯ N more lines · Ctrl+O to expand `. With `details`, the
/// label (`✻ Thought for 4s`) and then the whole text as markdown, grey and
/// italic, each row after a grey `┊`.
fn reasoning(
    summary: &[String],
    content: &[String],
    duration_ms: Option<u64>,
    finished: bool,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let label_style = if finished {
        theme::thought()
    } else {
        theme::secondary()
    };
    let text_style = theme::thinking_text();
    let summarized = summary.iter().any(|part| !part.trim().is_empty());
    let parts = if summarized { summary } else { content };
    let text = sanitize(&parts.join("\n\n"));
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let label = if !finished {
        "✻ Thinking…".to_owned()
    } else {
        match duration_ms {
            Some(ms) => format!("✻ Thought for {}", took(ms)),
            None => "✻ Thought".to_owned(),
        }
    };
    // The label, then `text` after `gap`, cut to the width.
    let row = |gap: &str, text: String| {
        let mut spans = vec![Span::styled(label.clone(), label_style)];
        if !text.is_empty() {
            spans.push(Span::styled(gap.to_owned(), label_style));
            spans.push(Span::styled(text, text_style));
        }
        vec![Row::text_from(
            Line::from(truncate(spans, usize::from(width))),
            2,
        )]
    };
    if finished && text.trim().is_empty() {
        return row(" · ", "no summary".into());
    }
    // The whole text as markdown in `style`, its first row after `first`
    // and the others after `rest`.
    let markdown = |style: Style, first: Span<'static>, rest: Span<'static>| {
        let restyle = |spans: Vec<Span<'static>>| {
            spans
                .into_iter()
                .map(|span| span.patch_style(style))
                .collect()
        };
        let room = width.saturating_sub(str_width(&rest.content) as u16);
        let mut rows = Vec::new();
        for (at, line) in markdown::render(&text, room).into_iter().enumerate() {
            let line = LogicalLine {
                indent: restyle(line.indent),
                hang: restyle(line.hang),
                body: restyle(line.body),
                marker: line.marker,
                // Thinking is not the answer: its addresses are not links.
                links: Vec::new(),
            };
            let lead = if at == 0 { first.clone() } else { rest.clone() };
            rows.extend(wrap_rows(&line.prefixed(lead, rest.clone()), width));
        }
        rows
    };
    if details {
        let bar = || Span::styled("  ┊ ", theme::secondary());
        let mut rows = row("", String::new());
        rows.extend(markdown(theme::thinking(), bar(), bar()));
        return rows;
    }
    // The folded row is plain text: bold markers (GPT summary titles) would
    // show as literal `**`.
    if !finished {
        let Some(latest) = lines.next_back() else {
            return row("", String::new());
        };
        let latest = latest.replace("**", "");
        let room = usize::from(width).saturating_sub(str_width(&label) + 1);
        let shown = if str_width(&latest) <= room {
            latest
        } else {
            format!("…{}", tail(&latest, room.saturating_sub(1)).trim_start())
        };
        return row(" ", shown);
    }
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    // A summary starts with its title in bold: the title alone is the row.
    // GPT summaries arrive as content, so this does not depend on `summary`.
    if let Some((title, rest)) = first
        .strip_prefix("**")
        .and_then(|after| after.split_once("**"))
        .filter(|(title, _)| !title.trim().is_empty())
    {
        let more = if !rest.trim().is_empty() || lines.next().is_some() {
            "…"
        } else {
            ""
        };
        return row(" · ", format!("{}{more}", title.trim().replace("**", "")));
    }
    // A raw thought (DeepSeek's) has no title to stand for it: the label
    // alone on its row, then the thought itself, grey and upright, at most
    // FOLDED_THOUGHT_ROWS rows with blank ones left out, then how many more
    // Ctrl+O shows. Grey all through (11-13): inline code and bold take no
    // colour or weight of their own until Ctrl+O.
    let mut text: Vec<Row> = markdown(theme::secondary(), Span::raw("  "), Span::raw("  "))
        .into_iter()
        .filter(|row| {
            row.line
                .spans
                .iter()
                .any(|span| !span.content.trim().is_empty())
        })
        .map(|mut row| {
            for span in &mut row.line.spans {
                span.style = theme::secondary();
            }
            row
        })
        .collect();
    if text.len() > FOLDED_THOUGHT_ROWS {
        let hidden = text.len() - FOLDED_THOUGHT_ROWS;
        text.truncate(FOLDED_THOUGHT_ROWS);
        text.push(Row::marks(Line::from(vec![
            Span::raw("  "),
            more_lines(hidden),
        ])));
    }
    let mut rows = row("", String::new());
    rows.extend(text);
    rows
}

/// The rows a folded raw thought shows before ` ⋯ N more lines`.
const FOLDED_THOUGHT_ROWS: usize = 3;

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

/// An image's format and size: `PNG · 12.0 KB`.
fn image_detail(mime_type: &str, size_bytes: u64) -> String {
    let format = mime_type
        .strip_prefix("image/")
        .unwrap_or(mime_type)
        .to_uppercase();
    format!(
        "{} · {}",
        visible(&format),
        format_size(size_bytes as usize)
    )
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
            let mut spans = vec![
                dot(theme::secondary()),
                Span::styled(verb.to_owned(), Style::new().bold()),
                Span::raw(visible(name)),
                Span::styled(
                    format!("  {}", image_detail(mime_type, *size_bytes)),
                    theme::secondary(),
                ),
            ];
            fit_path_spans(&mut spans, &[2], usize::from(width));
            Some(cut_row(spans, Vec::new(), width))
        })
        .collect()
}

/// `path` as shown in a card ([`display_path`]): in the colour of a path a
/// click opens when it is one.
fn path_span(path: &str, cwd: &str) -> Span<'static> {
    let shown = display_path(path, cwd);
    if links::path(path, cwd).is_some() {
        Span::styled(shown, theme::path_link())
    } else {
        Span::raw(shown)
    }
}

/// What a read row says after its path: how it went, after a grey ` · `;
/// or the lines read (`lines 1–37`) or an image's format and size, after
/// two spaces.
fn read_detail(item: &Item) -> Vec<Span<'static>> {
    let Item::FileRead {
        start_line,
        content,
        status,
        content_items,
        ..
    } = item
    else {
        return Vec::new();
    };
    let dim = theme::secondary();
    let image = content_items.iter().find_map(|item| match item {
        ToolContentItem::LocalImage {
            mime_type,
            size_bytes,
            ..
        } => Some(image_detail(mime_type, *size_bytes)),
        _ => None,
    });
    let lines = content.lines().count() as u32;
    vec![match status {
        ItemStatus::InProgress => return Vec::new(),
        ItemStatus::Failed => return vec![status_separator(), failure_status("failed")],
        ItemStatus::Declined => return vec![status_separator(), Span::styled("declined", dim)],
        _ if image.is_some() => Span::styled(format!("  {}", image.unwrap_or_default()), dim),
        _ if lines == 0 => Span::styled("  empty", dim),
        _ if lines == 1 => Span::styled(format!("  line {start_line}"), dim),
        _ => Span::styled(
            format!("  lines {start_line}–{}", start_line + lines - 1),
            dim,
        ),
    }]
}

/// `Read <path>  lines 1–37` after `lead`, the path a link, fitted to
/// `width`; a copy takes it from `Read`.
fn read_row(lead: Vec<Span<'static>>, verb: Style, item: &Item, cwd: &str, width: u16) -> Row {
    let Item::FileRead { path, .. } = item else {
        return Row::blank();
    };
    let at = lead.len() + 1;
    let mut spans = lead;
    spans.push(Span::styled("Read ", verb));
    spans.push(path_span(path, cwd));
    spans.extend(read_detail(item));
    fit_path_spans(&mut spans, &[at], usize::from(width));
    let links: Vec<_> = links::path(path, cwd)
        .map(|target| span_link(&spans, at, target))
        .into_iter()
        .collect();
    let start = drawn_width(&spans[..at - 1]);
    Row {
        text: Some(start..usize::MAX),
        ..cut_row(spans, links, width)
    }
}

/// A read on a row of its own, as one that failed or was declined is:
/// `• Read <path>` and how it went.
fn file_read(item: &Item, cwd: &str, width: u16) -> Row {
    let (mark, verb) = match item {
        Item::FileRead {
            status: ItemStatus::Failed,
            ..
        } => failed(),
        _ => plain(theme::secondary()),
    };
    read_row(vec![mark], verb, item, cwd, width)
}

/// Whether a read goes in a `• Explored` block with the reads next to it:
/// one still going or done, not one that failed or was declined.
fn explores(item: &Item) -> bool {
    matches!(
        item,
        Item::FileRead {
            status: ItemStatus::InProgress | ItemStatus::Completed,
            ..
        }
    )
}

/// Where the reads in a row with the read at `index` start and end in
/// `items` ([`explores`]); `None` when that item is not such a read.
pub(crate) fn reads_around(items: &[Item], index: usize) -> Option<Range<usize>> {
    if !explores(items.get(index)?) {
        return None;
    }
    let start = (0..index)
        .rev()
        .take_while(|&at| explores(&items[at]))
        .last()
        .unwrap_or(index);
    let end = (index + 1..items.len())
        .take_while(|&at| explores(&items[at]))
        .last()
        .map_or(index + 1, |at| at + 1);
    Some(start..end)
}

/// Reads in a row ([`reads_around`]) as one block, drawn by the first of
/// them; the others draw nothing. `• Explored`, then `└ Read` and the names
/// of the files read so far, wrapped under the first; a name says as much
/// of its path as tells it apart from the others ([`short_names`]). With
/// `details` a row for each read, with its path and lines. Nothing until a
/// file has been read. `None` when the item at `index` is not such a read.
pub(crate) fn explored(
    items: &[Item],
    index: usize,
    details: bool,
    cwd: &str,
    width: u16,
) -> Option<Vec<Row>> {
    let reads = reads_around(items, index)?;
    if reads.start != index {
        return Some(Vec::new());
    }
    let done: Vec<&Item> = items[reads]
        .iter()
        .filter(|item| {
            matches!(
                item,
                Item::FileRead {
                    status: ItemStatus::Completed,
                    ..
                }
            )
        })
        .collect();
    if done.is_empty() {
        return Some(Vec::new());
    }
    let dim = theme::secondary();
    let (mark, verb) = self::done(theme::explored());
    let mut rows = vec![Row::text_from(
        Line::from(vec![mark, Span::styled("Explored", verb)]),
        2,
    )];
    if details {
        for (at, item) in done.iter().enumerate() {
            let lead = if at == 0 {
                vec![Span::raw("  "), Span::styled("└ ", theme::guide())]
            } else {
                vec![Span::raw("    ")]
            };
            rows.push(read_row(lead, theme::read(), item, cwd, width));
        }
        return Some(rows);
    }
    let mut paths: Vec<&str> = Vec::new();
    for item in &done {
        if let Item::FileRead { path, .. } = item
            && !paths.contains(&path.as_str())
        {
            paths.push(path);
        }
    }
    let shown: Vec<String> = paths.iter().map(|path| display_path(path, cwd)).collect();
    let mut body = Vec::new();
    let mut found = Vec::new();
    for (at, (path, name)) in paths.iter().zip(short_names(&shown)).enumerate() {
        if at > 0 {
            push_linked(&mut body, &mut found, Span::styled(", ", dim), None);
        }
        push_linked(
            &mut body,
            &mut found,
            Span::raw(name),
            links::path(path, cwd),
        );
    }
    let line = LogicalLine {
        indent: vec![
            Span::raw("  "),
            Span::styled("└ ", theme::guide()),
            Span::styled("Read ", theme::read()),
        ],
        hang: vec![Span::raw(" ".repeat(9))],
        body,
        // A copy keeps `Read `.
        marker: 5,
        links: found,
    };
    rows.extend(wrap_rows(&line, width));
    Some(rows)
}

/// The file names of `paths`, each with as many of its directories as
/// tell it apart from the others: `src/index.ts` and `tests/index.ts`.
fn short_names(paths: &[String]) -> Vec<String> {
    let parts: Vec<Vec<&str>> = paths
        .iter()
        .map(|path| path.split('/').filter(|part| !part.is_empty()).collect())
        .collect();
    let mut taken = vec![1; paths.len()];
    let name = |at: usize, taken: usize| {
        let parts = &parts[at];
        if taken >= parts.len() {
            return paths[at].clone();
        }
        parts[parts.len() - taken..].join("/")
    };
    loop {
        let names: Vec<String> = (0..paths.len()).map(|at| name(at, taken[at])).collect();
        let mut longer = false;
        for at in 0..paths.len() {
            let alike = names.iter().filter(|other| **other == names[at]).count() > 1;
            if alike && taken[at] < parts[at].len() {
                taken[at] += 1;
                longer = true;
            }
        }
        if !longer {
            return names;
        }
    }
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
        ..Row::text_from(line, 2)
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

/// An item waiting for an approval: only its title row; the approval box
/// below shows the whole command or diff, and the status line that it waits.
pub(crate) fn waiting(
    item: &Item,
    details: bool,
    cwd: &str,
    width: u16,
    sandbox: bool,
) -> Vec<Row> {
    match item {
        Item::CommandExecution { command, .. } => command_title(
            command,
            false,
            (plain(theme::secondary()), "Run "),
            Vec::new(),
            false,
            width,
        ),
        Item::FileChange { changes, .. } => changes
            .iter()
            .map(|change| {
                let diff = Diff::parse(&sanitize(&change.diff), &change.kind, &change.path);
                let (header, links) = change_header(
                    change,
                    &diff,
                    plain(theme::secondary()),
                    Vec::new(),
                    cwd,
                    usize::from(width),
                );
                cut_row(header, links, width)
            })
            .collect(),
        _ => self::item(item, false, details, cwd, width, None, sandbox),
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
    let (mark, status) = match status {
        ItemStatus::Blocked => (
            plain(theme::warning()),
            Some(Span::styled("blocked (Plan mode)", theme::warning())),
        ),
        ItemStatus::InProgress => (plain(theme::secondary()), None),
        ItemStatus::Failed => (failed(), Some(failure_status("failed"))),
        ItemStatus::Declined => (
            plain(theme::secondary()),
            Some(Span::styled("declined", theme::secondary())),
        ),
        _ => (done(theme::edited()), None),
    };
    let suffix = status.map_or_else(Vec::new, |status| vec![status_separator(), status]);
    let (header, links) = change_header(change, &diff, mark, suffix, cwd, usize::from(width));
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

/// `✓ Edit path  +a −r` and `suffix` (` · ` and a status): a file change's
/// title, its paths shortened to fit in `width` columns ([`fit_path_spans`]),
/// and the link on its path as drawn: the file as it is after the change, so
/// none for a deleted file, and the new path of a moved one.
fn change_header(
    change: &FileUpdateChange,
    diff: &Diff,
    (bullet, verb_style): Mark,
    suffix: Vec<Span<'static>>,
    cwd: &str,
    width: usize,
) -> (Vec<Span<'static>>, Vec<(Range<usize>, Target)>) {
    let (added, removed) = (diff.added(), diff.removed());
    let verb = match &change.kind {
        PatchChangeKind::Add => "Add ",
        PatchChangeKind::Delete => "Delete ",
        _ => "Edit ",
    };
    // Only the path a click opens is in its colour.
    let linked = !matches!(
        &change.kind,
        PatchChangeKind::Delete | PatchChangeKind::Update { move_path: Some(_) }
    );
    let mut header = vec![
        bullet,
        Span::styled(verb, verb_style),
        if linked {
            path_span(&change.path, cwd)
        } else {
            Span::raw(display_path(&change.path, cwd))
        },
    ];
    let mut paths = vec![2];
    let mut link = linked.then_some((2, change.path.as_str()));
    if let PatchChangeKind::Update {
        move_path: Some(to),
    } = &change.kind
    {
        header.push(Span::raw(ARROW));
        header.push(path_span(to, cwd));
        paths.push(header.len() - 1);
        link = Some((header.len() - 1, to.as_str()));
    }
    header.push(Span::raw("  "));
    match &change.kind {
        PatchChangeKind::Add => header.push(count(format!("+{added}"), added, theme::success())),
        PatchChangeKind::Delete => {
            header.push(count(format!("−{removed}"), removed, theme::failure()))
        }
        _ => header.extend([
            count(format!("+{added}"), added, theme::success()),
            Span::raw(" "),
            count(format!("−{removed}"), removed, theme::failure()),
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
    (details, sandbox): (bool, bool),
    width: u16,
) -> Vec<Row> {
    let (command, source) = command;
    let dim = theme::secondary();
    let searched_nothing = no_matches(command, exit_code, output);
    let (mark, mut status_spans) = match (status, exit_code) {
        (ItemStatus::InProgress, _) => (plain(dim), Vec::new()),
        (ItemStatus::Declined, _) => (plain(dim), vec![Span::styled("declined", dim)]),
        (ItemStatus::Blocked, _) => (
            plain(theme::warning()),
            vec![Span::styled("blocked (Plan mode)", theme::warning())],
        ),
        _ if searched_nothing => (plain(dim), vec![Span::styled("No matches", dim)]),
        (_, Some(0)) => (done(theme::success()), Vec::new()),
        (_, Some(code)) => (failed(), vec![failure_status(format!("exit {code}"))]),
        (ItemStatus::Failed, None) => (failed(), vec![failure_status("failed")]),
        (_, None) => (done(theme::success()), Vec::new()),
    };
    // How long it took, from a second on.
    if let Some(ms) = duration_ms.filter(|&ms| ms >= 1000 && status != ItemStatus::InProgress) {
        if !status_spans.is_empty() {
            status_spans.push(Span::styled(" · ", dim));
        }
        status_spans.push(Span::styled(format_duration(ms), dim));
    }
    // The same cases as the red `•` above.
    let failed = !searched_nothing
        && !matches!(status, ItemStatus::InProgress | ItemStatus::Declined)
        && exit_code.map_or(status == ItemStatus::Failed, |code| code != 0);
    let user = source == canter_protocol::ExecCommandSource::UserShell;
    if user && sandbox {
        not_sandboxed(&mut status_spans);
    }
    let verb = match status {
        ItemStatus::InProgress => "Running ",
        ItemStatus::Declined | ItemStatus::Blocked => "Run ",
        _ => "Ran ",
    };
    let mut rows = command_title(command, user, (mark, verb), status_spans, details, width);
    let grey = theme::output();
    let (fold, style) = if details {
        (Fold::Details, if failed { Style::new() } else { grey })
    } else if status == ItemStatus::InProgress {
        (Fold::Tail(LIVE_TAIL), grey)
    } else if failed {
        (Fold::Tail(FAILED_TAIL), Style::new())
    } else {
        (Fold::Head(FINISHED_LINES), grey)
    };
    rows.extend(lines_rows(output, fold, style, Gutter::Branch, width));
    rows
}

/// Grey `not sandboxed` after a card's status: it ran outside the sandbox that is on.
fn not_sandboxed(right: &mut Vec<Span<'static>>) {
    let dim = theme::secondary();
    if !right.is_empty() {
        right.push(Span::styled(" · ", dim));
    }
    right.push(Span::styled("not sandboxed", dim));
}

/// The bar before a card's output: red when the card failed, else a
/// grey fainter than the output ([`theme::guide`]).
fn bar(failed: bool) -> Style {
    if failed {
        theme::failure()
    } else {
        theme::guide()
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
    (details, sandbox): (bool, bool),
    width: u16,
) -> Vec<Row> {
    let (mark, mut right) = status_marks(status, theme::success());
    if sandbox {
        not_sandboxed(&mut right);
    }
    let mut object = vec![Span::raw(first_line(name))];
    let empty = arguments.is_null() || arguments.as_object().is_some_and(|args| args.is_empty());
    if !empty {
        object.push(Span::raw(format!(
            "  {}",
            first_line(&arguments.to_string())
        )));
    }
    let mut rows = vec![title(mark, "Call ", object, right, None, width)];
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

/// The four sub-agent tools (P2-179): `● Spawned explorer · <name>` and
/// the task, `● Waiting for <name>, <name> · 12s` and how each came back
/// with its final reply folded, `● Sent to <name>` and the message,
/// `● Closed <name>`. Names are bold. A failed call is a failed card with
/// its error.
fn agent_tool(item: &Item, details: bool, width: u16) -> Vec<Row> {
    let Item::CollabAgentToolCall {
        tool,
        status,
        prompt,
        agents,
        error,
        started_at,
        duration_ms,
        ..
    } = item
    else {
        return Vec::new();
    };
    let dim = theme::secondary();
    let bold = Style::new().bold();
    let names = || {
        let mut spans = Vec::new();
        for (index, agent) in agents.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(", "));
            }
            spans.push(Span::styled(visible(&agent.name), bold));
        }
        spans
    };
    let (mark, right) = match status {
        ItemStatus::InProgress => (plain(dim), Vec::new()),
        ItemStatus::Interrupted => (plain(dim), vec![Span::styled("interrupted", dim)]),
        other => status_marks(*other, theme::success()),
    };
    // While it runs, once it worked, once it did not.
    let tense = |running, done, not| match status {
        ItemStatus::InProgress => running,
        ItemStatus::Completed => done,
        _ => not,
    };
    let (verb, object) = match tool {
        CollabAgentTool::SpawnAgent => {
            let agent = agents.first();
            let kind = agent
                .map_or("agent", |agent| {
                    crate::agents::type_label(&agent.agent_type)
                })
                .to_owned();
            let mut object = vec![Span::raw(kind)];
            if let Some(agent) = agent.filter(|agent| !agent.name.is_empty()) {
                object.push(Span::raw(" · "));
                object.push(Span::styled(visible(&agent.name), bold));
            }
            (tense("Spawning ", "Spawned ", "Spawn "), object)
        }
        CollabAgentTool::Wait => {
            let mut object = names();
            let time = match (duration_ms, started_at) {
                (Some(ms), _) => Some(took(*ms)),
                (None, Some(start)) if *status == ItemStatus::InProgress => {
                    Some(format_elapsed(crate::now_ms() - start))
                }
                _ => None,
            };
            if let Some(time) = time {
                object.push(Span::raw(format!(" · {time}")));
            }
            ("Waiting for ", object)
        }
        CollabAgentTool::SendInput => (tense("Sending to ", "Sent to ", "Send to "), names()),
        CollabAgentTool::CloseAgent | CollabAgentTool::Unknown => {
            (tense("Closing ", "Closed ", "Close "), names())
        }
    };
    let mut rows = vec![title(mark, verb, object, right, None, width)];
    let branch = |spans: Vec<Span<'static>>| {
        let mut line = vec![Span::raw("    "), Span::styled("└ ", theme::guide())];
        line.extend(spans);
        Row::text_from(Line::from(truncate(line, usize::from(width))), 6)
    };
    // With details, the whole task, wrapped; otherwise its first line.
    if let Some(prompt) = prompt.as_deref().filter(|_| details) {
        for (index, line) in visible(prompt).lines().enumerate() {
            let indent = if index == 0 {
                vec![Span::raw("    "), Span::styled("└ ", theme::guide())]
            } else {
                vec![Span::raw("      ")]
            };
            rows.extend(wrap_rows(
                &LogicalLine::new(indent, vec![Span::styled(line.to_owned(), dim)]),
                width,
            ));
        }
    } else if let Some(prompt) = prompt {
        rows.push(branch(vec![Span::styled(first_line(prompt), dim)]));
    }
    let fold = || {
        if details {
            Fold::Details
        } else {
            Fold::Head(MCP_HEAD)
        }
    };
    if *tool == CollabAgentTool::Wait && *status == ItemStatus::Completed {
        for agent in agents {
            let state = agent.status.unwrap_or(AgentStatus::Unknown);
            let said = match state {
                AgentStatus::Running => "still running",
                other => crate::agents::status_label(other, false),
            };
            rows.push(branch(vec![
                crate::agents::mark(state),
                Span::styled(visible(&agent.name), bold),
                Span::raw(format!(" {said}")),
            ]));
            if let Some(result) = &agent.result {
                rows.extend(output_rows(result, fold(), dim, bar(false), width));
            }
        }
    }
    if let Some(error) = error.as_deref().filter(|_| *status == ItemStatus::Failed) {
        rows.extend(output_rows(error, fold(), Style::new(), bar(true), width));
    }
    rows
}

/// `● Fetch <url>` and the size fetched. The page is for the model, so it
/// shows only with details; a one-line note (a redirect to another host,
/// content left out) shows as it is, and a failure's reason in red.
fn web_fetch(
    url: &str,
    result: &str,
    status: ItemStatus,
    (details, sandbox): (bool, bool),
    width: u16,
) -> Vec<Row> {
    let dim = theme::secondary();
    let (mark, mut right) = status_marks(status, theme::explored());
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
    if sandbox {
        not_sandboxed(&mut right);
    }
    let mut rows = vec![title(
        mark,
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
        Fold::Head(FINISHED_LINES)
    };
    let failed = status == ItemStatus::Failed;
    let style = if failed { theme::failure() } else { dim };
    rows.extend(output_rows(result, fold, style, bar(failed), width));
    rows
}

fn web_search(
    query: &str,
    results: &[canter_protocol::WebSearchResult],
    status: ItemStatus,
    error: Option<&str>,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let (mark, mut right) = status_marks(status, theme::explored());
    if status == ItemStatus::Completed {
        right = vec![Span::styled(
            format!(
                "{} result{}",
                results.len(),
                if results.len() == 1 { "" } else { "s" }
            ),
            theme::secondary(),
        )];
    }
    let mut rows = vec![title(
        mark,
        "Search ",
        vec![Span::raw(first_line(query))],
        right,
        None,
        width,
    )];
    if let Some(error) = error {
        rows.extend(output_rows(
            error,
            Fold::Details,
            theme::failure(),
            bar(true),
            width,
        ));
    }
    if details {
        let text = results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                format!(
                    "{}. {}\n{}",
                    i + 1,
                    visible(&result.title),
                    visible(&result.url)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        rows.extend(output_rows(
            &text,
            Fold::Details,
            theme::secondary(),
            bar(false),
            width,
        ));
    }
    rows
}

/// `● AST search <pattern>` and the number of matches on this page; details
/// list each `path:line:column` and the matched code.
fn ast_grep(
    pattern: &str,
    matches: &[canter_protocol::AstGrepMatch],
    status: ItemStatus,
    error: Option<&str>,
    details: bool,
    width: u16,
) -> Vec<Row> {
    let (mark, mut right) = status_marks(status, theme::explored());
    if status == ItemStatus::Completed {
        right = vec![Span::styled(
            format!(
                "{} match{}",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" }
            ),
            theme::secondary(),
        )];
    }
    let mut rows = vec![title(
        mark,
        "AST search ",
        vec![Span::raw(first_line(pattern))],
        right,
        None,
        width,
    )];
    if let Some(error) = error {
        rows.extend(output_rows(
            error,
            Fold::Details,
            theme::failure(),
            bar(true),
            width,
        ));
    }
    if details && !matches.is_empty() {
        let mut text = String::new();
        for matched in matches {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "{}:{}:{}\n",
                matched.path, matched.line, matched.column
            ));
            for line in matched.text.lines() {
                text.push_str("  ");
                text.push_str(line);
                text.push('\n');
            }
        }
        rows.extend(output_rows(
            &text,
            Fold::Details,
            theme::secondary(),
            bar(false),
            width,
        ));
    }
    rows
}

/// The mark, verb style and right-hand status of a card that has no exit
/// code; once it worked, the mark and verb in `kind`, its kind's colour.
fn status_marks(status: ItemStatus, kind: Style) -> (Mark, Vec<Span<'static>>) {
    let dim = theme::secondary();
    match status {
        ItemStatus::Blocked => (
            plain(theme::warning()),
            vec![Span::styled("blocked (Plan mode)", theme::warning())],
        ),
        ItemStatus::InProgress => (plain(dim), vec![Span::styled("running", dim)]),
        ItemStatus::Declined => (plain(dim), vec![Span::styled("declined", dim)]),
        ItemStatus::Failed => (failed(), vec![failure_status("failed")]),
        _ => (done(kind), Vec::new()),
    }
}

/// Which output lines a card keeps.
enum Fold {
    /// The first lines, then how many more there are.
    Head(usize),
    /// How many there are before them, then the last lines.
    Tail(usize),
    /// All of them, up to `DETAILS_LINES`: past that the first and last half
    /// with one row between saying how many are left out.
    Details,
}

/// What output rows start with.
enum Gutter {
    /// `    ┃ ` in the style given, and ` ⋯ N more lines · Ctrl+O to expand `
    /// for what is folded ([`more_lines`]).
    Bar(Style),
    /// A command's: `  └ ` on the first row, four spaces on the others;
    /// ` ⋯ N more lines · Ctrl+O to expand ` after the first lines,
    /// ` ⋯ N lines above · Ctrl+O to expand ` before the last ones
    /// ([`more_lines`], [`lines_above`]). Lines that look like a diff's
    /// are coloured as one ([`diff_like`]).
    Branch,
}

/// Output rows, each after `    ┃ ` in `bar` and cut to `width`, in `style`.
fn output_rows(output: &str, fold: Fold, style: Style, bar: Style, width: u16) -> Vec<Row> {
    lines_rows(output, fold, style, Gutter::Bar(bar), width)
}

/// Output rows, each after `gutter` and cut to `width`, in `style`.
fn lines_rows(output: &str, fold: Fold, style: Style, gutter: Gutter, width: u16) -> Vec<Row> {
    /// A row of output: an output line, or a note about what is left out.
    enum Shown {
        Line(usize),
        Note(String),
        /// What is folded: [`more_lines`], [`lines_above`].
        Chip(Span<'static>),
    }
    let dim = theme::secondary();
    let output = sanitize(output);
    let mut lines: Vec<&str> = output.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let all = lines.len();
    let branch = matches!(gutter, Gutter::Branch);
    let diff = if branch {
        diff_like(&lines)
    } else {
        Vec::new()
    };
    let mut shown = Vec::new();
    match fold {
        Fold::Head(keep) => {
            shown.extend((0..all.min(keep)).map(Shown::Line));
            if all > keep {
                shown.push(Shown::Chip(more_lines(all - keep)));
            }
        }
        Fold::Tail(keep) => {
            if all > keep {
                shown.push(Shown::Chip(lines_above(all - keep)));
            }
            shown.extend((all.saturating_sub(keep)..all).map(Shown::Line));
        }
        Fold::Details if all > DETAILS_LINES => {
            let half = DETAILS_LINES / 2;
            shown.extend((0..half).map(Shown::Line));
            shown.push(Shown::Note(format!(
                "… {} lines not shown",
                all - DETAILS_LINES
            )));
            shown.extend((all - half..all).map(Shown::Line));
        }
        Fold::Details => shown.extend((0..all).map(Shown::Line)),
    }
    shown
        .into_iter()
        .enumerate()
        .map(|(at, shown)| {
            let lead = match gutter {
                Gutter::Bar(bar) => Span::styled(GUTTER, bar),
                Gutter::Branch if at == 0 => Span::styled(BRANCH, theme::guide()),
                Gutter::Branch => Span::raw(" ".repeat(str_width(BRANCH))),
            };
            let from = str_width(&lead.content);
            match shown {
                Shown::Line(source) => {
                    let spans = truncate(
                        vec![
                            lead,
                            Span::styled(
                                lines[source].to_owned(),
                                diff.get(source).copied().flatten().unwrap_or(style),
                            ),
                        ],
                        usize::from(width),
                    );
                    Row {
                        source: Some(source),
                        ..Row::text_from(Line::from(spans), from)
                    }
                }
                Shown::Note(text) => Row::marks(Line::from(vec![lead, Span::styled(text, dim)])),
                Shown::Chip(chip) => Row::marks(Line::from(vec![lead, chip])),
            }
        })
        .collect()
}

/// What says a card is folded after the lines it shows (P2-247):
/// ` ⋯ 63 more lines · Ctrl+O to expand ` ([`fold_chip`]).
pub(crate) fn more_lines(hidden: usize) -> Span<'static> {
    fold_chip(&format!(
        "⋯ {hidden} more {} · Ctrl+O to expand",
        lines_word(hidden)
    ))
}

/// What says a card is folded before the lines it shows (P2-247):
/// ` ⋯ 2 lines above · Ctrl+O to expand ` ([`fold_chip`]).
fn lines_above(hidden: usize) -> Span<'static> {
    fold_chip(&format!(
        "⋯ {hidden} {} above · Ctrl+O to expand",
        lines_word(hidden)
    ))
}

fn lines_word(count: usize) -> &'static str {
    if count == 1 { "line" } else { "lines" }
}

/// `text` as a fold note: dim, a space either side, no background (11-13:
/// the one chip on screen is the hint to jump to the bottom).
fn fold_chip(text: &str) -> Span<'static> {
    Span::styled(format!(" {text} "), theme::secondary())
}

/// The colour of each of a command's output lines that looks like a line
/// of a diff, `None` for the others: once a line starting `@@ ` has come,
/// a line starting `+` or `-` (not `+++` or `---`); and anywhere, a line
/// starting, after any spaces, with a number, a space, `+` or `-` and a
/// space. Added lines green on light green, removed ones red on light red,
/// as a file change's diff.
fn diff_like(lines: &[&str]) -> Vec<Option<Style>> {
    let on = |style: Style, background: Option<Color>| match background {
        Some(color) => style.bg(color),
        None => style,
    };
    let added = on(theme::diff_added(), theme::diff_added_bg());
    let removed = on(theme::diff_removed(), theme::diff_removed_bg());
    let mut hunk = false;
    lines
        .iter()
        .map(|line| {
            if line.starts_with("@@ ") {
                hunk = true;
                return None;
            }
            if hunk {
                if line.starts_with('+') && !line.starts_with("+++") {
                    return Some(added);
                }
                if line.starts_with('-') && !line.starts_with("---") {
                    return Some(removed);
                }
            }
            let rest = line.trim_start_matches(' ');
            let after = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            if after.len() == rest.len() {
                return None;
            }
            if after.starts_with(" + ") {
                Some(added)
            } else if after.starts_with(" - ") {
                Some(removed)
            } else {
                None
            }
        })
        .collect()
}

/// `• Ran <command>` (`verb` after the mark), its status after it: on the
/// same row when the command takes one row and the status fits after it,
/// else on a row of its own under the command. The command is coloured as
/// Bash, as in the approval box, and each of its lines wraps by words
/// under where it starts. Unless `whole`, it takes at most `COMMAND_ROWS`
/// rows, then a row saying how many more there are. Control characters in
/// the command show as escapes. The programs it runs are blue and bold
/// ([`programs`]).
/// With `bang`, the user ran it: `! ` before it, the `!` red.
fn command_title(
    command: &str,
    bang: bool,
    ((bullet, verb_style), verb): (Mark, &'static str),
    status: Vec<Span<'static>>,
    whole: bool,
    width: u16,
) -> Vec<Row> {
    let clean = visible(command);
    let mut lines: Vec<&str> = clean.lines().collect();
    if lines.is_empty() {
        lines.push("");
    }
    let mut bodies: Vec<Vec<Span<'static>>> = lines
        .iter()
        .map(|line| vec![Span::raw((*line).to_owned())])
        .collect();
    programs::emphasize(&mut bodies, theme::program(), theme::operator());
    if bang {
        bodies[0].splice(0..0, [Span::styled("!", theme::shell()), Span::raw(" ")]);
    }
    // Where the command starts: after `• Ran `.
    let start = str_width(&bullet.content) + str_width(verb);
    let hang = || vec![Span::raw(" ".repeat(start))];
    let mut rows = Vec::new();
    for body in bodies {
        let line = LogicalLine {
            indent: hang(),
            hang: hang(),
            body,
            marker: 0,
            links: Vec::new(),
        };
        rows.extend(wrap_rows(&line, width));
    }
    // The mark and verb in place of the first row's indent.
    let mut first = vec![bullet, Span::styled(verb, verb_style)];
    first.extend(rows[0].line.spans.drain(1..));
    let first_width = spans_width(&first);
    let status_width = spans_width(&status);
    let same_row = rows.len() == 1
        && first_width + str_width(STATUS_SEPARATOR) + status_width <= usize::from(width);
    let (line, left) = if same_row || status.is_empty() {
        after_title(first, status.clone(), usize::from(width))
    } else {
        (Line::from(first), usize::MAX)
    };
    rows[0] = card_row((line, left), Vec::new());
    if !whole && rows.len() > COMMAND_ROWS {
        let hidden = rows.len() - COMMAND_ROWS;
        rows.truncate(COMMAND_ROWS);
        let mut spans = hang();
        spans.push(more_lines(hidden));
        rows.push(Row::marks(Line::from(spans)));
    }
    if !same_row && !status.is_empty() {
        let mut spans = hang();
        spans.extend(status);
        rows.push(Row::marks(Line::from(truncate(spans, usize::from(width)))));
    }
    rows
}

/// A card's first row: `● Verb object` with `right` flush with the right
/// edge; the object a link to `link` when there is one.
fn title(
    (bullet, verb_style): Mark,
    verb: &'static str,
    object: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    link: Option<Target>,
    width: u16,
) -> Row {
    let mut left = vec![bullet, Span::styled(verb, verb_style)];
    let start = drawn_width(&left);
    let links = link
        .map(|target| (start..start + drawn_width(&object), target))
        .into_iter()
        .collect();
    left.extend(object);
    card_row(after_title(left, right, usize::from(width)), links)
}

/// A card's title row: a copy takes it from the verb to the end of the
/// left side, leaving out the mark and the status on the right. `links`
/// are on the left side.
fn card_row((line, left): (Line<'static>, usize), links: Vec<(Range<usize>, Target)>) -> Row {
    let end = left.min(drawn_width(&line.spans));
    Row {
        text: Some(2..left),
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

/// A card's mark: `•` in `style`.
fn dot(style: Style) -> Span<'static> {
    Span::styled("• ", style)
}

/// A card's mark and the style of the verb after it.
type Mark = (Span<'static>, Style);

/// A finished card's mark and verb: a green `✓`, as on every card that
/// worked, and the verb bold in `style`, the colour of its kind.
fn done(style: Style) -> Mark {
    (Span::styled("✓ ", theme::success()), style.bold())
}

/// A `+N` or `−N` count in `style`, grey when it counts nothing.
fn count(text: String, n: usize, style: Style) -> Span<'static> {
    Span::styled(text, if n == 0 { theme::secondary() } else { style })
}

/// A mark in `style` and the verb plainly bold: a card still going,
/// declined or blocked.
fn plain(style: Style) -> Mark {
    (dot(style), Style::new().bold())
}

/// A failed card's mark and verb: a red `✗` and the verb red and bold.
fn failed() -> Mark {
    let style = theme::failure();
    (Span::styled("✗ ", style), style.bold())
}

/// A failed card's status, such as `exit 1` or `failed`: red and bold.
fn failure_status(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), theme::failure().bold())
}

/// Between a card's title and its status: a grey ` · `.
fn status_separator() -> Span<'static> {
    Span::styled(STATUS_SEPARATOR, theme::secondary())
}

/// `left`, then a grey ` · ` and `right` (a card's status); the left side
/// is cut to leave room for it. Also returns where the left side ends as
/// drawn (`usize::MAX` when `right` was left out, the window being too
/// narrow).
fn after_title(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> (Line<'static>, usize) {
    let room = left_room(&right, width);
    if room == width {
        return (Line::from(truncate(left, width)), usize::MAX);
    }
    let mut spans = truncate(left, room);
    let left_end = drawn_width(&spans);
    spans.push(status_separator());
    spans.extend(right);
    (Line::from(spans), left_end)
}

/// The columns [`after_title`] leaves the left side: all of `width` when
/// `right` is left out, otherwise up to the ` · ` before it.
fn left_room(right: &[Span<'static>], width: usize) -> usize {
    let right_width = spans_width(right);
    if right.is_empty() || width < right_width + 12 {
        width
    } else {
        width - right_width - str_width(STATUS_SEPARATOR)
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
