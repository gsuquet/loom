//! Pure Server-Sent Events (`text/event-stream`) parser for `GET /v1/events`.

use loom_api::{ChangeEvent, EventCursor};

/// One decoded SSE message from `GET /v1/events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// Event cursor from `id:`, if present and valid.
    pub id: Option<EventCursor>,
    /// Decoded [`ChangeEvent`] from `data:`.
    pub event: ChangeEvent,
}

/// Incremental line-buffered SSE parser.
#[derive(Debug, Default, Clone)]
pub struct SseParser {
    buffer: String,
    current_id: Option<String>,
    current_data: Vec<String>,
}

impl SseParser {
    /// Creates an empty [`SseParser`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of UTF-8 text from the HTTP response body and returns any completed
    /// [`SseFrame`]s.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buffer.push_str(chunk);
        let mut out = Vec::new();
        while let Some(newline) = self.buffer.find('\n') {
            let mut line = self.buffer[..newline].to_owned();
            self.buffer.drain(..=newline);
            if line.ends_with('\r') {
                line.pop();
            }
            if let Some(frame) = self.process_line(&line) {
                out.push(frame);
            }
        }
        out
    }

    fn process_line(&mut self, line: &str) -> Option<SseFrame> {
        if line.is_empty() {
            return self.finish_event();
        }
        if line.starts_with(':') {
            // Heartbeat comment.
            return None;
        }
        let (field, value) = line
            .split_once(':')
            .map_or((line, ""), |(f, v)| (f, v.strip_prefix(' ').unwrap_or(v)));
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
        let event = serde_json::from_str::<ChangeEvent>(&payload).ok()?;
        Some(SseFrame { id, event })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunked_sse_frames_heartbeats_and_resync() {
        let mut parser = SseParser::new();
        assert_eq!(parser.feed(": keep-alive\n\n").len(), 0);

        let part1 = "id: 1791792000000-1\nevent: task.changed\n";
        let part2 =
            "data: {\"type\":\"task.changed\",\"id\":\"k3q7zm2a\",\"context\":\"work\"}\n\n";
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

        let resync = parser.feed("event: resync\ndata: {\"type\":\"resync\"}\r\n\r\n");
        assert_eq!(
            resync,
            vec![SseFrame {
                id: None,
                event: ChangeEvent::Resync,
            }]
        );
    }
}
