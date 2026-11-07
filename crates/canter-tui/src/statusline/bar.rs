//! What the rows at the bottom show when no status command draws them
//! (P2-245): which items, as `/statusline` sets it, kept in `tui.json`
//! beside `config.json`, which the terminal never writes; and where each
//! goes, which is fixed. All on the left between ` · `: the first row has
//! the model, thinking level, permission mode, sandbox and cost; the second
//! context, cache, speed, version and session name; the third the
//! directory, branch and lines changed alone, so numbers changing above
//! never move it.

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

    /// As `/statusline` lists them: as the rows show them, left to right.
    pub const LISTED: [Item; 12] = [
        Item::Model,
        Item::Thinking,
        Item::Permission,
        Item::Cost,
        Item::Context,
        Item::Cache,
        Item::Tps,
        Item::Version,
        Item::Session,
        Item::Directory,
        Item::Branch,
        Item::Diff,
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

/// Before a dangerous setting, `Allow all` or `Sandbox off`, in its red
/// (P2-246).
pub(crate) const DANGER: &str = "▲ ";
const SANDBOX_ON: &str = "Sandbox on";
const SANDBOX_OFF: &str = "▲ Sandbox off";
/// Between the items of a row.
const DOT: &str = " · ";

/// What the permission item adds after the mode when the server says whether
/// the sandbox is on: ` · Sandbox on`, or `▲ Sandbox off` in red as
/// `▲ Allow all`.
pub(crate) fn sandbox(enabled: bool) -> [Span<'static>; 2] {
    [
        Span::styled(DOT, theme::secondary()),
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

/// The rows drawn, each no wider than `width`: the first, with `end` (such
/// as a status command's error) at its end; the second and the directory's
/// when anything on them fits. Also whether the first shows all of the
/// permission mode.
pub(crate) struct Rows {
    pub rows: Vec<Vec<Span<'static>>>,
    pub mode_in_full: bool,
}

impl Bar {
    pub fn shows(&self, item: Item) -> bool {
        self.shown[item as usize]
    }

    fn value<'a>(&self, values: &'a Values, item: Item) -> Option<&'a Vec<Span<'static>>> {
        values[item as usize].as_ref().filter(|_| self.shows(item))
    }

    /// Where each item goes is fixed (P2-245): model, thinking level,
    /// permission mode, sandbox and cost on the first row; context, cache,
    /// speed, version and session name on the second; the directory with
    /// its branch and the lines changed on the third.
    pub fn rows(&self, values: &Values, width: usize, end: Vec<Span<'static>>) -> Rows {
        let (first, mode_in_full) = self.first_row(values, width, end);
        let mut rows = vec![first];
        let usage: Vec<&[Span<'static>]> = [
            Item::Context,
            Item::Cache,
            Item::Tps,
            Item::Version,
            Item::Session,
        ]
        .into_iter()
        .filter_map(|item| self.value(values, item).map(Vec::as_slice))
        .collect();
        let (usage, _) = fitting(&usage, width);
        rows.extend(usage);
        rows.extend(self.directory_row(values, width));
        Rows { rows, mode_in_full }
    }

    /// The items that fit, from the first; the first cut at its end when
    /// not even it fits, so the row is never empty while something shows.
    fn first_row(
        &self,
        values: &Values,
        width: usize,
        end: Vec<Span<'static>>,
    ) -> (Vec<Span<'static>>, bool) {
        let mut items: Vec<&[Span<'static>]> = Vec::new();
        for item in [Item::Model, Item::Thinking] {
            items.extend(self.value(values, item).map(Vec::as_slice));
        }
        let mut mode_at = None;
        if let Some(value) = self.value(values, Item::Permission) {
            // The sandbox state goes on its own: in full or not at all.
            let sandbox = value.len() > 2
                && value
                    .last()
                    .is_some_and(|span| [SANDBOX_ON, SANDBOX_OFF].contains(&span.content.as_ref()));
            mode_at = Some(items.len());
            if sandbox {
                let (mode, sandbox) = value.split_at(value.len() - 2);
                items.push(mode);
                items.push(&sandbox[1..]);
            } else {
                items.push(value);
            }
        }
        items.extend(self.value(values, Item::Cost).map(Vec::as_slice));
        let room = width.saturating_sub(spans_width(&end));
        let (row, kept) = fitting(&items, room);
        let mut row = match (row, items.first()) {
            (Some(row), _) => row,
            (None, Some(first)) => {
                let mut row = vec![Span::raw(" ")];
                row.extend(first.iter().cloned());
                truncate(row, room)
            }
            (None, None) => vec![Span::raw(" ")],
        };
        row.extend(end);
        (
            truncate(row, width),
            mode_at.is_some_and(|mode| mode < kept),
        )
    }

    /// The directory, then its branch, then ` · ` and the lines changed.
    /// The directory's room depends only on `width` and the branch: when it
    /// does not fit, directories in its middle go, as the welcome header
    /// shows it, down to `…/<last part>`. The lines changed show only when
    /// they fit whole. `None` when nothing on it shows.
    fn directory_row(&self, values: &Values, width: usize) -> Option<Vec<Span<'static>>> {
        let branch = self.value(values, Item::Branch);
        let mut row = vec![Span::raw(" ")];
        if let Some(spans) = self.value(values, Item::Directory) {
            let path = text(spans);
            let style = spans.first().map_or_else(Style::default, |span| span.style);
            let room = width.saturating_sub(1 + branch.map_or(0, |branch| 1 + spans_width(branch)));
            let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            let least = str_width(&path).min(str_width(last) + str_width("…/"));
            row.push(Span::styled(fit_path(&path, room.max(least)), style));
        }
        if let Some(branch) = branch {
            if row.len() > 1 {
                row.push(Span::raw(" "));
            }
            row.extend(branch.iter().cloned());
        }
        if let Some(diff) = self.value(values, Item::Diff) {
            let gap = if row.len() > 1 { DOT } else { "" };
            if spans_width(&row) + str_width(gap) + spans_width(diff) <= width {
                row.push(Span::styled(gap, theme::secondary()));
                row.extend(diff.iter().cloned());
            }
        }
        (row.len() > 1).then(|| truncate(row, width))
    }
}

/// After a space, as many of `items` as fit in `width` from the first,
/// between ` · `, so the ones that do not go whole from the end (P2-245);
/// `None` when not even the first fits. Also how many fit.
fn fitting(items: &[&[Span<'static>]], width: usize) -> (Option<Vec<Span<'static>>>, usize) {
    let mut row = vec![Span::raw(" ")];
    let mut kept = 0;
    for item in items {
        let gap = if kept > 0 { DOT } else { "" };
        if spans_width(&row) + str_width(gap) + spans_width(item) > width {
            break;
        }
        if kept > 0 {
            row.push(Span::styled(DOT, theme::secondary()));
        }
        row.extend(item.iter().cloned());
        kept += 1;
    }
    ((kept > 0).then_some(row), kept)
}

impl Bar {
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
