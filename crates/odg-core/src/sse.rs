//! Server-sent events framing, shared by every streaming client.

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
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            // Lines are split on the newline byte, so a line is always whole UTF-8 characters.
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    events.push(SseEvent { event: std::mem::take(&mut self.event), data: self.data.join("\n") });
                    self.data.clear();
                }
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
            }
        }
        events
    }
}
