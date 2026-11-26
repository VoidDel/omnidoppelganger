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
    discarding_event: bool,
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
                let dropped = std::mem::replace(&mut self.discarding_event, false);
                if !dropped && !self.data.is_empty() {
                    events.push(SseEvent {
                        event: event_name,
                        data: self.data.join("\n"),
                    });
                }
                self.data.clear();
                self.data_bytes = 0;
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
                if self.discarding_event {
                    continue;
                }
                let piece = value.strip_prefix(' ').unwrap_or(value);
                if self.data_bytes.saturating_add(piece.len()) <= MAX_SSE_BUFFER_BYTES {
                    self.data_bytes += piece.len() + 1;
                    self.data.push(piece.to_string());
                } else {
                    self.discarding_event = true;
                    self.data.clear();
                    self.data_bytes = 0;
                }
            } else if line == "data" && !self.discarding_event {
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
                    SseEvent {
                        event: String::new(),
                        data: "{\"ok\":1}".into()
                    },
                    SseEvent {
                        event: "custom".into(),
                        data: "line1\nline2".into()
                    },
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
            vec![SseEvent {
                event: String::new(),
                data: "{\"recovered\":true}".into()
            }]
        );
    }

    #[test]
    fn reassembles_4_byte_utf8_emojis_across_single_byte_chunks() {
        let payload = "event: delta\ndata: {\"emoji\":\"🚀🦀🧪🎉👨‍👩‍👧‍👦\",\"zh\":\"水墨修真\"}\n\n";
        let mut parser = SseParser::default();
        let mut events = Vec::new();
        for &byte in payload.as_bytes() {
            events.extend(parser.feed(std::slice::from_ref(&byte)));
        }
        assert_eq!(
            events,
            vec![SseEvent {
                event: "delta".into(),
                data: "{\"emoji\":\"🚀🦀🧪🎉👨‍👩‍👧‍👦\",\"zh\":\"水墨修真\"}".into(),
            }]
        );
        assert!(!events[0].data.contains('\u{fffd}'));
    }

    #[test]
    fn handles_crlf_crlf_split_across_arbitrary_chunk_boundaries() {
        let chunks: &[&[u8]] = &[
            b"event: message\r",
            b"\ndata: first\r",
            b"\n: comment mid-event\r",
            b"\ndata: second\r",
            b"\n\r",
            b"\ndata: after-crlf\r\n",
            b"\r",
            b"\n",
        ];
        let mut parser = SseParser::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk));
        }
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "message".into(),
                    data: "first\nsecond".into(),
                },
                SseEvent {
                    event: String::new(),
                    data: "after-crlf".into(),
                },
            ]
        );
    }

    #[test]
    fn concatenates_multiline_data_and_ignores_colon_comments() {
        let stream = concat!(
            ": opening comment\n",
            ":\n",
            "event: patch\n",
            "data: alpha\n",
            "data:  two-leading-spaces\n",
            ": comment inside multiline data\n",
            "data\n",
            "data:\n",
            "data: omega\n",
            "\n",
        );
        let mut parser = SseParser::default();
        let events = parser.feed(stream.as_bytes());
        assert_eq!(
            events,
            vec![SseEvent {
                event: "patch".into(),
                data: "alpha\n two-leading-spaces\n\n\nomega".into(),
            }]
        );
    }

    #[test]
    fn discards_oversized_multiline_data_event_whole_and_recovers_on_following_event() {
        let mut parser = SseParser::default();
        let line_payload = "a".repeat(1024 * 1024); // 1 MiB per `data:` line
        let line = format!("data: {line_payload}\n");
        for _ in 0..33 {
            assert!(parser.feed(line.as_bytes()).is_empty());
        }
        // Trailing small line before `\n\n` must NOT be emitted as a truncated fragment!
        assert!(parser.feed(b"data: trailing_fragment\n\n").is_empty());

        // Next event after the blank line recovers cleanly.
        let next = parser.feed(b"event: ok\ndata: clean\n\n");
        assert_eq!(
            next,
            vec![SseEvent {
                event: "ok".into(),
                data: "clean".into(),
            }]
        );
    }
}
