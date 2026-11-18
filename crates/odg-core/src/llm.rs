//! The model seam: what the agent loop asks of a model provider.

use std::collections::HashMap;
use std::future::Future;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// One model request. `messages` comes from [`crate::session::derive_messages`].
#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
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
            LlmError::MissingKey | LlmError::Cancelled | LlmError::Protocol(_) => false,
        }
    }
}

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
