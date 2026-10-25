//! A single question in the same space and with the same guards as approval.
use canter_protocol::UserInputRequest;
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::{
    approval::ApprovalBox,
    text::{LogicalLine, visible, wrap},
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn layout(
    question: &UserInputRequest,
    selected: usize,
    custom: Option<&str>,
    scroll: usize,
    taken: bool,
    width: u16,
    max_height: u16,
) -> ApprovalBox {
    let inner = width.saturating_sub(4).max(1);
    let lines = |text: String| wrap(&LogicalLine::new(Vec::new(), vec![Span::raw(text)]), inner);
    let body = lines(visible(&question.question));
    let mut tail = vec![Line::default()];
    if let Some(text) = custom {
        tail.push(Line::from("Write your own:"));
        // Keep the end of a long single-line answer and its cursor visible.
        let input = lines(format!("› {}▏", visible(text)));
        let room = usize::from(max_height.saturating_sub(6)).max(1);
        tail.extend(
            input
                .into_iter()
                .rev()
                .take(room)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
    } else {
        for index in 0..=question.options.len() {
            let text = match question.options.get(index) {
                Some(option) => format!(
                    "{}. {} — {}",
                    index + 1,
                    visible(&option.label),
                    visible(&option.description)
                ),
                None => format!("{}. Write your own", index + 1),
            };
            let style = if index == selected {
                Style::new().cyan().bold()
            } else {
                Style::new()
            };
            let prefix = if index == selected { "› " } else { "  " };
            tail.extend(wrap(
                &LogicalLine::new(vec![Span::raw(prefix)], vec![Span::styled(text, style)]),
                inner,
            ));
        }
    }
    if !taken {
        for row in &mut tail {
            *row = row.clone().patch_style(Style::new().dim());
        }
    }
    let room = usize::from(max_height).saturating_sub(2 + tail.len());
    let fits = room > 0;
    let shown = body.len().min(room);
    let max_scroll = body.len().saturating_sub(shown);
    let scroll = scroll.min(max_scroll);
    let mut rows = if fits {
        let mut rows = body[scroll..scroll + shown].to_vec();
        rows.extend(tail);
        rows
    } else {
        vec![Line::from(
            "Window too small for the question; make it taller to answer",
        )]
    };
    for row in &mut rows {
        row.spans.insert(0, Span::raw(" "));
    }
    let hint = if !fits {
        "Esc interrupt"
    } else if custom.is_some() {
        "Enter send, Backspace/↑ back if empty, PgUp/PgDn scroll, Esc interrupt"
    } else {
        "↑↓ select, Enter confirm, number keys pick, PgUp/PgDn scroll, Esc interrupt"
    };
    ApprovalBox {
        title: Line::from(Span::styled("─ Ask user ", Style::new().yellow().bold())),
        position: (fits && max_scroll > 0).then(|| {
            Line::from(format!(
                " {}–{} / {} ─",
                scroll + 1,
                scroll + shown,
                body.len()
            ))
            .right_aligned()
        }),
        rows,
        footer: Line::from(Span::styled(format!("─ {hint} ─"), Style::new().dim())),
        page: shown.max(1),
        max_scroll,
        fits,
    }
}
