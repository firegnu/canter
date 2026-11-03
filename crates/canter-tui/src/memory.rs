use crate::{
    report::{Part, Report, clean},
    theme,
};
use canter_protocol::{MemoryListResponse, MemoryScope};
use ratatui::{style::Style, text::Span};

pub(crate) struct View {
    pub list: MemoryListResponse,
    pub selected: usize,
    pub confirming: bool,
}

impl View {
    pub fn report(&self) -> Report {
        let mut body = Vec::new();
        let mut focus = None;
        if self.confirming {
            if let Some(entry) = self.list.data.get(self.selected) {
                body.push(Part::Wrap {
                    indent: 1,
                    text: format!("Delete memory \"{}\"?", clean(&entry.name)),
                    style: Style::default(),
                    rows: usize::MAX,
                });
            }
        } else if self.list.data.is_empty() {
            body.push(Part::Line(vec![Span::raw(" No memories yet.")]));
        } else {
            for (index, entry) in self.list.data.iter().enumerate() {
                let selected = self.selected == index;
                if selected {
                    focus = Some(body.len());
                }
                let scope = match entry.scope {
                    MemoryScope::Global => "Global",
                    MemoryScope::Project => "Project",
                };
                body.push(Part::Line(vec![Span::styled(
                    format!(
                        " {} {} · {scope}",
                        if selected { "›" } else { " " },
                        clean(&entry.name)
                    ),
                    if selected {
                        theme::accent().bold()
                    } else {
                        Style::default()
                    },
                )]));
                body.push(Part::Wrap {
                    indent: 3,
                    text: if entry.description.is_empty() {
                        "Invalid memory header; open to edit.".into()
                    } else {
                        clean(&entry.description)
                    },
                    style: theme::secondary(),
                    rows: 2,
                });
            }
        }
        Report::new(crate::report::title("Memory", ""), body, Vec::new())
            .keys(if self.confirming {
                " y confirm · n/Esc cancel"
            } else if self.list.data.is_empty() {
                " Esc close"
            } else {
                " ↑↓ select · Enter edit · d delete · Esc close"
            })
            .focus(focus)
    }
}

pub(crate) fn error(error: &str) -> Report {
    Report::new(
        crate::report::title("Memory", ""),
        vec![Part::Wrap {
            indent: 1,
            text: clean(error),
            style: theme::failure(),
            rows: usize::MAX,
        }],
        Vec::new(),
    )
}
