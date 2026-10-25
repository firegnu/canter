use crate::{app::tokens, approval::shorten_home};
use canter_protocol::{ContextResponse, ContextSectionKind};

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
            ContextSectionKind::GlobalInstructions => "Global AGENTS.md".into(),
            ContextSectionKind::ProjectInstructions => "Project AGENTS.md".into(),
            ContextSectionKind::Skills => format!("Skills ({})", report.skill_count),
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
