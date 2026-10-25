//! Slash commands, typed in the input box as `/name` and what follows. Each
//! command's name, its line of help and what it does are in [`COMMANDS`];
//! adding a command means adding it there.
//!
//! A first word that starts with `/` and has no other `/` in it is a command;
//! one like `/tmp/a.txt` is a message.

use canter_protocol::{PermissionMode, SessionUpdateParams};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Action, App};
use crate::text::{LogicalLine, str_width, truncate};

pub(crate) struct Command {
    pub name: &'static str,
    /// One line, shown in the command list and by `/help`.
    pub about: &'static str,
    /// Runs the command with the text after its name, trimmed. Returns what
    /// the caller should do; an error is shown in the conversation, and the
    /// input box keeps what was typed.
    pub run: fn(&mut App, &str) -> Result<Option<Action>, String>,
}

/// In the order the command list shows them.
pub(crate) const COMMANDS: &[Command] = &[
    Command {
        name: "compact",
        about: "Summarize the conversation to free up context",
        run: |_, _| Ok(Some(Action::Compact)),
    },
    Command {
        name: "exit",
        about: "Quit canter",
        run: |_, _| Ok(Some(Action::Quit)),
    },
    Command {
        name: "help",
        about: "Show available commands",
        run: |app, _| {
            app.show(help());
            Ok(None)
        },
    },
    Command {
        name: "mode",
        about: "Change the permission mode",
        run: mode,
    },
    Command {
        name: "new",
        about: "Start a new session",
        run: |_, _| Ok(Some(Action::NewSession)),
    },
    Command {
        name: "resume",
        about: "Switch to another session",
        run: |_, _| Ok(Some(Action::PickSession)),
    },
];

/// The permission modes as `/mode` takes them, with what each means.
const MODES: [(&str, PermissionMode, &str); 3] = [
    (
        "always-ask",
        PermissionMode::AlwaysAsk,
        "Ask before editing files or running commands",
    ),
    (
        "ask-for-execution",
        PermissionMode::AskForExecution,
        "Edit files freely; ask before running commands",
    ),
    ("allow-all", PermissionMode::AllowAll, "Never ask"),
];

/// `/mode <mode>` changes the permission mode; `/mode` alone offers the three.
fn mode(app: &mut App, args: &str) -> Result<Option<Action>, String> {
    let set = |mode| {
        Action::Update(SessionUpdateParams {
            approval_policy: Some(mode),
            ..SessionUpdateParams::default()
        })
    };
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
                action: set(mode),
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
        Some(&(_, mode, _)) => Ok(Some(set(mode))),
        None => Err(format!(
            "Unknown mode: {args} (use always-ask, ask-for-execution or allow-all)"
        )),
    }
}

/// `/help`: every command with its line, after a blank row.
fn help() -> Vec<LogicalLine> {
    let width = COMMANDS.iter().map(|command| command.name.len()).max();
    let width = width.unwrap_or(0) + 1;
    let mut lines = vec![LogicalLine::default()];
    for command in COMMANDS {
        let name = format!("{:width$}   ", format!("/{}", command.name));
        lines.push(LogicalLine::new(
            vec![Span::raw("  "), Span::styled(name, Style::new().cyan())],
            vec![Span::styled(command.about, Style::new().dim())],
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
    let name = text.strip_prefix('/')?;
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
    let rest = text.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.contains('/')).then_some((name, args.trim()))
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
