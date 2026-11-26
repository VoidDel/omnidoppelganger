//! Durable session events. Everything a model request contains is derived
//! from these, so a new model-visible input needs a new event here.

use std::path::Path;

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

/// A picture the user attached to a message: a pasted screenshot, a photo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// One of [`IMAGE_TYPES`].
    pub media_type: String,
    /// The file's bytes in base64.
    pub data: String,
}

/// The formats every provider here that takes images accepts.
pub const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
/// The most pictures one message may carry.
pub const IMAGES_PER_MESSAGE: usize = 8;
/// The longest base64 text of one picture: 5 MB, the tightest limit among the providers.
pub const IMAGE_BASE64_BYTES: usize = 5_000_000;

/// Whether a message's pictures can be sent: few enough, of a known format, small enough, and base64.
pub fn check_images(images: &[Image]) -> Result<(), String> {
    if images.len() > IMAGES_PER_MESSAGE {
        return Err(format!("a message takes at most {IMAGES_PER_MESSAGE} images, not {}", images.len()));
    }
    for (index, image) in images.iter().enumerate() {
        let name = format!("image {}", index + 1);
        if !IMAGE_TYPES.contains(&image.media_type.as_str()) {
            return Err(format!("{name} is {}; only PNG, JPEG, GIF, and WebP images can be sent", image.media_type));
        }
        if image.data.is_empty() || !image.data.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')) {
            return Err(format!("{name} is not base64 data"));
        }
        if image.data.len() > IMAGE_BASE64_BYTES {
            return Err(format!("{name} is larger than the 3.7 MB an image may be"));
        }
    }
    Ok(())
}

/// A file the user attached to a message, by where it is on disk. Nothing is
/// copied: the agent reads it where it lies, and is let to without asking,
/// wherever that is, because the user pointed at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    /// Its absolute path.
    pub path: String,
    /// Its size when it was attached.
    pub bytes: u64,
    /// Its text as it was then, when it was small enough to send with the
    /// message: the model has it without reading. Absent for a larger file,
    /// which the model reads in parts with `read_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// For a document (a PDF, a Word, Excel, or PowerPoint file), what kind
    /// it is; its `content` is then the text taken out of it, not the file
    /// as it is. See [`crate::documents`]. Since format 14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<crate::documents::Kind>,
    /// For a document, how many pages, sheets, or slides it has; none for Word. Since format 14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<u32>,
    /// True when the user attached a folder rather than a regular file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dir: bool,
}

/// The most files one message may carry.
pub const FILES_PER_MESSAGE: usize = 10;
/// The largest file whose text goes with the message: about 25,000 tokens of
/// English and more of Chinese, which is what one read of a file is held to.
pub const ATTACHED_INLINE_BYTES: u64 = 100_000;
/// The largest file that can be attached at all: `read_file` reads none larger.
pub const ATTACHED_BYTES: u64 = crate::tools::READ_FILE_BYTES;
/// The largest picture read from a file: its base64 text is a third longer, and must fit [`IMAGE_BASE64_BYTES`].
pub const IMAGE_FILE_BYTES: u64 = (IMAGE_BASE64_BYTES / 4 * 3) as u64;

/// What a path the user picked turns out to be: a file to attach, a picture
/// to send as one, or neither, with why in a word the client puts into its
/// own: `missing`, `directory`, `binary`, `too_large`, `image_too_large`, or
/// for a document one of [`crate::documents::DocError::word`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Picked {
    /// `inline` when its text will go with the message; otherwise the agent
    /// reads it in parts. `document` and `parts` as [`Attached`] has them.
    File {
        path: String,
        name: String,
        bytes: u64,
        inline: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        document: Option<crate::documents::Kind>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parts: Option<u32>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_dir: bool,
    },
    Image { name: String, image: Image },
    Refused { name: String, why: &'static str },
}

/// The media type of a picture by its file name's ending; `None` for any other file.
pub(crate) fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Bytes as base64 text, padded.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for at in 0..4 {
            // Three bytes make four characters; a last group of one or two bytes makes two or three, then padding.
            out.push(if at <= chunk.len() { ALPHABET[(group >> (18 - 6 * at)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

/// A file to attach, read now: its size, and its text when that is short
/// enough to go with the message. Unsupported, binary, oversized, or
/// non-extractable files still attach by path (`content: None`) so the model
/// can inspect or process them directly with its tools.
pub fn attach(path: &Path) -> Result<Attached, &'static str> {
    if !path.is_absolute() {
        return Err("missing");
    }
    let metadata = std::fs::metadata(path).map_err(|_| "missing")?;
    let path_str = path.to_string_lossy().into_owned();
    if metadata.is_dir() {
        return Ok(Attached { path: path_str, bytes: 0, content: None, document: None, parts: None, is_dir: true });
    }
    let bytes_len = metadata.len();
    // A document goes as its words when extractable and short enough; otherwise attach by path so the model can inspect it directly.
    if let Some(found) = crate::documents::kind_of_file(path) {
        let Ok(kind) = found else {
            return Ok(Attached { path: path_str, bytes: bytes_len, content: None, document: None, parts: None, is_dir: false });
        };
        if bytes_len <= crate::documents::DOCUMENT_BYTES
            && let Ok(read) = crate::documents::file_text(path, kind).map_err(|_| "missing")?
        {
            let inline = read.text.len() as u64 <= ATTACHED_INLINE_BYTES;
            return Ok(Attached {
                path: path_str,
                bytes: bytes_len,
                content: inline.then(|| read.text.clone()),
                document: Some(kind),
                parts: read.parts.map(|parts| u32::try_from(parts).unwrap_or(u32::MAX)),
                is_dir: false,
            });
        }
        return Ok(Attached { path: path_str, bytes: bytes_len, content: None, document: Some(kind), parts: None, is_dir: false });
    }
    let content = if bytes_len <= ATTACHED_INLINE_BYTES {
        let bytes = std::fs::read(path).map_err(|_| "missing")?;
        (!bytes.iter().take(8192).any(|byte| *byte == 0)).then(|| String::from_utf8_lossy(&bytes).into_owned())
    } else {
        None
    };
    Ok(Attached { path: path_str, bytes: bytes_len, content, document: None, parts: None, is_dir: false })
}

/// What a path the user picked is, for the client to show before the message is sent.
pub fn pick(path: &Path) -> Picked {
    let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |name| name.to_string_lossy().into_owned());
    if let Some(media_type) = image_type(path) {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= IMAGE_FILE_BYTES => {
                return match std::fs::read(path) {
                    Ok(bytes) => Picked::Image { name, image: Image { media_type: media_type.to_string(), data: base64(&bytes) } },
                    Err(_) => Picked::Refused { name, why: "missing" },
                };
            }
            Ok(_) => {
                // Oversized image or folder falls through to path-only attachment.
            }
            Err(_) => return Picked::Refused { name, why: "missing" },
        }
    }
    match attach(path) {
        Ok(file) => Picked::File {
            path: file.path,
            name,
            bytes: file.bytes,
            inline: file.content.is_some(),
            document: file.document,
            parts: file.parts,
            is_dir: file.is_dir,
        },
        Err(why) => Picked::Refused { name, why },
    }
}

/// How an attached file or folder is put to the model, ahead of the words sent with it:
/// its text between tags that name it, or for a large/unsupported file or folder its path so the model can read it directly.
pub fn attached_text(file: &Attached) -> String {
    if file.is_dir {
        return format!(
            "<attached_folder path=\"{}\">\n[Folder path: {}]\nThe user attached this folder. Inspect or work with files inside this directory directly using your tools without asking.\n</attached_folder>",
            file.path, file.path
        );
    }
    let parts = file.parts.map(|parts| parts as usize);
    match (&file.content, file.document) {
        (Some(content), None) => format!(
            "<attached_file path=\"{}\">\n[File path: {}]\n{}\n</attached_file>",
            file.path,
            file.path,
            content.trim_end_matches('\n')
        ),
        (None, None) => format!(
            "<attached_file path=\"{}\" bytes=\"{}\">\n[File path: {}]\nThe user attached this file. Its contents are not inlined (it may be large, binary, or an unsupported format): read or inspect it directly from this path with read_file or other tools without asking.\n</attached_file>",
            file.path, file.bytes, file.path
        ),
        (Some(content), Some(kind)) => format!(
            "<attached_file path=\"{}\">\n[It is {} File path: {} — you may also inspect or process the original file directly from this path with your tools without asking.]\n{}\n</attached_file>",
            file.path,
            crate::documents::describe(kind, parts),
            file.path,
            content.trim_end_matches('\n')
        ),
        (None, Some(kind)) if parts.is_none() => format!(
            "<attached_file path=\"{}\" bytes=\"{}\">\n[File path: {}]\nThe user attached this file, {}. Plain text could not be extracted automatically or the file is large: inspect it directly from this path using your tools without asking.\n</attached_file>",
            file.path,
            file.bytes,
            file.path,
            crate::documents::name(kind, None)
        ),
        (None, Some(kind)) => format!(
            "<attached_file path=\"{}\" bytes=\"{}\">\n[File path: {}]\nThe user attached this file, {}. Its text is too long to include here: read the parts you need with read_file, which gives a document's text and may read this path without asking.\n</attached_file>",
            file.path,
            file.bytes,
            file.path,
            crate::documents::name(kind, parts)
        ),
    }
}

/// A skill as the model is told of it; see [`crate::skills`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// The Git worktree a session works in, made for it so that it shares no
/// files with other sessions at work on the same repository. See [`crate::worktree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// The folder the user chose, in the repository's own checkout. The session's workspace stands for it.
    pub origin: String,
    /// The branch made for the worktree.
    pub branch: String,
    /// The top of the worktree's checkout.
    pub checkout: String,
}

/// Where a session's conversation comes from, when its log began as a copy
/// of another session's: a fork, made to go on from an earlier point another way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    /// Id of the session the conversation was copied from.
    pub session: String,
    /// The last turn that was copied; 0 when none was, only what came before the first.
    pub turn: u64,
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

/// One question of an `ask_user` call, as the user is shown it. Since format 17.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A few words naming the question, on its tab.
    pub header: String,
    pub question: String,
    pub kind: QuestionKind,
    /// What may be chosen; none for a question answered in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Choice>,
}

/// How a question is answered: one option, any of them, or in the user's own words. A question with
/// options may be answered in words as well, under "其他".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Single,
    Multiple,
    Text,
}

/// An option of a question: what it is called and, if it needs it, what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// The user's answer to one question: the labels chosen, and anything written. Both empty when it was
/// left unanswered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl Reply {
    pub fn is_empty(&self) -> bool {
        self.chosen.is_empty() && self.text.trim().is_empty()
    }
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
        /// Id of the session whose agent set this one off as a subagent;
        /// absent for a session the user started. Since format 8.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Set when `workspace` is in a worktree made for the session. Since format 8.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<Worktree>,
        /// Name of the schedule that started the session and gave it its
        /// first message, with nobody there; see [`crate::schedule`]. Since format 9.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schedule: Option<String>,
        /// Set when the events after this one, up to the end of a turn, were
        /// copied from another session's log. Since format 10.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<Fork>,
        /// Id of the session this one is a side conversation (旁聊) beside: a read-only session the user talks
        /// to while that one's agent works, told how it is getting on. Since format 20.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aside: Option<String>,
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
    /// The skills on offer when a turn started, logged when they differ
    /// from the last time. The latest list is part of the system prompt of
    /// every request after it. Since format 6.
    #[serde(rename = "skills/listed")]
    SkillsListed { skills: Vec<SkillInfo> },
    /// The user moved the session to another model. Requests after this go
    /// to `model`. Within a provider the conversation is sent as it is: the
    /// provider leaves out the reasoning its new model cannot read. Since format 7.
    #[serde(rename = "model/changed")]
    ModelChanged {
        model: String,
        /// The provider moved to, when it is another than the one the
        /// session was on; absent for a model of the same one. What earlier
        /// providers answered is then sent in a form any provider takes
        /// (see [`crate::session::derive_messages`]). Since format 15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    /// The user set how hard the model works on this session's requests, one
    /// of [`crate::settings::EFFORTS`], in place of the settings' default. Since format 15.
    #[serde(rename = "effort/changed")]
    EffortChanged { effort: String },
    /// The user named the session. Until then its title is the first message. Since format 3.
    #[serde(rename = "session/renamed")]
    SessionRenamed { title: String },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    /// In a side conversation (旁聊): how the session it is beside was getting on when the user's next message
    /// joined, written for the model. It goes ahead of that message's words in the request; a briefing like the
    /// last one is not logged again. See [`crate::aside`]. Since format 20.
    #[serde(rename = "aside/briefed")]
    AsideBriefed { brief: String },
    /// Conversation handle on an external agent CLI backend (e.g. `antigravity` / `agy`),
    /// stored so subsequent turns and app restarts resume the same external conversation. Since format 21.
    #[serde(rename = "external/session")]
    ExternalSession { backend: String, session_id: String },
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
        /// Pictures sent with the message, shown to the model ahead of the
        /// text. Since format 5.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
        /// Files attached to the message, shown to the model after the
        /// pictures and ahead of the text. Since format 13.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<Attached>,
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
        /// Optional user-specified preservation focus (from `/compact <focus>`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<String>,
        /// Earliest completed turn kept verbatim after `summary` (sliding recent tail).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_from_turn: Option<u64>,
        /// What the request that wrote the summary cost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<Tokens>,
    },
    /// A `delegate` call set off a subagent: `session` is its own session,
    /// with its own log, named `name`. It is here for whoever reads this
    /// log to find the subagent's; the model reads the subagents' reports in
    /// the call's result. Since format 8.
    #[serde(rename = "subagent/started")]
    SubagentStarted { tool_use_id: String, session: String, name: String },
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
        /// RFC 3339 time at which, with no answer, the request is declined by itself. Set in auto
        /// mode, where nobody may be there to answer. Since format 12.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
    },
    #[serde(rename = "approval/resolved")]
    ApprovalResolved {
        tool_use_id: String,
        decision: Decision,
        /// Nobody answered in time, and the request was declined by itself. Since format 12.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        timed_out: bool,
    },
    /// The agent asked the user questions with `ask_user`, and waits for the answers. Since format 17.
    #[serde(rename = "question/asked")]
    QuestionAsked {
        tool_use_id: String,
        questions: Vec<Question>,
        /// RFC 3339 time at which, with no answer, the agent goes on deciding for itself. Set in
        /// auto and bypass mode, where nobody may be there to answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<String>,
    },
    /// What came of the questions: an answer to each, in their order; or none, when the user would
    /// rather the agent decided, or nobody answered in time. Since format 17.
    #[serde(rename = "question/answered")]
    QuestionAnswered {
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        answers: Vec<Reply>,
        /// The user chose not to answer: the agent decides.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        declined: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        timed_out: bool,
    },
    /// The guardian, a model asked in auto mode about a command the rules
    /// cannot pass, judged it: allowed it to run, or left it to the user.
    /// Never sent to the model that works. Since format 12.
    #[serde(rename = "guardian/reviewed")]
    GuardianReviewed {
        tool_use_id: String,
        /// The model that judged.
        model: String,
        allowed: bool,
        /// Why, in a sentence; or, when the guardian could not be asked, what went wrong.
        reason: String,
        /// The verdict line as the guardian wrote it, up to 200 characters; empty when it could
        /// not be asked, and in logs written before it was kept. A build that does not know it
        /// ignores it, so it needs no new format.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        verdict: String,
        /// The guardian could not be asked, so the user is.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        failed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<Tokens>,
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
        /// Pictures returned to the model with the text: an image `read_file` read. Since format 18.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
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
    /// Files of the kinds a 分身 makes for the user (documents, pictures, media, archives) that were new or changed in
    /// the workspace when the turn ended, found by looking, not told by a tool: what a script or a command left there.
    /// Absolute paths. Never sent to the model. Since format 16; `snapshots` and `sizes` since format 19.
    #[serde(rename = "files/made")]
    FilesMade {
        turn: u64,
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        snapshots: std::collections::BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        sizes: std::collections::BTreeMap<String, u64>,
    },
    /// The user restored a deliverable to an earlier snapshotted version. The model is told at its next message.
    /// Since format 19.
    #[serde(rename = "artifact/restored")]
    ArtifactRestored {
        /// Absolute path of the restored file in the workspace.
        file: String,
        /// Display path relative to the workspace.
        path: String,
        /// 1-based version number restored to.
        version: u32,
        /// Snapshot name written back to `file`.
        snapshot: String,
        /// Size in bytes of the restored file.
        bytes: u64,
        /// If the live file on disk held unrecorded changes before restoring, their snapshot is saved here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_bytes: Option<u64>,
    },
    #[serde(rename = "turn/end")]
    TurnEnd {
        turn: u64,
        /// `completed`, `cancelled`, `error`, `refusal`, `max_tokens`, `step_limit`, or `interrupted`.
        reason: String,
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_path_is_a_file_a_picture_or_refused_with_why() {
        let directory = tempfile::tempdir().unwrap();
        let at = |name: &str| directory.path().join(name);
        let write = |name: &str, bytes: &[u8]| std::fs::write(at(name), bytes).unwrap();
        write("notes.md", "# 笔记\nline two\n".as_bytes());
        write("big.log", "x".repeat(ATTACHED_INLINE_BYTES as usize + 1).as_bytes());
        write("data.bin", &[1, 2, 0, 3]);
        write("shot.PNG", &[0x89, b'P', b'N', b'G', 0, 1]);
        std::fs::create_dir(at("folder")).unwrap();

        // A small text file goes with the message, text and all.
        let small = attach(&at("notes.md")).unwrap();
        assert_eq!((small.bytes, small.content.as_deref()), (18, Some("# 笔记\nline two\n")));
        assert_eq!(pick(&at("notes.md")), Picked::File { path: small.path.clone(), name: "notes.md".into(), bytes: 18, inline: true, document: None, parts: None, is_dir: false });
        // A large one is attached by its path alone.
        let large = attach(&at("big.log")).unwrap();
        assert_eq!((large.bytes, large.content), (ATTACHED_INLINE_BYTES + 1, None));
        assert!(matches!(pick(&at("big.log")), Picked::File { inline: false, is_dir: false, .. }));
        // A binary or unsupported file is attached by its path alone so the model can inspect it directly.
        let bin = attach(&at("data.bin")).unwrap();
        assert_eq!((bin.bytes, bin.content, bin.document, bin.is_dir), (4, None, None, false));
        assert!(matches!(pick(&at("data.bin")), Picked::File { inline: false, document: None, is_dir: false, .. }));
        // A folder is attached by its path alone with `is_dir: true`.
        let folder = attach(&at("folder")).unwrap();
        assert_eq!((folder.bytes, folder.content, folder.is_dir), (0, None, true));
        assert!(matches!(pick(&at("folder")), Picked::File { inline: false, is_dir: true, .. }));
        // A missing path or relative path is still refused.
        assert_eq!(attach(&at("gone.txt")), Err("missing"));
        assert_eq!(attach(Path::new("notes.md")), Err("missing"));
        assert_eq!(pick(&at("gone.txt")), Picked::Refused { name: "gone.txt".into(), why: "missing" });
        // A picture, whatever the case of its ending, is read as one, though its bytes are not text.
        match pick(&at("shot.PNG")) {
            Picked::Image { name, image } => assert_eq!((name.as_str(), image.media_type.as_str(), image.data.as_str()), ("shot.PNG", "image/png", "iVBORwAB")),
            other => panic!("{other:?}"),
        }
        assert!(check_images(&[Image { media_type: "image/png".into(), data: base64(&[1, 2, 3, 4]) }]).is_ok());
    }

    #[test]
    fn base64_pads_what_is_not_a_multiple_of_three() {
        assert_eq!([base64(b""), base64(b"f"), base64(b"fo"), base64(b"foo"), base64(b"foob")], ["", "Zg==", "Zm8=", "Zm9v", "Zm9vYg=="]);
    }

    #[test]
    fn an_attached_file_is_put_to_the_model_by_name() {
        let small = Attached { path: "/x/notes.md".into(), bytes: 9, content: Some("one\ntwo\n".into()), document: None, parts: None, is_dir: false };
        assert_eq!(attached_text(&small), "<attached_file path=\"/x/notes.md\">\n[File path: /x/notes.md]\none\ntwo\n</attached_file>");
        let large = Attached { path: "/x/big.log".into(), bytes: 300_000, content: None, document: None, parts: None, is_dir: false };
        let told = attached_text(&large);
        assert!(told.starts_with("<attached_file path=\"/x/big.log\" bytes=\"300000\">\n[File path: /x/big.log]\n") && told.contains("read_file") && told.contains("without asking"), "{told}");
        let dir = Attached { path: "/x/docs".into(), bytes: 0, content: None, document: None, parts: None, is_dir: true };
        let told = attached_text(&dir);
        assert!(told.starts_with("<attached_folder path=\"/x/docs\">\n[Folder path: /x/docs]\n") && told.contains("without asking"), "{told}");
        // A document is said to be one, carries its explicit file path, and what goes with it is its text.
        use crate::documents::Kind;
        let sheet = Attached { path: "/x/sheet.pdf".into(), bytes: 52_000, content: Some("[page 1 of 1]\nVIN\n".into()), document: Some(Kind::Pdf), parts: Some(1), is_dir: false };
        let told = attached_text(&sheet);
        assert!(told.starts_with("<attached_file path=\"/x/sheet.pdf\">\n[It is a PDF of 1 page. This is its text") && told.contains("File path: /x/sheet.pdf") && told.ends_with("[page 1 of 1]\nVIN\n</attached_file>"), "{told}");
        let deck = Attached { path: "/x/deck.pptx".into(), bytes: 9_000_000, content: None, document: Some(Kind::PowerPoint), parts: Some(30), is_dir: false };
        let told = attached_text(&deck);
        assert!(told.starts_with("<attached_file path=\"/x/deck.pptx\" bytes=\"9000000\">\n[File path: /x/deck.pptx]\nThe user attached this file, a presentation of 30 slides.") && told.contains("read_file"), "{told}");
    }

    #[test]
    fn a_document_is_attached_as_its_text_or_refused_with_why() {
        use crate::documents::{Kind, tests::*};
        use crate::pdf::tests::sample;
        let directory = tempfile::tempdir().unwrap();
        let at = |name: &str| directory.path().join(name);
        std::fs::write(at("sheet.pdf"), sample(&[&["Pin 1 is VIN"], &["Absolute maximum 42 V"]])).unwrap();
        // Many pages of words: more than goes with a message.
        let long = vec!["A line of words that goes on for a while, sixty characters."; 40];
        std::fs::write(at("manual.pdf"), sample(&vec![long.as_slice(); 60])).unwrap();
        std::fs::write(at("scan.pdf"), sample(&[&[], &[]])).unwrap();
        std::fs::write(at("broken.pdf"), b"%PDF-1.7\nnot really a PDF").unwrap();
        std::fs::write(at("说明.docx"), word_document(&paragraph(None, "先装驱动。"))).unwrap();
        std::fs::write(at("old.ppt"), [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).unwrap();

        let sheet = attach(&at("sheet.pdf")).unwrap();
        assert_eq!((sheet.document, sheet.parts), (Some(Kind::Pdf), Some(2)));
        assert!(sheet.content.as_deref().is_some_and(|text| text.contains("[page 2 of 2]") && text.contains("Absolute maximum 42 V")), "{sheet:?}");
        assert!(matches!(pick(&at("sheet.pdf")), Picked::File { inline: true, document: Some(Kind::Pdf), parts: Some(2), .. }));
        let manual = attach(&at("manual.pdf")).unwrap();
        assert_eq!((manual.parts, manual.content), (Some(60), None));
        let word = attach(&at("说明.docx")).unwrap();
        assert_eq!((word.document, word.parts, word.content.as_deref()), (Some(Kind::Word), None, Some("先装驱动。")));
        let scan = attach(&at("scan.pdf")).unwrap();
        assert_eq!((scan.document, scan.parts, scan.content.as_deref()), (Some(Kind::Pdf), None, None));
        assert!(attached_text(&scan).contains("Plain text could not be extracted automatically"));
        let broken = attach(&at("broken.pdf")).unwrap();
        assert_eq!((broken.document, broken.parts, broken.content.as_deref()), (Some(Kind::Pdf), None, None));
        let old_ppt = attach(&at("old.ppt")).unwrap();
        assert_eq!((old_ppt.document, old_ppt.parts, old_ppt.content.as_deref()), (None, None, None));
        assert!(matches!(pick(&at("scan.pdf")), Picked::File { inline: false, document: Some(Kind::Pdf), parts: None, .. }));
    }
}
