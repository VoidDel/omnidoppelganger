//! Durable session events. Everything a model request contains is derived
//! from these, so a new model-visible input needs a new event here.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::Tokens;
use crate::policy::Mode;

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
    /// Allow this and later requests with the same grant in this workspace
    /// for good; see [`crate::grants`]. For a grant that cannot be kept it
    /// is [`Decision::AllowSession`]. Since format 4.
    AllowAlways,
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
        /// Id of the model provider; absent means Anthropic, the only provider
        /// before format 2. See [`crate::providers`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        model: String,
        format: u32,
    },
    /// The project's own instructions for agents, read from a file at the
    /// top of the workspace when a turn started and found new or changed.
    /// The latest one is part of the system prompt of every request after
    /// it; empty `content` means the file is gone. Since format 4.
    #[serde(rename = "instructions/loaded")]
    InstructionsLoaded {
        /// The file's name, like `AGENTS.md`.
        path: String,
        content: String,
    },
    /// The user named the session. Until then its title is the first message. Since format 3.
    #[serde(rename = "session/renamed")]
    SessionRenamed { title: String },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    #[serde(rename = "user/message")]
    UserMessage {
        text: String,
        /// True when the message arrived while a turn was already running.
        steering: bool,
        /// The session's mode when the message joined the conversation. The
        /// model is told when plan mode starts or ends from this. Left out
        /// for edit mode, so logs from before modes read unchanged.
        #[serde(default, skip_serializing_if = "Mode::is_edit")]
        mode: Mode,
    },
    /// A complete assistant message. `content` holds the API content blocks
    /// exactly as received: thinking blocks must be replayed unchanged.
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        content: Vec<Value>,
        stop_reason: Option<String>,
        model: String,
        /// The provider's own account of the request, as received.
        usage: Value,
        /// The same in one shape for every provider; absent in logs written
        /// before format 3, and when the provider sent no count.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<Tokens>,
    },
    /// The conversation grew past the context limit and was replaced by a
    /// summary: requests after this start from `summary`, followed by any
    /// user message the model had not answered yet. Nothing before it is
    /// replayed. Since format 3.
    #[serde(rename = "context/compacted")]
    ContextCompacted {
        summary: String,
        /// About how many tokens the conversation held before.
        tokens_before: u64,
        /// What the request that wrote the summary cost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<Tokens>,
    },
    #[serde(rename = "approval/requested")]
    ApprovalRequested {
        tool_use_id: String,
        tool: String,
        /// Which rule asked: `command`, `edit`, `outside_read`, `outside_write`, or `git_write`.
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
    /// The user undid the agent's edits from `turn` on. The model is told at
    /// its next message. Since format 3.
    #[serde(rename = "files/restored")]
    FilesRestored {
        turn: u64,
        /// Files put back to what they held before those edits, or removed
        /// when the agent had created them; paths as the tool results show them.
        restored: Vec<String>,
        /// Files left alone because they changed after the agent's last edit.
        skipped: Vec<String>,
    },
    #[serde(rename = "turn/end")]
    TurnEnd {
        turn: u64,
        /// `completed`, `cancelled`, `error`, `refusal`, `max_tokens`, `step_limit`, or `interrupted`.
        reason: String,
        error: Option<String>,
    },
}
