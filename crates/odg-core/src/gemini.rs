//! Google Gemini (`generativelanguage.googleapis.com/v1beta`) native streaming client.
//!
//! Converts `odg`'s canonical Anthropic-shaped session messages and JSON Schema tool specs into
//! Gemini `contents`, `systemInstruction`, `tools.functionDeclarations`, and `thinkingConfig`,
//! streaming SSE chunks from `:streamGenerateContent?alt=sse` and preserving `thoughtSignature`
//! on thinking and `functionCall` parts across multi-step tool loops.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::llm::{
    AssistantTurn, Attempt, EndpointSource, LlmError, LlmRequest, StreamDelta, TOOL_PICTURES,
    http_failure, tool_result_parts, with_retries,
};
use crate::sse::SseParser;

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub const MODELS: &[(&str, &str)] = &[
    ("gemini-2.5-pro", "Gemini 2.5 Pro"),
    ("gemini-2.5-flash", "Gemini 2.5 Flash"),
];

const THINKING_INDEX: usize = 0;
const TEXT_INDEX: usize = 1;
const TOOL_BASE_INDEX: usize = 2;

/// Strip JSON Schema keywords that Gemini's `v1beta` `Schema` validator rejects
/// (such as `$schema` and `additionalProperties`).
pub fn sanitize_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if matches!(k.as_str(), "$schema" | "additionalProperties" | "$defs" | "definitions") {
                    continue;
                }
                out.insert(k.clone(), sanitize_schema(v));
            }
            if !out.contains_key("type") && out.contains_key("properties") {
                out.insert("type".to_string(), json!("object"));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_schema).collect()),
        other => other.clone(),
    }
}

fn gemini_tools(tools: &[Value]) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut decl = json!({
                "name": tool["name"],
                "description": tool["description"],
            });
            if tool["input_schema"].is_object() {
                decl["parameters"] = sanitize_schema(&tool["input_schema"]);
            }
            decl
        })
        .collect();
    Some(json!([{ "functionDeclarations": declarations }]))
}

fn supports_thinking(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("2.5") || lower.contains("gemini-3") || lower.contains("thinking")
}

fn thinking_budget(effort: &str) -> u32 {
    match effort {
        "low" => 1024,
        "medium" => 4096,
        "high" => 16_384,
        "xhigh" => 24_576,
        "max" => 32_768,
        _ => 8192,
    }
}

/// Convert `odg` canonical messages into Gemini `contents`.
pub fn gemini_contents(messages: &[Value]) -> Vec<Value> {
    let mut tool_names: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if msg["role"] == "assistant"
            && let Some(blocks) = msg["content"].as_array()
        {
            for block in blocks {
                if block["type"] == "tool_use"
                    && let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str())
                {
                    tool_names.insert(id.to_string(), name.to_string());
                }
            }
        }
    }

    let mut contents = Vec::new();
    let mut prev_assistant_calls: Vec<(String, String)> = Vec::new();
    for msg in messages {
        let blocks = msg["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        if msg["role"] == "assistant" {
            prev_assistant_calls.clear();
            let mut parts = Vec::new();
            for block in blocks {
                match block["type"].as_str() {
                    Some("thinking") => {
                        if let Some(thinking) = block["thinking"].as_str().filter(|t| !t.is_empty()) {
                            let mut part = json!({ "text": thinking, "thought": true });
                            if let Some(sig) = block["signature"].as_str().filter(|s| !s.is_empty()) {
                                part["thoughtSignature"] = json!(sig);
                            }
                            parts.push(part);
                        }
                    }
                    Some("text") => {
                        if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            let mut part = json!({ "text": text });
                            if let Some(sig) = block["thought_signature"].as_str().filter(|s| !s.is_empty()) {
                                part["thoughtSignature"] = json!(sig);
                            }
                            parts.push(part);
                        }
                    }
                    Some("tool_use") => {
                        let call_id = block["id"].as_str().unwrap_or_default().to_string();
                        let call_name = block["name"].as_str().unwrap_or_default().to_string();
                        prev_assistant_calls.push((call_id, call_name));
                        let args = if block["input"].is_object() {
                            block["input"].clone()
                        } else {
                            json!({})
                        };
                        let mut part = json!({
                            "functionCall": {
                                "name": block["name"],
                                "args": args,
                            }
                        });
                        if let Some(sig) = block["thought_signature"].as_str().filter(|s| !s.is_empty()) {
                            part["thoughtSignature"] = json!(sig);
                        }
                        parts.push(part);
                    }
                    _ => {}
                }
            }
            if !parts.is_empty() {
                contents.push(json!({ "role": "model", "parts": parts }));
            }
        } else {
            let mut parts = Vec::new();
            let mut returned_images: Vec<Value> = Vec::new();
            let mut used_prev = vec![false; prev_assistant_calls.len()];
            let mut result_idx = 0usize;
            for block in blocks {
                match block["type"].as_str() {
                    Some("tool_result") => {
                        let id = block["tool_use_id"].as_str().unwrap_or_default();
                        let adjacent_by_id = (!id.is_empty())
                            .then(|| {
                                prev_assistant_calls
                                    .iter()
                                    .enumerate()
                                    .find(|(i, (cid, _))| !used_prev[*i] && cid == id)
                                    .map(|(i, (_, name))| {
                                        used_prev[i] = true;
                                        name.clone()
                                    })
                            })
                            .flatten();
                        let name = adjacent_by_id
                            .or_else(|| {
                                if id.is_empty()
                                    && result_idx < prev_assistant_calls.len()
                                    && !used_prev[result_idx]
                                {
                                    used_prev[result_idx] = true;
                                    Some(prev_assistant_calls[result_idx].1.clone())
                                } else {
                                    None
                                }
                            })
                            .or_else(|| tool_names.get(id).cloned())
                            .or_else(|| {
                                if result_idx < prev_assistant_calls.len() && !used_prev[result_idx] {
                                    used_prev[result_idx] = true;
                                    Some(prev_assistant_calls[result_idx].1.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| "tool".to_string());
                        result_idx += 1;
                        let (words, pictures) = tool_result_parts(block);
                        let is_error = block["is_error"].as_bool().unwrap_or(false);
                        parts.push(json!({
                            "functionResponse": {
                                "name": name,
                                "response": {
                                    "output": words,
                                    "is_error": is_error,
                                }
                            }
                        }));
                        returned_images.extend(pictures);
                    }
                    Some("image") => {
                        let source = &block["source"];
                        parts.push(json!({
                            "inlineData": {
                                "mimeType": source["media_type"],
                                "data": source["data"],
                            }
                        }));
                    }
                    Some("text") => {
                        if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            parts.push(json!({ "text": text }));
                        }
                    }
                    _ => {}
                }
            }
            prev_assistant_calls.clear();
            if !returned_images.is_empty() {
                parts.push(json!({ "text": TOOL_PICTURES }));
                for pic in returned_images {
                    let source = &pic["source"];
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": source["media_type"],
                            "data": source["data"],
                        }
                    }));
                }
            }
            if !parts.is_empty() {
                contents.push(json!({ "role": "user", "parts": parts }));
            }
        }
    }
    contents
}

/// Build the JSON body for `:streamGenerateContent?alt=sse`.
pub fn request_body(request: &LlmRequest) -> Value {
    let mut body = json!({
        "contents": gemini_contents(&request.messages),
    });
    if !request.system.trim().is_empty() {
        body["systemInstruction"] = json!({
            "parts": [{ "text": request.system }]
        });
    }
    if let Some(tools) = gemini_tools(&request.tools) {
        body["tools"] = tools;
    }
    if supports_thinking(&request.model) {
        body["generationConfig"] = json!({
            "thinkingConfig": {
                "includeThoughts": true,
                "thinkingBudget": thinking_budget(&request.effort),
            }
        });
    }
    body
}

#[derive(Default)]
struct GeminiCall {
    id: String,
    name: String,
    args: Value,
    thought_signature: Option<String>,
}

/// Accumulates Gemini SSE `GenerateContentResponse` frames into one [`AssistantTurn`].
#[derive(Default)]
pub struct GeminiAccumulator {
    thinking: String,
    thinking_sig: Option<String>,
    text: String,
    text_sig: Option<String>,
    calls: Vec<GeminiCall>,
    finish_reason: Option<String>,
    model: String,
    usage: Value,
}

impl GeminiAccumulator {
    pub fn apply(&mut self, chunk: &Value) -> Result<Vec<StreamDelta>, LlmError> {
        if chunk["error"].is_object() {
            let err = &chunk["error"];
            let status = err["code"].as_u64().map(|c| c as u16);
            let status_str = err["status"].as_str().unwrap_or("");
            let retryable = matches!(status, Some(408 | 429 | 500 | 502 | 503 | 504))
                || matches!(
                    status_str,
                    "RESOURCE_EXHAUSTED" | "UNAVAILABLE" | "INTERNAL" | "DEADLINE_EXCEEDED"
                );
            let kind = if status_str.is_empty() {
                "gemini_error".to_string()
            } else {
                status_str.to_string()
            };
            let message = err["message"].as_str().unwrap_or("Gemini stream error").to_string();
            return Err(LlmError::Api {
                status,
                kind,
                message,
                retryable,
            });
        }
        if let Some(ver) = chunk["modelVersion"].as_str().filter(|v| !v.is_empty()) {
            self.model = ver.to_string();
        }
        if chunk["usageMetadata"].is_object() {
            self.usage = chunk["usageMetadata"].clone();
        }
        if let Some(block_reason) = chunk["promptFeedback"]["blockReason"].as_str() {
            self.finish_reason = Some(format!("BLOCKED:{block_reason}"));
        }

        let mut deltas = Vec::new();
        if let Some(candidates) = chunk["candidates"].as_array()
            && let Some(first) = candidates.first()
        {
            if let Some(reason) = first["finishReason"].as_str() {
                self.finish_reason = Some(reason.to_string());
            }
            if let Some(parts) = first["content"]["parts"].as_array() {
                for part in parts {
                    let sig = part["thoughtSignature"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    if part["thought"].as_bool() == Some(true) {
                        if let Some(t) = part["text"].as_str().filter(|s| !s.is_empty()) {
                            self.thinking.push_str(t);
                            deltas.push(StreamDelta::ThinkingDelta {
                                index: THINKING_INDEX,
                                text: t.to_string(),
                            });
                        }
                        if sig.is_some() {
                            self.thinking_sig = sig;
                        }
                    } else if part["functionCall"].is_object() {
                        let fc = &part["functionCall"];
                        let name = fc["name"].as_str().unwrap_or_default().to_string();
                        let args = if fc["args"].is_object() {
                            fc["args"].clone()
                        } else {
                            json!({})
                        };
                        let call_idx = self.calls.len();
                        let stream_idx = TOOL_BASE_INDEX + call_idx;
                        let id = fc["id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                format!("gemini_call_{}_{call_idx}", uuid::Uuid::now_v7().simple())
                            });
                        let partial = args.to_string();
                        deltas.push(StreamDelta::ToolUseStart {
                            index: stream_idx,
                            id: id.clone(),
                            name: name.clone(),
                        });
                        deltas.push(StreamDelta::ToolInputDelta {
                            index: stream_idx,
                            partial,
                        });
                        deltas.push(StreamDelta::BlockStop { index: stream_idx });
                        self.calls.push(GeminiCall {
                            id,
                            name,
                            args,
                            thought_signature: sig,
                        });
                    } else if let Some(t) = part["text"].as_str() {
                        if !t.is_empty() {
                            self.text.push_str(t);
                            deltas.push(StreamDelta::TextDelta {
                                index: TEXT_INDEX,
                                text: t.to_string(),
                            });
                        }
                        if sig.is_some() {
                            self.text_sig = sig;
                        }
                    }
                }
            }
        }
        Ok(deltas)
    }

    pub fn finished(&self) -> bool {
        self.finish_reason.is_some()
    }

    pub fn finish(self, requested_model: &str) -> Result<AssistantTurn, LlmError> {
        let stop_reason = if !self.calls.is_empty() {
            "tool_use"
        } else {
            match self.finish_reason.as_deref() {
                Some("MAX_TOKENS") => "max_tokens",
                Some(r)
                    if r.starts_with("BLOCKED:")
                        || matches!(
                            r,
                            "SAFETY"
                                | "RECITATION"
                                | "PROHIBITED_CONTENT"
                                | "BLOCKLIST"
                                | "SPII"
                                | "IMAGE_SAFETY"
                        ) =>
                {
                    "refusal"
                }
                Some("MALFORMED_FUNCTION_CALL") => {
                    return Err(LlmError::Protocol(
                        "Gemini reported MALFORMED_FUNCTION_CALL".to_string(),
                    ));
                }
                _ => "end_turn",
            }
        };

        let mut turn = AssistantTurn {
            stop_reason: Some(stop_reason.to_string()),
            model: if self.model.is_empty() {
                requested_model.to_string()
            } else {
                self.model
            },
            usage: if self.usage.is_null() { json!({}) } else { self.usage },
            ..Default::default()
        };

        if !self.thinking.is_empty() {
            let mut block = json!({ "type": "thinking", "thinking": self.thinking });
            if let Some(sig) = self.thinking_sig {
                block["signature"] = json!(sig);
            }
            turn.content.push(block);
        }
        if !self.text.is_empty() {
            let mut block = json!({ "type": "text", "text": self.text });
            if let Some(sig) = self.text_sig {
                block["thought_signature"] = json!(sig);
            }
            turn.content.push(block);
        }
        for call in self.calls {
            let mut block = json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.args,
            });
            if let Some(sig) = call.thought_signature {
                block["thought_signature"] = json!(sig);
            }
            turn.content.push(block);
        }
        Ok(turn)
    }
}

pub struct GeminiClient {
    http: reqwest::Client,
    endpoints: EndpointSource,
}

impl GeminiClient {
    pub fn new(endpoints: EndpointSource) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| LlmError::Network(e.to_string()))?;
        Ok(Self { http, endpoints })
    }

    pub async fn stream(
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

    async fn attempt(
        &self,
        request: &LlmRequest,
        deltas: &UnboundedSender<StreamDelta>,
        cancel: &CancellationToken,
        forwarded: &mut bool,
    ) -> Attempt {
        let endpoint = (self.endpoints)(&request.provider);
        let base = endpoint.base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err((
                LlmError::NotConfigured(format!(
                    "no address is set for provider `{}`",
                    request.provider
                )),
                None,
            ));
        }
        let key = endpoint
            .key
            .filter(|k| !k.trim().is_empty())
            .ok_or((LlmError::MissingKey, None))?;
        let model_id = request.model.trim().trim_start_matches("models/");
        let url = format!("{base}/models/{model_id}:streamGenerateContent?alt=sse");
        let send = self
            .http
            .post(url)
            .header("x-goog-api-key", key.trim())
            .json(&request_body(request))
            .send();
        let response = tokio::select! {
            res = send => res.map_err(|e| (LlmError::Network(e.to_string()), None))?,
            () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
        };
        if !response.status().is_success() {
            return Err(http_failure(response).await);
        }

        let mut parser = SseParser::default();
        let mut accumulator = GeminiAccumulator::default();
        let mut stream = response.bytes_stream();
        'read: loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => chunk,
                () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
            };
            let chunk = match chunk {
                Err(_) => return Err((LlmError::Network("the stream went silent".to_string()), None)),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err((LlmError::Network(e.to_string()), None)),
                Ok(Some(Ok(bytes))) => bytes,
            };
            for event in parser.feed(&chunk) {
                let raw = event.data.trim();
                if raw.is_empty() || raw == "[DONE]" {
                    if raw == "[DONE]" {
                        break 'read;
                    }
                    continue;
                }
                let data: Value = serde_json::from_str(raw)
                    .map_err(|e| (LlmError::Protocol(format!("Gemini SSE chunk is not JSON: {e}")), None))?;
                for delta in accumulator.apply(&data).map_err(|e| (e, None))? {
                    *forwarded = true;
                    let _ = deltas.send(delta);
                }
            }
        }
        if !accumulator.finished() {
            return Err((
                LlmError::Network("the stream ended before Gemini sent a finishReason".to_string()),
                None,
            ));
        }
        accumulator.finish(model_id).map_err(|e| (e, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Endpoint;
    use crate::testing::serve;
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn converts_messages_tools_and_preserves_thought_signatures() {
        let req = LlmRequest {
            provider: "gemini".into(),
            session: "s1".into(),
            model: "gemini-2.5-pro".into(),
            system: "Be concise.".into(),
            messages: vec![
                json!({ "role": "user", "content": [{ "type": "text", "text": "Read Cargo.toml" }] }),
                json!({
                    "role": "assistant",
                    "content": [
                        { "type": "thinking", "thinking": "Need read_file", "signature": "sig-think-1" },
                        { "type": "tool_use", "id": "call_0", "name": "read_file", "input": { "path": "Cargo.toml" }, "thought_signature": "sig-call-1" }
                    ]
                }),
                json!({
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "call_0", "content": "[package]\nname = \"demo\"", "is_error": false }
                    ]
                }),
            ],
            tools: vec![json!({
                "name": "read_file",
                "description": "Read file",
                "input_schema": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "additionalProperties": false
                }
            })],
            effort: "high".into(),
        };

        let body = request_body(&req);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be concise.");
        let params = &body["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(params.get("additionalProperties").is_none());
        assert!(params.get("$schema").is_none());
        assert_eq!(body["generationConfig"]["thinkingConfig"]["thinkingBudget"], 16_384);

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[1]["parts"][0]["thought"], true);
        assert_eq!(contents[1]["parts"][0]["thoughtSignature"], "sig-think-1");
        assert_eq!(contents[1]["parts"][1]["functionCall"]["name"], "read_file");
        assert_eq!(contents[1]["parts"][1]["thoughtSignature"], "sig-call-1");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read_file");
    }

    #[tokio::test]
    async fn streams_gemini_sse_thoughts_tool_calls_and_usage() {
        let sse_chunks = [
            json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "text": "Checking workspace...", "thought": true }] }
                }],
                "modelVersion": "gemini-2.5-pro"
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{
                            "functionCall": { "name": "find_files", "args": { "glob": "*.rs" } },
                            "thoughtSignature": "opaque-sig-42"
                        }]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 500,
                    "cachedContentTokenCount": 200,
                    "candidatesTokenCount": 30,
                    "thoughtsTokenCount": 45
                }
            }),
        ];
        let payload: String = sse_chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{payload}"
        );
        let (addr, recorded) = serve(vec![response]).await;
        let client = GeminiClient::new(Arc::new(move |_| Endpoint {
            base_url: addr.clone(),
            key: Some("gem-test-key".into()),
        }))
        .unwrap();

        let (tx, mut rx) = unbounded_channel();
        let turn = client
            .stream(
                LlmRequest {
                    provider: "gemini".into(),
                    session: "s1".into(),
                    model: "gemini-2.5-pro".into(),
                    system: String::new(),
                    messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "find rust files" }] })],
                    tools: vec![],
                    effort: "medium".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(turn.model, "gemini-2.5-pro");
        assert_eq!(turn.content[0]["type"], "thinking");
        assert_eq!(turn.content[1]["type"], "tool_use");
        assert_eq!(turn.content[1]["name"], "find_files");
        assert_eq!(turn.content[1]["thought_signature"], "opaque-sig-42");

        let mut seen = Vec::new();
        while let Ok(d) = rx.try_recv() {
            seen.push(d);
        }
        assert!(seen.iter().any(|d| matches!(d, StreamDelta::ThinkingDelta { .. })));
        assert!(seen.iter().any(|d| matches!(d, StreamDelta::ToolUseStart { name, .. } if name == "find_files")));

        let raw_reqs = recorded.await.unwrap();
        assert!(raw_reqs[0].to_lowercase().contains("x-goog-api-key: gem-test-key"));
        assert!(raw_reqs[0].starts_with("POST /models/gemini-2.5-pro:streamGenerateContent?alt=sse "));
    }

    #[tokio::test]
    async fn truncated_stream_without_finish_reason_triggers_retry_and_resets() {
        let cut_off = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {}\n\n",
            json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "text": "half sentence..." }] }
                }],
                "modelVersion": "gemini-2.5-pro"
            })
        );
        let complete = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {}\n\n",
            json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "text": "full recovered answer" }] },
                    "finishReason": "STOP"
                }],
                "modelVersion": "gemini-2.5-pro"
            })
        );
        let (addr, recorded) = serve(vec![cut_off, complete]).await;
        let client = GeminiClient::new(Arc::new(move |_| Endpoint {
            base_url: addr.clone(),
            key: Some("gem-test-key".into()),
        }))
        .unwrap();

        let (tx, mut rx) = unbounded_channel();
        let turn = client
            .stream(
                LlmRequest {
                    provider: "gemini".into(),
                    session: "s1".into(),
                    model: "gemini-2.5-pro".into(),
                    system: String::new(),
                    messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hello" }] })],
                    tools: vec![],
                    effort: "low".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(turn.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(turn.content, vec![json!({ "type": "text", "text": "full recovered answer" })]);

        let mut deltas = Vec::new();
        while let Ok(d) = rx.try_recv() {
            deltas.push(d);
        }
        assert!(
            deltas.contains(&StreamDelta::Reset),
            "cut-off stream must emit Reset before retrying: {deltas:?}"
        );
        assert_eq!(recorded.await.unwrap().len(), 2);
    }

    #[test]
    fn multi_step_tool_calls_generate_unique_ids_and_preserve_legacy_pairing() {
        let mut step1 = GeminiAccumulator::default();
        step1
            .apply(&json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "functionCall": { "name": "read_file", "args": { "path": "a.rs" } } }] },
                    "finishReason": "STOP"
                }]
            }))
            .unwrap();
        let turn1 = step1.finish("gemini-2.5-pro").unwrap();

        let mut step2 = GeminiAccumulator::default();
        step2
            .apply(&json!({
                "candidates": [{
                    "content": { "role": "model", "parts": [{ "functionCall": { "name": "edit_file", "args": { "path": "a.rs" } } }] },
                    "finishReason": "STOP"
                }]
            }))
            .unwrap();
        let turn2 = step2.finish("gemini-2.5-pro").unwrap();

        let id1 = turn1.content[0]["id"].as_str().unwrap();
        let id2 = turn2.content[0]["id"].as_str().unwrap();
        assert_ne!(id1, id2);
        assert!(id1.len() <= 64 && id2.len() <= 64);

        // Legacy v0.8.0 sessions stored duplicate `gemini_call_0` across steps: adjacent pairing keeps names distinct.
        let legacy_messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "fix a.rs" }] }),
            json!({
                "role": "assistant",
                "content": [{ "type": "tool_use", "id": "gemini_call_0", "name": "read_file", "input": { "path": "a.rs" } }]
            }),
            json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": "gemini_call_0", "content": "fn main() {}", "is_error": false }]
            }),
            json!({
                "role": "assistant",
                "content": [{ "type": "tool_use", "id": "gemini_call_0", "name": "edit_file", "input": { "path": "a.rs" } }]
            }),
            json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": "gemini_call_0", "content": "edited", "is_error": false }]
            }),
        ];
        let contents = gemini_contents(&legacy_messages);
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read_file");
        assert_eq!(contents[4]["parts"][0]["functionResponse"]["name"], "edit_file");
    }

    #[tokio::test]
    async fn stream_400_error_fails_immediately_without_retries() {
        let err_stream = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {}\n\n",
            json!({
                "error": {
                    "code": 400,
                    "status": "INVALID_ARGUMENT",
                    "message": "Request payload exceeds context window limit"
                }
            })
        );
        let (addr, recorded) = serve(vec![err_stream]).await;
        let client = GeminiClient::new(Arc::new(move |_| Endpoint {
            base_url: addr.clone(),
            key: Some("gem-test-key".into()),
        }))
        .unwrap();

        let (tx, _rx) = unbounded_channel();
        let err = client
            .stream(
                LlmRequest {
                    provider: "gemini".into(),
                    session: "s1".into(),
                    model: "gemini-2.5-pro".into(),
                    system: String::new(),
                    messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "too big" }] })],
                    tools: vec![],
                    effort: "low".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert!(!err.retryable());
        assert!(err.context_overflow());
        assert_eq!(recorded.await.unwrap().len(), 1);
    }
}
