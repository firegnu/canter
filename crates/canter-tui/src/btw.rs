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
    pub done: bool,
}
impl Answer {
    pub fn new(question: String) -> Self {
        Self {
            handoff: false,
            question,
            text: String::new(),
            status: None,
            done: false,
        }
    }

    pub fn apply(&mut self, event: BtwEvent) {
        match event {
            BtwEvent::Delta { delta } => self.text.push_str(&delta),
            BtwEvent::Completed => {
                self.done = true;
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
        let mut lines = markdown::render(&sanitize(&self.text), width.saturating_sub(7));
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
