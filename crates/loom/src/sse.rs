//! Server-Sent Events parser for `GET /v1/events`.

use std::borrow::Cow;

use loom_api::{ChangeEvent, EventCursor};

/// One decoded message from `GET /v1/events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// Event cursor from `id:`, if present.
    pub id: Option<EventCursor>,
    /// Decoded [`ChangeEvent`] from `data:`.
    pub event: ChangeEvent,
}

/// Incremental byte-buffered Server-Sent Events parser.
///
/// Buffers raw bytes until line breaks (`\n` or `\r\n`) so multi-byte UTF-8
/// characters split across chunk boundaries decode correctly without corruption.
#[derive(Debug, Default, Clone)]
pub struct SseParser {
    buffer: Vec<u8>,
    current_id: Option<String>,
    current_data: Vec<String>,
}

impl SseParser {
    /// Maximum allowed buffer capacity in bytes (1 MiB) before dropping oversized lines.
    pub const MAX_BUFFER_CAPACITY: usize = 1024 * 1024;

    /// Creates an empty [`SseParser`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of bytes from the HTTP response body and returns any completed [`SseFrame`]s.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > Self::MAX_BUFFER_CAPACITY {
            self.buffer.clear();
            self.current_id = None;
            self.current_data.clear();
            return Vec::new();
        }

        let mut frames = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|&byte| byte == b'\n') {
            let mut line_bytes = self.buffer.drain(..=pos).collect::<Vec<u8>>();
            // Remove the trailing '\n'
            line_bytes.pop();
            // Remove the trailing '\r' if present (CRLF)
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }

            let line = std::str::from_utf8(&line_bytes)
                .map_or_else(|_| String::from_utf8_lossy(&line_bytes), Cow::Borrowed);

            if let Some(frame) = self.process_line(&line) {
                frames.push(frame);
            }
        }

        frames
    }

    fn process_line(&mut self, line: &str) -> Option<SseFrame> {
        if line.is_empty() {
            return self.finish_event();
        }
        if line.starts_with(':') {
            // Comment / heartbeat line; ignore.
            return None;
        }

        let (field, mut value) = match line.split_once(':') {
            Some((f, v)) => (f, v),
            None => (line, ""),
        };
        if value.starts_with(' ') {
            value = &value[1..];
        }

        match field {
            "id" => {
                self.current_id = Some(value.to_owned());
            }
            "data" => {
                self.current_data.push(value.to_owned());
            }
            _ => {}
        }
        None
    }

    fn finish_event(&mut self) -> Option<SseFrame> {
        let id = self
            .current_id
            .take()
            .and_then(|raw| raw.parse::<EventCursor>().ok());

        if self.current_data.is_empty() {
            return None;
        }

        let payload = self.current_data.join("\n");
        self.current_data.clear();
        let event = serde_json::from_str::<ChangeEvent>(&payload).unwrap_or(ChangeEvent::Resync);
        Some(SseFrame { id, event })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunked_sse_frames_heartbeats_and_resync() {
        let mut parser = SseParser::new();
        assert_eq!(parser.feed(b": keep-alive\n\n").len(), 0);

        let part1 = b"id: 1791792000000-1\nevent: task.changed\n";
        let part2 =
            b"data: {\"type\":\"task.changed\",\"id\":\"k3q7zm2a\",\"context\":\"work\"}\n\n";
        assert_eq!(parser.feed(part1).len(), 0);
        let frames = parser.feed(part2);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0],
            SseFrame {
                id: Some(EventCursor {
                    run: 1_791_792_000_000,
                    seq: 1,
                }),
                event: ChangeEvent::TaskChanged {
                    id: "k3q7zm2a".parse().unwrap(),
                    context: "work".parse().unwrap(),
                },
            }
        );

        let resync = parser.feed(b"event: resync\ndata: {\"type\":\"resync\"}\r\n\r\n");
        assert_eq!(
            resync,
            vec![SseFrame {
                id: None,
                event: ChangeEvent::Resync,
            }]
        );
    }

    #[test]
    fn buffers_raw_bytes_across_chunks_without_utf8_corruption() {
        let mut parser = SseParser::new();

        // 4-byte UTF-8 character '🦀' is [0xF0, 0x9F, 0xA6, 0x80].
        // Split chunk 1 right in the middle of '🦀' (after [0xF0, 0x9F]).
        let part1 =
            b"id: 1791792000000-2\ndata: {\"type\":\"repo.changed\",\"slug\":\"work/\xf0\x9f";
        let part2 = b"\xa6\x80-rust\"}\n\n";

        let frames1 = parser.feed(part1);
        assert_eq!(frames1.len(), 0);

        let frames2 = parser.feed(part2);
        assert_eq!(frames2.len(), 1);
        assert_eq!(
            frames2[0],
            SseFrame {
                id: Some(EventCursor {
                    run: 1_791_792_000_000,
                    seq: 2,
                }),
                event: ChangeEvent::RepoChanged {
                    slug: "work/🦀-rust".to_owned(),
                },
            }
        );
    }

    #[test]
    fn parses_multiple_frames_in_single_chunk() {
        let mut parser = SseParser::new();
        let chunk =
            b"id: 100-1\ndata: {\"type\":\"resync\"}\n\nid: 100-2\ndata: {\"type\":\"resync\"}\n\n";
        let frames = parser.feed(chunk);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, Some(EventCursor { run: 100, seq: 1 }));
        assert_eq!(frames[1].id, Some(EventCursor { run: 100, seq: 2 }));
    }

    #[test]
    fn unknown_or_future_event_payload_falls_back_to_resync_and_preserves_id() {
        let mut parser = SseParser::new();
        let chunk = b"id: 200-5\ndata: {\"type\":\"some.future.event\",\"details\":123}\n\n";
        let frames = parser.feed(chunk);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0],
            SseFrame {
                id: Some(EventCursor { run: 200, seq: 5 }),
                event: ChangeEvent::Resync,
            }
        );
    }

    #[test]
    fn buffers_over_one_megabyte_are_cleared() {
        let mut parser = SseParser::new();
        let chunk = vec![b'a'; SseParser::MAX_BUFFER_CAPACITY + 1];
        let frames = parser.feed(&chunk);
        assert_eq!(frames.len(), 0);
        // Ensure parser is still usable afterwards
        let normal = b"id: 1-1\ndata: {\"type\":\"resync\"}\n\n";
        let frames2 = parser.feed(normal);
        assert_eq!(frames2.len(), 1);
    }
}
