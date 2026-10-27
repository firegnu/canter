//! `/theme`: a list above the input box, like a choice such as `/mode`
//! alone, to pick the colours of code and diffs. Moving through it draws
//! the code on screen in the theme selected at once; a preview of a diff
//! and a code block sits beside the list in a wide window, under it in a
//! narrow one.
//!
//! Follows Codex `rust-v0.163.0` (`codex-rs/tui/src/theme_picker.rs`, the
//! side-by-side widths of `bottom_pane/list_selection_view.rs`); no code is
//! taken from it.

use std::path::Path;

use canter_protocol::PatchChangeKind;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::diff::Diff;
use crate::highlight::{self, Choice, CodeTheme};
use crate::statusline::setup::window;
use crate::text::{spans_width, truncate};
use crate::theme;

const TITLE: &str = " Theme · code and diff colours";
const KEYS: &str = " ↑/↓ preview · Enter save · Esc cancel";
const SIXTEEN: &str = " This terminal shows 16 colours; only terminal applies";
/// The most rows the list takes.
const LIST_ROWS: usize = 8;
/// Side by side, as Codex: the preview takes half the width less the gap,
/// at least this much, and leaves the list at least [`MIN_LIST`].
const MIN_PREVIEW: usize = 44;
const MIN_LIST: usize = 40;
const GAP: usize = 2;

const PREVIEW_DIFF: &str = "@@ -12,3 +12,3 @@
 fn greet(name: &str) -> String {
-    format!(\"Hello, {}!\", name)
+    format!(\"Hello, {name}!\")
 }
";
const PREVIEW_CODE: &str = "```rust
// Greet everyone
let names = [\"Ada\", \"Linus\"];
let count = names.len() + 1;
```
";

pub(crate) struct ThemePicker {
    choices: Vec<Choice>,
    selected: usize,
    /// The row of the theme in use when it opened.
    current: usize,
    /// What code was drawn in when it opened; Esc puts it back.
    before: CodeTheme,
}

impl ThemePicker {
    /// Lists the themes, those in `dir` among them, with `current` (the name
    /// of the theme in use) selected.
    pub fn new(current: &str, dir: Option<&Path>) -> Self {
        let choices = highlight::choices(dir);
        let current = choices
            .iter()
            .position(|choice| choice.name == current)
            .unwrap_or(0);
        Self {
            choices,
            selected: current,
            current,
            before: highlight::code_theme(),
        }
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.preview();
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.choices.len() - 1);
        self.preview();
    }

    /// Draws code in the theme selected; a file that cannot be read changes
    /// nothing.
    fn preview(&self) {
        if let Some(theme) = &self.choices[self.selected].theme {
            highlight::set_code_theme(theme.clone());
        }
    }

    /// The name and theme selected, to save; `None` for a file that cannot
    /// be read.
    pub fn chosen(&self) -> Option<(String, CodeTheme)> {
        let choice = &self.choices[self.selected];
        Some((choice.name.clone(), choice.theme.clone()?))
    }

    /// Draws code as before it opened.
    pub fn cancel(self) {
        highlight::set_code_theme(self.before);
    }

    /// At most `max` rows: the title (and with 16 colours a line saying only
    /// `terminal` applies), the list, the preview and the keys. The list
    /// shows at most [`LIST_ROWS`] around the row selected, with how many
    /// more there are above and below; with fewer than three rows left for
    /// it, the preview folds away; nothing when not even one fits.
    pub fn rows(&self, width: u16, max: usize) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let cut = |spans: Vec<Span<'static>>, width| Line::from(truncate(spans, width));
        let mut head = vec![cut(vec![Span::styled(TITLE, Style::new().bold())], width)];
        if !highlight::themes_apply() {
            head.push(cut(vec![Span::styled(SIXTEEN, theme::warning())], width));
        }
        let keys = cut(vec![Span::styled(KEYS, theme::secondary())], width);
        let fixed = head.len() + 1;
        let side = width.saturating_sub(GAP) / 2;
        let list_width = width.saturating_sub(GAP + side);
        let wide = side >= MIN_PREVIEW && list_width >= MIN_LIST;
        let preview = if wide {
            preview(side, true)
        } else {
            let mut rows = vec![Line::default()];
            rows.extend(preview(width, false));
            rows
        };
        let mut room = max.saturating_sub(fixed).min(LIST_ROWS);
        if !wide {
            room = max.saturating_sub(fixed + preview.len()).min(LIST_ROWS);
        }
        let with_preview = room >= 3;
        if !with_preview {
            room = max.saturating_sub(fixed).min(LIST_ROWS);
        }
        if room == 0 {
            return Vec::new();
        }
        let list = window(self.entries(), self.selected, room);
        let mut rows = head;
        if wide && with_preview {
            let blank = Line::default();
            for (index, entry) in list.into_iter().enumerate() {
                let mut spans = truncate(entry, list_width);
                let used = spans_width(&spans);
                spans.push(Span::raw(" ".repeat(list_width - used)));
                spans.push(Span::styled("│", theme::secondary()));
                spans.push(Span::raw(" "));
                let line = preview.get(index).unwrap_or(&blank);
                spans.extend(truncate(line.spans.clone(), side));
                rows.push(Line::from(spans));
            }
        } else {
            rows.extend(list.into_iter().map(|entry| cut(entry, width)));
            if with_preview {
                rows.extend(preview);
            }
        }
        rows.push(keys);
        rows
    }

    /// A row for each theme, not cut to any width: its name, `auto` with
    /// what it stands for, then whether it is a file, cannot be read, or is
    /// the one in use.
    fn entries(&self) -> Vec<Vec<Span<'static>>> {
        let label = |choice: &Choice| match choice.name.as_str() {
            "auto" => format!("auto · {}", highlight::auto_name()),
            name => name.to_owned(),
        };
        let pad = self
            .choices
            .iter()
            .map(|choice| crate::text::str_width(&label(choice)))
            .max()
            .unwrap_or(0)
            + 2;
        self.choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let chosen = index == self.selected;
                let label = label(choice);
                let mut tags = Vec::new();
                if choice.custom {
                    tags.push(Span::styled("(custom)", theme::secondary()));
                }
                if choice.theme.is_none() {
                    tags.push(Span::styled("(unreadable)", theme::failure()));
                }
                if index == self.current {
                    tags.push(Span::styled("(current)", theme::secondary()));
                }
                let text = if tags.is_empty() {
                    label
                } else {
                    let gap = pad.saturating_sub(crate::text::str_width(&label));
                    format!("{label}{}", " ".repeat(gap))
                };
                let mut spans = if chosen {
                    vec![
                        Span::styled(" › ", theme::accent().bold()),
                        Span::styled(text, Style::new().bold()),
                    ]
                } else {
                    vec![Span::raw("   "), Span::raw(text)]
                };
                for (at, tag) in tags.into_iter().enumerate() {
                    if at > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(tag);
                }
                spans
            })
            .collect()
    }
}

/// A diff drawn as the conversation draws one, `width` wide; with `code`, a
/// code block under it too.
fn preview(width: usize, code: bool) -> Vec<Line<'static>> {
    let width_u16 = u16::try_from(width).unwrap_or(u16::MAX);
    let diff = Diff::parse(
        PREVIEW_DIFF,
        &PatchChangeKind::Update { move_path: None },
        "greet.rs",
    );
    let mut rows: Vec<Line<'static>> = diff
        .rows(" ", width_u16, None)
        .into_iter()
        .map(|row| row.line)
        .collect();
    if code {
        rows.push(Line::default());
        for line in crate::markdown::render(PREVIEW_CODE, width_u16.saturating_sub(2)) {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(line.indent);
            spans.extend(line.body);
            rows.push(Line::from(truncate(spans, width)));
        }
    }
    rows
}
