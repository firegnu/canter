use std::collections::VecDeque;

pub const MAX_CLIENT_OUTPUT_BYTES: usize = 1024 * 1024;

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
            "{}\n[truncated {removed} bytes]\n{}",
            String::from_utf8_lossy(&self.head[..head_end]),
            String::from_utf8_lossy(&tail[tail_start..])
        )
    }
}
