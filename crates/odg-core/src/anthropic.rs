//! Anthropic Messages API over raw HTTP with server-sent events.
//!
//! There is no official Rust SDK, so this module owns the wire format:
//! [`SseParser`] frames the byte stream, [`StreamAccumulator`] rebuilds the
//! assistant message from its events, and [`AnthropicClient`] sends the
//! request; [`crate::llm::with_retries`] retries what can be retried.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::llm::{AssistantTurn, Attempt, Llm, LlmError, LlmRequest, StreamDelta, http_failure, with_retries};
use crate::sse::SseParser;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub(crate) const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Streaming has no HTTP timeout to stay under, so the model gets room to finish.
const MAX_TOKENS: u32 = 64_000;
/// The API sends pings while it works; this much silence means the connection is dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Models offered in settings: `(id, display name)`. The first is the default.
pub const MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-fable-5-2", "Claude Fable 5.2"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
];
pub const DEFAULT_MODEL: &str = MODELS[0].0;

/// Models whose safety refusals the API can re-run on another model inside the same call.
fn supports_server_fallback(model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-opus-5" | "claude-fable-5-2" | "claude-fable-5-1" | "claude-sonnet-5-5")
}

/// Rebuilds one assistant message from stream events.
///
/// Content blocks are kept as the JSON the API sent, with deltas applied, so
/// block types this code does not know still round-trip unchanged.
#[derive(Default)]
pub struct StreamAccumulator {
    blocks: Vec<Option<Value>>,
    partial_json: HashMap<usize, String>,
    turn: AssistantTurn,
    usage: Map<String, Value>,
    stopped: bool,
}

impl StreamAccumulator {
    /// Apply one event's JSON. Returns the fragments to show, or the error the stream reported.
    pub fn apply(&mut self, data: &Value) -> Result<Vec<StreamDelta>, LlmError> {
        let mut deltas = Vec::new();
        match data["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.turn.model = data["message"]["model"].as_str().unwrap_or_default().to_string();
                self.merge_usage(&data["message"]["usage"]);
            }
            "content_block_start" => {
                let index = block_index(data)?;
                let block = data["content_block"].clone();
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, None);
                }
                match block["type"].as_str() {
                    Some("tool_use") => deltas.push(StreamDelta::ToolUseStart {
                        index,
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                    }),
                    Some("text") => {
                        if let Some(text) = block["text"].as_str().filter(|text| !text.is_empty()) {
                            deltas.push(StreamDelta::TextDelta { index, text: text.to_string() });
                        }
                    }
                    _ => {}
                }
                self.blocks[index] = Some(block);
            }
            "content_block_delta" => {
                let index = block_index(data)?;
                let delta = &data["delta"];
                let Some(block) = self.blocks.get_mut(index).and_then(Option::as_mut) else {
                    return Err(LlmError::Protocol(format!("delta for content block {index} that never started")));
                };
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append_string(block, "text", text);
                        deltas.push(StreamDelta::TextDelta { index, text: text.to_string() });
                    }
                    Some("thinking_delta") => {
                        let text = delta["thinking"].as_str().unwrap_or_default();
                        append_string(block, "thinking", text);
                        deltas.push(StreamDelta::ThinkingDelta { index, text: text.to_string() });
                    }
                    Some("signature_delta") => append_string(block, "signature", delta["signature"].as_str().unwrap_or_default()),
                    // A source the text cites, such as a page a web search found: gathered on the block, as the API sends it whole.
                    Some("citations_delta") => match block["citations"].as_array_mut() {
                        Some(citations) => citations.push(delta["citation"].clone()),
                        None => block["citations"] = json!([delta["citation"]]),
                    },
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or_default();
                        self.partial_json.entry(index).or_default().push_str(partial);
                        deltas.push(StreamDelta::ToolInputDelta { index, partial: partial.to_string() });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = block_index(data)?;
                if let Some(block) = self.blocks.get_mut(index).and_then(Option::as_mut)
                    && block["type"] == "tool_use"
                {
                    let raw = self.partial_json.remove(&index).unwrap_or_default();
                    // Tool inputs stream unvalidated, so the text is parsed strictly here:
                    // anything but one JSON object is reported instead of being run.
                    let parsed = if raw.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str::<Value>(&raw) };
                    match parsed {
                        Ok(input) if input.is_object() => block["input"] = input,
                        _ => {
                            block["input"] = json!({});
                            let id = block["id"].as_str().unwrap_or_default().to_string();
                            self.turn.invalid_tool_inputs.insert(id, raw);
                        }
                    }
                }
                deltas.push(StreamDelta::BlockStop { index });
            }
            "message_delta" => {
                if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                    self.turn.stop_reason = Some(reason.to_string());
                }
                self.merge_usage(&data["usage"]);
            }
            "message_stop" => self.stopped = true,
            "error" => return Err(api_error(None, &data["error"])),
            // `ping` and event types added later carry nothing this client needs.
            _ => {}
        }
        Ok(deltas)
    }

    fn merge_usage(&mut self, usage: &Value) {
        if let Some(fields) = usage.as_object() {
            for (key, value) in fields {
                self.usage.insert(key.clone(), value.clone());
            }
        }
    }

    /// Whether `message_stop` arrived: a stream that ends without it was cut off.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn finish(mut self) -> AssistantTurn {
        self.turn.content = self.blocks.into_iter().flatten().collect();
        self.turn.usage = Value::Object(self.usage);
        self.turn
    }
}

fn block_index(data: &Value) -> Result<usize, LlmError> {
    data["index"].as_u64().map(|index| index as usize).ok_or_else(|| LlmError::Protocol("content block event without an index".to_string()))
}

fn append_string(block: &mut Value, field: &str, text: &str) {
    let mut current = block[field].as_str().unwrap_or_default().to_string();
    current.push_str(text);
    block[field] = Value::String(current);
}

/// An API error object, `{"type": "...", "message": "..."}`, as an [`LlmError`].
fn api_error(status: Option<u16>, error: &Value) -> LlmError {
    let kind = error["type"].as_str().unwrap_or("api_error").to_string();
    let retryable = match status {
        Some(status) => matches!(status, 408 | 409 | 429) || status >= 500,
        None => matches!(kind.as_str(), "overloaded_error" | "api_error" | "rate_limit_error" | "timeout_error"),
    };
    LlmError::Api { status, kind, message: error["message"].as_str().unwrap_or("no message").to_string(), retryable }
}

/// The JSON body of a streaming Messages request.
///
/// `official` is false behind a custom base URL: a gateway may reject fields
/// the real API accepts, so eager tool-input streaming and server-side
/// fallbacks are left out there.
pub fn request_body(request: &LlmRequest, official: bool) -> Value {
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            // Not on a tool of the API's own, such as web search, which runs on its side and takes no such field.
            if official && tool.get("type").is_none() {
                tool["eager_input_streaming"] = json!(true);
            }
            tool
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": [{ "type": "text", "text": request.system }],
        "messages": request.messages,
        "tools": tools,
        // Summaries let the client show what the model is working out; the default sends empty text.
        "thinking": { "type": "adaptive", "display": "summarized" },
        "output_config": { "effort": request.effort },
        // Caches the longest stable prefix: tools, system prompt, and the conversation so far.
        "cache_control": { "type": "ephemeral" },
    });
    if official && supports_server_fallback(&request.model) {
        body["fallbacks"] = json!("default");
    }
    // A request without tools (a summary written from a transcript) leaves the field out: an empty list is an error to some servers.
    if request.tools.is_empty() {
        body.as_object_mut().expect("the body is an object").remove("tools");
    }
    body
}

/// Supplies the API key for each request, so a key saved in settings applies to the next request.
pub type KeySource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
    key: KeySource,
}

impl AnthropicClient {
    pub fn new(base_url: impl Into<String>, key: KeySource) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| LlmError::Network(error.to_string()))?;
        Ok(Self { http, base_url: base_url.into().trim_end_matches('/').to_string(), key })
    }

    fn official(&self) -> bool {
        self.base_url == DEFAULT_BASE_URL
    }

    /// One HTTP attempt. `forwarded` is set once any fragment reached `deltas`.
    async fn attempt(
        &self,
        request: &LlmRequest,
        deltas: &UnboundedSender<StreamDelta>,
        cancel: &CancellationToken,
        forwarded: &mut bool,
    ) -> Attempt {
        let key = (self.key)().filter(|key| !key.trim().is_empty()).ok_or((LlmError::MissingKey, None))?;
        let mut builder = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", key.trim())
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json");
        // Gateways and other providers' Anthropic endpoints often take the key as a bearer token instead.
        if !self.official() {
            builder = builder.bearer_auth(key.trim());
        }
        if self.official() && supports_server_fallback(&request.model) {
            builder = builder.header("anthropic-beta", FALLBACK_BETA);
        }
        let send = builder.json(&request_body(request, self.official())).send();
        let response = tokio::select! {
            response = send => response.map_err(|error| (LlmError::Network(error.to_string()), None))?,
            () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
        };

        if !response.status().is_success() {
            return Err(http_failure(response).await);
        }

        let mut parser = SseParser::default();
        let mut accumulator = StreamAccumulator::default();
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => chunk,
                () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
            };
            let chunk = match chunk {
                Err(_) => return Err((LlmError::Network("the stream went silent".to_string()), None)),
                Ok(None) => break,
                Ok(Some(Err(error))) => return Err((LlmError::Network(error.to_string()), None)),
                Ok(Some(Ok(chunk))) => chunk,
            };
            for event in parser.feed(&chunk) {
                let data: Value = serde_json::from_str(&event.data)
                    .map_err(|error| (LlmError::Protocol(format!("event {} is not JSON: {error}", event.event)), None))?;
                for delta in accumulator.apply(&data).map_err(|error| (error, None))? {
                    *forwarded = true;
                    // A closed receiver only means nobody is watching; the message still completes.
                    let _ = deltas.send(delta);
                }
            }
        }
        if !accumulator.stopped() {
            return Err((LlmError::Network("the stream ended before the message was complete".to_string()), None));
        }
        Ok(accumulator.finish())
    }
}

impl Llm for AnthropicClient {
    async fn stream(
        &self,
        request: LlmRequest,
        deltas: UnboundedSender<StreamDelta>,
        cancel: CancellationToken,
    ) -> Result<AssistantTurn, LlmError> {
        with_retries(&deltas, &cancel, || {
            let (request, deltas, cancel) = (&request, &deltas, &cancel);
            async move {
                let mut forwarded = false;
                let outcome = self.attempt(request, deltas, cancel, &mut forwarded).await;
                (outcome, forwarded)
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = concat!(
        "event: message_start\r\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\r\n\r\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"读一下文件\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"c2ln\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"我先\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"看看。\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"src/\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"日志.rs\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":40}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    fn replay(stream: &[u8], chunk_size: usize) -> (AssistantTurn, Vec<StreamDelta>, bool) {
        let mut parser = SseParser::default();
        let mut accumulator = StreamAccumulator::default();
        let mut deltas = Vec::new();
        for chunk in stream.chunks(chunk_size) {
            for event in parser.feed(chunk) {
                deltas.extend(accumulator.apply(&serde_json::from_str(&event.data).unwrap()).unwrap());
            }
        }
        let stopped = accumulator.stopped();
        (accumulator.finish(), deltas, stopped)
    }

    #[test]
    fn rebuilds_the_message_whatever_the_chunking() {
        // Chunk sizes of 1 and 7 split multi-byte characters and CRLF pairs across reads.
        for chunk_size in [1, 7, 64, STREAM.len()] {
            let (turn, deltas, stopped) = replay(STREAM.as_bytes(), chunk_size);
            assert!(stopped);
            assert_eq!(turn.model, "claude-opus-5-5");
            assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
            assert_eq!(turn.usage, json!({ "input_tokens": 12, "output_tokens": 40 }));
            assert_eq!(turn.content, vec![
                json!({ "type": "thinking", "thinking": "读一下文件", "signature": "c2ln" }),
                json!({ "type": "text", "text": "我先看看。" }),
                json!({ "type": "tool_use", "id": "toolu_1", "name": "read_file", "input": { "path": "src/日志.rs" } }),
            ]);
            assert!(turn.invalid_tool_inputs.is_empty());
            assert_eq!(deltas.first(), Some(&StreamDelta::ThinkingDelta { index: 0, text: "读一下文件".into() }));
            assert!(deltas.contains(&StreamDelta::ToolUseStart { index: 2, id: "toolu_1".into(), name: "read_file".into() }));
            assert_eq!(deltas.last(), Some(&StreamDelta::BlockStop { index: 2 }));
        }
    }

    #[test]
    fn keeps_unknown_block_types_and_reports_bad_tool_input() {
        let stream = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"fallback\",\"from\":{\"model\":\"a\"},\"to\":{\"model\":\"b\"}}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"edit_file\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"a.rs\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{}}\n\n",
        );
        let (turn, _, stopped) = replay(stream.as_bytes(), 16);
        assert!(!stopped);
        assert_eq!(turn.content[0], json!({ "type": "fallback", "from": { "model": "a" }, "to": { "model": "b" } }));
        assert_eq!(turn.content[1]["input"], json!({}));
        assert_eq!(turn.invalid_tool_inputs.get("toolu_2").map(String::as_str), Some("{\"path\": \"a.rs"));
    }

    #[test]
    fn surfaces_a_stream_error_as_retryable_only_when_the_api_says_so() {
        let mut accumulator = StreamAccumulator::default();
        let overloaded = accumulator.apply(&json!({ "type": "error", "error": { "type": "overloaded_error", "message": "busy" } })).unwrap_err();
        assert!(overloaded.retryable());
        let invalid = accumulator.apply(&json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "bad" } })).unwrap_err();
        assert!(!invalid.retryable());
        assert!(!api_error(Some(400), &json!({ "type": "invalid_request_error", "message": "bad" })).retryable());
        assert!(api_error(Some(529), &json!({ "type": "overloaded_error", "message": "busy" })).retryable());
    }

    #[test]
    fn builds_the_request_for_the_official_api_and_for_a_gateway() {
        let request = LlmRequest {
            provider: "anthropic".into(),
            session: "s".into(),
            model: "claude-opus-5-5".into(),
            system: "sys".into(),
            messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })],
            tools: vec![json!({ "name": "read_file", "description": "d", "input_schema": { "type": "object" } })],
            effort: "high".into(),
        };
        let official = request_body(&request, true);
        assert_eq!(official["fallbacks"], "default");
        assert_eq!(official["tools"][0]["eager_input_streaming"], true);
        assert_eq!(official["thinking"], json!({ "type": "adaptive", "display": "summarized" }));
        assert_eq!(official["output_config"], json!({ "effort": "high" }));
        assert_eq!(official["stream"], true);
        // Sampling parameters are rejected by this API version, so none may appear.
        assert!(official.get("temperature").is_none());

        let gateway = request_body(&request, false);
        assert!(gateway.get("fallbacks").is_none());
        assert!(gateway["tools"][0].get("eager_input_streaming").is_none());

        let haiku = request_body(&LlmRequest { model: "claude-haiku-5-5".into(), ..request }, true);
        assert!(haiku.get("fallbacks").is_none());
    }

    #[test]
    fn keeps_a_web_search_and_the_citations_on_the_text_that_cites_it() {
        let result = json!({ "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [{ "type": "web_search_result", "url": "https://a.dev", "title": "A", "encrypted_content": "x" }] });
        let citation = json!({ "type": "web_search_result_location", "url": "https://a.dev", "title": "A", "cited_text": "a says", "encrypted_index": "y" });
        let events = [
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{\"query\":\"a\"}" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": result }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "citations_delta", "citation": citation } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "text_delta", "text": "A says so." } }),
            json!({ "type": "content_block_stop", "index": 2 }),
        ];
        let mut accumulator = StreamAccumulator::default();
        for event in &events {
            accumulator.apply(event).unwrap();
        }
        let turn = accumulator.finish();
        // The search's own call is the API's, not one to run here: never reported as a bad tool input.
        assert!(turn.invalid_tool_inputs.is_empty());
        assert_eq!(turn.content[1], result);
        assert_eq!(turn.content[2], json!({ "type": "text", "text": "A says so.", "citations": [citation] }));
    }

    #[test]
    fn sends_a_tool_of_the_apis_own_as_it_is() {
        let request = LlmRequest {
            provider: "anthropic".into(),
            session: "s".into(),
            model: "claude-opus-5-5".into(),
            system: "sys".into(),
            messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })],
            tools: vec![json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": 2 })],
            effort: "low".into(),
        };
        assert_eq!(request_body(&request, true)["tools"], json!([{ "type": "web_search_20260209", "name": "web_search", "max_uses": 2 }]));
    }
}
