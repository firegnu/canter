use canter_protocol::{BtwEvent, UserInput};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::{
    cells, markdown,
    text::{LogicalLine, sanitize, wrap},
};

pub(crate) struct Answer {
    question: String,
    text: String,
    status: Option<String>,
    pub done: bool,
}
impl Answer {
    pub fn new(question: String) -> Self {
        Self {
            question,
            text: String::new(),
            status: None,
            done: false,
        }
    }

    pub fn apply(&mut self, event: BtwEvent) {
        match event {
            BtwEvent::Delta { delta } => self.text.push_str(&delta),
            BtwEvent::Completed => self.done = true,
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

    pub fn rows(&self, width: u16) -> Vec<Line<'static>> {
        let mut rows = cells::prompt(
            &[UserInput::Text {
                text: format!("/btw {}", self.question),
            }],
            None,
            width,
        );
        let mut lines = markdown::render(&sanitize(&self.text));
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
            rows.extend(wrap(&line, width));
        }
        for row in &mut rows {
            for span in &mut row.spans {
                span.style = span
                    .style
                    .patch(Style::new().dark_gray().add_modifier(Modifier::DIM));
            }
        }
        rows
    }
}
