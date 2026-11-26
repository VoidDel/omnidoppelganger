//! Server-sent events framing, shared by every streaming client.

/// Upper bound on a single buffered SSE line or event payload, preventing an
/// unbounded newline-less stream from exhausting memory.
pub const MAX_SSE_BUFFER_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental server-sent-events framing. Bytes go in as they arrive, in any
/// chunking; complete events come out.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: Vec<String>,
    data_bytes: usize,
    discarding_line: bool,
}

impl SseParser {
    pub fn feed(&mut self, mut chunk: &[u8]) -> Vec<SseEvent> {
        if self.discarding_line {
            match chunk.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    chunk = &chunk[pos + 1..];
                    self.discarding_line = false;
                }
                None => return Vec::new(),
            }
        }
        if chunk.is_empty() {
            return Vec::new();
        }
        if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_BUFFER_BYTES {
            match chunk.iter().position(|&b| b == b'\n') {
                None => {
                    self.buffer.clear();
                    self.discarding_line = true;
                    return Vec::new();
                }
                Some(pos) if self.buffer.len().saturating_add(pos) > MAX_SSE_BUFFER_BYTES => {
                    self.buffer.clear();
                    chunk = &chunk[pos + 1..];
                }
                Some(_) => {}
            }
        }
        self.buffer.extend_from_slice(chunk);

        let mut events = Vec::new();
        let mut consumed = 0usize;
        while let Some(rel_end) = self.buffer[consumed..].iter().position(|&byte| byte == b'\n') {
            let line_end = consumed + rel_end;
            let raw_line = &self.buffer[consumed..line_end];
            consumed = line_end + 1;
            if raw_line.len() > MAX_SSE_BUFFER_BYTES {
                continue;
            }
            let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if raw_line.is_empty() {
                // Per WHATWG SSE §9.2.6 ("dispatch the event"): always reset the event type,
                // and if the `data` buffer is empty, return without dispatching an event.
                let event_name = std::mem::take(&mut self.event);
                if !self.data.is_empty() {
                    events.push(SseEvent { event: event_name, data: self.data.join("\n") });
                    self.data.clear();
                    self.data_bytes = 0;
                }
                continue;
            }
            if raw_line.starts_with(b":") {
                continue;
            }
            // Lines are split on the newline byte, so a valid UTF-8 stream has whole characters per line.
            let line = String::from_utf8_lossy(raw_line);
            if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                let piece = value.strip_prefix(' ').unwrap_or(value);
                if self.data_bytes.saturating_add(piece.len()) <= MAX_SSE_BUFFER_BYTES {
                    self.data_bytes += piece.len() + 1;
                    self.data.push(piece.to_string());
                } else {
                    self.data.clear();
                    self.data_bytes = 0;
                }
            } else if line == "data" {
                self.data_bytes += 1;
                self.data.push(String::new());
            } else if line == "event" {
                self.event.clear();
            }
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        if self.buffer.len() > MAX_SSE_BUFFER_BYTES {
            self.buffer.clear();
            self.discarding_line = true;
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_empty_data_heartbeats_and_comments_without_leaking_event_name() {
        let raw = concat!(
            "event: ping\n\n",
            ": keepalive comment\r\n\r\n",
            "event: heartbeat\r\n: still alive\r\n\r\n",
            "data: {\"ok\":1}\n\n",
            "event: custom\ndata: line1\ndata: line2\n\n",
        );
        for chunk_size in [1, 3, 17, raw.len()] {
            let mut parser = SseParser::default();
            let mut out = Vec::new();
            for chunk in raw.as_bytes().chunks(chunk_size) {
                out.extend(parser.feed(chunk));
            }
            assert_eq!(
                out,
                vec![
                    SseEvent { event: String::new(), data: "{\"ok\":1}".into() },
                    SseEvent { event: "custom".into(), data: "line1\nline2".into() },
                ]
            );
        }
    }

    #[test]
    fn drops_oversized_newline_free_stream_and_recovers_on_next_line() {
        let mut parser = SseParser::default();
        let half = vec![b'x'; MAX_SSE_BUFFER_BYTES / 2 + 1024];
        assert!(parser.feed(&half).is_empty());
        assert!(parser.feed(&half).is_empty());
        assert!(parser.buffer.is_empty());

        let recovered = parser.feed(b"tail_of_oversized_line\ndata: {\"recovered\":true}\n\n");
        assert_eq!(
            recovered,
            vec![SseEvent { event: String::new(), data: "{\"recovered\":true}".into() }]
        );
    }
}
