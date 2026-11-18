//! Durable session events. Everything a model request contains is derived
//! from these, so a new model-visible input needs a new event here.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line of a session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Position in the log, starting at 1.
    pub seq: u64,
    /// RFC 3339 time the event was appended.
    pub ts: String,
    #[serde(flatten)]
    pub event: Event,
}

/// The user's answer to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    /// Allow this and later requests with the same grant until the app
    /// restarts. For a request without a grant it allows this one only.
    AllowSession,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Event {
    #[serde(rename = "session/created")]
    SessionCreated {
        id: String,
        /// Absolute path of the directory the agent works in.
        workspace: String,
        model: String,
        format: u32,
    },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    #[serde(rename = "user/message")]
    UserMessage {
        text: String,
        /// True when the message arrived while a turn was already running.
        steering: bool,
    },
    /// A complete assistant message. `content` holds the API content blocks
    /// exactly as received: thinking blocks must be replayed unchanged.
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        content: Vec<Value>,
        stop_reason: Option<String>,
        model: String,
        usage: Value,
    },
    #[serde(rename = "approval/requested")]
    ApprovalRequested {
        tool_use_id: String,
        tool: String,
        /// Which rule asked: `command`, `outside_read`, `outside_write`, or `git_write`.
        reason: String,
        /// The command or path the user is asked about.
        detail: String,
        /// What allowing for the session covers; absent when the request
        /// must be approved every time. Logs written before this field
        /// existed read it as absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grant: Option<String>,
    },
    #[serde(rename = "approval/resolved")]
    ApprovalResolved {
        tool_use_id: String,
        decision: Decision,
    },
    #[serde(rename = "tool/result")]
    ToolResult {
        tool_use_id: String,
        tool: String,
        /// The text returned to the model.
        content: String,
        is_error: bool,
        /// Presentation data for the client (diff hunks, exit code); never sent to the model.
        meta: Value,
    },
    #[serde(rename = "turn/end")]
    TurnEnd {
        turn: u64,
        /// `completed`, `cancelled`, `error`, `refusal`, `max_tokens`, `step_limit`, or `interrupted`.
        reason: String,
        error: Option<String>,
    },
}
