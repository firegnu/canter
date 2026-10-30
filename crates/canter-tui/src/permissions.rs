use canter_protocol::{CommandRule, CommandRuleDecision, CommandRuleListResponse};
use ratatui::{style::Style, text::Span};

use crate::{
    report::{Part, Report, clean},
    theme,
};

pub(crate) struct View {
    pub list: CommandRuleListResponse,
    pub selected: usize,
    pub confirming: bool,
}

pub(crate) fn label(decision: CommandRuleDecision) -> &'static str {
    match decision {
        CommandRuleDecision::Allow => "Allow",
        CommandRuleDecision::Prompt => "Ask",
        CommandRuleDecision::Forbidden => "Deny",
    }
}

pub(crate) fn description(rule: &CommandRule) -> String {
    format!(
        "\"{}\" ({})",
        clean(&rule.pattern.join(" ")),
        label(rule.decision)
    )
}

impl View {
    pub fn new(mut list: CommandRuleListResponse) -> Self {
        list.data.sort_by_key(|rule| rule.decision);
        Self {
            list,
            selected: 0,
            confirming: false,
        }
    }

    pub fn report(&self) -> Report {
        if self.confirming
            && let Some(rule) = self.list.data.get(self.selected)
        {
            return Report::new(
                vec![Span::styled(" Permissions", Style::new().bold())],
                vec![Part::Wrap {
                    indent: 1,
                    text: format!("Delete rule {}?", description(rule)),
                    style: Style::default(),
                    rows: usize::MAX,
                }],
                Vec::new(),
            )
            .keys(" Enter to confirm · Esc to cancel");
        }
        let mut body = Vec::new();
        let mut selected_row = None;
        if self.list.data.is_empty() {
            body.push(Part::Line(vec![Span::raw(" No command rules.")]));
            body.push(Part::Wrap {
                indent: 1,
                text: "Add rules with Always allow in an approval prompt, or edit the rules file."
                    .into(),
                style: theme::secondary(),
                rows: usize::MAX,
            });
        } else {
            for decision in [
                CommandRuleDecision::Allow,
                CommandRuleDecision::Prompt,
                CommandRuleDecision::Forbidden,
            ] {
                body.push(Part::Line(vec![Span::styled(
                    format!(" {}", label(decision)),
                    Style::new().bold(),
                )]));
                for (index, rule) in self
                    .list
                    .data
                    .iter()
                    .enumerate()
                    .filter(|(_, rule)| rule.decision == decision)
                {
                    if index == self.selected {
                        selected_row = Some(body.len());
                    }
                    let (marker, style) = if index == self.selected {
                        (" › ", theme::accent().bold())
                    } else {
                        ("   ", Style::default())
                    };
                    body.push(Part::Line(vec![Span::styled(
                        format!("{marker}{}", clean(&rule.pattern.join(" "))),
                        style,
                    )]));
                }
            }
        }
        Report::new(
            vec![Span::styled(" Permissions", Style::new().bold())],
            body,
            vec![
                vec![Span::raw(format!(
                    " Rules file: {}",
                    clean(&self.list.path)
                ))],
                vec![Span::styled(
                    " Add or edit rules in this file.",
                    theme::secondary(),
                )],
            ],
        )
        .keys(if self.list.data.is_empty() {
            " Esc close"
        } else {
            " ↑↓ select · Enter/Delete delete · Esc close"
        })
        .focus(selected_row)
    }
}

pub(crate) fn error(error: &str) -> Report {
    Report::new(
        vec![Span::styled(" Permissions", Style::new().bold())],
        vec![Part::Wrap {
            indent: 1,
            text: format!("Could not read command rules: {}", clean(error)),
            style: theme::failure(),
            rows: usize::MAX,
        }],
        Vec::new(),
    )
}
