//! The multi-line input box: text, a cursor and soft-wrapped rows.

use std::ops::Range;

use crate::text::{char_width, str_width};

#[derive(Debug, Default)]
pub(crate) struct InputBox {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
}

/// Soft-wrapped rows (byte ranges, without the newlines) and the cursor's
/// row and column.
pub(crate) struct InputLayout {
    pub rows: Vec<Range<usize>>,
    pub cursor: (usize, usize),
}

impl InputBox {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.take();
    }

    /// Inserts typed or pasted text. Line endings become `\n`, tabs become
    /// spaces, and other control characters are dropped.
    pub fn insert(&mut self, text: &str) {
        let mut clean = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    chars.next_if_eq(&'\n');
                    clean.push('\n');
                }
                '\n' => clean.push('\n'),
                '\t' => clean.push_str("    "),
                c if c.is_control() => {}
                c => clean.push(c),
            }
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
    }

    /// Moves one wrapped row up (`-1`) or down (`1`), keeping the column.
    pub fn vertical(&mut self, delta: isize, width: usize) {
        let layout = self.layout(width);
        let (row, column) = layout.cursor;
        let Some(target) = row.checked_add_signed(delta) else {
            return;
        };
        let Some(range) = layout.rows.get(target) else {
            return;
        };
        let mut offset = range.start;
        let mut used = 0;
        for c in self.text[range.clone()].chars() {
            let w = char_width(c);
            if used + w > column {
                break;
            }
            used += w;
            offset += c.len_utf8();
        }
        self.cursor = offset;
    }

    pub fn layout(&self, width: usize) -> InputLayout {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut start = 0;
        let mut used = 0;
        let mut cursor = None;
        for (i, c) in self.text.char_indices() {
            if c == '\n' {
                if i == self.cursor {
                    cursor = Some((rows.len(), used));
                }
                rows.push(start..i);
                start = i + 1;
                used = 0;
                continue;
            }
            let w = char_width(c);
            if used + w > width && used > 0 {
                rows.push(start..i);
                start = i;
                used = 0;
            }
            if i == self.cursor {
                cursor = Some((rows.len(), used));
            }
            used += w;
        }
        if self.cursor == self.text.len() {
            // A full last row leaves no column for the cursor; it starts the next row.
            if used >= width {
                rows.push(start..self.text.len());
                start = self.text.len();
            }
            cursor = Some((rows.len(), str_width(&self.text[start..])));
        }
        rows.push(start..self.text.len());
        InputLayout {
            rows,
            cursor: cursor.unwrap_or((0, 0)),
        }
    }
}
