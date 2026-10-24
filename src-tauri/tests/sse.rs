//! Streaming-parse regressions.
//!
//! The SSE splitter is pure logic with no Tauri dependency, so this test compiles the module
//! source directly instead of linking the library. Linking the library pulls in the Tauri
//! runtime (and its WebView2 DLL import), which aborts the test process at load time in
//! environments where that DLL is not beside a `deps/` binary — a loud failure that has
//! nothing to do with the code under test.

#[path = "../src/sse.rs"]
mod sse;

use sse::{truncate_chars, SseBuffer};

#[test]
fn split_utf8_character_is_not_corrupted() {
    // A Chinese character (3 bytes) straddles the chunk boundary. Decoding each chunk on its
    // own produced U+FFFD here, which showed up as mojibake in the streamed answer.
    let full = "data: {\"choices\":[{\"delta\":{\"content\":\"翻译\"}}]}\n".as_bytes();
    for cut in 1..full.len() {
        let mut buf = SseBuffer::default();
        let first = buf.push(&full[..cut]);
        let mut lines = buf.push(&full[cut..]);
        assert!(first.is_empty(), "no complete line before the final byte");
        let line = lines.pop().expect("the line must be complete");
        assert!(line.contains("翻译"), "cut at {cut} produced: {line}");
        assert!(!line.contains('\u{fffd}'), "cut at {cut} inserted a replacement char");
    }
}

#[test]
fn crlf_lines_are_split_and_trimmed() {
    let mut buf = SseBuffer::default();
    let lines = buf.push(b"data: a\r\ndata: b\r\n");
    assert_eq!(lines, vec!["data: a".to_string(), "data: b".to_string()]);
    assert_eq!(buf.finish(), None);
}

#[test]
fn multiple_lines_in_one_chunk() {
    let mut buf = SseBuffer::default();
    let lines = buf.push(b"one\ntwo\nthree");
    assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
    // The trailing partial line is only returned by finish().
    assert_eq!(buf.finish().as_deref(), Some("three"));
}

#[test]
fn final_line_without_newline_is_flushed() {
    let mut buf = SseBuffer::default();
    assert!(buf.push(b"data: tail").is_empty());
    assert_eq!(buf.finish().as_deref(), Some("data: tail"));
    assert_eq!(buf.finish(), None, "finish must not repeat the line");
}

#[test]
fn truncation_is_char_safe() {
    assert_eq!(truncate_chars("中文测试", 2), "中文…");
    assert_eq!(truncate_chars("中文测试", 10), "中文测试");
    // Never slice through a multi-byte character.
    assert_eq!(truncate_chars("🙂🙂🙂", 1), "🙂…");
}

#[test]
fn byte_chunks_split_at_every_boundary_never_panic() {
    let text = "data: {\"choices\":[{\"delta\":{\"content\":\"你好🙂world\"}}]}\n";
    let bytes = text.as_bytes();
    // Feed one byte at a time: the worst case for multi-byte handling.
    let mut buf = SseBuffer::default();
    let mut all = Vec::new();
    for b in bytes {
        all.extend(buf.push(std::slice::from_ref(b)));
    }
    let line = all.pop().expect("one line");
    assert!(line.contains("你好🙂world"), "got: {line}");
    assert_eq!(buf.finish(), None);
}
