//! Markdown to terminal lines: headings, lists, quotes, code blocks, inline
//! code, emphasis and links.
//!
//! Streaming renders only complete source lines, so the rendering of a prefix
//! must stay a prefix of the final rendering. That is why soft breaks keep the
//! source line breaks and why paragraphs inside list items get no blank lines
//! (a list turning "loose" later would otherwise change rows already written).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::text::{LogicalLine, str_width};
use crate::theme;

pub(crate) fn render(source: &str) -> Vec<LogicalLine> {
    let mut renderer = Renderer::default();
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(source, options) {
        renderer.event(event);
    }
    renderer.flush();
    renderer.lines
}

enum Nest {
    Quote,
    Item(usize),
}

#[derive(Default)]
struct Renderer {
    lines: Vec<LogicalLine>,
    current: Option<LogicalLine>,
    styles: Vec<Style>,
    nest: Vec<Nest>,
    /// Next number of each open list; `None` for bullet lists.
    lists: Vec<Option<u64>>,
    /// Marker waiting for the first line of the innermost list item.
    marker: Option<String>,
    code_block: bool,
    blank_pending: bool,
    /// Destination and text so far of each open link.
    links: Vec<(String, String)>,
}

impl Renderer {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.code_block {
                    self.push_lines(&text, theme::accent());
                } else {
                    for (_, link_text) in &mut self.links {
                        link_text.push_str(&text);
                    }
                    self.push(&text, self.style());
                }
            }
            Event::Code(text) => self.push(&text, self.style().patch(theme::accent())),
            Event::Html(text) | Event::InlineHtml(text) => self.push_lines(&text, self.style()),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push(&text, self.style()),
            Event::FootnoteReference(name) => self.push(&format!("[^{name}]"), self.style()),
            Event::SoftBreak | Event::HardBreak => self.flush(),
            Event::Rule => {
                self.block_start();
                self.push("────────", theme::secondary());
                self.block_end();
            }
            Event::TaskListMarker(done) => {
                self.push(if done { "[x] " } else { "[ ] " }, self.style());
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.block_start(),
            Tag::Heading { level, .. } => {
                self.block_start();
                let style = Style::new().bold();
                self.styles.push(if level == HeadingLevel::H1 {
                    style.underlined()
                } else {
                    style
                });
            }
            Tag::BlockQuote(_) => {
                self.block_start();
                self.nest.push(Nest::Quote);
            }
            Tag::CodeBlock(_) => {
                self.block_start();
                self.code_block = true;
            }
            Tag::List(start) => {
                if self.in_item() {
                    self.flush();
                } else {
                    self.block_start();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => "• ".to_owned(),
                };
                self.nest.push(Nest::Item(str_width(&marker)));
                self.marker = Some(marker);
            }
            Tag::Emphasis => self.styles.push(Style::new().italic()),
            Tag::Strong => self.styles.push(Style::new().bold()),
            Tag::Strikethrough => self.styles.push(Style::new().crossed_out()),
            Tag::Link { dest_url, .. } => {
                self.styles.push(Style::new().underlined());
                self.links.push((dest_url.into_string(), String::new()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.block_end(),
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.block_end();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.nest.pop();
                self.block_end();
            }
            TagEnd::CodeBlock => {
                self.code_block = false;
                self.block_end();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if !self.in_item() {
                    self.block_end();
                }
            }
            TagEnd::Item => {
                if self.marker.is_some() {
                    self.open_line();
                }
                self.flush();
                self.nest.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some((url, text)) = self.links.pop()
                    && !url.is_empty()
                    && url != text
                {
                    self.push(&format!(" ({url})"), theme::secondary());
                }
            }
            _ => {}
        }
    }

    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |style, next| style.patch(*next))
    }

    fn in_item(&self) -> bool {
        self.nest.iter().any(|nest| matches!(nest, Nest::Item(_)))
    }

    fn block_start(&mut self) {
        self.flush();
        if self.blank_pending && !self.lines.is_empty() && !self.in_item() {
            self.lines.push(LogicalLine::default());
        }
        self.blank_pending = false;
    }

    fn block_end(&mut self) {
        self.flush();
        self.blank_pending = true;
    }

    fn open_line(&mut self) {
        if self.current.is_some() {
            return;
        }
        let innermost_item = self
            .nest
            .iter()
            .rposition(|nest| matches!(nest, Nest::Item(_)));
        let mut indent = Vec::new();
        let mut hang = Vec::new();
        for (i, nest) in self.nest.iter().enumerate() {
            match nest {
                Nest::Quote => {
                    indent.push(Span::styled("▌ ", theme::secondary()));
                    hang.push(Span::styled("▌ ", theme::secondary()));
                }
                Nest::Item(width) => {
                    let blank = " ".repeat(*width);
                    let marker = if Some(i) == innermost_item {
                        self.marker.take()
                    } else {
                        None
                    };
                    indent.push(Span::raw(marker.unwrap_or_else(|| blank.clone())));
                    hang.push(Span::raw(blank));
                }
            }
        }
        if self.code_block {
            indent.push(Span::raw("  "));
            hang.push(Span::raw("  "));
        }
        self.current = Some(LogicalLine {
            indent,
            hang,
            body: Vec::new(),
        });
    }

    fn push(&mut self, text: &str, style: Style) {
        self.open_line();
        if let Some(line) = &mut self.current {
            line.body.push(Span::styled(text.to_owned(), style));
        }
    }

    /// Text that may hold newlines, such as a code block: each newline ends a row.
    fn push_lines(&mut self, text: &str, style: Style) {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.open_line();
                self.flush();
            }
            if !part.is_empty() {
                self.push(part, style);
            }
        }
    }

    fn flush(&mut self) {
        if let Some(line) = self.current.take() {
            self.lines.push(line);
        }
    }
}
