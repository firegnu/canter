use canter_protocol::{
    CommandRule, CommandRuleDecision, CommandRuleListResponse, FileRule, FileRuleDecision,
};
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

pub(crate) fn file_description(rule: &FileRule) -> String {
    let label = match rule.decision {
        FileRuleDecision::Prompt => "Ask",
        FileRuleDecision::Forbidden => "Deny",
    };
    format!("\"{}\" ({label})", clean(&rule.pattern))
}

impl View {
    pub fn new(mut list: CommandRuleListResponse) -> Self {
        list.data.sort_by_key(|rule| rule.decision);
        list.file_rules.sort_by_key(|rule| rule.decision);
        Self {
            list,
            selected: 0,
            confirming: false,
        }
    }

    pub fn len(&self) -> usize {
        self.list.data.len() + self.list.file_rules.len()
    }

    pub fn delete_action(&self) -> Option<crate::Action> {
        if let Some(rule) = self.list.data.get(self.selected) {
            Some(crate::Action::DeleteCommandRule(rule.clone()))
        } else {
            self.list
                .file_rules
                .get(self.selected.checked_sub(self.list.data.len())?)
                .cloned()
                .map(crate::Action::DeleteFileRule)
        }
    }

    pub fn report(&self) -> Report {
        if self.confirming
            && let Some(action) = self.delete_action()
        {
            let description = match action {
                crate::Action::DeleteCommandRule(rule) => {
                    format!("Delete rule {}?", description(&rule))
                }
                crate::Action::DeleteFileRule(rule) => {
                    format!("Delete file rule {}?", file_description(&rule))
                }
                _ => unreachable!(),
            };
            return Report::new(
                vec![Span::styled(" Permissions", Style::new().bold())],
                vec![Part::Wrap {
                    indent: 1,
                    text: description,
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
        body.push(Part::Line(vec![Span::styled(
            " File rules",
            Style::new().bold(),
        )]));
        if self.list.file_rules.is_empty() {
            body.push(Part::Wrap {
                indent: 1,
                text: "No file rules. Add file patterns under fileRules in rules.json.".into(),
                style: theme::secondary(),
                rows: usize::MAX,
            });
        } else {
            for (index, rule) in self.list.file_rules.iter().enumerate() {
                let selected = self.list.data.len() + index == self.selected;
                if selected {
                    selected_row = Some(body.len());
                }
                let (marker, style) = if selected {
                    (" › ", theme::accent().bold())
                } else {
                    ("   ", Style::default())
                };
                body.push(Part::Line(vec![Span::styled(
                    format!("{marker}{}", file_description(rule)),
                    style,
                )]));
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
        .keys(if self.len() == 0 {
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
            text: format!("Could not read rules: {}", clean(error)),
            style: theme::failure(),
            rows: usize::MAX,
        }],
        Vec::new(),
    )
}
