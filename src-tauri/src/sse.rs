//! Minimal SSE parsing used by the streaming chat client.
//!
//! Kept dependency-free so it can be exercised by an integration test (`tests/sse.rs`)
//! without pulling in the Tauri runtime.

/// Incremental SSE line splitter that never cuts a multi-byte UTF-8 character.
///
/// Bytes arrive in arbitrary chunks, so a Chinese character (or an emoji) can straddle a
/// chunk boundary. Decoding each chunk independently with `from_utf8_lossy` replaces the
/// split character with U+FFFD in the streamed output — a visible corruption that shows up
/// in exactly the languages this app is used with. The splitter therefore works on bytes,
/// cuts complete lines, and only then decodes.
#[derive(Default)]
pub struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    /// Append a chunk and return every complete line it now contains (decoded).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            // Drop the trailing '\n' and an optional '\r'.
            let mut end = line.len() - 1;
            if end > 0 && line[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(String::from_utf8_lossy(&line[..end]).into_owned());
        }
        lines
    }

    /// Flush whatever is left (a final line without a newline).
    pub fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let line = std::mem::take(&mut self.buf);
        Some(String::from_utf8_lossy(&line).into_owned())
    }
}

/// Truncate to at most `max` characters (never splits a UTF-8 sequence).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}
