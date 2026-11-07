//! What the two rows at the bottom show when no status command draws them
//! (P2-243): which items, as `/statusline` sets it, kept in `tui.json`
//! beside `config.json`, which the terminal never writes; and where each
//! goes, which is fixed. The first row has the model, thinking level,
//! permission mode and sandbox on the left and context, cache and cost on
//! the right; the second the directory, branch and lines changed, with the
//! session name and version on the right.

use std::io;
use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;
use serde_json::{Value, json};

use crate::short_path::fit_path;
use crate::text::{spans_width, str_width, truncate};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Model,
    Thinking,
    Permission,
    Cost,
    Directory,
    Branch,
    Diff,
    Context,
    Cache,
    Tps,
    Version,
    Session,
}

impl Item {
    /// In the order of [`Values`].
    pub const ALL: [Item; 12] = [
        Item::Model,
        Item::Thinking,
        Item::Permission,
        Item::Cost,
        Item::Directory,
        Item::Branch,
        Item::Diff,
        Item::Context,
        Item::Cache,
        Item::Tps,
        Item::Version,
        Item::Session,
    ];

    /// As `/statusline` lists them: as the rows show them, left to right,
    /// then the speed, which shows on the line saying what the turn does.
    pub const LISTED: [Item; 12] = [
        Item::Model,
        Item::Thinking,
        Item::Permission,
        Item::Context,
        Item::Cache,
        Item::Cost,
        Item::Directory,
        Item::Branch,
        Item::Diff,
        Item::Session,
        Item::Version,
        Item::Tps,
    ];

    /// As `/statusline` lists it and `tui.json` keeps it.
    pub fn name(self) -> &'static str {
        match self {
            Item::Model => "model",
            Item::Thinking => "thinking",
            Item::Permission => "permission",
            Item::Cost => "cost",
            Item::Directory => "directory",
            Item::Branch => "branch",
            Item::Diff => "diff",
            Item::Context => "context",
            Item::Cache => "cache",
            Item::Tps => "tps",
            Item::Version => "version",
            Item::Session => "session",
        }
    }
}

/// The name `tui.json` kept the start of the second row under, when the
/// order was the user's. Still written, so a terminal from before reads the
/// file back with the rows it had.
const LINE2: &str = "line2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bar {
    /// Whether each item shows, in the order of [`Item::ALL`].
    pub shown: [bool; 12],
    /// Whether the status command in `config.json`, when there is one,
    /// draws the rows instead.
    pub use_script: bool,
}

impl Default for Bar {
    /// All but the speed, the version and the session name.
    fn default() -> Self {
        Self {
            shown: Item::ALL.map(|item| !matches!(item, Item::Tps | Item::Version | Item::Session)),
            use_script: true,
        }
    }
}

/// What each item says now, in the order of [`Item::ALL`]; `None` while it
/// has nothing to say, and it is left out.
pub(crate) type Values = [Option<Vec<Span<'static>>>; 12];

const SANDBOX_ON: &str = "Sandbox on";
const SANDBOX_OFF: &str = "Sandbox off";
/// Between the groups of a row, and between the items on the right.
const GAP: &str = "   ";
/// Between the directory, the branch and the lines changed.
const NEAR: &str = "  ";

/// What the permission item adds after the mode when the server says whether
/// the sandbox is on: ` · Sandbox on`, or `Sandbox off` in red as `Allow all`.
pub(crate) fn sandbox(enabled: bool) -> [Span<'static>; 2] {
    [
        Span::styled(" · ", theme::secondary()),
        if enabled {
            Span::styled(SANDBOX_ON, theme::secondary())
        } else {
            Span::styled(SANDBOX_OFF, theme::failure())
        },
    ]
}

/// A percent in four columns, right-aligned, so `9%` and `100%` take the
/// same room; more than that as it is.
pub(crate) fn percent(percent: u128) -> String {
    format!("{:>4}", format!("{percent}%"))
}

/// The two rows, each no wider than `width`, with `end` (such as a status
/// command's error) at the end of the first; and the first row's text
/// through the end of the permission mode, when it shows.
pub(crate) struct Rows {
    pub rows: [Vec<Span<'static>>; 2],
    pub through_mode: Option<String>,
}

impl Bar {
    pub fn shows(&self, item: Item) -> bool {
        self.shown[item as usize]
    }

    fn value<'a>(&self, values: &'a Values, item: Item) -> Option<&'a Vec<Span<'static>>> {
        values[item as usize].as_ref().filter(|_| self.shows(item))
    }

    pub fn rows(&self, values: &Values, width: usize, end: Vec<Span<'static>>) -> Rows {
        let (first, through_mode) = self.first_row(values, width, end);
        Rows {
            rows: [first, self.second_row(values, width)],
            through_mode,
        }
    }

    /// Model and thinking level, then the permission mode and sandbox; on
    /// the right context, cache and cost, which go whole from the right,
    /// cache first, when they do not fit. Then the sandbox goes, all of it,
    /// and past that the row is cut at its end.
    fn first_row(
        &self,
        values: &Values,
        width: usize,
        end: Vec<Span<'static>>,
    ) -> (Vec<Span<'static>>, Option<String>) {
        let mut left = vec![Span::raw(" ")];
        if let Some(model) = self.value(values, Item::Model) {
            left.extend(model.iter().cloned());
        }
        if let Some(thinking) = self.value(values, Item::Thinking) {
            if left.len() > 1 {
                left.push(Span::raw(" "));
            }
            left.extend(thinking.iter().cloned());
        }
        let mut through_mode = None;
        let mut sandbox_at = None;
        if let Some(value) = self.value(values, Item::Permission) {
            if left.len() > 1 {
                left.push(Span::raw(GAP));
            }
            // The mode alone decides whether its shortcuts show.
            let sandbox = value.len() > 2
                && value
                    .last()
                    .is_some_and(|span| [SANDBOX_ON, SANDBOX_OFF].contains(&span.content.as_ref()));
            let mode = if sandbox {
                value.len() - 2
            } else {
                value.len()
            };
            left.extend(value[..mode].iter().cloned());
            through_mode = Some(text(&left));
            if sandbox {
                sandbox_at = Some(left.len()..left.len() + 2);
                left.extend(value[mode..].iter().cloned());
            }
        }
        let room = width.saturating_sub(spans_width(&end));
        let mut right: Vec<(Item, &Vec<Span<'static>>)> = [Item::Context, Item::Cache, Item::Cost]
            .into_iter()
            .filter_map(|item| Some((item, self.value(values, item)?)))
            .collect();
        // A column after the right ones, unless something comes after them.
        let margin = usize::from(end.is_empty());
        let right_width = |right: &[(Item, &Vec<Span<'static>>)]| {
            right
                .iter()
                .map(|(_, spans)| spans_width(spans))
                .sum::<usize>()
                + GAP.len() * right.len().saturating_sub(1)
        };
        for gone in [Item::Cache, Item::Cost, Item::Context] {
            if right.is_empty()
                || spans_width(&left) + GAP.len() + right_width(&right) + margin <= room
            {
                break;
            }
            right.retain(|(item, _)| *item != gone);
        }
        let mut row = left;
        if right.is_empty() {
            // The sandbox state shows in full or not at all: a cut
            // `Sandbox o…` would not say which.
            if let Some(range) = sandbox_at
                && spans_width(&row) > room
            {
                row.drain(range);
            }
            row = truncate(row, room);
        } else {
            let pad = room - margin - spans_width(&row) - right_width(&right);
            row.push(Span::raw(" ".repeat(pad)));
            for (at, (_, spans)) in right.iter().enumerate() {
                if at > 0 {
                    row.push(Span::raw(GAP));
                }
                row.extend(spans.iter().cloned());
            }
        }
        row.extend(end);
        (truncate(row, width), through_mode)
    }

    /// The directory, then its branch and the lines changed; the session
    /// name and version on the right. The directory's room depends only on
    /// `width` and the branch: when it does not fit, directories in its
    /// middle go, as the welcome header shows it, down to `…/<last part>`.
    /// What comes after the branch shows only when it fits whole.
    fn second_row(&self, values: &Values, width: usize) -> Vec<Span<'static>> {
        let branch = self.value(values, Item::Branch);
        let mut row = vec![Span::raw(" ")];
        if let Some(spans) = self.value(values, Item::Directory) {
            let path = text(spans);
            let style = spans.first().map_or_else(Style::default, |span| span.style);
            let room = width
                .saturating_sub(1 + branch.map_or(0, |branch| NEAR.len() + spans_width(branch)));
            let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            let least = str_width(&path).min(str_width(last) + str_width("…/"));
            row.push(Span::styled(fit_path(&path, room.max(least)), style));
        }
        if let Some(branch) = branch {
            if row.len() > 1 {
                row.push(Span::raw(NEAR));
            }
            row.extend(branch.iter().cloned());
        }
        if let Some(diff) = self.value(values, Item::Diff) {
            let gap = if row.len() > 1 { NEAR } else { "" };
            if spans_width(&row) + gap.len() + spans_width(diff) <= width {
                row.push(Span::raw(gap));
                row.extend(diff.iter().cloned());
            }
        }
        let mut right: Vec<&Vec<Span<'static>>> = [Item::Session, Item::Version]
            .into_iter()
            .filter_map(|item| self.value(values, item))
            .collect();
        while !right.is_empty() {
            let used = right.iter().map(|spans| spans_width(spans)).sum::<usize>()
                + GAP.len() * right.len();
            if spans_width(&row) + used < width {
                row.push(Span::raw(
                    " ".repeat(width - 1 - spans_width(&row) - used + GAP.len()),
                ));
                for (at, spans) in right.iter().enumerate() {
                    if at > 0 {
                        row.push(Span::raw(GAP));
                    }
                    row.extend(spans.iter().cloned());
                }
                break;
            }
            right.pop();
        }
        truncate(row, width)
    }

    /// Reads what [`Bar::to_json`] wrote, or what was written when the
    /// order was the user's: only whether each item shows counts, the first
    /// time it is listed. Entries it does not know (such as `compact`, which
    /// the context bar replaced) are skipped, and items not listed are
    /// hidden, except `diff` in files saved before it came, which shows when
    /// `branch` is listed; `None` when the file is not even the shape it
    /// writes.
    fn from_json(value: &Value) -> Option<Self> {
        let Some(setting) = value.as_object()?.get("statusLine") else {
            return Some(Self::default());
        };
        let setting = setting.as_object()?;
        let use_script = match setting.get("useScript") {
            Some(value) => value.as_bool()?,
            None => true,
        };
        let Some(listed) = setting.get("items") else {
            return Some(Self {
                use_script,
                ..Self::default()
            });
        };
        let mut shown: [Option<bool>; 12] = [None; 12];
        for value in listed.as_array()? {
            if let Some((item, show)) = entry(value) {
                shown[item as usize].get_or_insert(show);
            }
        }
        if shown[Item::Branch as usize].is_some() {
            shown[Item::Diff as usize].get_or_insert(true);
        }
        Some(Self {
            shown: shown.map(|show| show.unwrap_or(false)),
            use_script,
        })
    }

    /// What `tui.json` keeps under `statusLine`: the items in the order of
    /// [`Item::ALL`], with the line 2 entry where the rows used to break.
    fn to_json(&self) -> Value {
        let mut items: Vec<Value> = Item::ALL
            .iter()
            .map(|&item| json!({ "name": item.name(), "show": self.shows(item) }))
            .collect();
        items.insert(Item::Directory as usize, json!({ "name": LINE2 }));
        json!({ "items": items, "useScript": self.use_script })
    }
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

/// The settings saved at `path`; the defaults when there are none. Also
/// whether the file is there but cannot be read.
pub(crate) fn load(path: &Path) -> (Bar, bool) {
    match crate::tui_json::read(path) {
        Ok(None) => (Bar::default(), false),
        Ok(Some(value)) => match Bar::from_json(&value) {
            Some(bar) => (bar, false),
            None => (Bar::default(), true),
        },
        Err(()) => (Bar::default(), true),
    }
}

/// Keeps `bar` in `tui.json` at `path`; the other settings there stay.
pub(crate) fn save(path: &Path, bar: &Bar) -> io::Result<()> {
    crate::tui_json::save_key(path, "statusLine", bar.to_json())
}

/// `{"name": …, "show": …}` of an item it knows; `None` for anything else,
/// such as the line 2 entry.
fn entry(value: &Value) -> Option<(Item, bool)> {
    let name = value.get("name")?.as_str()?;
    let item = Item::ALL.into_iter().find(|item| item.name() == name)?;
    let show = match value.get("show") {
        Some(show) => show.as_bool()?,
        None => true,
    };
    Some((item, show))
}
