//! OpenAI's Responses API over raw HTTP with server-sent events.
//!
//! OpenAI's reasoning models call tools only through this API, and their
//! reasoning must go back with the tool results. Requests are stateless
//! (`store: false`): each one replays the whole conversation, and reasoning
//! items come back carrying `encrypted_content` so they can be replayed.
//!
//! Every output item is kept whole in the log, inside the content block that
//! shows it (`{"type": "text", "text": ..., "item": {...}}`), and replayed
//! exactly as received. The `response.completed` event carries the
//! authoritative list of items; the deltas before it are only for display.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::llm::{AssistantTurn, Attempt, EndpointSource, LlmError, LlmRequest, StreamDelta, http_failure, with_retries};
use crate::providers::{Protocol, effort_for};
use crate::sse::SseParser;

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The request's conversation as Responses input items.
pub fn input_items(messages: &[Value]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        let blocks = message["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        if message["role"] == "assistant" {
            for block in blocks {
                if block["item"].is_object() {
                    items.push(block["item"].clone());
                    continue;
                }
                // Blocks without a stored item cannot come from this API; they are rebuilt as plainly as possible.
                match block["type"].as_str() {
                    Some("text") => items.push(json!({ "role": "assistant", "content": block["text"] })),
                    Some("tool_use") => items.push(json!({ "type": "function_call", "call_id": block["id"], "name": block["name"], "arguments": block["input"].to_string() })),
                    _ => {}
                }
            }
        } else {
            // A call's output carries words only: the pictures tools returned follow it as a message of the user's.
            let mut returned: Vec<Value> = Vec::new();
            let flush = |returned: &mut Vec<Value>, items: &mut Vec<Value>| {
                if returned.is_empty() {
                    return;
                }
                let pictures = returned.drain(..).map(|block| {
                    let source = &block["source"];
                    json!({ "type": "input_image", "image_url": format!("data:{};base64,{}", source["media_type"].as_str().unwrap_or_default(), source["data"].as_str().unwrap_or_default()) })
                });
                let content: Vec<Value> = std::iter::once(json!({ "type": "input_text", "text": crate::llm::TOOL_PICTURES })).chain(pictures).collect();
                items.push(json!({ "role": "user", "content": content }));
            };
            for block in blocks {
                if block["type"] != "tool_result" {
                    flush(&mut returned, &mut items);
                }
                match block["type"].as_str() {
                    Some("tool_result") => {
                        let (words, pictures) = crate::llm::tool_result_parts(block);
                        items.push(json!({ "type": "function_call_output", "call_id": block["tool_use_id"], "output": words }));
                        returned.extend(pictures);
                    }
                    Some("text") => items.push(json!({ "role": "user", "content": block["text"] })),
                    // A picture is a message of its own, a list of one part; the text about it follows as the next.
                    Some("image") => {
                        let source = &block["source"];
                        let url = format!("data:{};base64,{}", source["media_type"].as_str().unwrap_or_default(), source["data"].as_str().unwrap_or_default());
                        items.push(json!({ "role": "user", "content": [{ "type": "input_image", "image_url": url }] }));
                    }
                    _ => {}
                }
            }
            flush(&mut returned, &mut items);
        }
    }
    items
}

/// The JSON body of a streaming Responses request.
pub fn request_body(request: &LlmRequest) -> Value {
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| match tool.get("type") {
            // A tool of the API's own, such as web search, goes as it is.
            Some(_) => tool.clone(),
            // The tool schemas have optional fields, which strict mode does not allow.
            None => json!({ "type": "function", "name": tool["name"], "description": tool["description"], "parameters": tool["input_schema"], "strict": false }),
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "stream": true,
        "store": false,
        "instructions": request.system,
        "input": input_items(&request.messages),
        "tools": tools,
        // Needed to replay reasoning items when nothing is stored on OpenAI's side.
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(effort) = effort_for(Protocol::Responses, &request.effort) {
        body["reasoning"] = json!({ "effort": effort });
    }
    // A request without tools (a summary written from a transcript) leaves the field out: an empty list is an error to some servers.
    if request.tools.is_empty() {
        body.as_object_mut().expect("the body is an object").remove("tools");
    }
    body
}

/// Rebuilds one response from Responses stream events.
#[derive(Default)]
pub struct ResponsesAccumulator {
    /// Items as each `response.output_item.done` delivered them, by output index.
    items: Vec<(usize, Value)>,
    /// The response from `response.completed` or `response.incomplete`.
    response: Option<Value>,
    model: String,
}

impl ResponsesAccumulator {
    /// Apply one event's JSON. Returns the fragments to show, or the error the stream reported.
    pub fn apply(&mut self, data: &Value) -> Result<Vec<StreamDelta>, LlmError> {
        let index = data["output_index"].as_u64().unwrap_or_default() as usize;
        let text = |field: &str| data[field].as_str().unwrap_or_default().to_string();
        let mut deltas = Vec::new();
        match data["type"].as_str().unwrap_or_default() {
            "response.created" | "response.in_progress" => {
                if let Some(model) = data["response"]["model"].as_str() {
                    self.model = model.to_string();
                }
            }
            "response.output_item.added" if data["item"]["type"] == "function_call" => {
                let item = &data["item"];
                deltas.push(StreamDelta::ToolUseStart {
                    index,
                    id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                });
            }
            "response.output_text.delta" => deltas.push(StreamDelta::TextDelta { index, text: text("delta") }),
            "response.reasoning_summary_text.delta" => deltas.push(StreamDelta::ThinkingDelta { index, text: text("delta") }),
            "response.function_call_arguments.delta" => deltas.push(StreamDelta::ToolInputDelta { index, partial: text("delta") }),
            "response.output_item.done" => {
                self.items.push((index, data["item"].clone()));
                deltas.push(StreamDelta::BlockStop { index });
            }
            "response.completed" | "response.incomplete" => self.response = Some(data["response"].clone()),
            "response.failed" => return Err(failure(&data["response"]["error"])),
            "error" => return Err(failure(data)),
            // Other events carry nothing this client needs.
            _ => {}
        }
        Ok(deltas)
    }

    /// Whether the response finished: a stream that ends before it was cut off.
    pub fn finished(&self) -> bool {
        self.response.is_some()
    }

    pub fn finish(mut self) -> AssistantTurn {
        let response = self.response.take().unwrap_or_default();
        let output: Vec<Value> = match response["output"].as_array() {
            Some(output) if !output.is_empty() => output.clone(),
            _ => {
                self.items.sort_by_key(|(index, _)| *index);
                self.items.into_iter().map(|(_, item)| item).collect()
            }
        };
        let mut content = Vec::new();
        let mut invalid = HashMap::new();
        let mut calls = false;
        for item in output {
            let block = match item["type"].as_str().unwrap_or_default() {
                "reasoning" => {
                    let summary: Vec<&str> = item["summary"].as_array().into_iter().flatten().filter_map(|part| part["text"].as_str()).collect();
                    json!({ "type": "thinking", "thinking": summary.join("\n\n") })
                }
                "message" => {
                    let text: String = item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|part| part["text"].as_str().or_else(|| part["refusal"].as_str()))
                        .collect();
                    json!({ "type": "text", "text": text })
                }
                "function_call" => {
                    calls = true;
                    let id = item["call_id"].as_str().unwrap_or_default().to_string();
                    let raw = item["arguments"].as_str().unwrap_or_default();
                    let input = match serde_json::from_str::<Value>(raw) {
                        Ok(input) if input.is_object() => input,
                        _ if raw.trim().is_empty() => json!({}),
                        _ => {
                            invalid.insert(id.clone(), raw.to_string());
                            json!({})
                        }
                    };
                    json!({ "type": "tool_use", "id": id, "name": item["name"], "input": input })
                }
                // Item types this client does not show are still replayed.
                other => json!({ "type": format!("openai_{other}") }),
            };
            let mut block = block;
            block["item"] = item;
            content.push(block);
        }
        let stop_reason = match (response["status"].as_str(), response["incomplete_details"]["reason"].as_str()) {
            (Some("incomplete"), Some("max_output_tokens")) => "max_tokens",
            (Some("incomplete"), Some("content_filter")) => "refusal",
            _ if calls => "tool_use",
            _ => "end_turn",
        };
        AssistantTurn {
            content,
            stop_reason: Some(stop_reason.to_string()),
            model: response["model"].as_str().map(str::to_string).unwrap_or(self.model),
            usage: if response["usage"].is_object() { response["usage"].clone() } else { json!({}) },
            invalid_tool_inputs: invalid,
        }
    }
}

/// An error object from the stream, `{"code": ..., "message": ...}`.
fn failure(error: &Value) -> LlmError {
    let kind = error["code"].as_str().or_else(|| error["type"].as_str()).unwrap_or("response_failed").to_string();
    let retryable = matches!(kind.as_str(), "server_error" | "rate_limit_exceeded" | "server_is_overloaded" | "slow_down");
    LlmError::Api { status: None, kind, message: error["message"].as_str().unwrap_or("no message").to_string(), retryable }
}

pub struct ResponsesClient {
    http: reqwest::Client,
    endpoints: EndpointSource,
}

impl ResponsesClient {
    pub fn new(endpoints: EndpointSource) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build().map_err(|error| LlmError::Network(error.to_string()))?;
        Ok(Self { http, endpoints })
    }

    pub async fn stream(&self, request: LlmRequest, deltas: UnboundedSender<StreamDelta>, cancel: CancellationToken) -> Result<AssistantTurn, LlmError> {
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

    async fn attempt(&self, request: &LlmRequest, deltas: &UnboundedSender<StreamDelta>, cancel: &CancellationToken, forwarded: &mut bool) -> Attempt {
        let endpoint = (self.endpoints)(&request.provider);
        let key = endpoint.key.filter(|key| !key.trim().is_empty()).ok_or((LlmError::MissingKey, None))?;
        let send = self
            .http
            .post(format!("{}/responses", endpoint.base_url.trim_end_matches('/')))
            .bearer_auth(key.trim())
            .json(&request_body(request))
            .send();
        let response = tokio::select! {
            response = send => response.map_err(|error| (LlmError::Network(error.to_string()), None))?,
            () = cancel.cancelled() => return Err((LlmError::Cancelled, None)),
        };
        if !response.status().is_success() {
            return Err(http_failure(response).await);
        }

        let mut parser = SseParser::default();
        let mut accumulator = ResponsesAccumulator::default();
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
                if event.data.trim() == "[DONE]" {
                    continue;
                }
                let data: Value = serde_json::from_str(&event.data).map_err(|error| (LlmError::Protocol(format!("event {} is not JSON: {error}", event.event)), None))?;
                for delta in accumulator.apply(&data).map_err(|error| (error, None))? {
                    *forwarded = true;
                    let _ = deltas.send(delta);
                }
            }
        }
        if !accumulator.finished() {
            return Err((LlmError::Network("the stream ended before the response was complete".to_string()), None));
        }
        Ok(accumulator.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reasoning() -> Value {
        json!({ "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque" })
    }

    fn call(arguments: &str) -> Value {
        json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_file", "arguments": arguments, "status": "completed" })
    }

    fn message(text: &str) -> Value {
        json!({ "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": text, "annotations": [] }] })
    }

    #[test]
    fn keeps_every_output_item_whole_from_the_completed_response() {
        let mut accumulator = ResponsesAccumulator::default();
        let events = [
            json!({ "type": "response.created", "response": { "model": "gpt-6.2-astra", "status": "in_progress" } }),
            json!({ "type": "response.output_item.added", "output_index": 1, "item": { "type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"path\":" }),
            json!({ "type": "response.function_call_arguments.delta", "output_index": 1, "delta": "\"a\"}" }),
            json!({ "type": "response.output_item.done", "output_index": 1, "item": call("{\"path\":\"a\"}") }),
            json!({ "type": "response.completed", "response": {
                "model": "gpt-6.2-astra", "status": "completed",
                "output": [reasoning(), call("{\"path\":\"a\"}")],
                "usage": { "input_tokens": 10, "output_tokens": 4 },
            } }),
        ];
        let mut deltas = Vec::new();
        for event in &events {
            deltas.extend(accumulator.apply(event).unwrap());
        }
        assert!(accumulator.finished());
        assert!(deltas.contains(&StreamDelta::ToolUseStart { index: 1, id: "call_1".into(), name: "read_file".into() }));
        let turn = accumulator.finish();
        assert_eq!(turn.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(turn.usage["input_tokens"], 10);
        assert_eq!(turn.content, vec![
            json!({ "type": "thinking", "thinking": "", "item": reasoning() }),
            json!({ "type": "tool_use", "id": "call_1", "name": "read_file", "input": { "path": "a" }, "item": call("{\"path\":\"a\"}") }),
        ]);
    }

    #[test]
    fn reports_cut_off_responses_failures_and_bad_arguments() {
        let finish = |response: Value| {
            let mut accumulator = ResponsesAccumulator::default();
            accumulator.apply(&json!({ "type": "response.incomplete", "response": response })).unwrap();
            accumulator.finish()
        };
        let cut = finish(json!({ "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" }, "output": [call("{\"path\": \"a")] }));
        assert_eq!(cut.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(cut.invalid_tool_inputs.get("call_1").map(String::as_str), Some("{\"path\": \"a"));
        let said = finish(json!({ "status": "completed", "output": [message("好的。")] }));
        assert_eq!((said.stop_reason.as_deref(), said.content[0]["text"].as_str()), (Some("end_turn"), Some("好的。")));

        let mut accumulator = ResponsesAccumulator::default();
        let failed = accumulator.apply(&json!({ "type": "response.failed", "response": { "error": { "code": "server_error", "message": "oops" } } })).unwrap_err();
        assert!(failed.retryable());
        let invalid = accumulator.apply(&json!({ "type": "error", "code": "invalid_prompt", "message": "no" })).unwrap_err();
        assert!(!invalid.retryable());
    }

    #[tokio::test]
    async fn streams_a_response_over_http() {
        use crate::testing::{event_stream, serve};
        let (address, server) = serve(vec![event_stream(
            &[
                json!({ "type": "response.output_text.delta", "output_index": 0, "delta": "好" }),
                json!({ "type": "response.completed", "response": { "model": "gpt-6.2-sol", "status": "completed", "output": [message("好")] } }),
            ],
            false,
        )])
        .await;
        let base_url = format!("{address}/v1");
        let client = ResponsesClient::new(std::sync::Arc::new(move |_: &str| crate::llm::Endpoint { base_url: base_url.clone(), key: Some("sk-openai".into()) })).unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let request = LlmRequest {
            provider: "openai".into(),
            session: "s".into(),
            model: "gpt-6.2-sol".into(),
            system: "sys".into(),
            messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })],
            tools: vec![],
            effort: "high".into(),
        };
        let turn = client.stream(request, sender, CancellationToken::new()).await.unwrap();
        assert_eq!((turn.model.as_str(), turn.content[0]["text"].as_str()), ("gpt-6.2-sol", Some("好")));
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /v1/responses HTTP/1.1"), "{}", requests[0]);
        assert!(requests[0].to_ascii_lowercase().contains("authorization: bearer sk-openai"));
    }

    #[test]
    fn replays_stored_items_and_answers_calls_with_outputs() {
        let messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "read a" }] }),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "", "item": reasoning() },
                { "type": "tool_use", "id": "call_1", "name": "read_file", "input": { "path": "a" }, "item": call("{\"path\":\"a\"}") },
            ] }),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_1", "content": "1\thi", "is_error": false },
                { "type": "text", "text": "also b" },
            ] }),
        ];
        assert_eq!(input_items(&messages), vec![
            json!({ "role": "user", "content": "read a" }),
            reasoning(),
            call("{\"path\":\"a\"}"),
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "1\thi" }),
            json!({ "role": "user", "content": "also b" }),
        ]);

        let request = LlmRequest {
            provider: "openai".into(),
            session: "s".into(),
            model: "gpt-6.2-astra".into(),
            system: "sys".into(),
            messages,
            tools: vec![json!({ "name": "read_file", "description": "d", "input_schema": { "type": "object" } })],
            effort: "xhigh".into(),
        };
        let body = request_body(&request);
        assert_eq!((body["store"].as_bool(), body["instructions"].as_str()), (Some(false), Some("sys")));
        assert_eq!(body["reasoning"], json!({ "effort": "xhigh" }));
        assert_eq!(body["tools"][0], json!({ "type": "function", "name": "read_file", "description": "d", "parameters": { "type": "object" }, "strict": false }));
        assert_eq!(body["input"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn sends_an_image_as_an_input_image_ahead_of_the_text() {
        let image = json!({ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "aGk=" } });
        let items = input_items(&[json!({ "role": "user", "content": [image, { "type": "text", "text": "what is this?" }] })]);
        assert_eq!(items, [
            json!({ "role": "user", "content": [{ "type": "input_image", "image_url": "data:image/jpeg;base64,aGk=" }] }),
            json!({ "role": "user", "content": "what is this?" }),
        ]);
    }

    #[test]
    fn a_picture_a_tool_returned_follows_the_outputs_as_the_users() {
        let picture = json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGk=" } });
        let read = json!({ "type": "tool_result", "tool_use_id": "c1", "content": [{ "type": "text", "text": "a.png is a PNG picture." }, picture], "is_error": false });
        let plain = json!({ "type": "tool_result", "tool_use_id": "c2", "content": "ok", "is_error": false });
        let items = input_items(&[json!({ "role": "user", "content": [read, plain, { "type": "text", "text": "and this" }] })]);
        assert_eq!(items, [
            json!({ "type": "function_call_output", "call_id": "c1", "output": "a.png is a PNG picture." }),
            json!({ "type": "function_call_output", "call_id": "c2", "output": "ok" }),
            json!({ "role": "user", "content": [{ "type": "input_text", "text": crate::llm::TOOL_PICTURES }, { "type": "input_image", "image_url": "data:image/png;base64,aGk=" }] }),
            json!({ "role": "user", "content": "and this" }),
        ]);
        // With nothing after the outputs, the pictures still follow them.
        let alone = input_items(&[json!({ "role": "user", "content": [json!({ "type": "tool_result", "tool_use_id": "c1", "content": [{ "type": "text", "text": "x" }, { "type": "image", "source": { "type": "base64", "media_type": "image/gif", "data": "aGk=" } }], "is_error": false })] })]);
        assert_eq!(alone.len(), 2);
        assert_eq!(alone[1]["content"][1]["image_url"], "data:image/gif;base64,aGk=");
    }

    #[test]
    fn sends_a_tool_of_the_apis_own_as_it_is() {
        let request = LlmRequest {
            provider: "openai".into(),
            session: "s".into(),
            model: "gpt-6.2-sol".into(),
            system: "sys".into(),
            messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })],
            tools: vec![json!({ "type": "web_search" }), json!({ "name": "read_file", "description": "d", "input_schema": { "type": "object" } })],
            effort: "low".into(),
        };
        let body = request_body(&request);
        assert_eq!(body["tools"][0], json!({ "type": "web_search" }));
        assert_eq!(body["tools"][1]["type"], "function");
    }
}
