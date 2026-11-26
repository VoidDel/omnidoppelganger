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
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolInputDelta {
        index: usize,
        partial: String,
    },
    BlockStop {
        index: usize,
    },
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
    Api {
        status: Option<u16>,
        kind: String,
        message: String,
        retryable: bool,
    },
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
        let LlmError::Api { kind, message, .. } = self else {
            return false;
        };
        let text = format!("{kind} {message}").to_lowercase();
        [
            "context window",
            "context length",
            "context_length",
            "maximum context",
            "prompt is too long",
            "input is too long",
            "too many tokens",
            "token limit",
        ]
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
pub async fn with_retries<F, Fut>(
    deltas: &UnboundedSender<StreamDelta>,
    cancel: &CancellationToken,
    mut attempt: F,
) -> Result<AssistantTurn, LlmError>
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
        let wait = retry_after
            .unwrap_or(Duration::from_secs(1 << (tries - 1)))
            .min(Duration::from_secs(60));
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = cancel.cancelled() => return Err(LlmError::Cancelled),
        }
    }
}

/// Parse a `Retry-After` HTTP header value per RFC 9110 §10.2.3: either non-negative delta-seconds
/// or an HTTP-date (IMF-fixdate / RFC 2822, RFC 850, ANSI C `asctime`, or RFC 3339).
pub(crate) fn parse_retry_after(raw: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<f64>() {
        if seconds.is_finite() && seconds >= 0.0 {
            return Some(Duration::from_secs_f64(seconds.min(86_400.0)));
        }
        return None;
    }
    let parsed_utc = chrono::DateTime::parse_from_rfc2822(trimmed)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(trimmed).map(|dt| dt.with_timezone(&chrono::Utc)))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(trimmed, "%A, %d-%b-%y %H:%M:%S GMT"))
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(trimmed, "%a %b %e %H:%M:%S %Y"))
                .map(|naive| naive.and_utc())
        })
        .ok()?;
    let delta_ms = (parsed_utc - now).num_milliseconds();
    if delta_ms <= 0 {
        Some(Duration::ZERO)
    } else {
        Some(Duration::from_millis((delta_ms as u64).min(86_400_000)))
    }
}

/// Turn a non-success HTTP response into an error. Every provider here wraps
/// its error as `{"error": {"message": ..., "type" or "code": ...}}`.
/// 408, 409, 429, and 5xx are retryable unless `x-should-retry` says otherwise.
pub async fn http_failure(response: reqwest::Response) -> (LlmError, Option<Duration>) {
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let retry_after = header("retry-after").and_then(|value| parse_retry_after(&value, chrono::Utc::now()));
    let should_retry = header("x-should-retry");
    let body = response.text().await.unwrap_or_default();
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
    let retryable = match should_retry.as_deref() {
        Some(value) => value == "true",
        None => matches!(status, 408 | 409 | 429) || status >= 500,
    };
    (
        LlmError::Api {
            status: Some(status),
            kind,
            message,
            retryable,
        },
        retry_after,
    )
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
            let words: Vec<&str> = parts
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect();
            (
                words.join("\n"),
                parts.iter().filter(|part| part["type"] == "image").cloned().collect(),
            )
        }
        None => (block["content"].as_str().unwrap_or_default().to_string(), Vec::new()),
    }
}

/// What precedes, in the user's turn, the pictures that tool results returned.
pub const TOOL_PICTURES: &str = "[The pictures the tool results above returned, in their order.]";

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[tokio::test]
    async fn parses_retry_after_integer_seconds_and_http_dates() {
        use chrono::TimeZone;

        let now = chrono::Utc.with_ymd_and_hms(2026, 11, 26, 12, 0, 0).unwrap();
        assert_eq!(parse_retry_after("5", now), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(" 0 ", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("-3", now), None);
        assert_eq!(parse_retry_after("1e100", now), Some(Duration::from_secs(86_400)));
        assert_eq!(
            parse_retry_after("Thu, 26 Nov 2026 12:00:42 GMT", now),
            Some(Duration::from_secs(42))
        );
        // Past HTTP-date clamps cleanly to Duration::ZERO.
        assert_eq!(
            parse_retry_after("Thu, 26 Nov 2026 11:59:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("not-a-date", now), None);

        // End-to-end check through `http_failure(reqwest::Response)`.
        let future_date = (chrono::Utc::now() + chrono::Duration::seconds(15))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let resp_secs = format!(
            "HTTP/1.1 429 Too Many Requests\r\nretry-after: 7\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let resp_date = format!(
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: {future_date}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let (addr, _srv) = crate::testing::serve(vec![resp_secs, resp_date]).await;
        let client = reqwest::Client::new();

        let r1 = client.get(&addr).send().await.unwrap();
        let (err1, wait1) = http_failure(r1).await;
        assert!(err1.retryable());
        assert_eq!(wait1, Some(Duration::from_secs(7)));

        let r2 = client.get(&addr).send().await.unwrap();
        let (err2, wait2) = http_failure(r2).await;
        assert!(err2.retryable());
        let wait2 = wait2.expect("expected Retry-After HTTP-date duration");
        assert!(
            wait2 >= Duration::from_secs(10) && wait2 <= Duration::from_secs(20),
            "unexpected HTTP-date wait: {wait2:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_token_interrupts_exponential_backoff_immediately() {
        let (tx, _rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_ref = calls.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel_clone.cancel();
        });

        let started = tokio::time::Instant::now();
        let outcome = with_retries(&tx, &cancel, || {
            let calls_ref = calls_ref.clone();
            async move {
                calls_ref.fetch_add(1, Ordering::SeqCst);
                (
                    Err((
                        LlmError::Api {
                            status: Some(429),
                            kind: "rate_limit".into(),
                            message: "throttled".into(),
                            retryable: true,
                        },
                        // None triggers 1s exponential backoff; cancellation must interrupt it in ~30ms!
                        None,
                    )),
                    false,
                )
            }
        })
        .await;

        assert_eq!(outcome, Err(LlmError::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "backoff sleep did not abort promptly on cancel: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn enforces_max_attempts_on_429_502_503_and_stops_immediately_when_not_retryable() {
        for status in [429u16, 502, 503] {
            let (tx, _rx) = unbounded_channel();
            let cancel = CancellationToken::new();
            let calls = Arc::new(AtomicUsize::new(0));
            let calls_ref = calls.clone();
            let err = with_retries(&tx, &cancel, || {
                let calls_ref = calls_ref.clone();
                async move {
                    calls_ref.fetch_add(1, Ordering::SeqCst);
                    (
                        Err((
                            LlmError::Api {
                                status: Some(status),
                                kind: "transient".into(),
                                message: format!("status {status}"),
                                retryable: true,
                            },
                            Some(Duration::ZERO),
                        )),
                        false,
                    )
                }
            })
            .await
            .unwrap_err();
            assert_eq!(
                calls.load(Ordering::SeqCst),
                MAX_ATTEMPTS as usize,
                "status {status} must try exactly MAX_ATTEMPTS ({MAX_ATTEMPTS}) times"
            );
            assert!(matches!(err, LlmError::Api { status: Some(s), .. } if s == status));
        }

        // Non-retryable 400 or `x-should-retry: false` stops on attempt 1.
        let body = r#"{"error":{"type":"overloaded","message":"do not retry"}}"#;
        let no_retry_resp = format!(
            "HTTP/1.1 429 Too Many Requests\r\nx-should-retry: false\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let (addr, _srv) = crate::testing::serve(vec![no_retry_resp]).await;
        let resp = reqwest::Client::new().get(&addr).send().await.unwrap();
        let (err, _) = http_failure(resp).await;
        assert!(!err.retryable());
    }

    #[tokio::test]
    async fn emits_stream_delta_reset_in_exact_order_only_after_forwarded_partial_attempts() {
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        let step = Arc::new(AtomicUsize::new(0));
        let step_ref = step.clone();

        let turn = with_retries(&tx, &cancel, || {
            let tx = tx.clone();
            let step_ref = step_ref.clone();
            async move {
                match step_ref.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        // Attempt 1 forwards a partial delta and then drops mid-stream -> must emit Reset!
                        let _ = tx.send(StreamDelta::TextDelta {
                            index: 0,
                            text: "partial-1".into(),
                        });
                        (
                            Err((LlmError::Network("mid-stream disconnect".into()), Some(Duration::ZERO))),
                            true,
                        )
                    }
                    1 => {
                        // Attempt 2 fails with 503 BEFORE forwarding any deltas -> must NOT emit duplicate Reset!
                        (
                            Err((
                                LlmError::Api {
                                    status: Some(503),
                                    kind: "unavailable".into(),
                                    message: "upstream busy".into(),
                                    retryable: true,
                                },
                                Some(Duration::ZERO),
                            )),
                            false,
                        )
                    }
                    _ => {
                        let _ = tx.send(StreamDelta::TextDelta {
                            index: 0,
                            text: "complete-answer".into(),
                        });
                        (
                            Ok(AssistantTurn {
                                model: "test-model".into(),
                                stop_reason: Some("end_turn".into()),
                                ..Default::default()
                            }),
                            true,
                        )
                    }
                }
            }
        })
        .await
        .unwrap();
        drop(tx);

        assert_eq!(turn.model, "test-model");
        let mut received = Vec::new();
        while let Some(delta) = rx.recv().await {
            received.push(delta);
        }
        assert_eq!(
            received,
            vec![
                StreamDelta::TextDelta {
                    index: 0,
                    text: "partial-1".into()
                },
                StreamDelta::Reset,
                StreamDelta::TextDelta {
                    index: 0,
                    text: "complete-answer".into()
                },
            ]
        );
    }
}
