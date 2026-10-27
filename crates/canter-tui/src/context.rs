use crate::report::{Part, Report, clean};
use crate::{app::tokens, approval::shorten_home};
use canter_protocol::{ContextResponse, ContextSectionKind};
use ratatui::style::Style;
use ratatui::text::Span;

/// Each line, and the path that ends it (empty when none).
pub(crate) fn lines(report: &ContextResponse, home: Option<&str>) -> Vec<(String, String)> {
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
    let mut paths = vec![String::new()];
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
        lines.push(format!("  {label:<24} {:>6}  ", tokens(section.tokens)));
        paths.push(path);
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
    paths.resize(lines.len(), String::new());
    lines.into_iter().zip(paths).collect()
}

/// `/context` above the input box until Esc or Enter, with the numbers of
/// when it opened: the first line, the section rows, the notes. A path
/// that does not fit loses directories.
pub(crate) fn popup(report: &ContextResponse, home: Option<&str>) -> Report {
    let mut lines: Vec<(String, String)> = lines(report, home)
        .iter()
        .map(|(line, path)| (clean(line), clean(path)))
        .collect();
    let notes = lines.split_off(lines.len() - 2);
    let sections = lines.split_off(1);
    Report::new(
        vec![Span::styled(
            format!(" {}", lines[0].0),
            Style::new().bold(),
        )],
        sections
            .into_iter()
            .map(|(section, path)| {
                let before = vec![Span::raw(format!(" {section}"))];
                if path.is_empty() {
                    Part::Line(before)
                } else {
                    Part::Path {
                        before,
                        path,
                        column: 0,
                        after: Vec::new(),
                    }
                }
            })
            .collect(),
        notes
            .into_iter()
            .map(|(note, _)| vec![Span::raw(format!(" {note}"))])
            .collect(),
    )
}
