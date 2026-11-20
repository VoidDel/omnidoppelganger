//! The model seam: what the agent loop asks of a model provider.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// One model request. `messages` comes from [`crate::session::derive_messages`].
#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    /// Id of the provider the session was created with; see [`crate::providers`].
    pub provider: String,
    /// The session's id, for providers that key their prompt cache on it.
    pub session: String,
    pub model: String,
    pub system: String,
    pub messages: Vec<Value>,
    /// Tool definitions: `name`, `description`, `input_schema`.
    pub tools: Vec<Value>,
    /// `low`, `medium`, `high`, `xhigh`, or `max`.
    pub effort: String,
}

/// Live fragments of the message being generated. They are for display only:
/// the durable record is the [`AssistantTurn`] returned at the end.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    TextDelta { index: usize, text: String },
    ThinkingDelta { index: usize, text: String },
    ToolUseStart { index: usize, id: String, name: String },
    ToolInputDelta { index: usize, partial: String },
    BlockStop { index: usize },
    /// The request is being retried: discard every fragment received so far.
    Reset,
}

/// What one request cost, in one shape whatever the provider's `usage` looks
/// like. See [`crate::providers::tokens`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
pub struct Tokens {
    /// The whole prompt, cached part included.
    pub input: u64,
    /// The part of the prompt read from the provider's cache.
    pub cached: u64,
    pub output: u64,
}

/// A complete assistant message.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantTurn {
    /// Content blocks exactly as the API sent them.
    pub content: Vec<Value>,
    pub stop_reason: Option<String>,
    pub model: String,
    pub usage: Value,
    /// Tool calls whose input was not a JSON object, by `tool_use` id, with the raw text received.
    pub invalid_tool_inputs: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("no API key is configured")]
    MissingKey,
    #[error("{0}")]
    NotConfigured(String),
    #[error("the request was cancelled")]
    Cancelled,
    #[error("{kind}: {message}")]
    Api { status: Option<u16>, kind: String, message: String, retryable: bool },
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

impl LlmError {
    /// Whether sending the same request again can succeed.
    pub fn retryable(&self) -> bool {
        match self {
            LlmError::Api { retryable, .. } => *retryable,
            LlmError::Network(_) => true,
            LlmError::MissingKey | LlmError::NotConfigured(_) | LlmError::Cancelled | LlmError::Protocol(_) => false,
        }
    }

    /// Whether the provider refused the request because the conversation no
    /// longer fits the model's context window. Providers word this each their
    /// own way and few document it, so this goes by what the message says.
    pub fn context_overflow(&self) -> bool {
        let LlmError::Api { kind, message, .. } = self else { return false };
        let text = format!("{kind} {message}").to_lowercase();
        ["context window", "context length", "context_length", "maximum context", "prompt is too long", "input is too long", "too many tokens", "token limit"]
            .iter()
            .any(|phrase| text.contains(phrase))
    }
}

/// The outcome of one HTTP attempt: the message, or the error together with
/// the wait the server asked for before trying again.
pub type Attempt = Result<AssistantTurn, (LlmError, Option<Duration>)>;

const MAX_ATTEMPTS: u32 = 4;

/// Run `attempt` until it succeeds, fails for good, or has been tried
/// [`MAX_ATTEMPTS`] times, waiting between tries. Each attempt returns its
/// outcome and whether it sent any fragment, so the client can be told to
/// discard them before the next try.
pub async fn with_retries<F, Fut>(deltas: &UnboundedSender<StreamDelta>, cancel: &CancellationToken, mut attempt: F) -> Result<AssistantTurn, LlmError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = (Attempt, bool)>,
{
    let mut tries = 0;
    loop {
        tries += 1;
        let (outcome, forwarded) = attempt().await;
        let (error, retry_after) = match outcome {
            Ok(turn) => return Ok(turn),
            Err(failure) => failure,
        };
        if !error.retryable() || tries >= MAX_ATTEMPTS {
            return Err(error);
        }
        if forwarded {
            let _ = deltas.send(StreamDelta::Reset);
        }
        let wait = retry_after.unwrap_or(Duration::from_secs(1 << (tries - 1))).min(Duration::from_secs(60));
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = cancel.cancelled() => return Err(LlmError::Cancelled),
        }
    }
}

/// Turn a non-success HTTP response into an error. Every provider here wraps
/// its error as `{"error": {"message": ..., "type" or "code": ...}}`.
/// 408, 409, 429, and 5xx are retryable unless `x-should-retry` says otherwise.
pub async fn http_failure(response: reqwest::Response) -> (LlmError, Option<Duration>) {
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_string);
    let retry_after = header("retry-after").and_then(|value| value.parse::<f64>().ok()).filter(|seconds| seconds.is_finite() && *seconds >= 0.0).map(Duration::from_secs_f64);
    let should_retry = header("x-should-retry");
    let body = response.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let error = &parsed["error"];
    let text = |value: &Value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    };
    let kind = text(&error["type"]).or_else(|| text(&error["code"])).unwrap_or_else(|| "http_error".to_string());
    let message = text(&error["message"]).unwrap_or_else(|| if body.trim().is_empty() { format!("HTTP {status}") } else { body.chars().take(500).collect() });
    let retryable = match should_retry.as_deref() {
        Some(value) => value == "true",
        None => matches!(status, 408 | 409 | 429) || status >= 500,
    };
    (LlmError::Api { status: Some(status), kind, message, retryable }, retry_after)
}

/// Where requests for one provider go, read fresh for every request so a
/// key or address saved in settings applies to the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub base_url: String,
    pub key: Option<String>,
}

pub type EndpointSource = std::sync::Arc<dyn Fn(&str) -> Endpoint + Send + Sync>;

pub trait Llm: Send + Sync + 'static {
    /// Generate one assistant message, sending fragments to `deltas` as they arrive.
    /// Resolves with [`LlmError::Cancelled`] soon after `cancel` fires.
    fn stream(
        &self,
        request: LlmRequest,
        deltas: UnboundedSender<StreamDelta>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<AssistantTurn, LlmError>> + Send;
}

/// A tool result's words, and the pictures that came with it (`image` blocks): its content is the words alone, or a
/// list of a text block and the pictures. For a provider that takes no pictures in a tool result, which then sends
/// them in the user's turn after the results.
pub fn tool_result_parts(block: &Value) -> (String, Vec<Value>) {
    match block["content"].as_array() {
        Some(parts) => {
            let words: Vec<&str> = parts.iter().filter(|part| part["type"] == "text").filter_map(|part| part["text"].as_str()).collect();
            (words.join("\n"), parts.iter().filter(|part| part["type"] == "image").cloned().collect())
        }
        None => (block["content"].as_str().unwrap_or_default().to_string(), Vec::new()),
    }
}

/// What precedes, in the user's turn, the pictures that tool results returned.
pub const TOOL_PICTURES: &str = "[The pictures the tool results above returned, in their order.]";
