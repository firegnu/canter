//! The multi-line input box: text, a cursor and soft-wrapped rows.

use std::ops::Range;

use crate::text::{char_width, str_width};

#[derive(Debug, Default, Clone)]
pub(crate) struct InputBox {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    images: Vec<(String, canter_protocol::UserInput)>,
    image_number: usize,
    /// Folded pastes: the marker and the full text it stands for.
    pastes: Vec<(String, String)>,
    paste_number: usize,
    /// The message box, not the search box: a draft that is a `!` command
    /// puts it in shell mode.
    shell: bool,
}

/// A paste over this many lines or characters folds into a marker.
const FOLD_LINES: usize = 10;
const FOLD_CHARS: usize = 1000;

/// A stretch of the draft: typed text, an image marker or a folded paste.
enum Piece<'a> {
    Text(&'a str),
    Image(&'a str, &'a canter_protocol::UserInput),
    Paste(&'a str),
}

/// Soft-wrapped rows (byte ranges, without the newlines) and the cursor's
/// row and column.
pub(crate) struct InputLayout {
    pub rows: Vec<Range<usize>>,
    pub cursor: (usize, usize),
}

impl InputBox {
    /// The message box, where a `!` command puts the box in shell mode.
    pub fn messages() -> Self {
        Self {
            shell: true,
            ..Self::default()
        }
    }

    /// In shell mode, where the text after the `!` starts: the `!` and the
    /// blanks before it are drawn as the prompt, not in the text, and the
    /// cursor stays past them. 0 otherwise.
    pub fn start(&self) -> usize {
        let blanks = self.text.len() - self.text.trim_start().len();
        if self.shell && self.text[blanks..].starts_with('!') {
            blanks + 1
        } else {
            0
        }
    }

    /// Keeps the cursor past the `!` of shell mode.
    fn settle(&mut self) {
        self.cursor = self.cursor.max(self.start());
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Byte offset into the text.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Puts `with` in place of `range` (on char boundaries), the cursor
    /// `cursor` bytes into it.
    pub fn replace(&mut self, range: Range<usize>, with: &str, cursor: usize) {
        self.cursor = range.start + cursor;
        self.text.replace_range(range, with);
        self.settle();
    }

    /// The draft with folded pastes in full; image markers stay.
    pub fn full_text(&self) -> String {
        self.pieces()
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(text) | Piece::Image(text, _) | Piece::Paste(text) => text,
            })
            .collect()
    }

    /// Puts `text` in place of the whole draft, the cursor at the end. An
    /// image stays while its marker is there; pastes are no longer folded.
    pub fn replace_all(&mut self, text: &str) {
        self.text = clean(text);
        self.cursor = self.text.len();
        self.pastes.clear();
        let text = &self.text;
        self.images
            .retain(|(marker, _)| text.contains(marker.as_str()));
    }

    /// Empties the box and returns the draft, folded pastes in full.
    pub fn take(&mut self) -> String {
        let text = self.full_text();
        self.cursor = 0;
        self.images.clear();
        self.pastes.clear();
        self.text.clear();
        text
    }

    /// A bracketed paste: over 10 lines or 1000 characters it goes in as
    /// one marker and the full text is kept aside.
    pub fn paste(&mut self, text: &str) {
        let clean = clean(text);
        let lines = clean.lines().count();
        let chars = clean.chars().count();
        if lines <= FOLD_LINES && chars <= FOLD_CHARS {
            self.insert(&clean);
            return;
        }
        self.paste_number += 1;
        let marker = if lines > FOLD_LINES {
            format!("[Pasted #{} · {lines} lines]", self.paste_number)
        } else {
            format!("[Pasted #{} · {chars} chars]", self.paste_number)
        };
        self.insert(&marker);
        self.pastes.push((marker, clean));
    }

    pub fn attach(&mut self, path: String, name: String) {
        self.image_number += 1;
        let marker = format!(
            "[Image #{} {}]",
            self.image_number,
            crate::text::sanitize(&name)
        );
        self.insert(&marker);
        self.images.push((
            marker,
            canter_protocol::UserInput::LocalImage {
                path,
                name: Some(name),
            },
        ));
    }

    /// What is sent: text with folded pastes in full, and the images.
    pub fn parts(&self) -> Vec<canter_protocol::UserInput> {
        use canter_protocol::UserInput;
        let mut parts = Vec::new();
        let mut text = String::new();
        for piece in self.pieces() {
            match piece {
                Piece::Text(more) | Piece::Paste(more) => text.push_str(more),
                Piece::Image(_, image) => {
                    if !text.is_empty() {
                        parts.push(UserInput::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    parts.push(image.clone());
                }
            }
        }
        if !text.is_empty() {
            parts.push(UserInput::Text { text });
        }
        parts
    }

    /// The draft split at the markers, in order.
    fn pieces(&self) -> Vec<Piece<'_>> {
        let images = self.images.iter().flat_map(|(marker, image)| {
            self.text
                .match_indices(marker.as_str())
                .map(move |(i, m)| (i, m, Piece::Image(m, image)))
        });
        let pastes = self.pastes.iter().flat_map(|(marker, text)| {
            self.text
                .match_indices(marker.as_str())
                .map(move |(i, m)| (i, m, Piece::Paste(text)))
        });
        let mut found: Vec<_> = images.chain(pastes).collect();
        found.sort_by_key(|(i, _, _)| *i);
        let mut pieces = Vec::new();
        let mut at = 0;
        for (i, marker, piece) in found {
            if i < at {
                continue;
            }
            if i > at {
                pieces.push(Piece::Text(&self.text[at..i]));
            }
            pieces.push(piece);
            at = i + marker.len();
        }
        if at < self.text.len() {
            pieces.push(Piece::Text(&self.text[at..]));
        }
        pieces
    }

    /// The text kept in the input history file: folded pastes in full,
    /// image markers left out.
    pub fn text_only(&self) -> String {
        self.pieces()
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(text) | Piece::Paste(text) => text,
                Piece::Image(..) => "",
            })
            .collect()
    }

    /// Puts back a draft sent before, as it was, the cursor at the end.
    pub fn recall(&mut self, draft: &Self) {
        *self = draft.clone();
        self.cursor = self.text.len();
    }

    pub fn restore(&mut self, draft: Self) {
        self.cursor = 0;
        self.insert(&draft.text);
        self.images.extend(draft.images);
        self.pastes.extend(draft.pastes);
    }

    pub fn clear(&mut self) {
        self.take();
    }

    /// Puts retracted messages, in full, before the draft; the draft's own
    /// folded pastes stay folded.
    pub fn prepend_inputs(&mut self, inputs: Vec<Vec<canter_protocol::UserInput>>) {
        use canter_protocol::UserInput;
        enum Part {
            Input(UserInput),
            Paste(String),
        }
        let draft: Vec<Part> = self
            .pieces()
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(text) => Part::Input(UserInput::Text { text: text.into() }),
                Piece::Image(_, image) => Part::Input(image.clone()),
                Piece::Paste(text) => Part::Paste(text.into()),
            })
            .collect();
        self.clear();
        for input in inputs
            .into_iter()
            .map(|parts| parts.into_iter().map(Part::Input).collect())
            .chain([draft])
            .filter(|parts: &Vec<Part>| !parts.is_empty())
        {
            if !self.is_empty() {
                self.insert("\n");
            }
            for part in input {
                match part {
                    Part::Input(UserInput::Skill { name }) => self.insert(&format!("/{name} ")),
                    Part::Input(UserInput::Text { text }) => self.insert(&text),
                    Part::Input(UserInput::LocalImage { path, name }) => {
                        let name = name.unwrap_or_else(|| {
                            std::path::Path::new(&path)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        });
                        self.attach(path, name);
                    }
                    Part::Input(UserInput::Unknown) => {}
                    Part::Paste(text) => self.paste(&text),
                }
            }
        }
    }

    /// Inserts typed or pasted text. Line endings become `\n`, tabs become
    /// spaces, and other control characters are dropped.
    pub fn insert(&mut self, text: &str) {
        let clean = clean(text);
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.settle();
    }

    /// Deletes the character before the cursor, or the whole marker. Right
    /// past the `!` of shell mode it leaves shell mode when nothing else is
    /// typed, and otherwise does nothing, so a command does not become a
    /// message.
    pub fn backspace(&mut self) {
        let start = self.start();
        if start > 0 && self.cursor == start {
            if self.text.len() == start {
                self.clear();
            }
            return;
        }
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.remove(self.cursor - c.len_utf8()..self.cursor);
        }
    }

    /// Deletes the character after the cursor, or the whole marker.
    pub fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.remove(self.cursor..self.cursor + c.len_utf8());
        }
    }

    /// One character left; over a marker in one step.
    pub fn left(&mut self) {
        if let Some(marker) = self.markers().into_iter().find(|m| m.end == self.cursor) {
            self.cursor = marker.start;
        } else if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
        self.settle();
    }

    /// One character right; over a marker in one step.
    pub fn right(&mut self) {
        if let Some(marker) = self.markers().into_iter().find(|m| m.start == self.cursor) {
            self.cursor = marker.end;
        } else if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// Where the cursor's line starts, past the `!` of shell mode.
    fn line_start(&self) -> usize {
        let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        start.max(self.start())
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
    }

    pub fn delete_word_before(&mut self) {
        self.remove(self.word_start()..self.cursor);
    }

    pub fn delete_word_after(&mut self) {
        self.remove(self.cursor..self.word_end());
    }

    /// Ctrl+K: to the end of the line, or the newline when already there.
    pub fn delete_to_line_end(&mut self) {
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        if line_end == self.cursor && line_end < self.text.len() {
            self.remove(self.cursor..self.cursor + 1);
        } else {
            self.remove(self.cursor..line_end);
        }
    }

    /// Ctrl+U: to the start of the line, or the newline before it when
    /// already there.
    pub fn delete_to_line_start(&mut self) {
        let line_start = self.line_start();
        if line_start == self.cursor && line_start > self.start() {
            self.remove(self.cursor - 1..self.cursor);
        } else {
            self.remove(line_start..self.cursor);
        }
    }

    /// Byte ranges of the image and paste markers in the text.
    fn markers(&self) -> Vec<Range<usize>> {
        self.images
            .iter()
            .map(|(marker, _)| marker)
            .chain(self.pastes.iter().map(|(marker, _)| marker))
            .flat_map(|marker| {
                self.text
                    .match_indices(marker.as_str())
                    .map(|(i, m)| i..i + m.len())
            })
            .collect()
    }

    /// Removes `range`, widened to whole markers, and puts the cursor where
    /// it was; an image or paste whose marker is gone goes with it.
    fn remove(&mut self, range: Range<usize>) {
        let mut range = range;
        for marker in self.markers() {
            if marker.start < range.end && range.start < marker.end {
                range.start = range.start.min(marker.start);
                range.end = range.end.max(marker.end);
            }
        }
        self.replace(range, "", 0);
        let text = &self.text;
        self.images
            .retain(|(marker, _)| text.contains(marker.as_str()));
        self.pastes
            .retain(|(marker, _)| text.contains(marker.as_str()));
    }

    /// Where the word before the cursor starts: past the separators, then
    /// past one run of the same kind; a marker is one word. Never before
    /// the `!` of shell mode.
    fn word_start(&self) -> usize {
        let markers = self.markers();
        let mut p = self.cursor;
        let before = |p: usize| self.text[..p].chars().next_back();
        loop {
            if let Some(marker) = markers.iter().find(|m| m.start < p && p <= m.end) {
                return marker.start;
            }
            match before(p) {
                Some(c) if word_kind(c).is_none() => p -= c.len_utf8(),
                _ => break,
            }
        }
        let kind = before(p).and_then(word_kind);
        while let Some(c) = before(p)
            && word_kind(c) == kind
        {
            p -= c.len_utf8();
        }
        p.max(self.start())
    }

    /// Where the word after the cursor ends, like [`Self::word_start`].
    fn word_end(&self) -> usize {
        let markers = self.markers();
        let mut p = self.cursor;
        let after = |p: usize| self.text[p..].chars().next();
        loop {
            if let Some(marker) = markers.iter().find(|m| m.start <= p && p < m.end) {
                return marker.end;
            }
            match after(p) {
                Some(c) if word_kind(c).is_none() => p += c.len_utf8(),
                _ => break,
            }
        }
        let kind = after(p).and_then(word_kind);
        while let Some(c) = after(p)
            && word_kind(c) == kind
        {
            p += c.len_utf8();
        }
        p
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
        // Inside a marker the cursor goes after it.
        self.cursor = self
            .markers()
            .into_iter()
            .find(|m| m.start < offset && offset < m.end)
            .map_or(offset, |m| m.end);
    }

    /// A click `column` columns into `row`, a row of [`Self::layout`]: the
    /// cursor goes before the character drawn there, or past the row's last
    /// one to the row's end. On a marker it goes to the nearer side.
    pub fn click(&mut self, row: Range<usize>, column: usize) {
        let mut offset = row.start;
        let mut used = 0;
        // Half columns from the clicked character's start to the click.
        let mut into = 0;
        for c in self.text[row].chars() {
            let w = char_width(c);
            if used + w > column {
                into = 2 * (column - used) + 1;
                break;
            }
            used += w;
            offset += c.len_utf8();
        }
        let marker = self
            .markers()
            .into_iter()
            .find(|m| m.start < offset && offset < m.end);
        self.cursor = match marker {
            Some(m) => {
                // In half columns, against the marker's width in columns:
                // before its middle is nearer its start.
                let before = 2 * str_width(&self.text[m.start..offset]) + into;
                if before < str_width(&self.text[m.clone()]) {
                    m.start
                } else {
                    m.end
                }
            }
            None => offset,
        };
    }

    /// In shell mode the rows start past the `!`.
    pub fn layout(&self, width: usize) -> InputLayout {
        let width = width.max(1);
        let hidden = self.start();
        let mut rows = Vec::new();
        let mut start = hidden;
        let mut used = 0;
        let mut cursor = None;
        for (i, c) in self.text.char_indices().skip_while(|&(i, _)| i < hidden) {
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

/// Line endings become `\n`, tabs become spaces, and other control
/// characters are dropped.
fn clean(text: &str) -> String {
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
    clean
}

#[derive(PartialEq)]
enum WordKind {
    /// Letters, digits and `_`.
    Word,
    /// A run of Chinese characters.
    Han,
}

/// What kind of word `c` belongs to; `None` for separators (punctuation,
/// spaces, newlines).
fn word_kind(c: char) -> Option<WordKind> {
    let han = matches!(
        c,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{3134F}'
    );
    if han {
        Some(WordKind::Han)
    } else if c.is_alphanumeric() || c == '_' {
        Some(WordKind::Word)
    } else {
        None
    }
}
