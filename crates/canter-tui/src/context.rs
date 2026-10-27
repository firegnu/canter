use crate::text::{sanitize, truncate};
use crate::theme;
use crate::{app::tokens, approval::shorten_home};
use canter_protocol::{ContextResponse, ContextSectionKind};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

const KEYS: &str = " Esc close";

pub(crate) fn lines(report: &ContextResponse, home: Option<&str>) -> Vec<String> {
    let window = report.model_context_window;
    let mut lines = vec![match report.context_tokens {
        Some(used) if window > 0 => format!(
            "Context  ctx {}% {}/{} (last request)",
            (u128::from(used) * 100 + u128::from(window) / 2) / u128::from(window),
            tokens(used),
            tokens(window)
        ),
        Some(used) => format!("Context  {} (last request)", tokens(used)),
        None => "Context  no request yet".into(),
    }];
    for section in &report.prefix {
        let label = match section.kind {
            ContextSectionKind::System => "System prompt".into(),
            ContextSectionKind::ModelFamily => "Model notes".into(),
            ContextSectionKind::GlobalInstructions => "Global AGENTS.md".into(),
            ContextSectionKind::ProjectInstructions => "Project AGENTS.md".into(),
            ContextSectionKind::Skills => format!("Skills ({})", report.skill_count),
            ContextSectionKind::Environment => "Environment".into(),
            ContextSectionKind::Unknown => continue,
        };
        let path = section
            .path
            .as_deref()
            .map(|path| shorten_home(path, home))
            .unwrap_or_default();
        lines.push(format!(
            "  {label:<24} {:>6}  {path}",
            tokens(section.tokens)
        ));
    }
    lines.push(format!(
        "  {:<24} {:>6}",
        format!("Tools ({})", report.tool_count),
        tokens(report.tool_tokens)
    ));
    if let Some(summary) = report.compaction_tokens {
        lines.push(format!(
            "  {:<24} {:>6}",
            "Compaction summary",
            tokens(summary)
        ));
    }
    let messages = &report.messages;
    lines.push(format!(
        "  {:<24} {:>6}  user {} · assistant {} · tool results {} · images {}",
        format!("Messages ({})", messages.count),
        tokens(messages.tokens),
        tokens(messages.user),
        tokens(messages.assistant),
        tokens(messages.tool_results),
        tokens(messages.images)
    ));
    lines.push(format!(
        "  {:<24} {:>6}",
        "Free",
        tokens(report.free_tokens)
    ));
    lines.push("Estimated by size; the first line is what the model reported.".into());
    lines.push(
        "Currently loaded context; queued input and future tool results are not included.".into(),
    );
    lines
}

/// `/context` above the input box until Esc or Enter, with the numbers of
/// when it opened. When the section rows do not all fit, they scroll with
/// Up and Down, with how many more there are above and below; the first
/// line, the notes and the keys stay.
pub(crate) struct Popup {
    title: String,
    sections: Vec<String>,
    notes: Vec<String>,
    /// The first section row shown.
    top: usize,
    /// The largest `top`, for the room the last frame had.
    last_top: usize,
}

impl Popup {
    pub fn new(report: &ContextResponse, home: Option<&str>) -> Self {
        let mut lines: Vec<String> = lines(report, home)
            .iter()
            .map(|line| sanitize(line).replace('\n', " "))
            .collect();
        let notes = lines.split_off(lines.len() - 2);
        let sections = lines.split_off(1);
        Self {
            title: lines.remove(0),
            sections,
            notes,
            top: 0,
            last_top: 0,
        }
    }

    pub fn up(&mut self) {
        self.top = self.top.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.top = (self.top + 1).min(self.last_top);
    }

    /// At most `max` rows; nothing when not even one section row fits.
    pub fn rows(&mut self, width: u16, max: usize) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, width));
        let mut rows = vec![
            cut(vec![Span::styled(
                format!(" {}", self.title),
                Style::new().bold(),
            )]),
            Line::default(),
        ];
        let mut tail = vec![Line::default()];
        tail.extend(
            self.notes
                .iter()
                .map(|note| cut(vec![Span::raw(format!(" {note}"))])),
        );
        tail.push(cut(vec![Span::styled(KEYS, theme::secondary())]));
        let room = max.saturating_sub(rows.len() + tail.len());
        if room == 0 {
            return Vec::new();
        }
        let total = self.sections.len();
        // With three rows or more, a row each for how many more there are.
        self.last_top = match total.checked_sub(room) {
            None | Some(0) => 0,
            Some(over) if room < 3 => over,
            Some(over) => over + 1,
        };
        self.top = self.top.min(self.last_top);
        let first = self.top;
        let shown = if total <= room || room < 3 {
            room
        } else if first == 0 || first == self.last_top {
            room - 1
        } else {
            room - 2
        };
        let more = |arrow: char, count: usize| {
            Line::from(vec![Span::styled(
                format!("   {arrow} {count} more"),
                theme::secondary(),
            )])
        };
        if first > 0 && room >= 3 {
            rows.push(more('↑', first));
        }
        rows.extend(
            self.sections
                .iter()
                .skip(first)
                .take(shown)
                .map(|section| cut(vec![Span::raw(format!(" {section}"))])),
        );
        let below = total.saturating_sub(first + shown);
        if below > 0 && room >= 3 {
            rows.push(more('↓', below));
        }
        rows.extend(tail);
        rows
    }
}
