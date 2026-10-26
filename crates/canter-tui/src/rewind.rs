use crate::{
    Action, App,
    commands::{Choice, Chooser},
    text::{LogicalLine, sanitize},
};
use canter_protocol::{RewindListResponse, RewindPreviewResponse, SessionRewindParams, UserInput};
use ratatui::text::Span;

pub(crate) fn text(input: &[UserInput]) -> String {
    input
        .iter()
        .filter_map(|part| match part {
            UserInput::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn choose_turn(app: &mut App, report: RewindListResponse) {
    if report.data.is_empty() {
        app.warning("No turns available to rewind after the latest compaction.");
        return;
    }
    app.choose(Chooser {
        title: "Rewind before which message?",
        selected: 0,
        choices: report
            .data
            .into_iter()
            .map(|turn| Choice {
                label: sanitize(&text(&turn.input))
                    .replace('\n', " ")
                    .chars()
                    .take(48)
                    .collect(),
                about: format!("{} files", turn.file_count),
                action: Action::RewindMode(turn.turn_id),
            })
            .collect(),
    });
}

pub(crate) fn choose_mode(app: &mut App, turn: String) {
    app.choose(Chooser {
        title: "What should be rewound?",
        selected: 0,
        choices: [
            ("Conversation and files", true, true),
            ("Conversation only", false, true),
            ("Files only", true, false),
        ]
        .into_iter()
        .map(|(label, files, conversation)| Choice {
            label: label.into(),
            about: String::new(),
            action: Action::RewindPreview(SessionRewindParams {
                before_turn_id: turn.clone(),
                rewind_files: files,
                rewind_conversation: conversation,
                confirmed_external_changes: None,
            }),
        })
        .collect(),
    });
}

pub(crate) fn confirm(
    app: &mut App,
    mut params: SessionRewindParams,
    report: RewindPreviewResponse,
) {
    let mut lines = vec!["Rewind preview (Esc cancels):".to_owned()];
    for file in report.files {
        let detail = if let Some(reason) = file.unavailable {
            format!("cannot restore: {reason}")
        } else if file.external_change {
            "之后在 canter 以外被改过 — confirming overwrites these changes".into()
        } else {
            "will restore".into()
        };
        lines.push(format!("{}: {detail}", file.path));
    }
    if params.rewind_conversation {
        lines.push("The selected message and later conversation will be removed; the message returns to the input box.".into());
    }
    app.show(
        lines
            .into_iter()
            .map(|line| LogicalLine::new(vec![Span::raw("  ")], vec![Span::raw(sanitize(&line))]))
            .collect(),
    );
    params.confirmed_external_changes = Some(true);
    app.choose(Chooser {
        title: "Confirm rewind?",
        selected: 0,
        choices: vec![Choice {
            label: "Confirm rewind".into(),
            about: "Esc to cancel".into(),
            action: Action::Rewind(params),
        }],
    });
}
