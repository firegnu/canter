use crate::scratch::ScratchDirectory;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_CLIENT_OUTPUT_BYTES: usize = 1024 * 1024;

/// Most of one command's output that is saved for the model to read again.
pub(crate) const MAX_SAVED_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

pub fn truncate_model_output(text: &str) -> String {
    let mut output = BoundedText::new(crate::MAX_MODEL_OUTPUT_BYTES);
    output.push(text.as_bytes());
    output.render(true)
}

/// A command's output saved in the session scratch directory.
pub(crate) struct SavedOutput {
    /// The `/tmp/canter-shell-…` spelling the model is given, as in `$TMPDIR`.
    pub path: PathBuf,
    /// Bytes actually written, which stay below the output's size at the cap.
    pub bytes: u64,
}

/// Writes a command's whole output while it is read, so the middle stays
/// readable after the model result is truncated. Dropping it without
/// [`Self::finish`] removes the file again.
pub(crate) struct OutputStore {
    path: PathBuf,
    file: io::BufWriter<std::fs::File>,
    written: u64,
    scratch: Arc<ScratchDirectory>,
    full: bool,
    kept: bool,
}

impl OutputStore {
    /// One file per tool call, named after the call id.
    pub(crate) fn create(scratch: Arc<ScratchDirectory>, id: &str) -> io::Result<Self> {
        let directory = scratch
            .path()
            .ok_or_else(|| io::Error::other("scratch directory is unavailable"))?;
        let path = directory.join(format!("{}.txt", file_stem(id)));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path,
            // Writes go through this buffer, so reading the command's output
            // only touches the disk once per buffer, not once per pipe read.
            file: io::BufWriter::with_capacity(64 * 1024, file),
            written: 0,
            scratch,
            full: false,
            kept: false,
        })
    }

    /// Append output, stopping at the per-file and per-session limits.
    pub(crate) fn push(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !self.full && !bytes.is_empty() {
            let room = MAX_SAVED_OUTPUT_BYTES.saturating_sub(self.written);
            if room == 0 {
                self.full = true;
                break;
            }
            let count = (bytes.len() as u64).min(room) as usize;
            if !self.scratch.claim_saved_output(count as u64) {
                self.full = true;
                break;
            }
            self.file.write_all(&bytes[..count])?;
            self.written += count as u64;
            bytes = &bytes[count..];
        }
        Ok(())
    }

    /// Flush the file and keep it; None when nothing could be written, and a
    /// failed flush removes the partial file.
    pub(crate) fn finish(mut self) -> io::Result<Option<SavedOutput>> {
        self.file.flush()?;
        if self.written == 0 {
            return Ok(None);
        }
        self.kept = true;
        Ok(Some(SavedOutput {
            path: self.path.clone(),
            bytes: self.written,
        }))
    }
}

impl Drop for OutputStore {
    fn drop(&mut self) {
        if !self.kept {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Call ids become file names, so anything outside one safe path segment is replaced.
fn file_stem(id: &str) -> String {
    let name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "output".into()
    } else {
        name
    }
}

/// Retain only a bounded prefix and suffix, counting everything discarded in between.
pub(crate) struct BoundedText {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    limit: usize,
}

impl BoundedText {
    pub fn original_bytes_if_truncated(&self) -> Option<u64> {
        (self.total > self.limit as u64).then_some(self.total)
    }
    pub fn new(limit: usize) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            limit,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let half = self.limit / 2;
        let head_count = bytes.len().min(half - self.head.len());
        self.head.extend_from_slice(&bytes[..head_count]);
        let rest = &bytes[head_count..];
        if rest.len() >= half {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - half..]);
        } else {
            let remove = (self.tail.len() + rest.len()).saturating_sub(half);
            self.tail.drain(..remove);
            self.tail.extend(rest);
        }
    }

    pub fn render(&self, include_marker_in_limit: bool) -> String {
        self.render_saved(include_marker_in_limit, None)
    }

    /// Render with a saved file named in the truncation marker when one exists.
    pub fn render_saved(
        &self,
        include_marker_in_limit: bool,
        saved: Option<&SavedOutput>,
    ) -> String {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if self.total <= self.limit as u64 {
            let mut bytes = self.head.clone();
            bytes.extend(tail);
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        // 64 bytes accommodates the marker and a full u64 count. Both halves use
        // the same budget, and UTF-8 boundaries only reduce their retained size.
        let half = (self.limit - if include_marker_in_limit { 64 } else { 0 }) / 2;
        let mut head_end = half.min(self.head.len());
        while std::str::from_utf8(&self.head[..head_end]).is_err() {
            head_end -= 1;
        }
        let mut tail_start = tail.len().saturating_sub(half);
        while tail_start < tail.len() && tail[tail_start] & 0xc0 == 0x80 {
            tail_start += 1;
        }
        let removed = self.total - (head_end + tail.len() - tail_start) as u64;
        format!(
            "{}\n{}\n{}",
            String::from_utf8_lossy(&self.head[..head_end]),
            self.marker(removed, saved),
            String::from_utf8_lossy(&tail[tail_start..])
        )
    }

    fn marker(&self, removed: u64, saved: Option<&SavedOutput>) -> String {
        let Some(saved) = saved else {
            return format!("[truncated {removed} bytes]");
        };
        let path = saved.path.display();
        let hint = "this session only; use read with startLine, or rg, to view the middle";
        if saved.bytes >= self.total {
            format!("[truncated {removed} bytes; full output saved to {path} ({hint})]")
        } else {
            format!(
                "[truncated {removed} bytes; only the first {} of {} bytes saved to {path} ({hint})]",
                saved.bytes, self.total
            )
        }
    }
}
