//! What the two rows at the bottom show when no status command draws them,
//! as `/statusline` sets it: which items, in which order, and which go on
//! the second row. Kept in `tui.json`, beside `config.json`, which the
//! terminal never writes.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::text::Span;
use serde_json::{Value, json};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Model,
    Thinking,
    Permission,
    Cost,
    Directory,
    Branch,
    Context,
    Cache,
    Tps,
    Version,
    Session,
}

impl Item {
    /// In the order of [`Values`].
    pub const ALL: [Item; 11] = [
        Item::Model,
        Item::Thinking,
        Item::Permission,
        Item::Cost,
        Item::Directory,
        Item::Branch,
        Item::Context,
        Item::Cache,
        Item::Tps,
        Item::Version,
        Item::Session,
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
            Item::Context => "context",
            Item::Cache => "cache",
            Item::Tps => "tps",
            Item::Version => "version",
            Item::Session => "session",
        }
    }
}

/// The name `tui.json` keeps the line 2 entry under.
const LINE2: &str = "line2";

/// A row of `/statusline`'s list: an item, shown or not, or the line that
/// starts the second row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    Item(Item, bool),
    Line2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bar {
    /// Every item once, and the line 2 entry once.
    pub entries: Vec<Entry>,
    /// Whether the status command in `config.json`, when there is one,
    /// draws the rows instead.
    pub use_script: bool,
}

impl Default for Bar {
    /// The two rows as they always were; the newer items hidden.
    fn default() -> Self {
        let shown = |item| Entry::Item(item, true);
        let hidden = |item| Entry::Item(item, false);
        Self {
            entries: vec![
                shown(Item::Model),
                shown(Item::Thinking),
                shown(Item::Permission),
                shown(Item::Cost),
                Entry::Line2,
                shown(Item::Directory),
                shown(Item::Branch),
                shown(Item::Context),
                shown(Item::Cache),
                hidden(Item::Tps),
                hidden(Item::Version),
                hidden(Item::Session),
            ],
            use_script: true,
        }
    }
}

/// What each item says now, in the order of [`Item::ALL`]; `None` while it
/// has nothing to say, and it is left out.
pub(crate) type Values = [Option<Vec<Span<'static>>>; 11];

impl Bar {
    /// The items shown on each row, in order.
    fn rows(&self) -> [Vec<Item>; 2] {
        let mut rows = [Vec::new(), Vec::new()];
        let mut row = 0;
        for entry in &self.entries {
            match *entry {
                Entry::Line2 => row = 1,
                Entry::Item(item, true) => rows[row].push(item),
                Entry::Item(_, false) => {}
            }
        }
        rows
    }

    pub fn mode_on_first_row(&self) -> bool {
        self.rows()[0].contains(&Item::Permission)
    }

    /// The two rows, not cut to any width: after a space, the items with a
    /// value between ` · `, a branch right after the directory after a
    /// space. Also the first row's text up to the end of the permission
    /// mode, when it is there.
    pub fn draw(&self, values: &Values) -> ([Vec<Span<'static>>; 2], Option<String>) {
        let dim = theme::secondary();
        let mut through_mode = None;
        let mut rows = [Vec::new(), Vec::new()];
        for (index, (spans, items)) in rows.iter_mut().zip(self.rows()).enumerate() {
            let mut before = None;
            for item in items {
                let Some(value) = &values[item as usize] else {
                    continue;
                };
                let gap = match before {
                    None => " ",
                    Some(Item::Directory) if item == Item::Branch => " ",
                    Some(_) => " · ",
                };
                spans.push(Span::styled(gap, dim));
                spans.extend(value.iter().cloned());
                if index == 0 && item == Item::Permission {
                    through_mode = Some(spans.iter().map(|span| span.content.as_ref()).collect());
                }
                before = Some(item);
            }
        }
        (rows, through_mode)
    }

    /// Reads what [`Bar::to_json`] wrote. Entries it does not know are
    /// skipped and items not listed come hidden at the end; `None` when the
    /// file is not even the shape it writes.
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
        let mut entries: Vec<Entry> = Vec::new();
        for value in listed.as_array()? {
            let Some(entry) = entry(value) else {
                continue;
            };
            let known = entries.iter().any(|known| match (known, &entry) {
                (Entry::Item(known, _), Entry::Item(item, _)) => known == item,
                (known, entry) => known == entry,
            });
            if !known {
                entries.push(entry);
            }
        }
        if !entries.contains(&Entry::Line2) {
            entries.push(Entry::Line2);
        }
        for item in Item::ALL {
            if !entries
                .iter()
                .any(|entry| matches!(entry, Entry::Item(listed, _) if *listed == item))
            {
                entries.push(Entry::Item(item, false));
            }
        }
        Some(Self {
            entries,
            use_script,
        })
    }

    fn to_json(&self) -> Value {
        let items: Vec<Value> = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(item, show) => json!({ "name": item.name(), "show": show }),
                Entry::Line2 => json!({ "name": LINE2 }),
            })
            .collect();
        json!({ "statusLine": { "items": items, "useScript": self.use_script } })
    }
}

/// `{"name": …, "show": …}`, or the line 2 entry; `None` for anything else.
fn entry(value: &Value) -> Option<Entry> {
    let name = value.get("name")?.as_str()?;
    if name == LINE2 {
        return Some(Entry::Line2);
    }
    let item = Item::ALL.into_iter().find(|item| item.name() == name)?;
    let show = match value.get("show") {
        Some(show) => show.as_bool()?,
        None => true,
    };
    Some(Entry::Item(item, show))
}

/// `tui.json` in `CANTER_HOME`, or else in `~/.canter`, like `config.json`.
pub(crate) fn path(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    super::home(env).map(|home| home.join("tui.json"))
}

/// The settings saved at `path`; the defaults when there are none. Also
/// whether the file is there but cannot be read.
pub(crate) fn load(path: &Path) -> (Bar, bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (Bar::default(), false),
        Err(_) => return (Bar::default(), true),
    };
    match serde_json::from_str(&text)
        .ok()
        .and_then(|value| Bar::from_json(&value))
    {
        Some(bar) => (bar, false),
        None => (Bar::default(), true),
    }
}

/// Writes `bar` to `path`, readable only by the user: into a new file
/// beside it first, which then takes its name, so the file is never half
/// written.
pub(crate) fn save(path: &Path, bar: &Bar) -> io::Result<()> {
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let temp = dir.join(format!(
        ".tui.json.{}.{}.tmp",
        std::process::id(),
        SAVES.fetch_add(1, Ordering::Relaxed)
    ));
    let saved = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut text = serde_json::to_string_pretty(&bar.to_json())?;
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved
}
