//! `?` in the empty input box and `/help`: the shortcuts, by section, then
//! every command, in a box above the input box like `/context`'s.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::commands::COMMANDS;
use crate::report::{Part, Report};
use crate::text::str_width;
use crate::theme;

/// The shortcuts the box shows: each section's title, then each key (or keys
/// that do the same) with what it does.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Input",
        &[
            (
                "Enter",
                "Send, or run the command; in a list, complete first",
            ),
            ("Shift+Enter / Ctrl+J", "New line"),
            ("/", "Commands, typed first"),
            (
                "!command",
                "Run a shell command directly, while the session is idle",
            ),
            ("@", "Mention a file"),
            ("↑ / ↓", "Select in a list; otherwise move between lines"),
            (
                "↑ / ↓ (empty input)",
                "Earlier / later messages sent in this directory",
            ),
            ("Tab", "Complete the command or file selected in the list"),
            (
                "Tab (while working)",
                "Steer: send into the turn in progress; Enter queues",
            ),
            ("Esc", "Close the / or @ list"),
            ("Ctrl+V", "Attach the image on the clipboard"),
            ("Ctrl+G", "Edit the message in $VISUAL or $EDITOR"),
            (
                "Ctrl+B",
                "Move the running foreground shell command to the background",
            ),
            (
                "!command &",
                "Run in background; /ps to inspect, /stop to stop all",
            ),
            ("Alt+← / Ctrl+← / Alt+B", "Back a word"),
            ("Alt+→ / Ctrl+→ / Alt+F", "Forward a word"),
            (
                "Alt+Backspace / Ctrl+W",
                "Delete the word before the cursor",
            ),
            ("Alt+Delete / Alt+D", "Delete the word after the cursor"),
            ("Ctrl+A / Home", "Start of the line"),
            ("Ctrl+E / End", "End of the line"),
            ("Ctrl+K", "Delete to the end of the line"),
            ("Ctrl+U", "Delete to the start of the line"),
            ("Ctrl+C", "Clear the input; quit when it is empty"),
            ("Ctrl+D", "Quit when the input is empty"),
            ("?", "This help, when the input is empty"),
            (
                "Large paste",
                "Over 10 lines or 1000 chars folds into [Pasted #N]",
            ),
        ],
    ),
    (
        "Conversation",
        &[
            ("Esc", "Interrupt; before any output, take the message back"),
            (
                "Esc twice",
                "After /handoff, before sending: back to the original session",
            ),
            ("PgUp / PgDn", "Scroll a page"),
            ("Mouse wheel", "Scroll 3 lines"),
            ("End", "Jump to the bottom, when the input is empty"),
            (
                "Ctrl+O",
                "Show or fold thinking, long output and long diffs",
            ),
            ("Ctrl+F", "Find in the conversation"),
            (
                "Shift+Tab",
                "Next mode: Always ask → Ask for execution → Allow all → Plan",
            ),
            ("Drag", "Select text; letting go copies it"),
            ("Double / triple click", "Copy a word / a row"),
            ("Click", "Open a link, or a path in a card title"),
        ],
    ),
    (
        "Approvals and questions",
        &[
            (
                "Shift+Tab (plan)",
                "Choose the permission mode for implementation",
            ),
            ("↑ / ↓", "Select an option"),
            ("Enter", "Choose the selected option; send what you wrote"),
            ("1–9", "Choose option N"),
            (
                "Backspace / ↑",
                "Back to the options, while what you write is empty",
            ),
            ("PgUp / PgDn", "Scroll the box"),
            ("Esc", "Interrupt the turn"),
            ("Ctrl+C / Ctrl+D", "Quit canter"),
        ],
    ),
    (
        "Find",
        &[
            ("↑ / Ctrl+P", "Older match"),
            ("↓ / Ctrl+N", "Newer match"),
            ("Enter", "Close and stay here"),
            ("Esc", "Close and go back"),
            ("Ctrl+C", "Clear the words; close when they are empty"),
        ],
    ),
];

/// The box: the shortcut sections with their keys in one column, then
/// `Commands` with the names in a column of their own.
pub(crate) fn popup() -> Report {
    let names: Vec<(String, &str)> = COMMANDS
        .iter()
        .map(|command| (format!("/{}", command.name), command.about))
        .collect();
    let commands: Vec<(&str, &str)> = names
        .iter()
        .map(|(name, about)| (name.as_str(), *about))
        .collect();
    let mut body = Vec::new();
    let keys = SHORTCUTS.iter().flat_map(|(_, rows)| rows.iter());
    let width = keys.map(|(key, _)| str_width(key)).max().unwrap_or(0);
    for (title, rows) in SHORTCUTS {
        section(&mut body, title, rows, width);
    }
    let width = commands.iter().map(|(name, _)| str_width(name)).max();
    section(&mut body, "Commands", &commands, width.unwrap_or(0));
    Report::new(
        vec![Span::styled(" Help", Style::new().bold())],
        body,
        Vec::new(),
    )
}

/// `title` and its rows, the keys `width` columns wide, after a blank row
/// unless it is the first.
fn section(body: &mut Vec<Part>, title: &str, rows: &[(&str, &str)], width: usize) {
    if !body.is_empty() {
        body.push(Part::blank());
    }
    body.push(Part::Line(vec![Span::styled(
        format!(" {title}"),
        Style::new().bold(),
    )]));
    for (key, about) in rows {
        body.push(Part::Line(vec![
            Span::raw("   "),
            Span::styled(
                format!("{key}{}", " ".repeat(width - str_width(key) + 3)),
                theme::accent(),
            ),
            Span::styled((*about).to_owned(), theme::secondary()),
        ]));
    }
}
