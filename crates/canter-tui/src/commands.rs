//! Slash commands, typed in the input box as `/name` and what follows. Each
//! command's name, its line of help and what it does are in [`COMMANDS`];
//! adding a command means adding it there.
//!
//! A first word (after any leading blanks) that starts with `/` and has no
//! other `/` in it is a command; one like `/tmp/a.txt` is a message.

use canter_protocol::{PermissionMode, ReasoningEffort, SessionUpdateParams};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Action, App};
use crate::text::{LogicalLine, sanitize, str_width, truncate};

pub(crate) struct Command {
    pub name: &'static str,
    /// One line, shown in the command list and by `/help`.
    pub about: &'static str,
    /// Whether it takes text after its name; one that does not refuses any
    /// rather than drop it.
    pub takes_args: bool,
    /// Runs the command with the text after its name (btw preserves whitespace). Returns what
    /// the caller should do; an error is shown in the conversation, and the
    /// input box keeps what was typed.
    pub run: fn(&mut App, &str) -> Result<Option<Action>, String>,
}

/// In the order the command list shows them.
pub(crate) const COMMANDS: &[Command] = &[
    Command {
        name: "compact",
        about: "Summarize the conversation to free up context",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Compact)),
    },
    Command {
        name: "context",
        about: "Show context contents and token estimates",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Context)),
    },
    Command {
        name: "exit",
        about: "Quit canter",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Quit)),
    },
    Command {
        name: "files",
        about: "List session files and git status",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Files)),
    },
    Command {
        name: "help",
        about: "Show available commands",
        takes_args: false,
        run: |app, _| {
            app.show(help());
            Ok(None)
        },
    },
    Command {
        name: "mode",
        about: "Change the permission mode",
        takes_args: true,
        run: mode,
    },
    Command {
        name: "model",
        about: "Change the model",
        takes_args: true,
        run: |_, args| {
            Ok(Some(if args.is_empty() {
                Action::PickModel
            } else {
                Action::Update(SessionUpdateParams {
                    model: Some(args.to_owned()),
                    ..SessionUpdateParams::default()
                })
            }))
        },
    },
    Command {
        name: "new",
        about: "Start a new session",
        takes_args: false,
        run: |_, _| Ok(Some(Action::NewSession)),
    },
    Command {
        name: "resume",
        about: "Switch to another session",
        takes_args: false,
        run: |_, _| Ok(Some(Action::PickSession)),
    },
    Command {
        name: "skills",
        about: "List this session's skills",
        takes_args: false,
        run: |_, _| Ok(Some(Action::ListSkills)),
    },
    Command {
        name: "think",
        about: "Change thinking effort (none, low, high, max)",
        takes_args: true,
        run: think,
    },
    Command {
        name: "usage",
        about: "Show this session's token usage and cost",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Usage)),
    },
    Command {
        name: "btw",
        about: "Ask one question without adding it to the conversation",
        takes_args: true,
        run: App::start_btw,
    },
    Command {
        name: "handoff",
        about: "Draft a handoff note in a new session",
        takes_args: true,
        run: App::start_handoff,
    },
];

pub(crate) fn skills(skills: Vec<canter_protocol::Skill>) -> Vec<LogicalLine> {
    if skills.is_empty() {
        return [
            "No skills in this session.",
            "Add skills to either location, then reopen the session:",
            "Global: ~/.agents/skills/<name>/SKILL.md",
            "Project: <project root>/.agents/skills/<name>/SKILL.md",
        ]
        .into_iter()
        .map(|text| LogicalLine::new(vec![Span::raw("  ")], vec![Span::raw(text)]))
        .collect();
    }
    skills
        .into_iter()
        .map(|skill| {
            let source = match skill.source {
                canter_protocol::SkillSource::Global => "global",
                canter_protocol::SkillSource::Project => "project",
                canter_protocol::SkillSource::Unknown => "unknown",
            };
            let manual = if skill.model_invocation {
                ""
            } else {
                " [not for model]"
            };
            LogicalLine::new(
                vec![Span::raw("  ")],
                vec![
                    Span::styled(
                        sanitize(&skill.name).replace('\n', " "),
                        Style::new().cyan(),
                    ),
                    Span::styled(format!(" [{source}]{manual}  "), Style::new().dim()),
                    Span::raw(sanitize(&skill.description).replace('\n', " ")),
                ],
            )
        })
        .collect()
}

fn think(app: &mut App, args: &str) -> Result<Option<Action>, String> {
    let efforts = [
        ReasoningEffort::None,
        ReasoningEffort::Low,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ];
    let set = |effort| {
        Action::Update(SessionUpdateParams {
            reasoning_effort: Some(effort),
            ..SessionUpdateParams::default()
        })
    };
    if args.is_empty() {
        let current = match app.snapshot().settings.reasoning_effort {
            ReasoningEffort::Unknown => ReasoningEffort::High,
            effort => effort,
        };
        let choices = efforts
            .iter()
            .map(|&effort| Choice {
                label: if effort == current {
                    format!("{} (current)", effort.as_str())
                } else {
                    effort.as_str().to_owned()
                },
                about: String::new(),
                action: set(effort),
            })
            .collect();
        let selected = efforts
            .iter()
            .position(|&effort| effort == current)
            .unwrap_or(0);
        app.choose(Chooser {
            title: "Thinking effort",
            choices,
            selected,
        });
        Ok(None)
    } else {
        efforts
            .into_iter()
            .find(|effort| effort.as_str() == args)
            .map(|effort| Some(set(effort)))
            .ok_or_else(|| format!("Unknown thinking effort: {args} (use none, low, high or max)"))
    }
}

pub(crate) fn choose_model(app: &mut App, models: Vec<canter_protocol::Model>) {
    let current = &app.snapshot().settings.model;
    let selected = models
        .iter()
        .position(|model| &model.model == current)
        .unwrap_or(0);
    let choices = models
        .into_iter()
        .map(|model| Choice {
            label: if &model.model == current {
                format!("{} (current)", sanitize(&model.display_name))
            } else {
                sanitize(&model.display_name)
            },
            about: String::new(),
            action: Action::Update(SessionUpdateParams {
                model: Some(model.model),
                ..SessionUpdateParams::default()
            }),
        })
        .collect();
    app.choose(Chooser {
        title: "Model",
        choices,
        selected,
    });
}

/// The permission modes as `/mode` takes them, with what each means.
const MODES: [(&str, PermissionMode, &str); 3] = [
    (
        "always-ask",
        PermissionMode::AlwaysAsk,
        "Ask before edits and non-read-only commands",
    ),
    (
        "ask-for-execution",
        PermissionMode::AskForExecution,
        "Edit freely; ask before non-read-only commands",
    ),
    ("allow-all", PermissionMode::AllowAll, "Never ask"),
];

/// Changes the permission mode, for `/mode` and Shift+Tab alike.
fn set_mode(mode: PermissionMode) -> Action {
    Action::Update(SessionUpdateParams {
        approval_policy: Some(mode),
        ..SessionUpdateParams::default()
    })
}

/// Shift+Tab: changes to the mode after `shown` in [`MODES`], and from the
/// last back to the first. An unknown mode counts as always-ask, as the info
/// rows show it.
pub(crate) fn next_mode(shown: PermissionMode) -> Action {
    let index = MODES
        .iter()
        .position(|&(_, mode, _)| mode == shown)
        .unwrap_or(0);
    set_mode(MODES[(index + 1) % MODES.len()].1)
}

/// `/mode <mode>` changes the permission mode; `/mode` alone offers the three.
fn mode(app: &mut App, args: &str) -> Result<Option<Action>, String> {
    if args.is_empty() {
        let current = app.snapshot().settings.approval_policy;
        let choices = MODES
            .iter()
            .map(|&(name, mode, about)| Choice {
                label: if mode == current {
                    format!("{name} (current)")
                } else {
                    name.to_owned()
                },
                about: about.to_owned(),
                action: set_mode(mode),
            })
            .collect();
        let selected = MODES
            .iter()
            .position(|&(_, mode, _)| mode == current)
            .unwrap_or(0);
        app.choose(Chooser {
            title: "Permission mode",
            choices,
            selected,
        });
        return Ok(None);
    }
    match MODES.iter().find(|&&(name, ..)| name == args) {
        Some(&(_, mode, _)) => Ok(Some(set_mode(mode))),
        None => Err(format!(
            "Unknown mode: {args} (use always-ask, ask-for-execution or allow-all)"
        )),
    }
}

/// `/help`: every command with its line, after a blank row, then the
/// shortcut for the permission mode in the same columns.
fn help() -> Vec<LogicalLine> {
    let mut rows: Vec<(String, &str)> = COMMANDS
        .iter()
        .map(|command| (format!("/{}", command.name), command.about))
        .collect();
    rows.push(("Shift+Tab".to_owned(), "Cycle permission mode"));
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut lines = vec![LogicalLine::default()];
    for (name, about) in rows {
        lines.push(LogicalLine::new(
            vec![
                Span::raw("  "),
                Span::styled(format!("{name:width$}   "), Style::new().cyan()),
            ],
            vec![Span::styled(about, Style::new().dim())],
        ));
    }
    lines
}

pub(crate) fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

/// The command name being typed, while the input is still one word that
/// could be a command: `/` and letters, no space, no other `/`.
pub(crate) fn typing(text: &str) -> Option<&str> {
    let name = text.trim_start().strip_prefix('/')?;
    (!name.contains(|c: char| c == '/' || c.is_whitespace())).then_some(name)
}

/// The commands whose name starts with `typed`.
pub(crate) fn matching(typed: &str) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(typed))
        .collect()
}

/// A command's name and the text after it, when the first word is one.
pub(crate) fn parse(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.contains('/')).then_some((name, if name == "btw" { args } else { args.trim() }))
}

/// One choice to make in a list above the input box, such as the modes
/// `/mode` alone offers.
pub(crate) struct Chooser {
    pub title: &'static str,
    pub choices: Vec<Choice>,
    pub selected: usize,
}

pub(crate) struct Choice {
    pub label: String,
    pub about: String,
    /// What choosing it does.
    pub action: Action,
}

impl Chooser {
    /// Its title, then the choices; nothing when not even one choice fits in `max` rows.
    pub fn rows(&self, width: u16, max: usize) -> Vec<Line<'static>> {
        let entries: Vec<(&str, &str)> = self
            .choices
            .iter()
            .map(|choice| (choice.label.as_str(), choice.about.as_str()))
            .collect();
        let choices = list_rows(&entries, self.selected, width, max.saturating_sub(1));
        if choices.is_empty() {
            return choices;
        }
        let title = Line::from(truncate(
            vec![
                Span::styled(format!(" {}", self.title), Style::new().bold()),
                Span::styled(
                    "  ↑↓ select · Enter choose · Esc cancel",
                    Style::new().dim(),
                ),
            ],
            usize::from(width),
        ));
        std::iter::once(title).chain(choices).collect()
    }
}

/// Rows of a list above the input box, like the session list's: the selected
/// row marked with `›` and bold, each label followed by a dim line about it.
/// At most `max` rows, keeping the selected one in view.
pub(crate) fn list_rows(
    entries: &[(&str, &str)],
    selected: usize,
    width: u16,
    max: usize,
) -> Vec<Line<'static>> {
    let label_width = entries
        .iter()
        .map(|(label, _)| str_width(label))
        .max()
        .unwrap_or(0);
    let first = (selected + 1).saturating_sub(max);
    entries
        .iter()
        .enumerate()
        .skip(first)
        .take(max)
        .map(|(index, &(label, about))| {
            let padded = format!("{label}{}", " ".repeat(label_width - str_width(label) + 3));
            let spans = if index == selected {
                vec![
                    Span::styled(" › ", Style::new().cyan().bold()),
                    Span::styled(padded, Style::new().bold()),
                    Span::styled(about.to_owned(), Style::new().dim()),
                ]
            } else {
                vec![
                    Span::raw("   "),
                    Span::raw(padded),
                    Span::styled(about.to_owned(), Style::new().dim()),
                ]
            };
            Line::from(truncate(spans, usize::from(width)))
        })
        .collect()
}
