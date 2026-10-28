use canter_protocol::{BtwEvent, UserInput};
use ratatui::text::Span;

use crate::{
    cells, markdown,
    text::{LogicalLine, Row, sanitize, wrap_rows},
    theme,
};

pub(crate) struct Answer {
    pub handoff: bool,
    question: String,
    text: String,
    status: Option<String>,
    notices: Vec<String>,
    pub done: bool,
}
impl Answer {
    pub fn new(question: String) -> Self {
        Self {
            handoff: false,
            question,
            text: String::new(),
            status: None,
            notices: Vec::new(),
            done: false,
        }
    }

    pub fn apply(&mut self, event: BtwEvent) {
        match event {
            BtwEvent::Replace { text, interrupted } => {
                if interrupted && self.text != text {
                    let partial = self.text.strip_prefix(&text).unwrap_or(&self.text);
                    self.notices.push(format!("{partial}\n[Interrupted]"));
                }
                self.text = text;
                self.status = None;
            }
            BtwEvent::Retrying { message } => {
                self.notices.push(message.clone());
                if self.handoff {
                    self.status = Some(message);
                }
            }
            BtwEvent::Delta { delta } => {
                self.text.push_str(&delta);
                if self.handoff {
                    self.status = None;
                }
            }
            BtwEvent::Completed => {
                self.done = true;
                self.status = None;
                if self.handoff && self.text.trim().is_empty() {
                    self.status = Some("Error: the handoff note is empty".into());
                }
            }
            BtwEvent::Cancelled => {
                self.done = true;
                self.status = Some("Stopped".into());
            }
            BtwEvent::Error { message } => {
                self.done = true;
                self.status = Some(format!("Error: {}", sanitize(&message)));
            }
            BtwEvent::Unknown => {}
        }
    }

    pub fn handoff_note(&self) -> Option<&str> {
        (self.handoff && self.done && self.status.is_none()).then_some(self.text.as_str())
    }

    pub fn rows(&self, width: u16) -> Vec<Row> {
        if self.handoff {
            let status = self.status.as_deref().unwrap_or(if self.done {
                "Handoff note ready"
            } else {
                "Writing the handoff note… (Esc to cancel)"
            });
            return wrap_rows(
                &LogicalLine::new(
                    vec![Span::raw("  handoff  ")],
                    vec![Span::raw(status.to_owned())],
                ),
                width,
            );
        }
        let mut rows = cells::prompt(
            &[UserInput::Text {
                text: format!("/btw {}", self.question),
            }],
            None,
            width,
        );
        let shown = self
            .notices
            .iter()
            .chain(std::iter::once(&self.text))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut lines = markdown::render(&sanitize(&shown), width.saturating_sub(7));
        if let Some(status) = &self.status {
            lines.push(LogicalLine::new(
                Vec::new(),
                vec![Span::raw(status.clone())],
            ));
        } else if self.text.is_empty() {
            lines.push(LogicalLine::new(
                Vec::new(),
                vec![Span::raw(if self.done {
                    "No text reply"
                } else {
                    "Waiting for an answer…"
                })],
            ));
        }
        for (index, line) in lines.into_iter().enumerate() {
            let line = line.prefixed(
                Span::raw(if index == 0 { "  btw  " } else { "       " }),
                Span::raw("       "),
            );
            rows.extend(wrap_rows(&line, width));
        }
        for row in &mut rows {
            for span in &mut row.line.spans {
                span.style = span.style.patch(theme::aside());
            }
        }
        rows
    }
}
