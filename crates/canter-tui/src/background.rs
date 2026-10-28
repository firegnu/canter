use crate::{
    app::{Action, format_elapsed},
    commands::{Choice, Chooser},
    report::{Part, Report, clean},
    theme,
};
use canter_protocol::{BackgroundCommand, BackgroundCommandOutput};
use ratatui::{style::Style, text::Span};

pub(crate) fn chooser(commands: &[BackgroundCommand], now: i64) -> Chooser {
    Chooser {
        title: "Background commands · s stop",
        selected: 0,
        choices: commands
            .iter()
            .map(|command| Choice {
                label: format!("#{} {}", command.id, clean(&command.command)),
                about: format!(
                    "{} · {}",
                    format_elapsed(
                        command
                            .completed_at
                            .unwrap_or(now)
                            .saturating_sub(command.started_at)
                    ),
                    status(command)
                ),
                action: Action::BackgroundOutput(command.id),
            })
            .collect(),
    }
}
fn status(command: &BackgroundCommand) -> String {
    if command.running {
        "后台运行中".into()
    } else {
        format!(
            "exit code: {}",
            command
                .exit_code
                .map_or_else(|| "unknown".into(), |code| code.to_string())
        )
    }
}
pub(crate) fn empty() -> Report {
    Report::new(
        vec![Span::styled(" Background commands", Style::new().bold())],
        vec![Part::Line(vec![Span::raw(
            " No background commands in this session.",
        )])],
        Vec::new(),
    )
}
pub(crate) fn output(output: &BackgroundCommandOutput) -> Report {
    let mut body = vec![Part::Line(vec![Span::raw(format!(
        " {}",
        clean(&output.command.command)
    ))])];
    if output.skipped_bytes > 0 {
        body.push(Part::Line(vec![Span::styled(
            format!(" Skipped {} bytes; showing the tail.", output.skipped_bytes),
            theme::secondary(),
        )]));
    }
    body.extend(
        crate::text::sanitize(&output.output)
            .lines()
            .map(|text| Part::Wrap {
                indent: 1,
                text: text.into(),
                style: Style::default(),
                rows: usize::MAX,
            }),
    );
    Report::new(
        vec![Span::styled(
            format!(
                " Background #{} · {}",
                output.command.id,
                status(&output.command)
            ),
            Style::new().bold(),
        )],
        body,
        vec![vec![Span::styled(
            " s stop · r refresh · Backspace list",
            theme::secondary(),
        )]],
    )
}
