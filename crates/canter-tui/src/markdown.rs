//! Markdown to terminal lines: headings, lists, quotes, code blocks, inline
//! code, emphasis, links and tables.
//!
//! Streaming renders only complete source lines, so the rendering of a prefix
//! must stay a prefix of the final rendering. That is why soft breaks keep the
//! source line breaks and why paragraphs inside list items get no blank lines
//! (a list turning "loose" later would otherwise change rows already written).
//! Tables are the exception: each one is laid out whole for the width, so its
//! rows change as more of it arrives. That is fine now that the part still in
//! progress is redrawn every frame.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::highlight;
use crate::text::{LogicalLine, char_width, spans_width, str_width, wrap};
use crate::theme;

/// `width` is the columns the lines may take, indents included; only tables
/// need it, other lines are wrapped later.
pub(crate) fn render(source: &str, width: u16) -> Vec<LogicalLine> {
    let mut renderer = Renderer {
        width: usize::from(width),
        ..Renderer::default()
    };
    for event in Parser::new_ext(source, options()) {
        renderer.event(event);
    }
    renderer.flush();
    renderer.lines
}

fn options() -> Options {
    Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES
}

/// The text of each code block in `source`, without its fences or the
/// newline that ends its last line.
pub(crate) fn code_blocks(source: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<String> = None;
    for event in Parser::new_ext(source, options()) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => open = Some(String::new()),
            Event::Text(text) => {
                if let Some(code) = &mut open {
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(mut code) = open.take() {
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    blocks.push(code);
                }
            }
            _ => {}
        }
    }
    blocks
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
    /// The open code block's language, as its fence names it, and its text so far.
    code: Option<(String, String)>,
    blank_pending: bool,
    /// Destination and text so far of each open link.
    links: Vec<(String, String)>,
    width: usize,
    table: Option<Table>,
}

/// A table as it is read: its cells, laid out once it ends.
#[derive(Default)]
struct Table {
    alignments: Vec<Alignment>,
    head: Vec<Vec<Span<'static>>>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    /// The cell being read, if any.
    cell: Option<Vec<Span<'static>>>,
    in_head: bool,
}

impl Renderer {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&text);
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
            Tag::CodeBlock(kind) => {
                self.block_start();
                self.code_block = true;
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(start) => {
                if self.in_item() {
                    self.flush();
                } else {
                    self.block_start();
                }
                self.lists.push(start);
            }
            Tag::Table(alignments) => {
                self.block_start();
                self.table = Some(Table {
                    alignments,
                    ..Table::default()
                });
            }
            Tag::TableHead => {
                self.styles.push(Style::new().bold());
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell = Some(Vec::new());
                }
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
                if let Some((language, code)) = self.code.take() {
                    self.draw_code(&language, &code);
                }
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
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.draw_table(table);
                }
                self.block_end();
            }
            TagEnd::TableHead => {
                self.styles.pop();
                if let Some(table) = &mut self.table {
                    table.in_head = false;
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = table.cell.take().unwrap_or_default();
                    if table.in_head {
                        table.head.push(cell);
                    } else if let Some(row) = table.rows.last_mut() {
                        row.push(cell);
                    }
                }
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
        // A copy keeps the list markers and the list nesting around them,
        // but not quote bars or a code block's indent.
        let marker = if self.code_block {
            0
        } else {
            self.nest
                .iter()
                .rev()
                .map_while(|nest| match nest {
                    Nest::Item(width) => Some(*width),
                    Nest::Quote => None,
                })
                .sum()
        };
        if self.code_block {
            indent.push(Span::raw("  "));
            hang.push(Span::raw("  "));
        }
        self.current = Some(LogicalLine {
            indent,
            hang,
            body: Vec::new(),
            marker,
        });
    }

    fn push(&mut self, text: &str, style: Style) {
        if let Some(table) = &mut self.table {
            if let Some(cell) = &mut table.cell {
                cell.push(Span::styled(text.to_owned(), style));
            }
            return;
        }
        self.open_line();
        if let Some(line) = &mut self.current {
            line.body.push(Span::styled(text.to_owned(), style));
        }
    }

    /// Text that may hold newlines, such as a code block: each newline ends a
    /// row. Inside a table cell, a newline is a space.
    fn push_lines(&mut self, text: &str, style: Style) {
        if self.table.is_some() {
            self.push(&text.replace('\n', " "), style);
            return;
        }
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

    /// One finished row: the current indent, then `body`.
    fn push_row(&mut self, body: Vec<Span<'static>>) {
        self.open_line();
        if let Some(line) = &mut self.current {
            line.body = body;
        }
        self.flush();
    }

    /// One row per line, coloured by its language when it can be, else all
    /// in the accent colour.
    fn draw_code(&mut self, language: &str, code: &str) {
        let mut lines: Vec<&str> = code.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        let rows = highlight::code_block(language, &lines).unwrap_or_else(|| {
            lines
                .iter()
                .map(|line| match *line {
                    "" => Vec::new(),
                    line => vec![Span::styled(line.to_owned(), theme::accent())],
                })
                .collect()
        });
        for body in rows {
            self.push_row(body);
        }
    }

    /// Columns the open quotes and list items take before the text.
    fn nest_width(&self) -> usize {
        self.nest
            .iter()
            .map(|nest| match nest {
                Nest::Quote => 2,
                Nest::Item(width) => *width,
            })
            .sum()
    }

    /// A box-line table when it fits the width, wrapping inside cells if it
    /// must; otherwise one block of `header: value` rows per table row.
    fn draw_table(&mut self, table: Table) {
        let columns = table.alignments.len();
        let fit = |cells: Vec<Vec<Span<'static>>>| -> Vec<Vec<Span<'static>>> {
            let mut cells: Vec<_> = cells.into_iter().take(columns).collect();
            cells.resize(columns, Vec::new());
            cells
        };
        let head = fit(table.head);
        let rows: Vec<_> = table.rows.into_iter().map(fit).collect();
        let all = || std::iter::once(&head).chain(&rows);
        let natural: Vec<usize> = (0..columns)
            .map(|c| {
                all()
                    .map(|row| spans_width(&row[c]))
                    .max()
                    .unwrap_or(0)
                    .max(1)
            })
            .collect();
        let words: Vec<usize> = (0..columns)
            .map(|c| all().map(|row| longest_word(&row[c])).max().unwrap_or(0))
            .collect();
        let avail = self.width.saturating_sub(self.nest_width());
        match column_widths(&natural, &words, avail) {
            Some(widths) => self.draw_grid(&table.alignments, &widths, &head, &rows),
            None => self.draw_records(&head, &rows),
        }
    }

    fn draw_grid(
        &mut self,
        alignments: &[Alignment],
        widths: &[usize],
        head: &[Vec<Span<'static>>],
        rows: &[Vec<Vec<Span<'static>>>],
    ) {
        let wrapped = |row: &[Vec<Span<'static>>]| -> Vec<Vec<Vec<Span<'static>>>> {
            row.iter()
                .zip(widths)
                .map(|(cell, width)| wrap_cell(cell, *width))
                .collect()
        };
        let head = wrapped(head);
        let rows: Vec<_> = rows.iter().map(|row| wrapped(row)).collect();
        let lines_between = rows.iter().any(|row| row.iter().any(|cell| cell.len() > 1));
        self.push_row(rule(widths, '┌', '┬', '┐'));
        self.push_cells(alignments, widths, &head);
        self.push_row(rule(widths, '├', '┼', '┤'));
        for (i, row) in rows.iter().enumerate() {
            if i > 0 && lines_between {
                self.push_row(rule(widths, '├', '┼', '┤'));
            }
            self.push_cells(alignments, widths, row);
        }
        self.push_row(rule(widths, '└', '┴', '┘'));
    }

    /// The rows of one table row, each cell's wrapped lines side by side.
    fn push_cells(
        &mut self,
        alignments: &[Alignment],
        widths: &[usize],
        cells: &[Vec<Vec<Span<'static>>>],
    ) {
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut body = Vec::new();
            for (c, cell) in cells.iter().enumerate() {
                let text = cell.get(line).cloned().unwrap_or_default();
                let gap = widths[c].saturating_sub(spans_width(&text));
                let left = match alignments[c] {
                    Alignment::Right => gap,
                    Alignment::Center => gap / 2,
                    Alignment::Left | Alignment::None => 0,
                };
                body.push(Span::styled(
                    if c == 0 { "│ " } else { " │ " },
                    theme::secondary(),
                ));
                body.push(Span::raw(" ".repeat(left)));
                body.extend(text);
                body.push(Span::raw(" ".repeat(gap - left)));
            }
            body.push(Span::styled(" │", theme::secondary()));
            self.push_row(body);
        }
    }

    fn draw_records(&mut self, head: &[Vec<Span<'static>>], rows: &[Vec<Vec<Span<'static>>>]) {
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                self.push_row(vec![Span::styled("────────", theme::secondary())]);
            }
            for (name, value) in head.iter().zip(row) {
                let mut body = name.clone();
                body.push(Span::raw(": "));
                body.extend(value.iter().cloned());
                self.push_row(body);
            }
        }
    }
}

/// The most a column keeps for its longest word when the table must shrink.
const MAX_WORD: usize = 16;
/// Below this many columns a cell on average, a table becomes records.
const MIN_CELL: usize = 6;

/// Column widths for a table whose columns want `natural` columns and whose
/// longest words are `words`, in `avail` columns including the box lines;
/// `None` when it should become records.
///
/// The natural widths when they fit. Otherwise each column keeps its longest
/// word (at most [`MAX_WORD`], less if even that does not fit) and the rest is
/// shared out by how much each column still lacks; the columns left over from
/// rounding go to the columns closest to their natural width, so that fewer
/// columns wrap.
fn column_widths(natural: &[usize], words: &[usize], avail: usize) -> Option<Vec<usize>> {
    let columns = natural.len();
    let lines = 3 * columns + 1;
    if natural.iter().sum::<usize>() + lines <= avail {
        return Some(natural.to_vec());
    }
    let room = avail.checked_sub(lines)?;
    if room < MIN_CELL * columns {
        return None;
    }
    let floor = |cap: usize| -> Vec<usize> {
        natural
            .iter()
            .zip(words)
            .map(|(&natural, &word)| natural.min(word).min(cap))
            .collect()
    };
    // Ends by MIN_CELL: then each column takes at most MIN_CELL, which fits.
    let mut cap = MAX_WORD;
    while floor(cap).iter().sum::<usize>() > room {
        cap -= 1;
    }
    let mut widths = floor(cap);
    let extra = room - widths.iter().sum::<usize>();
    let lacking: Vec<usize> = natural.iter().zip(&widths).map(|(n, w)| n - w).collect();
    let total: usize = lacking.iter().sum();
    for (width, lack) in widths.iter_mut().zip(&lacking) {
        *width += extra * lack / total;
    }
    let mut left = room - widths.iter().sum::<usize>();
    let mut order: Vec<usize> = (0..columns).collect();
    order.sort_by_key(|&c| natural[c] - widths[c]);
    for c in order {
        if left == 0 {
            break;
        }
        if widths[c] < natural[c] {
            widths[c] += 1;
            left -= 1;
        }
    }
    Some(widths)
}

/// The widest run that wrapping keeps together: wrapping breaks after spaces
/// and next to wide (CJK) characters.
fn longest_word(spans: &[Span]) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in spans.iter().flat_map(|span| span.content.chars()) {
        let width = char_width(c);
        if c == ' ' {
            run = 0;
        } else if width > 1 {
            run = 0;
            longest = longest.max(width);
        } else {
            run += width;
            longest = longest.max(run);
        }
    }
    longest
}

/// A cell's text wrapped to `width`, one entry per line.
fn wrap_cell(cell: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let line = LogicalLine {
        body: cell.to_vec(),
        ..LogicalLine::default()
    };
    wrap(&line, u16::try_from(width).unwrap_or(u16::MAX))
        .into_iter()
        .map(|line| line.spans)
        .collect()
}

/// A box line across the table: `left`, `─` over each column, `middle`
/// between columns, `right`.
fn rule(widths: &[usize], left: char, middle: char, right: char) -> Vec<Span<'static>> {
    let mut line = String::from(left);
    for (c, width) in widths.iter().enumerate() {
        if c > 0 {
            line.push(middle);
        }
        line.push_str(&"─".repeat(width + 2));
    }
    line.push(right);
    vec![Span::styled(line, theme::secondary())]
}
