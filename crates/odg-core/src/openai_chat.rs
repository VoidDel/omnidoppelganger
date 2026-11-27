//! OpenAI-style Chat Completions over raw HTTP with server-sent events, for
//! DeepSeek, Kimi, GLM, and other compatible servers.
//!
//! The session log keeps Anthropic-shaped content blocks for every provider:
//! the reasoning becomes a `thinking` block, the reply a `text` block, and
//! each call a `tool_use` block that also keeps the raw `arguments` string.
//! [`chat_messages`] turns those back into Chat Completions messages. All
//! three named providers want the reasoning returned unchanged in
//! `reasoning_content` on later requests (DeepSeek rejects a request with
//! tools that leaves it out), so it is always sent back.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::llm::{
    AssistantTurn, Attempt, EndpointSource, LlmError, LlmRequest, StreamDelta, http_failure, with_retries,
};
use crate::providers::{Flavor, Protocol, effort_for};
use crate::sse::SseParser;

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Stream fragment indices: the reasoning, the reply, then one per tool call.
const THINKING: usize = 0;
const TEXT: usize = 1;
const TOOLS: usize = 2;

/// Tool definitions as Chat Completions functions.
fn chat_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| json!({ "type": "function", "function": { "name": tool["name"], "description": tool["description"], "parameters": tool["input_schema"] } }))
        .collect()
}

/// The request's conversation as Chat Completions messages: the system
/// prompt, then each message of the log. A user message's tool results come
/// first, one `tool` message each, and its text after them.
pub fn chat_messages(system: &str, messages: &[Value]) -> Vec<Value> {
    chat_messages_inner(system, messages, false)
}

fn chat_messages_inner(system: &str, messages: &[Value], require_reasoning_field: bool) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for message in messages {
        let blocks = message["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        let of = |kind: &'static str| blocks.iter().filter(move |block| block["type"] == kind);
        if message["role"] == "assistant" {
            let reasoning: String = of("thinking").filter_map(|block| block["thinking"].as_str()).collect();
            let text: Vec<&str> = of("text").filter_map(|block| block["text"].as_str()).collect();
            let calls: Vec<Value> = of("tool_use")
                .map(|block| {
                    let arguments = block["arguments"].as_str().map(str::to_string).unwrap_or_else(|| block["input"].to_string());
                    json!({ "id": block["id"], "type": "function", "function": { "name": block["name"], "arguments": arguments } })
                })
                .collect();
            let mut assistant = json!({ "role": "assistant", "content": text.join("\n\n") });
            if !reasoning.is_empty() || (require_reasoning_field && !calls.is_empty()) {
                assistant["reasoning_content"] = json!(reasoning);
            }
            if !calls.is_empty() {
                assistant["tool_calls"] = json!(calls);
            }
            out.push(assistant);
        } else {
            // A tool message carries words only: the pictures a tool returned follow in the user's turn.
            let mut returned: Vec<Value> = Vec::new();
            for block in of("tool_result") {
                let (words, pictures) = crate::llm::tool_result_parts(block);
                out.push(json!({ "role": "tool", "tool_call_id": block["tool_use_id"], "content": words }));
                returned.extend(pictures);
            }
            let mut text: Vec<&str> = of("text").filter_map(|block| block["text"].as_str()).collect();
            if !returned.is_empty() {
                text.insert(0, crate::llm::TOOL_PICTURES);
            }
            let images: Vec<Value> = returned
                .iter()
                .chain(of("image"))
                .map(|block| {
                    let source = &block["source"];
                    let url = format!(
                        "data:{};base64,{}",
                        source["media_type"].as_str().unwrap_or_default(),
                        source["data"].as_str().unwrap_or_default()
                    );
                    json!({ "type": "image_url", "image_url": { "url": url } })
                })
                .collect();
            if !images.is_empty() {
                // With pictures the content is a list of parts; a plain string cannot carry them.
                let words = (!text.is_empty()).then(|| json!({ "type": "text", "text": text.join("\n\n") }));
                out.push(json!({ "role": "user", "content": images.into_iter().chain(words).collect::<Vec<_>>() }));
            } else if !text.is_empty() {
                out.push(json!({ "role": "user", "content": text.join("\n\n") }));
            }
        }
    }
    out
}

/// The JSON body of a streaming Chat Completions request. No output limit is
/// sent: each provider's default already leaves room for long reasoning.
pub fn request_body(flavor: Flavor, request: &LlmRequest) -> Value {
    let effort = effort_for(Protocol::Chat(flavor), &request.effort);
    let require_reasoning_field = matches!(flavor, Flavor::DeepSeek | Flavor::Kimi) && effort.is_some();
    let mut body = json!({
        "model": request.model,
        "stream": true,
        "messages": chat_messages_inner(&request.system, &request.messages, require_reasoning_field),
        "tools": chat_tools(&request.tools),
    });
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort);
    }
    match flavor {
        // OpenAI-compatible gateways, DeepSeek, and Kimi send usage in the final chunk when asked via `stream_options`.
        Flavor::DeepSeek | Flavor::Generic => body["stream_options"] = json!({ "include_usage": true }),
        Flavor::Kimi => {
            body["stream_options"] = json!({ "include_usage": true });
            // Kimi recommends one cache key per session for coding agents.
            body["prompt_cache_key"] = json!(request.session);
        }
        // GLM puts usage in the last chunk by default.
        Flavor::Glm => {}
    }
    // A request without tools (a summary written from a transcript) leaves the field out: an empty list is an error to some servers.
    if request.tools.is_empty() {
        body.as_object_mut().expect("the body is an object").remove("tools");
    }
    body
}

#[derive(Default)]
struct Call {
    /// The `index` the provider gave the call, when it gives one.
    index: Option<u64>,
    id: String,
    has_provider_id: bool,
    name: String,
    arguments: String,
    /// Arguments sent as a JSON object rather than a string (GLM may do this).
    object: Option<Value>,
    started: bool,
}

/// Rebuilds one assistant message from Chat Completions stream chunks.
#[derive(Default)]
pub struct ChatAccumulator {
    reasoning: String,
    text: String,
    calls: Vec<Call>,
    finish_reason: Option<String>,
    model: String,
    usage: Value,
}

impl ChatAccumulator {
    /// Apply one chunk. Returns the fragments to show, or the error the stream reported.
    pub fn apply(&mut self, chunk: &Value) -> Result<Vec<StreamDelta>, LlmError> {
        if chunk["error"].is_object() {
            let error = &chunk["error"];
            let kind = error["type"]
                .as_str()
                .or_else(|| error["code"].as_str())
                .unwrap_or("stream_error")
                .to_string();
            let message = error["message"].as_str().unwrap_or("no message").to_string();
            // An error in the middle of a stream is the server's trouble, not the request's.
            return Err(LlmError::Api {
                status: None,
                kind,
                message,
                retryable: true,
            });
        }
        if let Some(model) = chunk["model"].as_str().filter(|model| !model.is_empty()) {
            self.model = model.to_string();
        }
        if chunk["usage"].is_object() {
            self.usage = chunk["usage"].clone();
        }
        let mut deltas = Vec::new();
        let Some(choice) = chunk["choices"].as_array().and_then(|choices| choices.first()) else {
            return Ok(deltas);
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["reasoning_content"].as_str().filter(|text| !text.is_empty()) {
            self.reasoning.push_str(text);
            deltas.push(StreamDelta::ThinkingDelta {
                index: THINKING,
                text: text.to_string(),
            });
        }
        if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
            self.text.push_str(text);
            deltas.push(StreamDelta::TextDelta {
                index: TEXT,
                text: text.to_string(),
            });
        }
        for fragment in delta["tool_calls"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let position = self.call_for(fragment);
            let call = &mut self.calls[position];
            if let Some(id) = fragment["id"].as_str().filter(|id| !id.is_empty()) {
                call.id = id.to_string();
                call.has_provider_id = true;
            }
            if let Some(name) = fragment["function"]["name"].as_str().filter(|name| !name.is_empty()) {
                call.name = name.to_string();
            }
            if !call.started && !call.name.is_empty() {
                call.started = true;
                deltas.push(StreamDelta::ToolUseStart {
                    index: TOOLS + position,
                    id: call.id.clone(),
                    name: call.name.clone(),
                });
            }
            match &fragment["function"]["arguments"] {
                Value::String(partial) if !partial.is_empty() => {
                    call.arguments.push_str(partial);
                    deltas.push(StreamDelta::ToolInputDelta {
                        index: TOOLS + position,
                        partial: partial.clone(),
                    });
                }
                object @ Value::Object(_) => call.object = Some(object.clone()),
                _ => {}
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_string());
        }
        Ok(deltas)
    }

    /// The call a fragment continues, adding one when it starts a new call.
    /// Fragments are matched by `index`; without one, a new `id` starts a call
    /// and anything else continues the last.
    fn call_for(&mut self, fragment: &Value) -> usize {
        let index = fragment["index"].as_u64();
        let found = match index {
            Some(index) => self.calls.iter().position(|call| call.index == Some(index)),
            None => {
                let new_id = fragment["id"].as_str().filter(|id| !id.is_empty());
                match (self.calls.last(), new_id) {
                    (Some(last), Some(id)) if last.has_provider_id && last.id != id => None,
                    (Some(last), None)
                        if !last.has_provider_id
                            && last.started
                            && fragment["function"]["name"].as_str().is_some_and(|n| !n.is_empty()) =>
                    {
                        None
                    }
                    (Some(_), _) => Some(self.calls.len() - 1),
                    (None, _) => None,
                }
            }
        };
        found.unwrap_or_else(|| {
            let next_pos = self.calls.len();
            let id = format!("chat_call_{}_{next_pos}", uuid::Uuid::now_v7().simple());
            self.calls.push(Call {
                index,
                id,
                ..Default::default()
            });
            next_pos
        })
    }

    /// Whether a chunk with a finish reason arrived: a stream that ends without one was cut off.
    pub fn finished(&self) -> bool {
        self.finish_reason.is_some()
    }

    /// The complete message, or an error when the provider ended the stream on one.
    pub fn finish(self) -> Result<AssistantTurn, LlmError> {
        let reason = self.finish_reason.unwrap_or_default();
        let stop_reason = match reason.as_str() {
            "stop" => "end_turn",
            "tool_calls" | "function_call" => "tool_use",
            "length" => "max_tokens",
            "content_filter" | "sensitive" => "refusal",
            // These end a stream that failed on the server's side: worth another try.
            "network_error" | "insufficient_system_resource" | "aborted" => {
                return Err(LlmError::Network(format!(
                    "the provider stopped the response: {reason}"
                )));
            }
            "model_context_window_exceeded" => {
                return Err(LlmError::Api {
                    status: None,
                    kind: reason.clone(),
                    message: "the conversation is longer than the model's context window".to_string(),
                    retryable: false,
                });
            }
            other => other,
        };
        let mut turn = AssistantTurn {
            stop_reason: Some(stop_reason.to_string()),
            model: self.model,
            usage: if self.usage.is_null() { json!({}) } else { self.usage },
            ..Default::default()
        };
        if !self.reasoning.is_empty() {
            turn.content
                .push(json!({ "type": "thinking", "thinking": self.reasoning }));
        }
        if !self.text.is_empty() {
            turn.content.push(json!({ "type": "text", "text": self.text }));
        }
        let mut invalid = HashMap::new();
        for (position, call) in self.calls.into_iter().enumerate() {
            // Every call needs a globally unique id for its result to answer across steps.
            let id = if call.id.is_empty() {
                format!("chat_call_{}_{position}", uuid::Uuid::now_v7().simple())
            } else {
                call.id
            };
            let (input, arguments) = match call.object {
                Some(object) => (object.clone(), object.to_string()),
                None if call.arguments.trim().is_empty() => (json!({}), "{}".to_string()),
                None => match serde_json::from_str::<Value>(&call.arguments) {
                    Ok(input) if input.is_object() => (input, call.arguments),
                    _ => {
                        invalid.insert(id.clone(), call.arguments.clone());
                        (json!({}), call.arguments)
                    }
                },
            };
            turn.content.push(
                json!({ "type": "tool_use", "id": id, "name": call.name, "input": input, "arguments": arguments }),
            );
        }
        turn.invalid_tool_inputs = invalid;
        Ok(turn)
    }
}

pub struct ChatClient {
    http: reqwest::Client,
    endpoints: EndpointSource,
}

impl ChatClient {
    pub fn new(endpoints: EndpointSource) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| LlmError::Network(error.to_string()))?;
        Ok(Self { http, endpoints })
    }

    pub async fn stream(
        &self,
        flavor: Flavor,
        request: LlmRequest,
        deltas: UnboundedSender<StreamDelta>,
        cancel: CancellationToken,
    ) -> Result<AssistantTurn, LlmError> {
        with_retries(&deltas, &cancel, || {
            let (request, deltas, cancel) = (&request, &deltas, &cancel);
            async move {
                let mut forwarded = false;
                let outcome = self.attempt(flavor, request, deltas, cancel, &mut forwarded).await;
                (outcome, forwarded)
            }
        })
        .await
    }

    async fn attempt(
        &self,
        flavor: Flavor,
        request: &LlmRequest,
        deltas: &UnboundedSender<StreamDelta>,
        cancel: &CancellationToken,
        forwarded: &mut bool,
    ) -> Attempt {
        let endpoint = (self.endpoints)(&request.provider);
        if endpoint.base_url.trim().is_empty() {
            return Err((
                LlmError::NotConfigured(format!("no address is set for provider `{}`", request.provider)),
                None,
            ));
        }
        let key = endpoint
            .key
            .filter(|key| !key.trim().is_empty())
            .ok_or((LlmError::MissingKey, None))?;
        let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));
        let mut body = request_body(flavor, request);
        let mut req = self.http.post(&url).bearer_auth(key.trim()).json(&body);
        if flavor == Flavor::Kimi && url.to_ascii_lowercase().contains("/coding") {
            let dev_id = crate::kimi::device_id(&crate::settings::home());
            req = crate::kimi::apply_kimi_headers(req, &dev_id);
        }
        let send = req.send();
        let mut response = tokio::select! {
            response = send => response.map_err(|error| (LlmError::Network(error.to_string()), None))?,
            () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
        };
        if !response.status().is_success() {
            if response.status().as_u16() == 400 && !*forwarded && body.get("stream_options").is_some() {
                match check_bad_request_stream_options(response).await {
                    Ok(()) => {
                        body.as_object_mut()
                            .expect("the body is an object")
                            .remove("stream_options");
                        let retry_send = self.http.post(&url).bearer_auth(key.trim()).json(&body).send();
                        response = tokio::select! {
                            response = retry_send => response.map_err(|error| (LlmError::Network(error.to_string()), None))?,
                            () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
                        };
                        if !response.status().is_success() {
                            return Err(http_failure(response).await);
                        }
                    }
                    Err(failure) => return Err(failure),
                }
            } else {
                return Err(http_failure(response).await);
            }
        }

        let mut parser = SseParser::default();
        let mut accumulator = ChatAccumulator::default();
        let mut stream = response.bytes_stream();
        'read: loop {
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
                let raw = event.data.trim();
                if raw.is_empty() {
                    continue;
                }
                if raw == "[DONE]" {
                    break 'read;
                }
                let data: Value = serde_json::from_str(raw)
                    .map_err(|error| (LlmError::Protocol(format!("a chunk is not JSON: {error}")), None))?;
                for delta in accumulator.apply(&data).map_err(|error| (error, None))? {
                    *forwarded = true;
                    let _ = deltas.send(delta);
                }
            }
        }
        if !accumulator.finished() {
            return Err((
                LlmError::Network("the stream ended before the message was complete".to_string()),
                None,
            ));
        }
        accumulator.finish().map_err(|error| (error, None))
    }
}

/// Inspect a 400 HTTP response when `stream_options` was sent: return `Ok(())` if the error body
/// mentions `stream_options` or `include_usage` (so the caller can strip it and resend once),
/// or reconstruct the exact `(LlmError, Option<Duration>)` failure otherwise.
async fn check_bad_request_stream_options(response: reqwest::Response) -> Result<(), (LlmError, Option<Duration>)> {
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let retry_after = header("retry-after")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64);
    let should_retry = header("x-should-retry");
    let body = response.text().await.unwrap_or_default();
    let lower = body.to_ascii_lowercase();
    if lower.contains("stream_options") || lower.contains("include_usage") {
        return Ok(());
    }
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let error = &parsed["error"];
    let text = |value: &Value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    let kind = text(&error["type"])
        .or_else(|| text(&error["code"]))
        .unwrap_or_else(|| "http_error".to_string());
    let message = text(&error["message"]).unwrap_or_else(|| {
        if body.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            body.chars().take(500).collect()
        }
    });
    let retryable = should_retry.as_deref() == Some("true");
    Err((
        LlmError::Api {
            status: Some(status),
            kind,
            message,
            retryable,
        },
        retry_after,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay(chunks: &[Value]) -> (Result<AssistantTurn, LlmError>, Vec<StreamDelta>) {
        let mut accumulator = ChatAccumulator::default();
        let mut deltas = Vec::new();
        for chunk in chunks {
            deltas.extend(accumulator.apply(chunk).unwrap());
        }
        assert!(accumulator.finished());
        (accumulator.finish(), deltas)
    }

    fn delta(delta: Value) -> Value {
        json!({ "model": "deepseek-v4-pro", "choices": [{ "index": 0, "delta": delta, "finish_reason": null }] })
    }

    #[test]
    fn rebuilds_reasoning_text_and_tool_calls_streamed_in_fragments() {
        let (turn, deltas) = replay(&[
            delta(json!({ "role": "assistant", "reasoning_content": "先看" })),
            delta(json!({ "reasoning_content": "文件。" })),
            delta(json!({ "content": "我读一下。" })),
            delta(
                json!({ "tool_calls": [{ "index": 0, "id": "call_a", "type": "function", "function": { "name": "read_file", "arguments": "" } }] }),
            ),
            delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"path\": \"src/" } }] })),
            delta(
                json!({ "tool_calls": [{ "index": 1, "id": "call_b", "type": "function", "function": { "name": "search", "arguments": "{\"pattern\":\"x\"}" } }] }),
            ),
            delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "日志.rs\"}" } }] })),
            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }], "usage": { "prompt_tokens": 10, "completion_tokens": 5 } }),
        ]);
        let turn = turn.unwrap();
        assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(turn.model, "deepseek-v4-pro");
        assert_eq!(turn.usage["prompt_tokens"], 10);
        assert_eq!(
            turn.content,
            vec![
                json!({ "type": "thinking", "thinking": "先看文件。" }),
                json!({ "type": "text", "text": "我读一下。" }),
                json!({ "type": "tool_use", "id": "call_a", "name": "read_file", "input": { "path": "src/日志.rs" }, "arguments": "{\"path\": \"src/日志.rs\"}" }),
                json!({ "type": "tool_use", "id": "call_b", "name": "search", "input": { "pattern": "x" }, "arguments": "{\"pattern\":\"x\"}" }),
            ]
        );
        assert!(turn.invalid_tool_inputs.is_empty());
        assert_eq!(
            deltas.first(),
            Some(&StreamDelta::ThinkingDelta {
                index: THINKING,
                text: "先看".into()
            })
        );
        assert!(deltas.contains(&StreamDelta::ToolUseStart {
            index: TOOLS + 1,
            id: "call_b".into(),
            name: "search".into()
        }));
    }

    #[test]
    fn handles_calls_without_an_index_and_arguments_sent_as_an_object() {
        // Kimi's schema has no `index`; GLM may send arguments whole, even as an object.
        let (turn, _) = replay(&[
            delta(
                json!({ "tool_calls": [{ "id": "a", "function": { "name": "read_file", "arguments": "{\"path\":" } }] }),
            ),
            delta(json!({ "tool_calls": [{ "function": { "arguments": "\"a\"}" } }] })),
            delta(
                json!({ "tool_calls": [{ "id": "b", "function": { "name": "search", "arguments": { "pattern": "y" } } }] }),
            ),
            delta(
                json!({ "tool_calls": [{ "id": "c", "function": { "name": "edit_file", "arguments": "{\"path\": \"a" } }] }),
            ),
            json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
        ]);
        let turn = turn.unwrap();
        let inputs: Vec<&Value> = turn.content.iter().map(|block| &block["input"]).collect();
        assert_eq!(
            inputs,
            [&json!({ "path": "a" }), &json!({ "pattern": "y" }), &json!({})]
        );
        assert_eq!(turn.content[1]["arguments"], "{\"pattern\":\"y\"}");
        assert_eq!(
            turn.invalid_tool_inputs.get("c").map(String::as_str),
            Some("{\"path\": \"a")
        );
    }

    #[test]
    fn maps_finish_reasons_and_reports_failures() {
        let finish = |reason: &str| {
            replay(&[json!({ "choices": [{ "delta": { "content": "x" }, "finish_reason": reason }] })]).0
        };
        assert_eq!(finish("stop").unwrap().stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(finish("length").unwrap().stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(finish("sensitive").unwrap().stop_reason.as_deref(), Some("refusal"));
        assert!(finish("network_error").unwrap_err().retryable());
        assert!(!finish("model_context_window_exceeded").unwrap_err().retryable());

        let mut accumulator = ChatAccumulator::default();
        let error = accumulator
            .apply(&json!({ "error": { "message": "busy", "code": "1305" } }))
            .unwrap_err();
        assert!(matches!(error, LlmError::Api { ref kind, .. } if kind == "1305"));
        assert!(!accumulator.finished());
    }

    fn request(messages: Vec<Value>) -> LlmRequest {
        LlmRequest {
            provider: "deepseek".into(),
            session: "session-1".into(),
            model: "deepseek-v4-pro".into(),
            system: "sys".into(),
            messages,
            tools: vec![json!({ "name": "read_file", "description": "d", "input_schema": { "type": "object" } })],
            effort: "xhigh".into(),
        }
    }

    #[test]
    fn sends_reasoning_back_and_puts_tool_results_before_text() {
        let messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "read a" }] }),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "先读。" },
                { "type": "tool_use", "id": "call_a", "name": "read_file", "input": { "path": "a" }, "arguments": "{\"path\": \"a\"}" },
            ] }),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_a", "content": "1\thi", "is_error": false },
                { "type": "text", "text": "also b" },
            ] }),
        ];
        assert_eq!(
            chat_messages("sys", &messages),
            vec![
                json!({ "role": "system", "content": "sys" }),
                json!({ "role": "user", "content": "read a" }),
                json!({
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "先读。",
                    "tool_calls": [{ "id": "call_a", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\": \"a\"}" } }],
                }),
                json!({ "role": "tool", "tool_call_id": "call_a", "content": "1\thi" }),
                json!({ "role": "user", "content": "also b" }),
            ]
        );
    }

    #[tokio::test]
    async fn streams_over_http_and_retries_an_overloaded_provider() {
        use crate::testing::{error, event_stream, serve};
        let (address, server) = serve(vec![
            error(429, r#"{"error":{"code":"1305","message":"该模型当前访问量过大"}}"#),
            event_stream(
                &[
                    delta(json!({ "content": "你好" })),
                    json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
                ],
                true,
            ),
        ])
        .await;
        let base_url = format!("{address}/v1/");
        let client = ChatClient::new(std::sync::Arc::new(move |_: &str| crate::llm::Endpoint {
            base_url: base_url.clone(),
            key: Some("sk-test".into()),
        }))
        .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let turn = client
            .stream(Flavor::Glm, request(vec![]), sender, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(turn.content, vec![json!({ "type": "text", "text": "你好" })]);
        assert_eq!(turn.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(
            receiver.recv().await,
            Some(StreamDelta::TextDelta {
                index: TEXT,
                text: "你好".into()
            })
        );

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].starts_with("POST /v1/chat/completions HTTP/1.1"),
            "{}",
            requests[1]
        );
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test")
        );
        assert!(requests[1].contains("\"reasoning_effort\":\"max\""));
    }

    #[tokio::test]
    async fn does_not_retry_a_rejected_request_and_needs_an_address_and_key() {
        use crate::testing::{error, serve};
        let (address, server) = serve(vec![error(
            400,
            r#"{"error":{"message":"bad model","type":"invalid_request_error"}}"#,
        )])
        .await;
        let client = ChatClient::new(std::sync::Arc::new(move |_: &str| crate::llm::Endpoint {
            base_url: address.clone(),
            key: Some("k".into()),
        }))
        .unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let error = client
            .stream(Flavor::DeepSeek, request(vec![]), sender, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "invalid_request_error: bad model");
        assert_eq!(server.await.unwrap().len(), 1);

        let unset = |endpoint: crate::llm::Endpoint| {
            ChatClient::new(std::sync::Arc::new(move |_: &str| endpoint.clone())).unwrap()
        };
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let no_address = unset(crate::llm::Endpoint {
            base_url: String::new(),
            key: Some("k".into()),
        });
        assert!(matches!(
            no_address
                .stream(
                    Flavor::Generic,
                    request(vec![]),
                    sender.clone(),
                    CancellationToken::new()
                )
                .await,
            Err(LlmError::NotConfigured(_))
        ));
        let no_key = unset(crate::llm::Endpoint {
            base_url: "http://127.0.0.1:9".into(),
            key: None,
        });
        assert_eq!(
            no_key
                .stream(Flavor::Generic, request(vec![]), sender, CancellationToken::new())
                .await,
            Err(LlmError::MissingKey)
        );
    }

    #[test]
    fn builds_each_flavor_s_request() {
        let deepseek = request_body(Flavor::DeepSeek, &request(vec![]));
        assert_eq!(deepseek["reasoning_effort"], "high");
        assert_eq!(deepseek["stream_options"], json!({ "include_usage": true }));
        assert_eq!(
            deepseek["tools"][0],
            json!({ "type": "function", "function": { "name": "read_file", "description": "d", "parameters": { "type": "object" } } })
        );
        assert!(deepseek.get("prompt_cache_key").is_none());
        // Kimi and GLM fix their sampling parameters; none may be sent.
        assert!(deepseek.get("temperature").is_none());

        let kimi = request_body(Flavor::Kimi, &request(vec![]));
        assert_eq!(kimi["reasoning_effort"], "max");
        assert_eq!(kimi["prompt_cache_key"], "session-1");

        let glm = request_body(Flavor::Glm, &request(vec![]));
        assert_eq!(glm["reasoning_effort"], "max");
        assert!(glm.get("stream_options").is_none());

        let generic = request_body(Flavor::Generic, &request(vec![]));
        assert!(generic.get("reasoning_effort").is_none());
        assert_eq!(generic["stream_options"], json!({ "include_usage": true }));
    }

    #[test]
    fn deepseek_and_kimi_reasoning_mode_injects_empty_reasoning_content_on_carried_tool_calls() {
        let carried_messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "read Cargo.toml" }] }),
            json!({
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "tool_1", "name": "read_file", "input": { "path": "Cargo.toml" } }
                ]
            }),
            json!({
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "tool_1", "content": "[package]", "is_error": false }
                ]
            }),
            json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": "Plain reply without tool calls." }]
            }),
        ];
        let mut req = request(carried_messages);
        req.effort = "high".into();

        for flavor in [Flavor::DeepSeek, Flavor::Kimi] {
            let body = request_body(flavor, &req);
            assert_eq!(body["messages"][2]["role"], "assistant");
            assert_eq!(
                body["messages"][2]["reasoning_content"],
                json!(""),
                "{flavor:?} must inject empty reasoning_content on carried tool_calls"
            );
            assert!(
                body["messages"][4].get("reasoning_content").is_none(),
                "{flavor:?} need not inject reasoning_content when tool_calls are absent"
            );
        }

        for flavor in [Flavor::Generic, Flavor::Glm] {
            let body = request_body(flavor, &req);
            assert!(body["messages"][2].get("reasoning_content").is_none());
        }
    }

    #[tokio::test]
    async fn generic_flavor_falls_back_without_stream_options_and_ignores_sse_heartbeats() {
        use crate::testing::{error, serve};
        let sse_body = format!(
            "event: ping\n\n: comment\n\ndata: \n\ndata: {}\n\nevent: ping\n\ndata: {}\n\ndata: [DONE]\n\n",
            delta(json!({ "content": "兼容老网关" })),
            json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] })
        );
        let ok_stream =
            format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{sse_body}");
        let (address, server) = serve(vec![
            error(400, r#"{"error":{"message":"Unknown parameter: 'stream_options'.","type":"invalid_request_error","param":"stream_options"}}"#),
            ok_stream,
        ])
        .await;
        let base_url = format!("{address}/v1");
        let client = ChatClient::new(std::sync::Arc::new(move |_: &str| crate::llm::Endpoint {
            base_url: base_url.clone(),
            key: Some("sk-local".into()),
        }))
        .unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut req = request(vec![]);
        req.provider = "custom".into();
        let turn = client
            .stream(Flavor::Generic, req, sender, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(turn.content, vec![json!({ "type": "text", "text": "兼容老网关" })]);

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("\"stream_options\""));
        assert!(!requests[1].contains("\"stream_options\""));
    }

    #[test]
    fn sends_images_as_parts_of_the_users_content() {
        let image =
            json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGk=" } });
        let part = json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,aGk=" } });
        let messages = chat_messages(
            "sys",
            &[json!({ "role": "user", "content": [image.clone(), { "type": "text", "text": "what is this?" }] })],
        );
        assert_eq!(
            messages[1],
            json!({ "role": "user", "content": [part.clone(), { "type": "text", "text": "what is this?" }] })
        );
        // A picture alone, sent while tools ran: the tool results first, then the picture.
        let result = json!({ "type": "tool_result", "tool_use_id": "c1", "content": "ok", "is_error": false });
        let messages = chat_messages("sys", &[json!({ "role": "user", "content": [result, image] })]);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[2], json!({ "role": "user", "content": [part] }));
        // Without pictures the content stays a plain string.
        let plain = chat_messages(
            "sys",
            &[json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })],
        );
        assert_eq!(plain[1], json!({ "role": "user", "content": "hi" }));
    }

    #[test]
    fn a_picture_a_tool_returned_follows_the_tool_messages_as_the_users() {
        let picture =
            json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGk=" } });
        let read = json!({ "type": "tool_result", "tool_use_id": "c1", "content": [{ "type": "text", "text": "a.png is a PNG picture." }, picture], "is_error": false });
        let plain = json!({ "type": "tool_result", "tool_use_id": "c2", "content": "ok", "is_error": false });
        let messages = chat_messages(
            "sys",
            &[json!({ "role": "user", "content": [read, plain, { "type": "text", "text": "and this" }] })],
        );
        // Each tool message has its words alone; the picture comes after them, said to be what they returned.
        assert_eq!(
            messages[1],
            json!({ "role": "tool", "tool_call_id": "c1", "content": "a.png is a PNG picture." })
        );
        assert_eq!(
            messages[2],
            json!({ "role": "tool", "tool_call_id": "c2", "content": "ok" })
        );
        assert_eq!(
            messages[3],
            json!({ "role": "user", "content": [
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,aGk=" } },
                { "type": "text", "text": format!("{}\n\nand this", crate::llm::TOOL_PICTURES) }
            ] })
        );
    }

    #[test]
    fn idless_tool_calls_generate_unique_non_empty_ids_across_steps() {
        let (turn1, deltas1) = replay(&[
            delta(
                json!({ "tool_calls": [{ "index": 0, "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" } }] }),
            ),
            json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
        ]);
        let (turn2, deltas2) = replay(&[
            delta(
                json!({ "tool_calls": [{ "index": 0, "function": { "name": "edit_file", "arguments": "{\"path\":\"a\"}" } }] }),
            ),
            json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
        ]);
        let id1 = turn1.unwrap().content[0]["id"].as_str().unwrap().to_string();
        let id2 = turn2.unwrap().content[0]["id"].as_str().unwrap().to_string();
        assert!(!id1.is_empty() && !id2.is_empty());
        assert_ne!(id1, id2);
        assert!(id1.len() <= 64 && id2.len() <= 64);
        assert!(deltas1.contains(&StreamDelta::ToolUseStart {
            index: TOOLS,
            id: id1,
            name: "read_file".into()
        }));
        assert!(deltas2.contains(&StreamDelta::ToolUseStart {
            index: TOOLS,
            id: id2,
            name: "edit_file".into()
        }));
    }
}
