//! Slash commands, typed in the input box as `/name` and what follows. Each
//! command's name, its line of help and what it does are in [`COMMANDS`];
//! adding a command means adding it there.
//!
//! A first word (after any leading blanks) that starts with `/` and has no
//! other `/` in it is a command; one like `/tmp/a.txt` is a message.

use canter_protocol::{PermissionMode, ReasoningEffort, SessionUpdateParams, Skill};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Action, App};
use crate::export::Export;
use crate::report::{Part, Report, clean};
use crate::text::{sanitize, str_width, truncate};
use crate::theme;

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
        name: "copy",
        about: "Copy the last answer, or code block N of it",
        takes_args: true,
        run: |app, args| app.copy_answer(args).map(|()| None),
    },
    Command {
        name: "diff",
        about: "Show uncommitted git changes",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Diff)),
    },
    Command {
        name: "exit",
        about: "Quit canter",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Quit)),
    },
    Command {
        name: "export",
        about: "Save or copy the conversation as Markdown",
        takes_args: true,
        run: export,
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
            app.open_help();
            Ok(None)
        },
    },
    Command {
        name: "init",
        about: "Create or improve AGENTS.md for this project",
        takes_args: false,
        run: |_, _| {
            Ok(Some(Action::Send(
                include_str!("../assets/init_prompt.md").into(),
            )))
        },
    },
    Command {
        name: "lsp",
        about: "Show language servers",
        takes_args: false,
        run: |_, _| Ok(Some(Action::LanguageServers)),
    },
    Command {
        name: "mcp",
        about: "Show MCP connections and tools",
        takes_args: false,
        run: |_, _| Ok(Some(Action::Mcp)),
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
        name: "clear",
        about: "Start a new session (same as /new)",
        takes_args: false,
        run: |_, _| Ok(Some(Action::NewSession)),
    },
    Command {
        name: "permissions",
        about: "View and delete command and file rules",
        takes_args: false,
        run: |_, _| Ok(Some(Action::CommandRules)),
    },
    Command {
        name: "plan",
        about: "Enter or leave read-only Plan mode",
        takes_args: false,
        run: |app, _| {
            Ok(Some(Action::Update(SessionUpdateParams {
                plan_mode: Some(!app.snapshot().settings.plan_mode),
                ..Default::default()
            })))
        },
    },
    Command {
        name: "resume",
        about: "Switch to another session",
        takes_args: false,
        run: |_, _| Ok(Some(Action::PickSession)),
    },
    Command {
        name: "sandbox",
        about: "Show what the sandbox allows",
        takes_args: false,
        run: |app, _| match app.snapshot().settings.sandbox_enabled {
            // An older server says nothing, which must not read as off.
            None => Err(
                "This server does not support /sandbox; run canter serve --replace and retry"
                    .into(),
            ),
            Some(_) => Ok(Some(Action::Sandbox)),
        },
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
    Command {
        name: "review",
        about: "Review uncommitted changes or a PR number/URL (read-only)",
        takes_args: true,
        run: |_, args| Ok(Some(Action::Review(args.to_owned()))),
    },
    Command {
        name: "rewind",
        about: "Rewind conversation, files, or both",
        takes_args: false,
        run: |_, _| Ok(Some(Action::RewindList)),
    },
    Command {
        name: "statusline",
        about: "Choose and order what the status line shows",
        takes_args: false,
        run: |app, _| {
            app.open_status_setup();
            Ok(None)
        },
    },
    Command {
        name: "ps",
        about: "List background commands and inspect or stop one",
        takes_args: false,
        run: |_, _| Ok(Some(Action::BackgroundList)),
    },
    Command {
        name: "stop",
        about: "Stop all background commands in this session",
        takes_args: false,
        run: |_, _| Ok(Some(Action::BackgroundStop(None))),
    },
    Command {
        name: "theme",
        about: "Choose the code colour theme",
        takes_args: false,
        run: |app, _| {
            app.open_theme_picker();
            Ok(None)
        },
    },
    Command {
        name: "add-dir",
        about: "Add a directory to this session",
        takes_args: true,
        run: |_, args| {
            if args.is_empty() {
                return Err("Usage: /add-dir <path>".into());
            }
            let path = args
                .strip_prefix('"')
                .and_then(|path| path.strip_suffix('"'))
                .unwrap_or(args);
            Ok(Some(Action::Update(SessionUpdateParams {
                add_directories: vec![path.to_owned()],
                ..Default::default()
            })))
        },
    },
    Command {
        name: "goal",
        about: "Set or view a goal; pause, resume, clear, or --budget USD",
        takes_args: true,
        run: |_, args| crate::goal::parse(args).map(|command| Some(Action::Goal(command))),
    },
];

/// `/skills`: global and project skills, each a section; every skill's
/// name, then its description in at most two rows.
pub(crate) fn skills(skills: &[canter_protocol::Skill]) -> Report {
    use canter_protocol::SkillSource;
    let count = |source| skills.iter().filter(|skill| skill.source == source).count();
    let title = vec![
        Span::styled(" Skills", Style::new().bold()),
        Span::styled(
            format!(
                "  {} · global {} · project {}",
                skills.len(),
                count(SkillSource::Global),
                count(SkillSource::Project)
            ),
            theme::secondary(),
        ),
    ];
    if skills.is_empty() {
        let body = [
            "No skills in this session.",
            "Add skills to either location, then reopen the session:",
            "Global: ~/.agents/skills/<name>/SKILL.md",
            "Project: <project root>/.agents/skills/<name>/SKILL.md",
        ]
        .into_iter()
        .map(|text| Part::Line(vec![Span::raw(format!(" {text}"))]))
        .collect();
        return Report::new(title, body, Vec::new());
    }
    let name_width = skills
        .iter()
        .map(|skill| str_width(&clean(&skill.name)))
        .max()
        .unwrap_or(0);
    let mut body = Vec::new();
    for (source, heading, directory) in [
        (SkillSource::Global, "Global", Some("~/.agents/skills")),
        (SkillSource::Project, "Project", Some(".agents/skills")),
        (SkillSource::Unknown, "Other", None),
    ] {
        let mut shown = skills
            .iter()
            .filter(|skill| skill.source == source)
            .peekable();
        if shown.peek().is_none() {
            continue;
        }
        if !body.is_empty() {
            body.push(Part::blank());
        }
        let mut spans = vec![Span::styled(format!(" {heading}"), Style::new().bold())];
        if let Some(directory) = directory {
            spans.push(Span::styled(format!("  {directory}"), theme::secondary()));
        }
        body.push(Part::Line(spans));
        for skill in shown {
            let name = clean(&skill.name);
            let mut spans = vec![
                Span::raw("   "),
                Span::styled(name.clone(), theme::accent()),
            ];
            if !skill.model_invocation {
                spans.push(Span::raw(" ".repeat(name_width - str_width(&name) + 3)));
                spans.push(Span::styled("not for model", theme::secondary()));
            }
            body.push(Part::Line(spans));
            let description = clean(&skill.description);
            if !description.trim().is_empty() {
                body.push(Part::Wrap {
                    indent: 5,
                    text: description,
                    style: theme::secondary(),
                    rows: 2,
                });
            }
        }
    }
    Report::new(title, body, Vec::new())
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

/// Shift+Tab cycles through the three permissions, Plan, then always-ask.
/// An unknown permission counts as always-ask, as the info rows show it.
pub(crate) fn next_mode(shown: PermissionMode, plan_mode: bool) -> Action {
    if plan_mode {
        return Action::Update(SessionUpdateParams {
            plan_mode: Some(false),
            approval_policy: Some(PermissionMode::AlwaysAsk),
            ..Default::default()
        });
    }
    if shown == PermissionMode::AllowAll {
        return Action::Update(SessionUpdateParams {
            plan_mode: Some(true),
            ..Default::default()
        });
    }
    set_mode(next_permission(shown))
}

pub(crate) fn next_permission(shown: PermissionMode) -> PermissionMode {
    let index = MODES
        .iter()
        .position(|&(_, mode, _)| mode == shown)
        .unwrap_or(0);
    MODES[(index + 1) % MODES.len()].1
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

/// `/export <path>` writes there; `/export` alone offers a file with the
/// default name or the clipboard.
fn export(app: &mut App, args: &str) -> Result<Option<Action>, String> {
    let path = args.trim();
    if !path.is_empty() {
        return Ok(Some(Action::Export(Export::File(Some(path.to_owned())))));
    }
    app.choose(Chooser {
        title: "Export conversation",
        choices: vec![
            Choice {
                label: "Save to file".into(),
                about: "canter-<session name>-<time>.md in the session's directory".into(),
                action: Action::Export(Export::File(None)),
            },
            Choice {
                label: "Copy to clipboard".into(),
                about: "The same Markdown".into(),
                action: Action::Export(Export::Clipboard),
            },
        ],
        selected: 0,
    });
    Ok(None)
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

pub(crate) struct Completion {
    pub name: String,
    pub about: String,
}

/// Builtins first, then session skills; a builtin owns its name.
pub(crate) fn matching(typed: &str, skills: &[Skill]) -> Vec<Completion> {
    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(typed))
        .map(|command| Completion {
            name: command.name.into(),
            about: command.about.into(),
        })
        .chain(
            skills
                .iter()
                .filter(|skill| skill.name.starts_with(typed) && find(&skill.name).is_none())
                .map(|skill| Completion {
                    name: skill.name.clone(),
                    about: format!("skill · {}", clean(&skill.description)),
                }),
        )
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
                    theme::secondary(),
                ),
            ],
            usize::from(width),
        ));
        std::iter::once(title).chain(choices).collect()
    }
}

/// The first entry [`list_rows`] shows in `max` rows.
pub(crate) fn first_shown(selected: usize, max: usize) -> usize {
    (selected + 1).saturating_sub(max)
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
    let first = first_shown(selected, max);
    entries
        .iter()
        .enumerate()
        .skip(first)
        .take(max)
        .map(|(index, &(label, about))| {
            let padded = format!("{label}{}", " ".repeat(label_width - str_width(label) + 3));
            let spans = if index == selected {
                vec![
                    Span::styled(" › ", theme::accent().bold()),
                    Span::styled(padded, Style::new().bold()),
                    Span::styled(about.to_owned(), theme::secondary()),
                ]
            } else {
                vec![
                    Span::raw("   "),
                    Span::raw(padded),
                    Span::styled(about.to_owned(), theme::secondary()),
                ]
            };
            Line::from(truncate(spans, usize::from(width)))
        })
        .collect()
}

/// `/sandbox`: what the server says this session's model commands may touch
/// now, paths under `home` as `~`; when it is off, how to turn it on.
pub(crate) fn sandbox(
    report: &canter_protocol::SessionSandboxResponse,
    home: Option<&str>,
) -> Report {
    let line = |text: String| Part::Wrap {
        indent: 1,
        text: clean(&text),
        style: Style::new(),
        rows: usize::MAX,
    };
    if !report.enabled {
        return Report::new(
            vec![Span::styled(" Sandbox off.", Style::new().bold())],
            vec![line(
                "Set \"sandbox\": true in config.json (or remove the line), then run canter serve --replace."
                    .into(),
            )],
            Vec::new(),
        );
    }
    let paths = |paths: &[String]| {
        paths
            .iter()
            .map(|path| crate::approval::shorten_home(path, home))
            .collect::<Vec<_>>()
    };
    let mut read_only = vec![".git config and hooks".to_owned()];
    read_only.extend(paths(&report.read_only));
    let body = vec![
        line(format!("Writable: {}", paths(&report.writable).join(", "))),
        line(format!("Read-only: {}", read_only.join(", "))),
        line(format!(
            "No access: {}",
            paths(&report.no_access).join(", ")
        )),
        line(
            if report.network {
                "Network: on (Allow all)"
            } else {
                "Network: off unless approved for one command"
            }
            .into(),
        ),
        line("Not sandboxed: ! commands, MCP tools, web_fetch".into()),
    ];
    Report::new(
        vec![Span::styled(" Sandbox on", Style::new().bold())],
        body,
        Vec::new(),
    )
}

/// `/mcp`: each service with whether it is connected, then its tools'
/// short names filling rows, and its warnings.
pub(crate) fn mcp(report: &canter_protocol::McpListResponse) -> Report {
    let plural =
        |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let tools: usize = report.data.iter().map(|server| server.tools.len()).sum();
    let title = vec![
        Span::styled(" MCP", Style::new().bold()),
        Span::styled(
            format!(
                "  {} · {}",
                plural(report.data.len(), "service"),
                plural(tools, "tool")
            ),
            theme::secondary(),
        ),
    ];
    if report.data.is_empty() {
        let body = vec![Part::Line(vec![Span::raw(" No MCP services configured.")])];
        return Report::new(title, body, Vec::new());
    }
    let name_width = report
        .data
        .iter()
        .map(|server| str_width(&clean(&server.name)))
        .max()
        .unwrap_or(0);
    let mut body = Vec::new();
    for server in &report.data {
        if !body.is_empty() {
            body.push(Part::blank());
        }
        let name = clean(&server.name);
        let pad = " ".repeat(name_width - str_width(&name) + 3);
        let (mark, status) = if server.connected {
            (
                Span::styled("●", theme::success()),
                Span::styled(
                    format!("connected · {}", plural(server.tools.len(), "tool")),
                    theme::secondary(),
                ),
            )
        } else {
            (
                Span::styled("○", theme::failure()),
                Span::styled("disconnected", theme::failure()),
            )
        };
        body.push(Part::Line(vec![
            Span::raw(" "),
            mark,
            Span::raw(" "),
            Span::styled(name, Style::new().bold()),
            Span::raw(pad),
            status,
        ]));
        if let Some(error) = &server.error {
            body.push(Part::Line(vec![Span::raw(format!("   {}", clean(error)))]));
        }
        if !server.tools.is_empty() {
            body.push(Part::Flow {
                indent: 3,
                words: server.tools.iter().map(|tool| clean(&tool.tool)).collect(),
            });
        }
        for warning in &server.warnings {
            body.push(Part::Line(vec![Span::styled(
                format!("   {}", clean(warning)),
                theme::secondary(),
            )]));
        }
    }
    Report::new(
        title,
        body,
        vec![vec![Span::styled(
            " Tools are called as mcp__<service>__<tool>.",
            theme::secondary(),
        )]],
    )
}
