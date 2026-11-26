//! The append-only session log on disk and the projections derived from it.
//!
//! A session is one file, `<root>/<id>/session.v1.jsonl`, one [`Envelope`]
//! per line. Lines are only ever appended.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::events::{Envelope, Event, Fork, SkillInfo, Worktree};
use crate::fsutil::ensure_private_dir;
use crate::policy::Mode;
use crate::providers::DEFAULT_PROVIDER;
use crate::sync_ext::MutexExt;

/// The newest format this build reads, and the one it writes. Format 2 added
/// the `provider` field to the header. Format 3 added the `context/compacted`,
/// `files/restored`, and `session/renamed` events and the `tokens` field.
/// Format 4 added the `instructions/loaded` event and the `allow_always`
/// decision. Format 5 added `images` on a user message. Format 6 adds the
/// `skills/listed` event. Format 7 adds the `model/changed` event. Format 8
/// adds `parent` and `worktree` to the header and the `subagent/started` event.
/// Format 9 adds `schedule` to the header, and format 10 `fork`. Format 11
/// adds the `bypass` mode a message can be sent in. Format 12 adds the
/// `guardian/reviewed` event, `expires_at` on an approval request and
/// `timed_out` on its answer. Format 13 adds `files` on a user message, and
/// format 14 a document's kind and parts on an attached file. Format 15 adds
/// `provider` on `model/changed`, for a move to another provider, and the
/// `effort/changed` event. Format 16 adds the `files/made` event, and format 17
/// the `question/asked` and `question/answered` events. Format 18 adds `images`
/// on a tool result, and the `delete_file` and `move_file` edits, whose undoing
/// an older build would not know. Format 19 adds snapshots on `files/made` and
/// the `artifact/restored` event for deliverable version history. Format 20
/// adds `aside` to the header and the `aside/briefed` event, for side
/// conversations. Format 21 adds the `external/session` event for external
/// CLI agent backends (such as Antigravity `agy`). A build from before a
/// format refuses its sessions rather than misread them.
pub const FORMAT_VERSION: u32 = 21;
const FILE_NAME: &str = "session.v1.jsonl";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session {0} does not exist")]
    NotFound(String),
    #[error("session log {path} is damaged at line {line}: {message}")]
    Damaged { path: String, line: usize, message: String },
    #[error("session log {0} has no session/created header")]
    MissingHeader(String),
    #[error("session format {0} is newer than this build reads")]
    FutureFormat(u32),
    /// What was asked for cannot be done with this session.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What a session list shows, folded from the log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub workspace: String,
    /// The provider and model the session is on now.
    pub provider: String,
    pub model: String,
    /// How hard the model works for this session, when set for it; else the settings' default holds.
    pub effort: Option<String>,
    /// Id of the session whose agent set this one off as a subagent.
    pub parent: Option<String>,
    /// The worktree made for the session, when it works apart from the repository's own checkout.
    pub worktree: Option<Worktree>,
    /// Name of the schedule that started the session, when one did.
    pub schedule: Option<String>,
    /// Where the conversation was copied from, when the session is a fork.
    pub fork: Option<Fork>,
    /// The session this one is a side conversation (旁聊) beside, when it is one.
    pub aside: Option<String>,
    /// The mode of the last message, which a reopened session resumes.
    pub mode: Mode,
    pub created_at: String,
    pub updated_at: String,
    /// A turn has started and not ended.
    pub turn_open: bool,
    /// An approval was requested and not resolved.
    pub awaiting_approval: bool,
}

type CachedSummary = (Option<std::time::SystemTime>, u64, SessionSummary);

const MAX_SUMMARY_CACHE_ENTRIES: usize = 4096;

#[derive(Clone)]
pub struct SessionStore {
    root: PathBuf,
    summary_cache: std::sync::Arc<std::sync::Mutex<HashMap<String, CachedSummary>>>,
}

/// What a session is created with.
pub struct NewSession<'a> {
    /// The directory the agent works in.
    pub workspace: &'a Path,
    pub provider: &'a str,
    pub model: &'a str,
    /// The session whose agent sets this one off as a subagent.
    pub parent: Option<&'a str>,
    /// The worktree `workspace` is in, when one was made for the session.
    pub worktree: Option<Worktree>,
    /// Name of the schedule that starts the session.
    pub schedule: Option<&'a str>,
    /// The session this one is a side conversation beside.
    pub aside: Option<&'a str>,
}

fn open_new_private_log_file(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            summary_cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Root folder holding stored session directories.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Evict a cached summary entry for `id` (if present).
    pub fn invalidate_cache(&self, id: &str) {
        self.summary_cache.lock_unpoisoned().remove(id);
    }

    /// The store a log is kept in.
    pub fn of(log: &SessionLog) -> Self {
        let root = log
            .path()
            .parent()
            .and_then(Path::parent)
            .expect("a log sits in its session's directory, inside the store");
        Self::new(root)
    }

    /// Create a session whose agent works in `workspace` with `model` from `provider`.
    pub fn create(&self, workspace: &Path, provider: &str, model: &str) -> Result<SessionLog, SessionError> {
        self.create_with(NewSession {
            workspace,
            provider,
            model,
            parent: None,
            worktree: None,
            schedule: None,
            aside: None,
        })
    }

    /// Create a session: a subagent's, one that works in a worktree, one a schedule starts, a side conversation, or a plain one.
    pub fn create_with(&self, new: NewSession) -> Result<SessionLog, SessionError> {
        let NewSession {
            workspace,
            provider,
            model,
            parent,
            worktree,
            schedule,
            aside,
        } = new;
        let id = uuid::Uuid::now_v7().to_string();
        let directory = self.root.join(&id);
        ensure_private_dir(&self.root)?;
        ensure_private_dir(&directory)?;
        let path = directory.join(FILE_NAME);
        let file = open_new_private_log_file(&path)?;
        let mut log = SessionLog {
            id: id.clone(),
            path,
            events: Vec::new(),
            file,
        };
        log.append(Event::SessionCreated {
            id,
            workspace: workspace.to_string_lossy().into_owned(),
            provider: (provider != DEFAULT_PROVIDER).then(|| provider.to_string()),
            model: model.to_string(),
            format: FORMAT_VERSION,
            parent: parent.map(str::to_string),
            worktree,
            schedule: schedule.map(str::to_string),
            fork: None,
            aside: aside.map(str::to_string),
        })?;
        Ok(log)
    }

    /// Start a session that goes on from a point in another's conversation:
    /// a fork. Its log begins as a copy of `events`, the other session's log,
    /// up to where turn `before` starts; or of the whole of it when `before`
    /// is none, less a turn still running at the end. The copied events keep
    /// their times. The fork works in the same workspace, on the same
    /// provider and model, and is the user's own: a subagent's session is
    /// refused, since its agent waits for one report.
    ///
    /// Only the conversation is copied. The files are the workspace's, as
    /// they are now, and the snapshots that undo the copied turns' edits stay
    /// with the session that made them: see [`crate::undo::plan`].
    pub fn fork(&self, events: &[Envelope], before: Option<u64>) -> Result<SessionLog, SessionError> {
        let Some(Event::SessionCreated {
            id: source,
            workspace,
            provider,
            model,
            parent,
            worktree,
            schedule,
            aside,
            ..
        }) = events.first().map(|envelope| &envelope.event)
        else {
            return Err(SessionError::Refused("the session to fork has no header".to_string()));
        };
        if parent.is_some() {
            return Err(SessionError::Refused(
                "a subagent's session cannot be forked; fork the session of the agent that set it off".to_string(),
            ));
        }
        if aside.is_some() {
            return Err(SessionError::Refused(
                "a side conversation cannot be forked".to_string(),
            ));
        }
        let starts = |wanted: u64| {
            events
                .iter()
                .position(|envelope| matches!(envelope.event, Event::TurnStart { turn } if turn == wanted))
        };
        let end = match before.or_else(|| open_turn(events)) {
            Some(turn) => starts(turn)
                .ok_or_else(|| SessionError::Refused(format!("this session has no turn {turn} to fork before")))?,
            None => events.len(),
        };
        let copied = &events[1..end];
        let turn = copied
            .iter()
            .filter_map(|envelope| {
                if let Event::TurnStart { turn } = envelope.event {
                    Some(turn)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);

        let id = uuid::Uuid::now_v7().to_string();
        let directory = self.root.join(&id);
        ensure_private_dir(&self.root)?;
        ensure_private_dir(&directory)?;
        let path = directory.join(FILE_NAME);
        let file = open_new_private_log_file(&path)?;
        let mut log = SessionLog {
            id: id.clone(),
            path,
            events: Vec::new(),
            file,
        };
        log.append(Event::SessionCreated {
            id,
            workspace: workspace.clone(),
            provider: provider.clone(),
            model: model.clone(),
            format: FORMAT_VERSION,
            parent: None,
            worktree: worktree.clone(),
            schedule: schedule.clone(),
            fork: Some(Fork {
                session: source.clone(),
                turn,
            }),
            aside: None,
        })?;
        for envelope in copied {
            log.append_at(envelope.ts.clone(), envelope.event.clone())?;
        }
        Ok(log)
    }

    fn path(&self, id: &str) -> Result<PathBuf, SessionError> {
        // An id is one directory name; anything else could point outside the store.
        let plain = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let path = self.root.join(id).join(FILE_NAME);
        if plain && path.is_file() {
            Ok(path)
        } else {
            Err(SessionError::NotFound(id.to_string()))
        }
    }

    /// The `snapshots` directory beside session `id`'s log, when `id` names a stored session.
    pub fn snapshots_dir(&self, id: &str) -> Option<PathBuf> {
        Some(self.path(id).ok()?.parent()?.join("snapshots"))
    }

    /// The source session id when `id` was forked from another session, read from its header alone.
    pub fn fork_source(&self, id: &str) -> Option<String> {
        let path = self.path(id).ok()?;
        let line = std::io::BufRead::lines(std::io::BufReader::new(File::open(path).ok()?))
            .next()?
            .ok()?;
        let envelope: Envelope = serde_json::from_str(&line).ok()?;
        match envelope.event {
            Event::SessionCreated { fork: Some(fork), .. } => Some(fork.session),
            _ => None,
        }
    }

    /// Remove a session for good: its log and the file snapshots beside it.
    /// The caller must have no open handle on it.
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        let directory = self
            .path(id)?
            .parent()
            .expect("the log sits in the session's directory")
            .to_path_buf();
        self.invalidate_cache(id);
        Ok(fs::remove_dir_all(directory)?)
    }

    /// Whether the session's title, workspace, or anything the user or the
    /// agent said holds `query`; see [`matches`]. Reads only.
    pub fn search(&self, id: &str, query: &str) -> bool {
        self.path(id)
            .ok()
            .and_then(|path| parse(&path, &fs::read(&path).ok()?).ok())
            .is_some_and(|(events, _)| matches(&events, query))
    }

    /// Open a stored session for appending. A final line cut off by a crash is dropped.
    ///
    /// The caller must be the only writer: a log another handle is still
    /// appending to can look cut off mid-line, and this truncates it.
    pub fn open(&self, id: &str) -> Result<SessionLog, SessionError> {
        let path = self.path(id)?;
        let _ = ensure_private_dir(&self.root);
        if let Some(dir) = path.parent() {
            let _ = ensure_private_dir(dir);
        }
        let bytes = fs::read(&path)?;
        let (events, good_bytes) = parse(&path, &bytes)?;
        if good_bytes < bytes.len() {
            // Windows refuses to truncate through an append-only handle.
            OpenOptions::new().write(true).open(&path)?.set_len(good_bytes as u64)?;
            self.invalidate_cache(id);
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        #[cfg(unix)]
        if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_symlink()) {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
        }
        Ok(SessionLog {
            id: id.to_string(),
            path,
            events,
            file,
        })
    }

    /// Read a stored session's summary without changing its file, so it is
    /// safe while another handle appends to the log. Unchanged files hit an
    /// in-memory `(modified_time, file_len)` cache without re-parsing JSONL.
    pub fn peek(&self, id: &str) -> Result<SessionSummary, SessionError> {
        let path = self.path(id)?;
        let meta = fs::metadata(&path)?;
        let len = meta.len();
        let mtime = meta.modified().ok();
        if mtime.is_some()
            && let Some((cached_mtime, cached_len, cached_summary)) = self.summary_cache.lock_unpoisoned().get(id)
            && *cached_mtime == mtime
            && *cached_len == len
        {
            return Ok(cached_summary.clone());
        }
        let (events, _) = parse(&path, &fs::read(&path)?)?;
        let summary = summarize(&events);
        if mtime.is_some() {
            let mut cache = self.summary_cache.lock_unpoisoned();
            if cache.len() >= MAX_SUMMARY_CACHE_ENTRIES && !cache.contains_key(id) {
                cache.clear();
            }
            cache.insert(id.to_string(), (mtime, len, summary.clone()));
        }
        Ok(summary)
    }

    /// Read a stored (non-live) session's summary normalized to idle state
    /// (`turn_open: false`, `awaiting_approval: false`) for list/single-item queries.
    pub fn peek_idle(&self, id: &str) -> Result<SessionSummary, SessionError> {
        let summary = self.peek(id)?;
        Ok(SessionSummary {
            turn_open: false,
            awaiting_approval: false,
            ..summary
        })
    }

    /// Read a stored session's events without changing its file, as [`SessionStore::peek`] does.
    pub fn read(&self, id: &str) -> Result<Vec<Envelope>, SessionError> {
        let path = self.path(id)?;
        Ok(parse(&path, &fs::read(&path)?)?.0)
    }

    /// Ids of every stored session, in no particular order.
    pub fn ids(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect()
    }

    /// Summaries of every readable session, most recently updated first. Reads only.
    pub fn list(&self) -> Vec<SessionSummary> {
        let mut summaries: Vec<SessionSummary> = self.ids().iter().filter_map(|id| self.peek(id).ok()).collect();
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        summaries
    }
}

/// Parse a log's bytes into its events and the number of leading bytes that
/// hold complete lines. Only the last line may be incomplete or unreadable:
/// a writer was in the middle of it.
fn parse(path: &Path, bytes: &[u8]) -> Result<(Vec<Envelope>, usize), SessionError> {
    let mut events = Vec::new();
    let mut good_bytes = 0usize;
    let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let complete = line.ends_with(b"\n");
        let parsed = std::str::from_utf8(line)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str::<Envelope>(text.trim_end()).map_err(|error| error.to_string()));
        match parsed {
            Ok(envelope) if complete => {
                events.push(envelope);
                good_bytes += line.len();
            }
            _ if index + 1 == lines.len() => break,
            Ok(_) => unreachable!("only the last line can lack a newline"),
            Err(message) => {
                return Err(SessionError::Damaged {
                    path: path.display().to_string(),
                    line: index + 1,
                    message,
                });
            }
        }
    }
    match events.first().map(|envelope| &envelope.event) {
        Some(Event::SessionCreated { format, .. }) if *format > FORMAT_VERSION => {
            Err(SessionError::FutureFormat(*format))
        }
        Some(Event::SessionCreated { .. }) => Ok((events, good_bytes)),
        _ => Err(SessionError::MissingHeader(path.display().to_string())),
    }
}

pub struct SessionLog {
    id: String,
    path: PathBuf,
    events: Vec<Envelope>,
    file: File,
}

impl SessionLog {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn events(&self) -> &[Envelope] {
        &self.events
    }

    /// The workspace recorded when the session was created, and the provider and model it is on now.
    pub fn header(&self) -> (PathBuf, String, String) {
        match &self.events[0].event {
            Event::SessionCreated { workspace, .. } => {
                (PathBuf::from(workspace), provider(&self.events), model(&self.events))
            }
            _ => unreachable!("create and open both guarantee the header"),
        }
    }

    /// Append one event and flush it to disk before returning it.
    pub fn append(&mut self, event: Event) -> io::Result<Envelope> {
        self.append_at(
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            event,
        )
    }

    /// Append an event that happened at `ts`: now, or when it is copied from another log, then.
    fn append_at(&mut self, ts: String, event: Event) -> io::Result<Envelope> {
        let envelope = Envelope {
            seq: self.events.len() as u64 + 1,
            ts,
            event,
        };
        let mut line = serde_json::to_vec(&envelope).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.events.push(envelope.clone());
        Ok(envelope)
    }

    /// Close a turn that a crash left open: answer every unanswered tool call
    /// and end the turn as `interrupted`. Returns the appended events.
    pub fn repair_tail(&mut self) -> io::Result<Vec<Envelope>> {
        let Some(turn) = open_turn(&self.events) else {
            return Ok(Vec::new());
        };
        let mut appended = Vec::new();
        for (tool_use_id, tool) in unanswered_tool_uses(&self.events) {
            appended.push(self.append(Event::ToolResult {
                tool_use_id,
                tool,
                content: "The app closed before this tool ran.".to_string(),
                is_error: true,
                meta: Value::Null,
                images: Vec::new(),
            })?);
        }
        appended.push(self.append(Event::TurnEnd {
            turn,
            reason: "interrupted".to_string(),
            error: None,
        })?);
        Ok(appended)
    }
}

/// Number of the turn that has started and not ended, if any.
pub fn open_turn(events: &[Envelope]) -> Option<u64> {
    let mut open = None;
    for envelope in events {
        match &envelope.event {
            Event::TurnStart { turn } => open = Some(*turn),
            Event::TurnEnd { .. } => open = None,
            _ => {}
        }
    }
    open
}

/// `(tool_use_id, tool name)` of every tool call that has no result yet, in call order.
pub fn unanswered_tool_uses(events: &[Envelope]) -> Vec<(String, String)> {
    let mut open: Vec<(String, String)> = Vec::new();
    for envelope in events {
        match &envelope.event {
            Event::AssistantMessage { content, .. } => {
                for block in content.iter().filter(|block| block["type"] == "tool_use") {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        open.push((id.to_string(), name.to_string()));
                    }
                }
            }
            Event::ToolResult { tool_use_id, .. } => {
                if let Some(pos) = open.iter().position(|(id, _)| id == tool_use_id) {
                    open.remove(pos);
                }
            }
            Event::ContextCompacted {
                kept_from_turn: None, ..
            } => {
                open.clear();
            }
            _ => {}
        }
    }
    open
}

/// What the model reads, ahead of the user's message, when plan mode starts.
pub const PLAN_ON: &str = "[Plan mode is on] The user wants a plan before any change. Explore and think, but change nothing: do not edit files, and run only commands that change nothing. Once you understand the task, present a concrete plan with `present_plan` (what you would change, where, and how you would verify it); your turn ends with it, and the user switches modes when they want it carried out.";
/// What the model reads, ahead of the user's message, when plan mode ends.
pub const PLAN_OFF: &str = "[Plan mode is off] You may change files and run commands again. Carry out the plan you proposed unless the user says otherwise.";

/// The `messages` array of a model request, derived from the log alone.
///
/// Tool results and user text between two assistant messages form one user
/// message; the log orders results before any steering text, which is the
/// order the API requires. A message sent in a different mode than the one
/// before is preceded by [`PLAN_ON`] or [`PLAN_OFF`] when plan mode starts or
/// ends there; the other modes only change what asks, which the model sees
/// in its tool results.
///
/// A [`Event::FilesRestored`] becomes a line telling the model what was undone.
/// A [`Event::ContextCompacted`] starts the conversation over: nothing before
/// it is replayed. The request then opens with its summary, followed by the
/// user's messages the model had not answered yet, in their own words.
///
/// What a provider answered stays as it came while the session is on that
/// provider. Once it has moved to another, what earlier ones answered is sent
/// in the form every provider takes: see [`carried`].
struct PrescanMeta<'a> {
    speaker: String,
    planning: bool,
    todos: Option<Vec<(String, String)>>,
    brief: Option<&'a str>,
}

/// Zero-copy prefix scan before `replay_start` for [`derive_messages`], extracting only the
/// active `speaker`, `planning` mode state, latest valid `todos` list, and any unconsumed
/// `AsideBriefed` without cloning historical Base64 images or tool result payloads.
fn prescan_metadata_before_replay(events: &[Envelope]) -> PrescanMeta<'_> {
    let mut speaker = match events.first().map(|envelope| &envelope.event) {
        Some(Event::SessionCreated { provider, .. }) => {
            provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
        }
        _ => DEFAULT_PROVIDER.to_string(),
    };
    let mut planning = false;
    let mut todos: Option<Vec<(String, String)>> = None;
    let mut brief: Option<&str> = None;
    for envelope in events {
        match &envelope.event {
            Event::AsideBriefed { brief: told } => brief = Some(told),
            Event::UserMessage { mode, .. } => {
                planning = *mode == Mode::Plan;
                brief = None;
            }
            Event::ModelChanged {
                provider: Some(provider),
                ..
            } => speaker = provider.clone(),
            Event::AssistantMessage { content, .. } if !content.is_empty() => {
                for block in content
                    .iter()
                    .filter(|block| block["type"] == "tool_use" && block["name"] == "update_todos")
                {
                    if let Ok(items) = crate::tools::todo_items(&block["input"]) {
                        todos = Some(items);
                    }
                }
            }
            _ => {}
        }
    }
    PrescanMeta {
        speaker,
        planning,
        todos,
        brief,
    }
}

/// Compute the index in `events` from which [`derive_messages`] must construct full message
/// blocks so its output is 100% identical to replaying from index `0`.
fn derive_replay_start(events: &[Envelope]) -> usize {
    let Some(compacted_idx) = events
        .iter()
        .rposition(|e| matches!(e.event, Event::ContextCompacted { .. }))
    else {
        return 0;
    };
    let base_start = match &events[compacted_idx].event {
        Event::ContextCompacted {
            kept_from_turn: Some(k),
            ..
        } => events[..compacted_idx]
            .iter()
            .rposition(|e| matches!(e.event, Event::TurnStart { turn } if turn == *k))
            .unwrap_or(compacted_idx),
        _ => compacted_idx,
    };
    // Step back to just after the last non-empty `AssistantMessage` before `base_start` so any
    // unanswered user messages or pending restoration notices right before `base_start` are preserved.
    events[..base_start]
        .iter()
        .rposition(|e| matches!(&e.event, Event::AssistantMessage { content, .. } if !content.is_empty()))
        .map_or(0, |idx| idx + 1)
}

pub fn derive_messages(events: &[Envelope]) -> Vec<Value> {
    let text = |text: &str| json!({ "type": "text", "text": text });
    let replay_start = derive_replay_start(events);
    let PrescanMeta {
        mut speaker,
        mut planning,
        mut todos,
        mut brief,
    } = if replay_start > 0 {
        prescan_metadata_before_replay(&events[..replay_start])
    } else {
        let initial_speaker = match events.first().map(|envelope| &envelope.event) {
            Some(Event::SessionCreated { provider, .. }) => {
                provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
            }
            _ => DEFAULT_PROVIDER.to_string(),
        };
        PrescanMeta {
            speaker: initial_speaker,
            planning: false,
            todos: None,
            brief: None,
        }
    };
    let mut messages = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let mut spoken: Vec<(usize, String)> = Vec::new();
    // Snapshot of `(messages.len(), spoken.len())` at the start of each turn, for `kept_from_turn` tail retention.
    let mut turn_starts: HashMap<u64, (usize, usize)> = HashMap::new();
    let mut current_turn_msg_start: usize = 0;
    // What the user said since the model last spoke.
    let mut unanswered: Vec<Value> = Vec::new();
    for envelope in &events[replay_start..] {
        match &envelope.event {
            Event::TurnStart { turn } => {
                turn_starts.insert(*turn, (messages.len(), spoken.len()));
                current_turn_msg_start = messages.len();
            }
            Event::AsideBriefed { brief: told } => brief = Some(told),
            Event::UserMessage {
                text: said,
                mode,
                images,
                files,
                ..
            } => {
                if (*mode == Mode::Plan) != planning {
                    planning = !planning;
                    pending.push(text(if planning { PLAN_ON } else { PLAN_OFF }));
                }
                // Not kept with what goes on after a compaction: by then it would be stale, and the next message brings a new one.
                if let Some(told) = brief.take() {
                    pending.push(text(told));
                }
                // Pictures come before the words about them, as the providers advise, and attached files likewise.
                // A message of pictures alone has no text block: an empty one is an error to the API.
                let blocks = images
                    .iter()
                    .map(|image| json!({ "type": "image", "source": { "type": "base64", "media_type": image.media_type, "data": image.data } }))
                    .chain(files.iter().map(|file| text(&crate::events::attached_text(file))))
                    .chain((!said.is_empty()).then(|| text(said)));
                for block in blocks {
                    pending.push(block.clone());
                    unanswered.push(block);
                }
            }
            Event::FilesRestored {
                turn,
                restored,
                skipped,
            } => {
                let mut told = format!("[The user undid your file changes from turn {turn} on.");
                if !restored.is_empty() {
                    told.push_str(&format!(
                        " Back to what they held before those changes, or removed if you had created them: {}.",
                        restored.join(", ")
                    ));
                }
                if !skipped.is_empty() {
                    told.push_str(&format!(
                        " Left as they are, because they changed after your last edit: {}.",
                        skipped.join(", ")
                    ));
                }
                told.push_str(" Read a file again before relying on what it held.]");
                pending.push(text(&told));
            }
            Event::ArtifactRestored { path, version, .. } => {
                let told = format!(
                    "[The user restored {path} to version v{version}. Read it again before relying on what it held.]"
                );
                pending.push(text(&told));
            }
            Event::ModelChanged {
                provider: Some(provider),
                ..
            } => speaker = provider.clone(),
            Event::ContextCompacted {
                summary,
                kept_from_turn,
                ..
            } => {
                let mut header = vec![text(&format!("{COMPACTED}\n\n{summary}"))];
                if planning {
                    header.push(text(PLAN_ON));
                }
                // The list is the model's own bookkeeping; a summary cannot be trusted to carry it whole.
                if let Some(todos) = todos.as_ref().filter(|todos| !todos.is_empty()) {
                    let lines: Vec<String> = todos
                        .iter()
                        .map(|(item, status)| format!("- [{status}] {item}"))
                        .collect();
                    header.push(text(&format!(
                        "[Your to-do list as you last wrote it; update it with `update_todos` as you go.]\n{}",
                        lines.join("\n")
                    )));
                }
                let retained = kept_from_turn
                    .and_then(|turn| turn_starts.get(&turn).copied())
                    .filter(|(msg_from, _)| *msg_from < messages.len());
                if let Some((msg_from, spoken_from)) = retained {
                    let mut kept_msgs: Vec<Value> = messages.drain(msg_from..).collect();
                    let mut kept_spoken: Vec<(usize, String)> = spoken
                        .drain(spoken_from..)
                        .filter(|(idx, _)| *idx >= msg_from)
                        .map(|(idx, spk)| (idx - msg_from, spk))
                        .collect();
                    let inserted_header = if let Some(first_content) = kept_msgs
                        .first_mut()
                        .filter(|m| m["role"] == "user")
                        .and_then(|m| m["content"].as_array_mut())
                    {
                        header.append(first_content);
                        *first_content = header;
                        0usize
                    } else {
                        kept_msgs.insert(0, json!({ "role": "user", "content": header }));
                        1usize
                    };
                    if inserted_header > 0 {
                        for (idx, _) in &mut kept_spoken {
                            *idx += inserted_header;
                        }
                    }
                    turn_starts.retain(|_, (m_idx, _)| *m_idx >= msg_from);
                    for (m_idx, s_idx) in turn_starts.values_mut() {
                        *m_idx = (*m_idx - msg_from) + inserted_header;
                        *s_idx = s_idx.saturating_sub(spoken_from);
                    }
                    current_turn_msg_start = if current_turn_msg_start >= msg_from {
                        (current_turn_msg_start - msg_from) + inserted_header
                    } else {
                        0
                    };
                    messages = kept_msgs;
                    spoken = kept_spoken;
                    // Keep `pending` (and `unanswered`) intact: every item in `pending` was recorded
                    // after the last AssistantMessage in `kept_msgs` (e.g. active turn's ToolResult,
                    // FilesRestored, ArtifactRestored, or steering UserMessage).
                } else {
                    messages.clear();
                    spoken.clear();
                    pending.clear();
                    turn_starts.clear();
                    current_turn_msg_start = 0;
                    pending.extend(header);
                    pending.extend(unanswered.iter().cloned());
                }
            }
            Event::ToolResult {
                tool_use_id,
                content,
                is_error,
                images,
                ..
            } => {
                let words = if content.is_empty() {
                    "(no output)"
                } else {
                    content.as_str()
                };
                // With pictures the content is a list: the words, then each picture. A provider that takes
                // no pictures in a tool result moves them to the user's turn; see `openai_chat`.
                let content = if images.is_empty() {
                    json!(words)
                } else {
                    let pictures = images.iter().map(|image| json!({ "type": "image", "source": { "type": "base64", "media_type": image.media_type, "data": image.data } }));
                    Value::Array(std::iter::once(text(words)).chain(pictures).collect())
                };
                pending.push(json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error }));
            }
            Event::AssistantMessage { content, .. } if !content.is_empty() => {
                let merged = if !pending.is_empty() {
                    messages.push(json!({ "role": "user", "content": std::mem::take(&mut pending) }));
                    false
                } else if messages.last().is_some_and(|m| m["role"] == "assistant")
                    && spoken.last().is_some_and(|(idx, spk)| {
                        *idx >= current_turn_msg_start && *idx == messages.len() - 1 && spk == &speaker
                    })
                {
                    if let Some(existing) = messages.last_mut().and_then(|m| m["content"].as_array_mut()) {
                        existing.extend(content.iter().cloned());
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !merged {
                    spoken.push((messages.len(), speaker.clone()));
                    messages.push(json!({ "role": "assistant", "content": content }));
                }
                unanswered.clear();
                for block in content
                    .iter()
                    .filter(|block| block["type"] == "tool_use" && block["name"] == "update_todos")
                {
                    // A list the tool refused is not the list.
                    if let Ok(items) = crate::tools::todo_items(&block["input"]) {
                        todos = Some(items);
                    }
                }
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        messages.push(json!({ "role": "user", "content": pending }));
    }
    // Collect tool call ids already spoken by the active provider so normalized ids from
    // earlier providers never collide with them.
    let mut used_ids: HashSet<String> = HashSet::new();
    for &(at, ref from) in &spoken {
        if from == &speaker {
            for block in messages[at]["content"].as_array().into_iter().flatten() {
                if block["type"] == "tool_use"
                    && let Some(id) = block["id"].as_str().filter(|id| !id.is_empty())
                {
                    used_ids.insert(id.to_string());
                }
            }
        }
    }
    // What other providers answered, in the form this one takes; any renamed tool call ids
    // are rewritten strictly in the immediately paired `user` message (`at + 1`), never globally.
    for (at, from) in spoken {
        if from != speaker {
            let mut local_renamed: HashMap<String, VecDeque<String>> = HashMap::new();
            let mut any_renamed = false;
            let content = carried(
                messages[at]["content"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &mut used_ids,
                &mut local_renamed,
                &mut any_renamed,
            );
            messages[at]["content"] = Value::Array(content);
            if any_renamed && let Some(next_user) = messages.get_mut(at + 1).filter(|m| m["role"] == "user") {
                for block in next_user["content"].as_array_mut().into_iter().flatten() {
                    if block["type"] == "tool_result"
                        && let Some(raw_id) = block["tool_use_id"].as_str()
                        && let Some(queue) = local_renamed.get_mut(raw_id)
                        && let Some(new_id) = if queue.len() > 1 {
                            queue.pop_front()
                        } else {
                            queue.front().cloned()
                        }
                    {
                        block["tool_use_id"] = json!(new_id);
                    }
                }
            }
        }
    }
    messages
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_tool_id(raw_id: &str, used_ids: &mut HashSet<String>) -> String {
    let mut sanitized: String = raw_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        sanitized = "call".to_string();
    }
    let base = if sanitized.len() > 64 {
        let hash = fnv1a_64(raw_id.as_bytes());
        format!("{}_{hash:016x}", &sanitized[..47])
    } else {
        sanitized
    };
    let mut candidate = base.clone();
    let mut suffix_num = 2u64;
    while used_ids.contains(&candidate) {
        let suffix = format!("_{suffix_num}");
        let max_prefix = 64usize.saturating_sub(suffix.len());
        let prefix = &base[..base.len().min(max_prefix)];
        candidate = format!("{prefix}{suffix}");
        suffix_num += 1;
    }
    used_ids.insert(candidate.clone());
    candidate
}

/// What one provider answered, in the form every provider takes: its words
/// and its tool calls, with nothing but the fields all of them know. Its
/// reasoning is left out, since another provider cannot check it (a signed
/// thinking block, an encrypted reasoning item) and would refuse it, and so
/// is anything else of that provider's own. A tool call's id is given only
/// up to 64 `[A-Za-z0-9_-]` characters without colliding with `used_ids`
/// (`functions.read:0` from Kimi is refused by Anthropic), and `local_renamed`
/// records it so that only the paired result message follows. A message of
/// reasoning alone keeps its place with a mark.
fn carried(
    content: &[Value],
    used_ids: &mut HashSet<String>,
    local_renamed: &mut HashMap<String, VecDeque<String>>,
    any_renamed: &mut bool,
) -> Vec<Value> {
    let mut kept: Vec<Value> = content
        .iter()
        .filter_map(|block| match block["type"].as_str() {
            Some("text") => block["text"].as_str().filter(|words| !words.is_empty()).map(|words| json!({ "type": "text", "text": words })),
            Some("tool_use") => {
                let id = block["id"].as_str().unwrap_or_default();
                let plain = normalize_tool_id(id, used_ids);
                if plain != id {
                    *any_renamed = true;
                }
                local_renamed.entry(id.to_string()).or_default().push_back(plain.clone());
                Some(json!({ "type": "tool_use", "id": plain, "name": block["name"], "input": if block["input"].is_object() { block["input"].clone() } else { json!({}) } }))
            }
            _ => None,
        })
        .collect();
    if kept.is_empty() {
        kept.push(json!({ "type": "text", "text": "…" }));
    }
    kept
}

/// The names a project's instructions for agents go by, in the order they are looked for.
pub const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
/// Instructions are sent with every request, so a file longer than this is cut.
const INSTRUCTION_CHARS: usize = 30_000;

/// The project's instructions for agents: the first of [`INSTRUCTION_FILES`]
/// at the top of the workspace that holds text, as `(name, contents)`.
pub fn read_instructions(workspace: &Path) -> Option<(String, String)> {
    INSTRUCTION_FILES.iter().find_map(|name| {
        let text = fs::read_to_string(workspace.join(name)).ok()?;
        let text = text.replace("\r\n", "\n");
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut content: String = text.chars().take(INSTRUCTION_CHARS).collect();
        if text.chars().count() > INSTRUCTION_CHARS {
            content.push_str(&format!("\n[{name} is longer than {INSTRUCTION_CHARS} characters; the rest is not shown here. Read the file for it.]"));
        }
        Some((name.to_string(), content))
    })
}

/// The instructions in force: the last ones loaded, unless that file has since gone.
pub fn instructions(events: &[Envelope]) -> Option<(&str, &str)> {
    events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::InstructionsLoaded { path, content } => Some((path.as_str(), content.as_str())),
            _ => None,
        })
        .filter(|(_, content)| !content.is_empty())
}

/// Id of the session whose agent set this one off as a subagent, if one did.
pub fn parent(events: &[Envelope]) -> Option<&str> {
    match &events.first()?.event {
        Event::SessionCreated { parent, .. } => parent.as_deref(),
        _ => None,
    }
}

/// Where the session's conversation was copied from, if it is a fork.
pub fn forked_from(events: &[Envelope]) -> Option<&Fork> {
    match &events.first()?.event {
        Event::SessionCreated { fork, .. } => fork.as_ref(),
        _ => None,
    }
}

/// Name of the schedule that started the session, if one did.
pub fn schedule(events: &[Envelope]) -> Option<&str> {
    match &events.first()?.event {
        Event::SessionCreated { schedule, .. } => schedule.as_deref(),
        _ => None,
    }
}

/// The worktree made for the session, if it works in one.
pub fn worktree(events: &[Envelope]) -> Option<&Worktree> {
    match &events.first()?.event {
        Event::SessionCreated { worktree, .. } => worktree.as_ref(),
        _ => None,
    }
}

/// Id of the session this one is a side conversation beside, if it is one.
pub fn aside(events: &[Envelope]) -> Option<&str> {
    match &events.first()?.event {
        Event::SessionCreated { aside, .. } => aside.as_deref(),
        _ => None,
    }
}

/// The briefing on the session a side conversation is beside that was logged last, if any.
pub fn last_brief(events: &[Envelope]) -> Option<&str> {
    events.iter().rev().find_map(|envelope| match &envelope.event {
        Event::AsideBriefed { brief } => Some(brief.as_str()),
        _ => None,
    })
}

/// The most recent external backend conversation id logged for `backend` (e.g. `antigravity`).
/// In a forked session, external ids copied from turns `<= fork.turn` belong to the source session
/// and are ignored so the fork starts an independent CLI conversation seeded via handoff brief.
pub fn external_session<'a>(events: &'a [Envelope], backend: &str) -> Option<&'a str> {
    let own_events = if let Some(fork) = forked_from(events) {
        let start = events
            .iter()
            .position(|env| matches!(env.event, Event::TurnStart { turn } if turn > fork.turn))
            .unwrap_or(events.len());
        &events[start..]
    } else {
        events
    };
    own_events.iter().rev().find_map(|envelope| match &envelope.event {
        Event::ExternalSession {
            backend: kind,
            session_id,
        } if kind == backend => Some(session_id.as_str()),
        _ => None,
    })
}

/// How a session's work came out, for the agent that delegated it: what its
/// agent said last, and why its last turn ended, as `(reason, error)`. A
/// turn still open, or none yet, reads as `running`.
pub fn report(events: &[Envelope]) -> (String, String, Option<String>) {
    let said = events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::AssistantMessage { content, .. } => {
                let text: Vec<&str> = content
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                Some(text.join("\n")).filter(|text| !text.trim().is_empty())
            }
            _ => None,
        })
        .unwrap_or_default();
    let ended = match events
        .iter()
        .rev()
        .find(|envelope| matches!(envelope.event, Event::TurnStart { .. } | Event::TurnEnd { .. }))
        .map(|envelope| &envelope.event)
    {
        Some(Event::TurnEnd { reason, error, .. }) => (reason.clone(), error.clone()),
        _ => ("running".to_string(), None),
    };
    (said, ended.0, ended.1)
}

/// The model the session is on: the last one it was moved to, or else the one it was created with.
pub fn model(events: &[Envelope]) -> String {
    events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::ModelChanged { model, .. } | Event::SessionCreated { model, .. } => Some(model.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The provider the session is on: the last one it was moved to, or else the one it was created with.
pub fn provider(events: &[Envelope]) -> String {
    events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::ModelChanged {
                provider: Some(provider),
                ..
            } => Some(provider.clone()),
            Event::SessionCreated { provider, .. } => {
                Some(provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.to_string()))
            }
            _ => None,
        })
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
}

/// How hard the model works on the session's requests, when the user set it for this session; else the settings say.
pub fn effort(events: &[Envelope]) -> Option<String> {
    events.iter().rev().find_map(|envelope| match &envelope.event {
        Event::EffortChanged { effort } => Some(effort.clone()),
        _ => None,
    })
}

/// The skills the model is told of: the last list logged.
pub fn skills(events: &[Envelope]) -> &[SkillInfo] {
    events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::SkillsListed { skills } => Some(skills.as_slice()),
            _ => None,
        })
        .unwrap_or_default()
}

/// What opens a request after a compaction, ahead of the summary.
pub const COMPACTED: &str = "[The conversation up to here grew too long and was replaced by the summary below. The earlier messages are gone: read a file again before relying on what it held. Carry on with the task from where the summary leaves it.]";

struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_slice_serialized_len(content: &[Value]) -> usize {
    let mut counter = ByteCounter(0);
    let _ = serde_json::to_writer(&mut counter, content);
    counter.0
}

fn context_estimate_start(events: &[Envelope]) -> (usize, u64, bool) {
    let Some(compacted_idx) = events
        .iter()
        .rposition(|e| matches!(e.event, Event::ContextCompacted { .. }))
    else {
        return (0, 0, false);
    };
    match &events[compacted_idx].event {
        Event::ContextCompacted {
            kept_from_turn: Some(from_turn),
            ..
        } => {
            let mut earliest_kept: Option<usize> = None;
            let mut turn_before: Option<u64> = None;
            let mut answered_before = false;
            for idx in (0..compacted_idx).rev() {
                if let Event::TurnStart { turn } = events[idx].event {
                    if turn >= *from_turn {
                        earliest_kept = Some(idx);
                    } else {
                        turn_before = Some(turn);
                        answered_before = events[idx + 1..earliest_kept.unwrap_or(compacted_idx)]
                            .iter()
                            .any(|e| matches!(e.event, Event::AssistantMessage { .. }));
                        break;
                    }
                }
            }
            match earliest_kept {
                Some(start_idx) => (start_idx, turn_before.unwrap_or(0), answered_before),
                None => {
                    // `from_turn` has no `TurnStart`: check whether the active turn before `compacted_idx` was answered.
                    let last_ts = events[..compacted_idx]
                        .iter()
                        .rposition(|e| matches!(e.event, Event::TurnStart { .. }));
                    let search_from = last_ts.map_or(0, |i| i + 1);
                    let cur_turn = last_ts.map_or(0, |i| match events[i].event {
                        Event::TurnStart { turn } => turn,
                        _ => 0,
                    });
                    if events[search_from..compacted_idx]
                        .iter()
                        .any(|e| matches!(e.event, Event::AssistantMessage { .. }))
                    {
                        (compacted_idx, cur_turn, true)
                    } else {
                        (last_ts.unwrap_or(0), 0, false)
                    }
                }
            }
        }
        _ => {
            // `kept_from_turn: None`: `retained` is 0; if the active turn already has an `AssistantMessage`,
            // `unanswered_added` is unused so we can start directly at `compacted_idx`.
            let mut saw_assistant = false;
            let mut cur_turn = 0u64;
            let mut last_ts_idx: Option<usize> = None;
            for idx in (0..compacted_idx).rev() {
                match events[idx].event {
                    Event::AssistantMessage { .. } if last_ts_idx.is_none() => {
                        saw_assistant = true;
                    }
                    Event::TurnStart { turn } => {
                        cur_turn = turn;
                        last_ts_idx = Some(idx);
                        break;
                    }
                    _ => {}
                }
            }
            if saw_assistant {
                (compacted_idx, cur_turn, true)
            } else {
                (last_ts_idx.unwrap_or(0), 0, false)
            }
        }
    }
}

/// About how many tokens the next request will hold. The last request's own
/// count is exact; what was added since (tool results, user messages), or a
/// log with no counts at all, is estimated at one token per three bytes,
/// which runs high for English and about right for Chinese.
pub fn context_estimate(events: &[Envelope]) -> u64 {
    let guess = |text: &str| text.len() as u64 / 3;
    let (start_idx, mut current_turn, mut turn_answered) = context_estimate_start(events);
    let mut estimate = 0;
    let mut turn_added: HashMap<u64, u64> = HashMap::new();
    let mut unanswered_added = 0u64;
    for envelope in &events[start_idx..] {
        match &envelope.event {
            Event::TurnStart { turn } => {
                current_turn = *turn;
                turn_answered = false;
                unanswered_added = 0;
            }
            Event::ContextCompacted {
                summary,
                kept_from_turn,
                ..
            } => {
                let retained: u64 = match kept_from_turn {
                    Some(from_turn) => turn_added
                        .iter()
                        .filter(|(t, _)| **t >= *from_turn && (**t < current_turn || turn_answered))
                        .map(|(_, v)| *v)
                        .sum(),
                    None => 0,
                };
                turn_added.clear();
                estimate = guess(summary) + retained + if !turn_answered { unanswered_added } else { 0 };
            }
            Event::AssistantMessage {
                tokens: Some(tokens),
                content,
                ..
            } => {
                turn_answered = true;
                estimate = tokens.input + tokens.output;
                *turn_added.entry(current_turn).or_default() += json_slice_serialized_len(content) as u64 / 3;
            }
            Event::AssistantMessage {
                content, tokens: None, ..
            } => {
                turn_answered = true;
                let g = json_slice_serialized_len(content) as u64 / 3;
                estimate += g;
                *turn_added.entry(current_turn).or_default() += g;
            }
            // A picture's cost depends on its size and the provider; this is a middling guess until the next count.
            Event::UserMessage {
                text, images, files, ..
            } => {
                let g = guess(text)
                    + 1500 * images.len() as u64
                    + files
                        .iter()
                        .map(|file| guess(&crate::events::attached_text(file)))
                        .sum::<u64>();
                estimate += g;
                *turn_added.entry(current_turn).or_default() += g;
                if !turn_answered {
                    unanswered_added += g;
                }
            }
            Event::ToolResult { content, images, .. } => {
                let g = guess(content) + 1500 * images.len() as u64;
                estimate += g;
                *turn_added.entry(current_turn).or_default() += g;
            }
            Event::AsideBriefed { brief } => {
                let g = guess(brief);
                estimate += g;
                *turn_added.entry(current_turn).or_default() += g;
            }
            _ => {}
        }
    }
    estimate
}

/// The conversation since the last compaction as plain text of at most
/// `max_bytes`, for writing a summary when the conversation itself no longer
/// fits a request. Tool calls and results are cut short, and when it is still
/// too long the middle goes: the start holds the task, the end where it stands.
pub fn transcript(events: &[Envelope], max_bytes: usize) -> String {
    let cut = |text: &str, max: usize| {
        let count = text.chars().count();
        if count <= max {
            return text.to_string();
        }
        let head: String = text.chars().take(max / 2).collect();
        let tail: String = text.chars().skip(count - max / 2).collect();
        format!("{head}\n[... {} characters omitted ...]\n{tail}", count - max)
    };
    let start = events
        .iter()
        .rposition(|envelope| matches!(envelope.event, Event::ContextCompacted { .. }))
        .unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    for envelope in &events[start..] {
        match &envelope.event {
            Event::ContextCompacted { summary, .. } => parts.push(format!("SUMMARY OF EARLIER WORK:\n{summary}")),
            Event::UserMessage {
                text, images, files, ..
            } => {
                // What was attached is named, not shown: the files are where they were, to be read again.
                let mut said = String::from("USER:\n");
                if !images.is_empty() {
                    said.push_str(&format!("[attached {} image(s), not shown here]\n", images.len()));
                }
                if !files.is_empty() {
                    said.push_str(&format!(
                        "[attached files, not shown here: {}]\n",
                        files
                            .iter()
                            .map(|file| file.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                parts.push(said + text);
            }
            Event::AssistantMessage { content, .. } => {
                for block in content {
                    match block["type"].as_str() {
                        Some("text") => {
                            parts.push(format!("ASSISTANT:\n{}", block["text"].as_str().unwrap_or_default()))
                        }
                        Some("tool_use") => parts.push(format!(
                            "ASSISTANT CALLED {}:\n{}",
                            block["name"].as_str().unwrap_or_default(),
                            cut(&block["input"].to_string(), 600)
                        )),
                        _ => {}
                    }
                }
            }
            Event::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => {
                parts.push(format!(
                    "RESULT OF {tool}{}:\n{}",
                    if *is_error { " (failed)" } else { "" },
                    cut(content, 2000)
                ));
            }
            _ => {}
        }
    }
    let whole = parts.join("\n\n");
    if whole.len() <= max_bytes {
        return whole;
    }
    let boundary = |mut at: usize| {
        while !whole.is_char_boundary(at) {
            at += 1;
        }
        at
    };
    let (head, tail) = (boundary(max_bytes / 5), boundary(whole.len() - max_bytes * 4 / 5));
    format!(
        "{}\n\n[... the middle of the conversation is omitted ...]\n\n{}",
        &whole[..head],
        &whole[tail..]
    )
}

/// Whether the session's title, workspace, or anything the user or the agent
/// said in it holds `query`, whatever the letter case. Tool calls and their
/// results are not searched: they would match nearly any file name.
pub fn matches(events: &[Envelope], query: &str) -> bool {
    let query = query.trim().to_lowercase();
    let holds = |text: &str| text.to_lowercase().contains(&query);
    query.is_empty()
        || events.iter().any(|envelope| match &envelope.event {
            Event::SessionCreated { workspace, .. } => holds(workspace),
            Event::SessionRenamed { title } => holds(title),
            Event::UserMessage { text, .. } => holds(text),
            Event::AssistantMessage { content, .. } => content
                .iter()
                .any(|block| block["type"] == "text" && block["text"].as_str().is_some_and(holds)),
            _ => false,
        })
}

/// Fold a log into its list entry. The title is the name the user gave the
/// session, or else its first message cut to 40 characters.
pub fn summarize(events: &[Envelope]) -> SessionSummary {
    let Some((first_ts, id, workspace)) = events.first().and_then(|first| match &first.event {
        Event::SessionCreated { id, workspace, .. } => Some((first.ts.clone(), id.clone(), workspace.clone())),
        _ => None,
    }) else {
        return SessionSummary {
            id: String::new(),
            title: String::new(),
            workspace: String::new(),
            provider: DEFAULT_PROVIDER.to_string(),
            model: String::new(),
            effort: None,
            parent: None,
            worktree: None,
            schedule: None,
            fork: None,
            aside: None,
            mode: Mode::default(),
            created_at: String::new(),
            updated_at: String::new(),
            turn_open: false,
            awaiting_approval: false,
        };
    };
    let (provider, model, effort) = (provider(events), model(events), effort(events));
    let named = events.iter().rev().find_map(|envelope| match &envelope.event {
        Event::SessionRenamed { title } => Some(title.clone()),
        _ => None,
    });
    let first_message = || {
        events
            .iter()
            .find_map(|envelope| match &envelope.event {
                // A message of pictures alone is titled as such.
                Event::UserMessage { text, images, .. } if text.trim().is_empty() && !images.is_empty() => {
                    Some("[images]".to_string())
                }
                // And one of files alone, by the first of them.
                Event::UserMessage { text, files, .. } if text.trim().is_empty() && !files.is_empty() => Some(
                    std::path::Path::new(&files[0].path)
                        .file_name()
                        .map_or_else(|| "[files]".to_string(), |name| name.to_string_lossy().into_owned()),
                ),
                Event::UserMessage { text, .. } => Some(text.split_whitespace().collect::<Vec<_>>().join(" ")),
                _ => None,
            })
            .map(|text| {
                let mut title: String = text.chars().take(40).collect();
                if text.chars().count() > 40 {
                    title.push('…');
                }
                title
            })
            .unwrap_or_default()
    };
    // An empty name gives the session its first message back as title.
    let title = named.filter(|title| !title.is_empty()).unwrap_or_else(first_message);
    let mut requested: HashSet<&str> = HashSet::new();
    for envelope in events {
        match &envelope.event {
            // A question waits on the user as an approval does.
            Event::ApprovalRequested { tool_use_id, .. } | Event::QuestionAsked { tool_use_id, .. } => {
                requested.insert(tool_use_id);
            }
            Event::ApprovalResolved { tool_use_id, .. }
            | Event::QuestionAnswered { tool_use_id, .. }
            | Event::ToolResult { tool_use_id, .. } => {
                requested.remove(tool_use_id.as_str());
            }
            _ => {}
        }
    }
    let turn_open = open_turn(events).is_some();
    let mode = events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            Event::UserMessage { mode, .. } => Some(*mode),
            _ => None,
        })
        .unwrap_or_default();
    SessionSummary {
        id,
        title,
        workspace,
        provider,
        model,
        effort,
        parent: parent(events).map(str::to_string),
        worktree: worktree(events).cloned(),
        schedule: schedule(events).map(str::to_string),
        fork: forked_from(events).cloned(),
        aside: aside(events).map(str::to_string),
        mode,
        created_at: first_ts.clone(),
        // A fork is new when it is made, though the last event copied into it is as old as it was.
        updated_at: match events.last() {
            Some(last) => first_ts.max(last.ts.clone()),
            None => first_ts,
        },
        turn_open,
        awaiting_approval: turn_open && !requested.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Decision;

    fn store() -> (tempfile::TempDir, SessionStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        (directory, store)
    }

    fn assistant(content: Vec<Value>) -> Event {
        Event::AssistantMessage {
            content,
            stop_reason: Some("tool_use".into()),
            model: "m".into(),
            usage: json!({}),
            tokens: None,
        }
    }

    #[test]
    fn reopens_what_it_appended() {
        let (_directory, store) = store();
        let mut log = store
            .create(Path::new("/work"), "anthropic", "claude-opus-5-5")
            .unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage {
            text: "hello".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        let id = log.id().to_string();
        let written = log.events().to_vec();
        drop(log);

        let reopened = store.open(&id).unwrap();
        assert_eq!(reopened.events(), written.as_slice());
        assert_eq!(
            reopened.events().iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            reopened.header(),
            (
                PathBuf::from("/work"),
                "anthropic".to_string(),
                "claude-opus-5-5".to_string()
            )
        );
    }

    #[test]
    fn sends_what_another_provider_answered_plainly_and_a_providers_own_as_it_was() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "claude").unwrap();
        let said = |text: &str| Event::UserMessage {
            text: text.into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        };
        let signed = json!({ "type": "thinking", "thinking": "hm", "signature": "sig" });
        log.append(said("one")).unwrap();
        log.append(assistant(vec![
            signed.clone(),
            json!({ "type": "text", "text": "from claude" }),
        ]))
        .unwrap();
        log.append(Event::ModelChanged {
            model: "deepseek-chat".into(),
            provider: Some("deepseek".into()),
        })
        .unwrap();
        log.append(said("two")).unwrap();
        // Reasoning alone, as a provider may answer when cut off: it keeps its place in the conversation.
        log.append(assistant(vec![
            json!({ "type": "thinking", "thinking": "deep thoughts" }),
        ]))
        .unwrap();
        log.append(said("three")).unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "from deepseek" })]))
            .unwrap();
        // A model of the same provider moves nothing; back on Anthropic, its own answer goes as it was.
        log.append(Event::ModelChanged {
            model: "deepseek-reasoner".into(),
            provider: None,
        })
        .unwrap();
        assert_eq!(provider(log.events()), "deepseek");
        log.append(Event::ModelChanged {
            model: "claude".into(),
            provider: Some("anthropic".into()),
        })
        .unwrap();
        log.append(said("four")).unwrap();
        let messages = derive_messages(log.events());
        assert_eq!(
            messages[1]["content"],
            json!([signed, { "type": "text", "text": "from claude" }])
        );
        assert_eq!(messages[3]["content"], json!([{ "type": "text", "text": "…" }]));
        assert_eq!(
            messages[5]["content"],
            json!([{ "type": "text", "text": "from deepseek" }])
        );
        assert_eq!(
            (provider(log.events()), summarize(log.events()).provider),
            ("anthropic".to_string(), "anthropic".to_string())
        );
        // The move is written in the log only when it is to another provider.
        let lines: Vec<Value> = log
            .events()
            .iter()
            .map(|envelope| serde_json::to_value(envelope).unwrap())
            .collect();
        let moves: Vec<&Value> = lines
            .iter()
            .filter(|line| line["type"] == "model/changed")
            .map(|line| &line["data"])
            .collect();
        assert_eq!(
            moves,
            [
                &json!({ "model": "deepseek-chat", "provider": "deepseek" }),
                &json!({ "model": "deepseek-reasoner" }),
                &json!({ "model": "claude", "provider": "anthropic" })
            ]
        );
    }

    #[test]
    fn writes_the_current_format_and_names_the_provider_unless_it_is_anthropic() {
        let (_directory, store) = store();
        let header = |log: &SessionLog| serde_json::to_value(&log.events()[0]).unwrap()["data"].clone();
        let anthropic = store
            .create(Path::new("/work"), "anthropic", "claude-opus-5-5")
            .unwrap();
        assert_eq!(header(&anthropic)["format"], FORMAT_VERSION);
        assert!(header(&anthropic).get("provider").is_none());
        let deepseek = store.create(Path::new("/work"), "deepseek", "deepseek-v4-pro").unwrap();
        assert_eq!(
            (
                header(&deepseek)["format"].as_u64(),
                header(&deepseek)["provider"].as_str()
            ),
            (Some(u64::from(FORMAT_VERSION)), Some("deepseek"))
        );

        let id = deepseek.id().to_string();
        drop(deepseek);
        let reopened = store.open(&id).unwrap();
        assert_eq!(
            reopened.header(),
            (
                PathBuf::from("/work"),
                "deepseek".to_string(),
                "deepseek-v4-pro".to_string()
            )
        );
        assert_eq!(store.peek(&id).unwrap().provider, "deepseek");
    }

    #[test]
    fn drops_a_line_cut_off_by_a_crash_and_keeps_appending() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        let (id, path) = (log.id().to_string(), log.path().to_path_buf());
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":3,"ts":"2026-01-01T00:00:00Z","type":"user/mess"#)
            .unwrap();
        drop(file);

        let mut reopened = store.open(&id).unwrap();
        assert_eq!(reopened.events().len(), 2);
        reopened
            .append(Event::UserMessage {
                text: "again".into(),
                steering: false,
                mode: Mode::Edit,
                images: vec![],
                files: vec![],
            })
            .unwrap();
        drop(reopened);
        assert_eq!(store.open(&id).unwrap().events().len(), 3);
    }

    #[test]
    fn listing_never_changes_a_log_that_is_being_written() {
        let (_directory, store) = store();
        let log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let (id, path) = (log.id().to_string(), log.path().to_path_buf());
        // Another handle is halfway through appending a line.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":2,"ts":"2026-01-01T00:00:00Z","type":"turn/st"#)
            .unwrap();
        let before = fs::read(&path).unwrap();

        assert_eq!(store.list().len(), 1);
        assert_eq!(store.peek(&id).unwrap().workspace, "/work");
        assert_eq!(fs::read(&path).unwrap(), before);

        // The writer finishes its line, and the log reads back whole.
        file.write_all(b"art\",\"data\":{\"turn\":1}}\n").unwrap();
        drop((file, log));
        assert_eq!(store.open(&id).unwrap().events().len(), 2);
    }

    #[test]
    fn reads_an_approval_logged_before_grants_existed() {
        let line = r#"{"seq":4,"ts":"2026-01-01T00:00:00Z","type":"approval/requested","data":{"tool_use_id":"t1","tool":"run_command","reason":"command","detail":"ls"}}"#;
        let envelope: Envelope = serde_json::from_str(line).unwrap();
        assert!(matches!(envelope.event, Event::ApprovalRequested { grant: None, .. }));
        // An absent grant is also written as absent, so the line reads back the same.
        assert_eq!(serde_json::to_string(&envelope).unwrap(), line);
    }

    #[test]
    fn reports_damage_in_the_middle_of_a_log() {
        let (_directory, store) = store();
        let log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let (id, path) = (log.id().to_string(), log.path().to_path_buf());
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"not json\n{\"seq\":3}\n").unwrap();
        drop(file);
        assert!(matches!(store.open(&id), Err(SessionError::Damaged { line: 2, .. })));
    }

    #[test]
    fn derives_messages_with_results_before_steering_text() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let thinking = json!({ "type": "thinking", "thinking": "", "signature": "sig" });
        let call = json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "a" } });
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage {
            text: "read a".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        log.append(assistant(vec![thinking.clone(), call.clone()])).unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "read_file".into(),
            content: String::new(),
            is_error: false,
            meta: Value::Null,
            images: Vec::new(),
        })
        .unwrap();
        log.append(Event::UserMessage {
            text: "also b".into(),
            steering: true,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();

        let messages = derive_messages(log.events());
        assert_eq!(
            messages,
            vec![
                json!({ "role": "user", "content": [{ "type": "text", "text": "read a" }] }),
                json!({ "role": "assistant", "content": [thinking, call] }),
                json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "(no output)", "is_error": false },
                { "type": "text", "text": "also b" },
            ] }),
            ]
        );
    }

    #[test]
    fn renames_searches_and_deletes_a_session() {
        let (directory, store) = store();
        let mut log = store.create(Path::new("/work/Parser"), "anthropic", "m").unwrap();
        let id = log.id().to_string();
        log.append(Event::UserMessage {
            text: "fix the tokenizer".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        log.append(assistant(vec![
            json!({ "type": "text", "text": "The lexer drops a byte." }),
            json!({ "type": "tool_use", "id": "t", "name": "read_file", "input": { "path": "secret.rs" } }),
        ]))
        .unwrap();
        assert_eq!(summarize(log.events()).title, "fix the tokenizer");
        log.append(Event::SessionRenamed {
            title: "Lexer bug".into(),
        })
        .unwrap();
        assert_eq!(summarize(log.events()).title, "Lexer bug");

        // The title, the workspace, and what was said match in any letter case; tool calls do not.
        for (query, found) in [
            ("lexer BUG", true),
            ("parser", true),
            ("TOKENIZER", true),
            ("drops a byte", true),
            ("  ", true),
            ("secret.rs", false),
            ("nothing", false),
        ] {
            assert_eq!(matches(log.events(), query), found, "{query}");
            assert_eq!(store.search(&id, query), found, "{query}");
        }
        // Clearing the name gives the first message back.
        log.append(Event::SessionRenamed { title: String::new() }).unwrap();
        assert_eq!(summarize(log.events()).title, "fix the tokenizer");

        drop(log);
        // An id that is not a plain name never reaches the disk.
        std::fs::write(directory.path().join("keep.txt"), "x").unwrap();
        assert!(matches!(store.delete(".."), Err(SessionError::NotFound(_))));
        assert!(matches!(
            store.delete(&format!("{id}/../{id}")),
            Err(SessionError::NotFound(_))
        ));
        store.delete(&id).unwrap();
        assert!(store.ids().iter().all(|other| other != &id) && directory.path().join("keep.txt").exists());
        assert!(matches!(store.open(&id), Err(SessionError::NotFound(_))));
    }

    #[test]
    fn a_compaction_starts_the_conversation_over_from_its_summary() {
        use crate::llm::Tokens;
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let user = |text: &str, steering| Event::UserMessage {
            text: text.into(),
            steering,
            mode: Mode::Plan,
            images: vec![],
            files: vec![],
        };
        let counted = |input, output| Event::AssistantMessage {
            content: vec![json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "a" } })],
            stop_reason: Some("tool_use".into()),
            model: "m".into(),
            usage: json!({}),
            tokens: Some(Tokens {
                input,
                cached: 0,
                output,
            }),
        };
        let result = |content: &str| Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "read_file".into(),
            content: content.into(),
            is_error: false,
            meta: Value::Null,
            images: Vec::new(),
        };
        log.append(user("read a", false)).unwrap();
        log.append(counted(1000, 50)).unwrap();
        log.append(result(&"x".repeat(300))).unwrap();
        log.append(user("and hurry", true)).unwrap();
        // The last request's own count, plus a guess for what was added since.
        assert_eq!(context_estimate(log.events()), 1050 + 100 + 3);
        assert!(transcript(log.events(), 10_000).contains(
            "USER:
read a

ASSISTANT CALLED read_file:"
        ));
        let short = transcript(log.events(), 100);
        assert!(
            short.len() < 200
                && short.starts_with("USER:")
                && short.contains("omitted")
                && short.ends_with("and hurry"),
            "{short}"
        );

        log.append(Event::ContextCompacted {
            summary: "Read a; it holds x.".into(),
            tokens_before: 1153,
            focus: None,
            kept_from_turn: None,
            tokens: None,
        })
        .unwrap();
        // Only the summary goes back, then plan mode (still on), then what the model had not answered.
        // (No to-do list was written in this conversation, so none is carried over.)
        let texts = |events: &[Envelope]| -> Vec<String> {
            derive_messages(events)
                .iter()
                .flat_map(|message| message["content"].as_array().unwrap().clone())
                .filter_map(|block| block["text"].as_str().map(str::to_string))
                .collect()
        };
        assert_eq!(derive_messages(log.events()).len(), 1);
        assert_eq!(
            texts(log.events()),
            [
                format!(
                    "{COMPACTED}

Read a; it holds x."
                ),
                PLAN_ON.to_string(),
                "and hurry".to_string()
            ]
        );
        assert_eq!(context_estimate(log.events()), "Read a; it holds x.".len() as u64 / 3);

        // What follows the compaction is replayed as usual.
        log.append(assistant(vec![json!({ "type": "text", "text": "on it" })]))
            .unwrap();
        log.append(user("thanks", false)).unwrap();
        let messages = derive_messages(log.events());
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["content"], json!([{ "type": "text", "text": "thanks" }]));
    }

    #[test]
    fn sends_a_messages_files_after_its_images_and_ahead_of_its_text() {
        use crate::events::{Attached, Image};
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let png = Image {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        let notes = Attached {
            path: "/x/notes.md".into(),
            bytes: 4,
            content: Some("one\n".into()),
            document: None,
            parts: None,
            is_dir: false,
        };
        let big = Attached {
            path: "/x/big.log".into(),
            bytes: 300_000,
            content: None,
            document: None,
            parts: None,
            is_dir: false,
        };
        log.append(Event::UserMessage {
            text: "compare them".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![png],
            files: vec![notes.clone(), big.clone()],
        })
        .unwrap();
        let content = derive_messages(log.events())[0]["content"].clone();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(
            content[1],
            json!({ "type": "text", "text": "<attached_file path=\"/x/notes.md\">\n[File path: /x/notes.md]\none\n</attached_file>" })
        );
        assert!(
            content[2]["text"]
                .as_str()
                .unwrap()
                .starts_with("<attached_file path=\"/x/big.log\" bytes=\"300000\">")
        );
        assert_eq!(content[3], json!({ "type": "text", "text": "compare them" }));
        // The log line carries the files; a message without any reads as before.
        let line = |event: &Event| serde_json::to_value(event).unwrap()["data"].clone();
        assert_eq!(
            line(&log.events()[1].event)["files"],
            json!([{ "path": "/x/notes.md", "bytes": 4, "content": "one\n" }, { "path": "/x/big.log", "bytes": 300_000 }])
        );
        // A summary names what was attached without repeating it, and counts it towards the context.
        assert!(transcript(log.events(), 10_000).contains("[attached files, not shown here: /x/notes.md, /x/big.log]"));
        assert!(context_estimate(log.events()) > 20);
        // Files alone: a message of their blocks, titled by the first.
        let mut alone = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        alone
            .append(Event::UserMessage {
                text: String::new(),
                steering: false,
                mode: Mode::Edit,
                images: vec![],
                files: vec![notes],
            })
            .unwrap();
        assert_eq!(
            derive_messages(alone.events())[0]["content"].as_array().unwrap().len(),
            1
        );
        assert_eq!(summarize(alone.events()).title, "notes.md");
    }

    #[test]
    fn sends_a_messages_images_ahead_of_its_text() {
        use crate::events::Image;
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let png = Image {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        let shot = |text: &str| Event::UserMessage {
            text: text.into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![png.clone()],
            files: vec![],
        };
        log.append(shot("what is wrong here?")).unwrap();
        let block =
            json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGVsbG8=" } });
        assert_eq!(
            derive_messages(log.events())[0]["content"],
            json!([block, { "type": "text", "text": "what is wrong here?" }])
        );
        assert_eq!(context_estimate(log.events()), 1500 + 6);
        assert!(
            transcript(log.events(), 1000)
                .starts_with("USER:\n[attached 1 image(s), not shown here]\nwhat is wrong here?")
        );

        // Pictures alone: no empty text block, and the session is titled for what it holds.
        let mut alone = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        alone.append(shot("")).unwrap();
        assert_eq!(derive_messages(alone.events())[0]["content"], json!([block]));
        assert_eq!(summarize(alone.events()).title, "[images]");

        // What cannot be sent is refused before it is logged: too many, an unknown format, not base64, too large.
        use crate::events::{IMAGE_BASE64_BYTES, IMAGES_PER_MESSAGE, check_images};
        let of = |media_type: &str, data: String| Image {
            media_type: media_type.into(),
            data,
        };
        assert!(check_images(std::slice::from_ref(&png)).is_ok() && check_images(&[]).is_ok());
        assert!(
            check_images(&vec![png.clone(); IMAGES_PER_MESSAGE + 1])
                .unwrap_err()
                .contains("at most")
        );
        assert!(
            check_images(&[of("image/svg+xml", "aGk=".into())])
                .unwrap_err()
                .contains("only PNG")
        );
        assert!(
            check_images(&[of("image/png", "not base64!".into())])
                .unwrap_err()
                .contains("not base64")
        );
        assert!(check_images(&[of("image/png", String::new())]).is_err());
        assert!(
            check_images(&[of("image/png", "A".repeat(IMAGE_BASE64_BYTES + 1))])
                .unwrap_err()
                .contains("larger")
        );

        // The field is left out of a message without pictures, and read as none when absent.
        let line = |event: &Event| serde_json::to_value(event).unwrap()["data"].clone();
        assert!(
            line(&Event::UserMessage {
                text: "x".into(),
                steering: false,
                mode: Mode::Edit,
                images: vec![],
                files: vec![]
            })
            .get("images")
            .is_none()
        );
        assert_eq!(line(&shot("x"))["images"][0]["media_type"], "image/png");
    }

    #[test]
    fn carries_the_todo_list_across_a_compaction() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let listed = |todos: Value| {
            assistant(vec![
                json!({ "type": "tool_use", "id": "t", "name": "update_todos", "input": { "todos": todos } }),
            ])
        };
        let result = Event::ToolResult {
            tool_use_id: "t".into(),
            tool: "update_todos".into(),
            content: "ok".into(),
            is_error: false,
            meta: Value::Null,
            images: Vec::new(),
        };
        log.append(Event::UserMessage {
            text: "go".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        log.append(listed(json!([{ "text": "read", "status": "pending" }])))
            .unwrap();
        log.append(result.clone()).unwrap();
        // The latest list counts; one the tool would refuse does not replace it.
        log.append(listed(
            json!([{ "text": "read", "status": "done" }, { "text": "fix", "status": "in_progress" }]),
        ))
        .unwrap();
        log.append(result.clone()).unwrap();
        log.append(listed(json!([{ "text": "bad", "status": "unknown" }])))
            .unwrap();
        log.append(result).unwrap();
        log.append(Event::ContextCompacted {
            summary: "Summary.".into(),
            tokens_before: 9,
            focus: None,
            kept_from_turn: None,
            tokens: None,
        })
        .unwrap();
        let messages = derive_messages(log.events());
        let texts: Vec<&str> = messages[0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect();
        assert_eq!(texts.len(), 2);
        assert!(
            texts[1].ends_with("as you go.]\n- [done] read\n- [in_progress] fix"),
            "{}",
            texts[1]
        );
    }

    #[test]
    fn tells_the_model_when_plan_mode_starts_and_ends() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        let says = |text: &str| assistant(vec![json!({ "type": "text", "text": text })]);
        let user = |text: &str, mode| Event::UserMessage {
            text: text.into(),
            steering: false,
            mode,
            images: vec![],
            files: vec![],
        };
        for event in [
            user("look first", Mode::Plan),
            says("plan"),
            user("and the tests?", Mode::Plan),
            says("plan 2"),
            user("go", Mode::Auto),
            says("done"),
            user("thanks", Mode::Edit),
        ] {
            log.append(event).unwrap();
        }
        let texts: Vec<Vec<String>> = derive_messages(log.events())
            .iter()
            .filter(|message| message["role"] == "user")
            .map(|message| {
                message["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|block| block["text"].as_str().unwrap().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(
            texts,
            [
                vec![PLAN_ON.to_string(), "look first".into()],
                vec!["and the tests?".into()],
                vec![PLAN_OFF.to_string(), "go".into()],
                vec!["thanks".into()],
            ]
        );
        assert_eq!(summarize(log.events()).mode, Mode::Edit);

        // Edit mode is written as no mode at all, which is how logs from before modes read.
        let line = |event: &Event| serde_json::to_value(event).unwrap()["data"].clone();
        assert!(line(&user("x", Mode::Edit)).get("mode").is_none());
        assert_eq!(line(&user("x", Mode::Plan))["mode"], "plan");
        let old: Event =
            serde_json::from_value(json!({ "type": "user/message", "data": { "text": "x", "steering": false } }))
                .unwrap();
        assert_eq!(old, user("x", Mode::Edit));
    }

    #[test]
    fn repairs_a_turn_left_open_by_a_crash() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "anthropic", "m").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage {
            text: "go".into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        log.append(assistant(vec![
            json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": {} }),
            json!({ "type": "tool_use", "id": "t2", "name": "run_command", "input": {} }),
        ]))
        .unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "read_file".into(),
            content: "x".into(),
            is_error: false,
            meta: Value::Null,
            images: Vec::new(),
        })
        .unwrap();
        log.append(Event::ApprovalRequested {
            tool_use_id: "t2".into(),
            tool: "run_command".into(),
            reason: "command".into(),
            detail: "ls".into(),
            grant: Some("command".into()),
            expires_at: None,
        })
        .unwrap();
        assert!(summarize(log.events()).awaiting_approval);

        let appended = log.repair_tail().unwrap();
        assert_eq!(appended.len(), 2);
        assert!(
            matches!(&appended[0].event, Event::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "t2")
        );
        assert!(matches!(&appended[1].event, Event::TurnEnd { turn: 1, reason, .. } if reason == "interrupted"));
        assert!(unanswered_tool_uses(log.events()).is_empty());
        let summary = summarize(log.events());
        assert!(!summary.turn_open && !summary.awaiting_approval);
        assert!(log.repair_tail().unwrap().is_empty());
    }

    #[test]
    fn summarizes_title_and_lists_newest_first() {
        let (_directory, store) = store();
        let mut first = store.create(Path::new("/a"), "anthropic", "m").unwrap();
        first
            .append(Event::UserMessage {
                text: "  fix   the\nparser ".into(),
                steering: false,
                mode: Mode::Edit,
                images: vec![],
                files: vec![],
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut second = store.create(Path::new("/b"), "anthropic", "m").unwrap();
        second
            .append(Event::UserMessage {
                text: "x".repeat(50),
                steering: false,
                mode: Mode::Edit,
                images: vec![],
                files: vec![],
            })
            .unwrap();
        second
            .append(Event::ApprovalResolved {
                tool_use_id: "none".into(),
                decision: Decision::Deny,
                timed_out: false,
            })
            .unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].workspace, "/b");
        assert_eq!(list[0].title.chars().count(), 41);
        assert_eq!(list[1].title, "fix the parser");
    }

    #[test]
    fn a_fork_starts_as_a_copy_of_the_conversation_up_to_a_turn() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/a"), "anthropic", "m").unwrap();
        let said = |text: &str| Event::UserMessage {
            text: text.into(),
            steering: false,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        };
        let edited = Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "edit_file".into(),
            content: "ok".into(),
            is_error: false,
            meta: json!({ "file": "/a/x.txt", "path": "x.txt", "after": "1-0" }),
            images: Vec::new(),
        };
        for event in [
            Event::TurnStart { turn: 1 },
            said("first"),
            assistant(vec![
                json!({ "type": "tool_use", "id": "t1", "name": "edit_file", "input": {} }),
            ]),
            edited,
            Event::TurnEnd {
                turn: 1,
                reason: "completed".into(),
                error: None,
            },
            Event::SessionRenamed { title: "named".into() },
            Event::TurnStart { turn: 2 },
            said("second"),
            Event::TurnEnd {
                turn: 2,
                reason: "completed".into(),
                error: None,
            },
            Event::TurnStart { turn: 3 },
            said("third"),
        ] {
            log.append(event).unwrap();
        }
        let kinds = |log: &SessionLog| {
            log.events()
                .iter()
                .map(|envelope| {
                    serde_json::to_value(envelope).unwrap()["type"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };

        // Before turn 2: the first turn and what came between the two, each event at its old time and place.
        let fork = store.fork(log.events(), Some(2)).unwrap();
        assert_eq!(kinds(&fork), kinds(&log)[..7]);
        assert_eq!(fork.events()[1..], log.events()[1..7]);
        let summary = summarize(fork.events());
        assert_eq!(
            summary.fork,
            Some(Fork {
                session: log.id().to_string(),
                turn: 1
            })
        );
        assert_eq!(
            (summary.title.as_str(), summary.workspace.as_str(), summary.turn_open),
            ("named", "/a", false)
        );
        assert_eq!(
            summary.updated_at, summary.created_at,
            "a fork is as new as its header, not as old as what it copied"
        );
        assert_ne!(fork.id(), log.id());
        // It reopens like any session, and goes on from there by itself.
        let fork_id = fork.id().to_string();
        drop(fork);
        let mut fork = store.open(&fork_id).unwrap();
        fork.append(Event::TurnStart { turn: 2 }).unwrap();
        assert_eq!((fork.events().len(), log.events().len()), (8, 12));
        // The edit it copied is the other session's to undo: its snapshot is kept there.
        assert_eq!(crate::undo::plan(log.events(), 1).len(), 1);
        assert_eq!(crate::undo::plan(fork.events(), 1), []);

        // The whole conversation, less the turn that is still running; and before the first turn, nothing but the header.
        let whole = store.fork(log.events(), None).unwrap();
        assert_eq!(
            (kinds(&whole).len(), summarize(whole.events()).fork.unwrap().turn),
            (10, 2)
        );
        let bare = store.fork(log.events(), Some(1)).unwrap();
        assert_eq!(
            (bare.events().len(), summarize(bare.events()).fork.unwrap().turn),
            (1, 0)
        );

        assert!(
            store
                .fork(log.events(), Some(9))
                .err()
                .unwrap()
                .to_string()
                .contains("no turn 9")
        );
        let child = store
            .create_with(NewSession {
                workspace: Path::new("/a"),
                provider: "anthropic",
                model: "m",
                parent: Some(log.id()),
                worktree: None,
                schedule: None,
                aside: None,
            })
            .unwrap();
        assert!(
            store
                .fork(child.events(), None)
                .err()
                .unwrap()
                .to_string()
                .contains("subagent")
        );
    }

    #[test]
    fn a_subagents_session_names_its_parent_and_reports_how_it_ended() {
        let (_directory, store) = store();
        let parent = store.create(Path::new("/a"), "anthropic", "m").unwrap();
        let tree = Worktree {
            origin: "/a".into(),
            branch: "odg/1".into(),
            checkout: "/w".into(),
        };
        let mut child = store
            .create_with(NewSession {
                workspace: Path::new("/w"),
                provider: "anthropic",
                model: "m",
                parent: Some(parent.id()),
                worktree: Some(tree.clone()),
                schedule: None,
                aside: None,
            })
            .unwrap();
        let timed = store
            .create_with(NewSession {
                workspace: Path::new("/a"),
                provider: "anthropic",
                model: "m",
                parent: None,
                worktree: None,
                schedule: Some("nightly"),
                aside: None,
            })
            .unwrap();
        assert_eq!(
            (
                summarize(timed.events()).schedule.as_deref(),
                summarize(child.events()).schedule
            ),
            (Some("nightly"), None)
        );
        assert_eq!(
            (summarize(child.events()).worktree, summarize(parent.events()).worktree),
            (Some(tree), None)
        );
        assert_eq!(super::parent(child.events()), Some(parent.id()));
        assert_eq!(super::parent(parent.events()), None);
        assert_eq!(summarize(child.events()).parent.as_deref(), Some(parent.id()));
        assert_eq!(SessionStore::of(&child).ids().len(), 3);
        assert_eq!(summarize(child.events()).fork, None);
        // A header written before subagents has no parent, and reads as none.
        let header = serde_json::to_value(&parent.events()[0]).unwrap();
        assert!(header["data"].get("parent").is_none());

        assert_eq!(report(child.events()), (String::new(), "running".to_string(), None));
        child.append(Event::TurnStart { turn: 1 }).unwrap();
        child
            .append(assistant(vec![
                json!({ "type": "text", "text": "Looking." }),
                json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": {} }),
            ]))
            .unwrap();
        child
            .append(assistant(vec![json!({ "type": "text", "text": "It is fine." })]))
            .unwrap();
        assert_eq!(
            report(child.events()),
            ("It is fine.".to_string(), "running".to_string(), None)
        );
        child
            .append(Event::TurnEnd {
                turn: 1,
                reason: "error".into(),
                error: Some("no key".into()),
            })
            .unwrap();
        assert_eq!(
            report(child.events()),
            (
                "It is fine.".to_string(),
                "error".to_string(),
                Some("no key".to_string())
            )
        );
    }

    #[test]
    fn a_picture_a_tool_returned_goes_to_the_model_inside_its_result() {
        let (_directory, store) = store();
        let mut log = store
            .create(Path::new("/work"), "anthropic", "claude-opus-5-5")
            .unwrap();
        log.append(Event::UserMessage {
            text: "look".into(),
            steering: false,
            mode: Mode::Edit,
            images: Vec::new(),
            files: Vec::new(),
        })
        .unwrap();
        log.append(assistant(vec![
            json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "a.png" } }),
        ]))
        .unwrap();
        let image = crate::events::Image {
            media_type: "image/png".into(),
            data: "aGk=".into(),
        };
        let before = context_estimate(log.events());
        log.append(Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "read_file".into(),
            content: "a.png is a PNG picture.".into(),
            is_error: false,
            meta: Value::Null,
            images: vec![image],
        })
        .unwrap();
        let messages = derive_messages(log.events());
        assert_eq!(
            messages.last().unwrap()["content"][0]["content"],
            json!([{ "type": "text", "text": "a.png is a PNG picture." }, { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "aGk=" } }])
        );
        // A picture is counted as one in a message is, not by the length of its base64.
        assert_eq!(
            context_estimate(log.events()) - before,
            "a.png is a PNG picture.".len() as u64 / 3 + 1500
        );
    }

    #[test]
    fn forked_cli_session_does_not_reuse_source_external_session_id() {
        let (_directory, store) = store();
        let mut source = store
            .create(Path::new("/work"), "antigravity", "gemini-4-neon")
            .unwrap();
        source.append(Event::TurnStart { turn: 1 }).unwrap();
        source
            .append(Event::UserMessage {
                text: "hi".into(),
                steering: false,
                mode: Mode::Auto,
                images: Vec::new(),
                files: Vec::new(),
            })
            .unwrap();
        source
            .append(Event::ExternalSession {
                backend: "antigravity".into(),
                session_id: "src-conv-1".into(),
            })
            .unwrap();
        source
            .append(assistant(vec![json!({ "type": "text", "text": "hello" })]))
            .unwrap();
        source
            .append(Event::TurnEnd {
                turn: 1,
                reason: "completed".into(),
                error: None,
            })
            .unwrap();

        let mut forked = store.fork(source.events(), None).unwrap();
        assert_eq!(external_session(source.events(), "antigravity"), Some("src-conv-1"));
        assert_eq!(external_session(forked.events(), "antigravity"), None);

        forked.append(Event::TurnStart { turn: 2 }).unwrap();
        forked
            .append(Event::ExternalSession {
                backend: "antigravity".into(),
                session_id: "fork-conv-2".into(),
            })
            .unwrap();
        assert_eq!(external_session(forked.events(), "antigravity"), Some("fork-conv-2"));
    }

    #[test]
    fn mid_turn_compaction_with_kept_from_turn_preserves_pending_tool_results() {
        let (_directory, store) = store();
        let mut log = store
            .create(Path::new("/work"), "anthropic", "claude-sonnet-5-5")
            .unwrap();
        let user = |text: &str, steering| Event::UserMessage {
            text: text.into(),
            steering,
            mode: Mode::Auto,
            images: vec![],
            files: vec![],
        };

        // Turn 1
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(user("turn 1 question", false)).unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "turn 1 answer" })]))
            .unwrap();
        log.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        // Turn 2
        log.append(Event::TurnStart { turn: 2 }).unwrap();
        log.append(user("turn 2 question", false)).unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "turn 2 answer" })]))
            .unwrap();
        log.append(Event::TurnEnd {
            turn: 2,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        // Turn 3 mid-turn: tool call + ToolResult + steering UserMessage + ContextCompacted(kept_from_turn: Some(2))
        log.append(Event::TurnStart { turn: 3 }).unwrap();
        log.append(user("read Cargo.toml", false)).unwrap();
        log.append(assistant(vec![json!({
            "type": "tool_use",
            "id": "t1",
            "name": "read_file",
            "input": { "path": "Cargo.toml" }
        })]))
        .unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "t1".into(),
            tool: "read_file".into(),
            content: "[package]\nname = \"demo\"".into(),
            is_error: false,
            meta: Value::Null,
            images: vec![],
        })
        .unwrap();
        log.append(user("also check dependencies", true)).unwrap();
        log.append(Event::ContextCompacted {
            summary: "Turn 1 summary.".into(),
            tokens_before: 1200,
            focus: None,
            kept_from_turn: Some(2),
            tokens: None,
        })
        .unwrap();

        let messages = derive_messages(log.events());
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"][0]["id"], "t1");

        let tail = &messages[4];
        assert_eq!(tail["role"], "user");
        let tail_blocks = tail["content"].as_array().unwrap();
        assert_eq!(
            tail_blocks,
            &vec![
                json!({
                    "type": "tool_result",
                    "tool_use_id": "t1",
                    "content": "[package]\nname = \"demo\"",
                    "is_error": false
                }),
                json!({
                    "type": "text",
                    "text": "also check dependencies"
                }),
            ],
            "pending ToolResult must remain paired and steering text must appear exactly once"
        );

        // Also verify inserted_header == 1 offset when kept_msgs begins with an assistant message
        // and the session later switches providers (`carried` must rewrite the shifted assistant slot).
        let mut alt = store
            .create(Path::new("/work"), "anthropic", "claude-sonnet-5-5")
            .unwrap();
        alt.append(Event::TurnStart { turn: 1 }).unwrap();
        alt.append(user("first", false)).unwrap();
        alt.append(assistant(vec![json!({ "type": "text", "text": "ans 1" })]))
            .unwrap();
        alt.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();
        alt.append(Event::TurnStart { turn: 2 }).unwrap();
        alt.append(assistant(vec![
            json!({ "type": "thinking", "thinking": "secret", "signature": "sig" }),
            json!({ "type": "text", "text": "ans 2 without prior user msg" }),
        ]))
        .unwrap();
        alt.append(Event::TurnEnd {
            turn: 2,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();
        alt.append(Event::ContextCompacted {
            summary: "Summary.".into(),
            tokens_before: 500,
            focus: None,
            kept_from_turn: Some(2),
            tokens: None,
        })
        .unwrap();
        alt.append(Event::ModelChanged {
            model: "deepseek-chat".into(),
            provider: Some("deepseek".into()),
        })
        .unwrap();
        alt.append(user("next", false)).unwrap();
        let alt_msgs = derive_messages(alt.events());
        assert_eq!(alt_msgs[0]["role"], "user");
        assert_eq!(alt_msgs[1]["role"], "assistant");
        assert_eq!(
            alt_msgs[1]["content"],
            json!([{ "type": "text", "text": "ans 2 without prior user msg" }])
        );
    }

    #[test]
    fn unanswered_tool_uses_tracks_reused_ids_per_occurrence() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "openai", "gpt-5").unwrap();

        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(assistant(vec![json!({
            "type": "tool_use",
            "id": "dup_0",
            "name": "read_file",
            "input": {}
        })]))
        .unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "dup_0".into(),
            tool: "read_file".into(),
            content: "ok".into(),
            is_error: false,
            meta: Value::Null,
            images: vec![],
        })
        .unwrap();
        log.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        log.append(Event::TurnStart { turn: 2 }).unwrap();
        log.append(assistant(vec![json!({
            "type": "tool_use",
            "id": "dup_0",
            "name": "edit_file",
            "input": {}
        })]))
        .unwrap();

        assert_eq!(
            unanswered_tool_uses(log.events()),
            vec![("dup_0".to_string(), "edit_file".to_string())]
        );

        // Full compaction (`kept_from_turn: None`) discards all prior messages and clears open tool calls.
        log.append(Event::ContextCompacted {
            summary: "Compacted.".into(),
            tokens_before: 100,
            focus: None,
            kept_from_turn: None,
            tokens: None,
        })
        .unwrap();
        assert!(unanswered_tool_uses(log.events()).is_empty());
    }

    #[test]
    fn provider_switch_renames_only_paired_user_message_and_caps_id_at_64_chars() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "moonshot", "kimi-k3.1-pro").unwrap();
        let said = |text: &str| Event::UserMessage {
            text: text.into(),
            steering: false,
            mode: Mode::Auto,
            images: vec![],
            files: vec![],
        };
        let tool_result = |id: &str, out: &str| Event::ToolResult {
            tool_use_id: id.into(),
            tool: "read_file".into(),
            content: out.into(),
            is_error: false,
            meta: Value::Null,
            images: vec![],
        };

        let overlong_a = format!("{}:tail_alpha", "x".repeat(95));
        let overlong_b = format!("{}:tail_bravo", "x".repeat(95));
        assert_eq!(overlong_a.len(), 106);

        // Turn 1 on `moonshot`: uses `functions.read:0`, colliding `a:1` & `a.1`, empty id `""`, and two 100+ char ids.
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(said("turn 1")).unwrap();
        log.append(assistant(vec![
            json!({ "type": "tool_use", "id": "functions.read:0", "name": "read_file", "input": { "path": "a" } }),
            json!({ "type": "tool_use", "id": "a:1", "name": "read_file", "input": { "path": "b" } }),
            json!({ "type": "tool_use", "id": "a.1", "name": "read_file", "input": { "path": "c" } }),
            json!({ "type": "tool_use", "id": "", "name": "read_file", "input": { "path": "d" } }),
            json!({ "type": "tool_use", "id": overlong_a, "name": "read_file", "input": { "path": "e" } }),
            json!({ "type": "tool_use", "id": overlong_b, "name": "read_file", "input": { "path": "f" } }),
        ]))
        .unwrap();
        log.append(tool_result("functions.read:0", "out-0")).unwrap();
        log.append(tool_result("a:1", "out-a1")).unwrap();
        log.append(tool_result("a.1", "out-a2")).unwrap();
        log.append(tool_result("", "out-empty")).unwrap();
        log.append(tool_result(&overlong_a, "out-long-a")).unwrap();
        log.append(tool_result(&overlong_b, "out-long-b")).unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "done turn 1" })]))
            .unwrap();
        log.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        // Turn 2 switches to `openrouter` (current speaker) and ALSO emits `functions.read:0`.
        log.append(Event::ModelChanged {
            model: "moonshotai/kimi-k3".into(),
            provider: Some("openrouter".into()),
        })
        .unwrap();
        log.append(Event::TurnStart { turn: 2 }).unwrap();
        log.append(said("turn 2")).unwrap();
        log.append(assistant(vec![
            json!({ "type": "tool_use", "id": "functions.read:0", "name": "read_file", "input": { "path": "z" } }),
        ]))
        .unwrap();
        log.append(tool_result("functions.read:0", "out-turn2")).unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "done turn 2" })]))
            .unwrap();
        log.append(Event::TurnEnd {
            turn: 2,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        log.append(Event::TurnStart { turn: 3 }).unwrap();
        log.append(said("turn 3")).unwrap();

        let messages = derive_messages(log.events());

        // Turn 1 (`messages[1]` assistant, `messages[2]` user) was spoken by `moonshot != openrouter`:
        let t1_calls: Vec<&str> = messages[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].as_str().unwrap())
            .collect();
        let t1_results: Vec<&str> = messages[2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["tool_use_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            t1_calls, t1_results,
            "Turn 1 tool_use and tool_result ids must pair 1-to-1"
        );
        assert_eq!(t1_calls[0], "functions_read_0");
        assert_eq!(t1_calls[1], "a_1");
        assert_eq!(t1_calls[2], "a_1_2");
        assert_eq!(t1_calls[3], "call");
        assert_eq!(t1_calls[4].len(), 64);
        assert_eq!(t1_calls[5].len(), 64);
        assert_ne!(t1_calls[4], t1_calls[5]);
        for id in &t1_calls {
            assert!(!id.is_empty() && id.len() <= 64, "invalid length for {id}");
            assert!(
                id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "invalid chars in {id}"
            );
        }

        // Turn 2 (`messages[5]` assistant, `messages[6]` user) was spoken by `openrouter == speaker`:
        // neither its `tool_use.id` nor its `tool_result.tool_use_id` may be corrupted by Turn 1's rename!
        assert_eq!(messages[5]["content"][0]["id"], "functions.read:0");
        assert_eq!(messages[6]["content"][0]["tool_use_id"], "functions.read:0");
    }

    #[test]
    fn consecutive_cli_assistant_tool_uses_merge_into_single_assistant_turn() {
        use crate::llm::Tokens;
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "claude_code", "opus").unwrap();

        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage {
            text: "inspect a and b".into(),
            steering: false,
            mode: Mode::Auto,
            images: vec![],
            files: vec![],
        })
        .unwrap();
        // TurnRecorder emits separate AssistantMessage events for parallel CLI tool starts,
        // plus an optional empty-content token settlement AssistantMessage.
        log.append(assistant(vec![json!({
            "type": "tool_use",
            "id": "tool:1",
            "name": "read_file",
            "input": { "path": "a.rs" }
        })]))
        .unwrap();
        log.append(assistant(vec![json!({
            "type": "tool_use",
            "id": "tool:2",
            "name": "read_file",
            "input": { "path": "b.rs" }
        })]))
        .unwrap();
        log.append(Event::AssistantMessage {
            content: vec![],
            stop_reason: Some("end_turn".into()),
            model: "opus".into(),
            usage: json!({}),
            tokens: Some(Tokens {
                input: 100,
                cached: 20,
                output: 15,
            }),
        })
        .unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "tool:1".into(),
            tool: "read_file".into(),
            content: "fn a() {}".into(),
            is_error: false,
            meta: Value::Null,
            images: vec![],
        })
        .unwrap();
        log.append(Event::ToolResult {
            tool_use_id: "tool:2".into(),
            tool: "read_file".into(),
            content: "fn b() {}".into(),
            is_error: false,
            meta: Value::Null,
            images: vec![],
        })
        .unwrap();
        log.append(assistant(vec![json!({ "type": "text", "text": "Both look good." })]))
            .unwrap();
        log.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();

        // Switch from CLI to native Anthropic API and continue.
        log.append(Event::ModelChanged {
            model: "claude-opus-5-5".into(),
            provider: Some("anthropic".into()),
        })
        .unwrap();
        log.append(Event::TurnStart { turn: 2 }).unwrap();
        log.append(Event::UserMessage {
            text: "continue".into(),
            steering: false,
            mode: Mode::Auto,
            images: vec![],
            files: vec![],
        })
        .unwrap();

        let messages = derive_messages(log.events());
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(
            messages[1]["content"],
            json!([
                { "type": "tool_use", "id": "tool_1", "name": "read_file", "input": { "path": "a.rs" } },
                { "type": "tool_use", "id": "tool_2", "name": "read_file", "input": { "path": "b.rs" } },
            ])
        );
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(
            messages[2]["content"],
            json!([
                { "type": "tool_result", "tool_use_id": "tool_1", "content": "fn a() {}", "is_error": false },
                { "type": "tool_result", "tool_use_id": "tool_2", "content": "fn b() {}", "is_error": false },
            ])
        );
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(
            messages[3]["content"],
            json!([{ "type": "text", "text": "Both look good." }])
        );
        assert_eq!(messages[4]["role"], "user");
    }

    fn naive_derive_messages_for_test(events: &[Envelope]) -> Vec<Value> {
        let text = |text: &str| json!({ "type": "text", "text": text });
        let mut messages = Vec::new();
        let mut pending: Vec<Value> = Vec::new();
        let mut speaker = match events.first().map(|envelope| &envelope.event) {
            Some(Event::SessionCreated { provider, .. }) => {
                provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
            }
            _ => DEFAULT_PROVIDER.to_string(),
        };
        let mut spoken: Vec<(usize, String)> = Vec::new();
        let mut turn_starts: HashMap<u64, (usize, usize)> = HashMap::new();
        let mut current_turn_msg_start: usize = 0;
        let mut unanswered: Vec<Value> = Vec::new();
        let mut todos: Option<Vec<(String, String)>> = None;
        let mut planning = false;
        let mut brief: Option<&str> = None;
        for envelope in events {
            match &envelope.event {
                Event::TurnStart { turn } => {
                    turn_starts.insert(*turn, (messages.len(), spoken.len()));
                    current_turn_msg_start = messages.len();
                }
                Event::AsideBriefed { brief: told } => brief = Some(told),
                Event::UserMessage {
                    text: said,
                    mode,
                    images,
                    files,
                    ..
                } => {
                    if (*mode == Mode::Plan) != planning {
                        planning = !planning;
                        pending.push(text(if planning { PLAN_ON } else { PLAN_OFF }));
                    }
                    if let Some(told) = brief.take() {
                        pending.push(text(told));
                    }
                    let blocks = images
                        .iter()
                        .map(|image| json!({ "type": "image", "source": { "type": "base64", "media_type": image.media_type, "data": image.data } }))
                        .chain(files.iter().map(|file| text(&crate::events::attached_text(file))))
                        .chain((!said.is_empty()).then(|| text(said)));
                    for block in blocks {
                        pending.push(block.clone());
                        unanswered.push(block);
                    }
                }
                Event::FilesRestored {
                    turn,
                    restored,
                    skipped,
                } => {
                    let mut told = format!("[The user undid your file changes from turn {turn} on.");
                    if !restored.is_empty() {
                        told.push_str(&format!(
                            " Back to what they held before those changes, or removed if you had created them: {}.",
                            restored.join(", ")
                        ));
                    }
                    if !skipped.is_empty() {
                        told.push_str(&format!(
                            " Left as they are, because they changed after your last edit: {}.",
                            skipped.join(", ")
                        ));
                    }
                    told.push_str(" Read a file again before relying on what it held.]");
                    pending.push(text(&told));
                }
                Event::ArtifactRestored { path, version, .. } => {
                    let told = format!(
                        "[The user restored {path} to version v{version}. Read it again before relying on what it held.]"
                    );
                    pending.push(text(&told));
                }
                Event::ModelChanged {
                    provider: Some(provider),
                    ..
                } => speaker = provider.clone(),
                Event::ContextCompacted {
                    summary,
                    kept_from_turn,
                    ..
                } => {
                    let mut header = vec![text(&format!("{COMPACTED}\n\n{summary}"))];
                    if planning {
                        header.push(text(PLAN_ON));
                    }
                    if let Some(todos) = todos.as_ref().filter(|todos| !todos.is_empty()) {
                        let lines: Vec<String> = todos
                            .iter()
                            .map(|(item, status)| format!("- [{status}] {item}"))
                            .collect();
                        header.push(text(&format!(
                            "[Your to-do list as you last wrote it; update it with `update_todos` as you go.]\n{}",
                            lines.join("\n")
                        )));
                    }
                    let retained = kept_from_turn
                        .and_then(|turn| turn_starts.get(&turn).copied())
                        .filter(|(msg_from, _)| *msg_from < messages.len());
                    if let Some((msg_from, spoken_from)) = retained {
                        let mut kept_msgs: Vec<Value> = messages.drain(msg_from..).collect();
                        let mut kept_spoken: Vec<(usize, String)> = spoken
                            .drain(spoken_from..)
                            .filter(|(idx, _)| *idx >= msg_from)
                            .map(|(idx, spk)| (idx - msg_from, spk))
                            .collect();
                        let inserted_header = if let Some(first_content) = kept_msgs
                            .first_mut()
                            .filter(|m| m["role"] == "user")
                            .and_then(|m| m["content"].as_array_mut())
                        {
                            header.append(first_content);
                            *first_content = header;
                            0usize
                        } else {
                            kept_msgs.insert(0, json!({ "role": "user", "content": header }));
                            1usize
                        };
                        if inserted_header > 0 {
                            for (idx, _) in &mut kept_spoken {
                                *idx += inserted_header;
                            }
                        }
                        turn_starts.retain(|_, (m_idx, _)| *m_idx >= msg_from);
                        for (m_idx, s_idx) in turn_starts.values_mut() {
                            *m_idx = (*m_idx - msg_from) + inserted_header;
                            *s_idx = s_idx.saturating_sub(spoken_from);
                        }
                        current_turn_msg_start = if current_turn_msg_start >= msg_from {
                            (current_turn_msg_start - msg_from) + inserted_header
                        } else {
                            0
                        };
                        messages = kept_msgs;
                        spoken = kept_spoken;
                    } else {
                        messages.clear();
                        spoken.clear();
                        pending.clear();
                        turn_starts.clear();
                        current_turn_msg_start = 0;
                        pending.extend(header);
                        pending.extend(unanswered.iter().cloned());
                    }
                }
                Event::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    images,
                    ..
                } => {
                    let words = if content.is_empty() {
                        "(no output)"
                    } else {
                        content.as_str()
                    };
                    let content = if images.is_empty() {
                        json!(words)
                    } else {
                        let pictures = images.iter().map(|image| json!({ "type": "image", "source": { "type": "base64", "media_type": image.media_type, "data": image.data } }));
                        Value::Array(std::iter::once(text(words)).chain(pictures).collect())
                    };
                    pending.push(json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error }));
                }
                Event::AssistantMessage { content, .. } if !content.is_empty() => {
                    let merged = if !pending.is_empty() {
                        messages.push(json!({ "role": "user", "content": std::mem::take(&mut pending) }));
                        false
                    } else if messages.last().is_some_and(|m| m["role"] == "assistant")
                        && spoken.last().is_some_and(|(idx, spk)| {
                            *idx >= current_turn_msg_start && *idx == messages.len() - 1 && spk == &speaker
                        })
                    {
                        if let Some(existing) = messages.last_mut().and_then(|m| m["content"].as_array_mut()) {
                            existing.extend(content.iter().cloned());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !merged {
                        spoken.push((messages.len(), speaker.clone()));
                        messages.push(json!({ "role": "assistant", "content": content }));
                    }
                    unanswered.clear();
                    for block in content
                        .iter()
                        .filter(|block| block["type"] == "tool_use" && block["name"] == "update_todos")
                    {
                        if let Ok(items) = crate::tools::todo_items(&block["input"]) {
                            todos = Some(items);
                        }
                    }
                }
                _ => {}
            }
        }
        if !pending.is_empty() {
            messages.push(json!({ "role": "user", "content": pending }));
        }
        let mut used_ids: HashSet<String> = HashSet::new();
        for &(at, ref from) in &spoken {
            if from == &speaker {
                for block in messages[at]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use"
                        && let Some(id) = block["id"].as_str().filter(|id| !id.is_empty())
                    {
                        used_ids.insert(id.to_string());
                    }
                }
            }
        }
        for (at, from) in spoken {
            if from != speaker {
                let mut local_renamed: HashMap<String, VecDeque<String>> = HashMap::new();
                let mut any_renamed = false;
                let content = carried(
                    messages[at]["content"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    &mut used_ids,
                    &mut local_renamed,
                    &mut any_renamed,
                );
                messages[at]["content"] = Value::Array(content);
                if any_renamed && let Some(next_user) = messages.get_mut(at + 1).filter(|m| m["role"] == "user") {
                    for block in next_user["content"].as_array_mut().into_iter().flatten() {
                        if block["type"] == "tool_result"
                            && let Some(raw_id) = block["tool_use_id"].as_str()
                            && let Some(queue) = local_renamed.get_mut(raw_id)
                            && let Some(new_id) = if queue.len() > 1 {
                                queue.pop_front()
                            } else {
                                queue.front().cloned()
                            }
                        {
                            block["tool_use_id"] = json!(new_id);
                        }
                    }
                }
            }
        }
        messages
    }

    fn naive_context_estimate_for_test(events: &[Envelope]) -> u64 {
        let guess = |text: &str| text.len() as u64 / 3;
        let mut estimate = 0;
        let mut current_turn = 0u64;
        let mut turn_added: HashMap<u64, u64> = HashMap::new();
        let mut unanswered_added = 0u64;
        let mut turn_answered = false;
        for envelope in events {
            match &envelope.event {
                Event::TurnStart { turn } => {
                    current_turn = *turn;
                    turn_answered = false;
                    unanswered_added = 0;
                }
                Event::ContextCompacted {
                    summary,
                    kept_from_turn,
                    ..
                } => {
                    let retained: u64 = match kept_from_turn {
                        Some(from_turn) => turn_added
                            .iter()
                            .filter(|(t, _)| **t >= *from_turn && (**t < current_turn || turn_answered))
                            .map(|(_, v)| *v)
                            .sum(),
                        None => 0,
                    };
                    turn_added.clear();
                    estimate = guess(summary) + retained + if !turn_answered { unanswered_added } else { 0 };
                }
                Event::AssistantMessage {
                    tokens: Some(tokens),
                    content,
                    ..
                } => {
                    turn_answered = true;
                    estimate = tokens.input + tokens.output;
                    *turn_added.entry(current_turn).or_default() += guess(&Value::Array(content.clone()).to_string());
                }
                Event::AssistantMessage {
                    content, tokens: None, ..
                } => {
                    turn_answered = true;
                    let g = guess(&Value::Array(content.clone()).to_string());
                    estimate += g;
                    *turn_added.entry(current_turn).or_default() += g;
                }
                Event::UserMessage {
                    text, images, files, ..
                } => {
                    let g = guess(text)
                        + 1500 * images.len() as u64
                        + files
                            .iter()
                            .map(|file| guess(&crate::events::attached_text(file)))
                            .sum::<u64>();
                    estimate += g;
                    *turn_added.entry(current_turn).or_default() += g;
                    if !turn_answered {
                        unanswered_added += g;
                    }
                }
                Event::ToolResult { content, images, .. } => {
                    let g = guess(content) + 1500 * images.len() as u64;
                    estimate += g;
                    *turn_added.entry(current_turn).or_default() += g;
                }
                Event::AsideBriefed { brief } => {
                    let g = guess(brief);
                    estimate += g;
                    *turn_added.entry(current_turn).or_default() += g;
                }
                _ => {}
            }
        }
        estimate
    }

    #[test]
    fn derive_messages_and_context_estimate_skip_events_before_compaction() {
        use crate::events::Image;
        use crate::llm::Tokens;

        let mut events: Vec<Envelope> = Vec::new();
        fn push(events: &mut Vec<Envelope>, ev: Event) {
            let seq = events.len() as u64 + 1;
            events.push(Envelope {
                seq,
                ts: "2026-11-26T00:00:00Z".into(),
                event: ev,
            });
        }

        push(
            &mut events,
            Event::SessionCreated {
                id: "s-perf".into(),
                workspace: "/work".into(),
                provider: Some("moonshot".into()),
                model: "kimi-k3.1-pro".into(),
                format: FORMAT_VERSION,
                parent: None,
                worktree: None,
                schedule: None,
                fork: None,
                aside: None,
            },
        );

        let big_image = Image {
            media_type: "image/png".into(),
            data: "A".repeat(256 * 1024),
        };
        let big_tool_out = "x".repeat(64 * 1024);

        // 120 heavy turns before compaction, switching provider, toggling Plan mode, and setting todos.
        for turn in 1..=120u64 {
            push(&mut events, Event::TurnStart { turn });
            let mode = if turn >= 50 { Mode::Plan } else { Mode::Auto };
            push(
                &mut events,
                Event::UserMessage {
                    text: format!("turn {turn}"),
                    steering: false,
                    mode,
                    images: vec![big_image.clone()],
                    files: vec![],
                },
            );
            if turn == 40 {
                push(
                    &mut events,
                    Event::ModelChanged {
                        model: "deepseek-v4-pro".into(),
                        provider: Some("deepseek".into()),
                    },
                );
            }
            let call_id = format!("functions.read:{turn}");
            let mut blocks = vec![json!({
                "type": "tool_use",
                "id": call_id,
                "name": "read_file",
                "input": { "path": format!("file_{turn}.rs") }
            })];
            if turn == 80 {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": "todo_80",
                    "name": "update_todos",
                    "input": {
                        "todos": [
                            { "text": "audit session replay", "status": "done" },
                            { "text": "verify tail complexity", "status": "in_progress" }
                        ]
                    }
                }));
            }
            push(
                &mut events,
                Event::AssistantMessage {
                    content: blocks,
                    stop_reason: Some("tool_use".into()),
                    model: "deepseek-v4-pro".into(),
                    usage: json!({}),
                    tokens: Some(Tokens {
                        input: 20_000 + turn * 100,
                        cached: 10_000,
                        output: 150,
                    }),
                },
            );
            push(
                &mut events,
                Event::ToolResult {
                    tool_use_id: call_id,
                    tool: "read_file".into(),
                    content: big_tool_out.clone(),
                    is_error: false,
                    meta: Value::Null,
                    images: vec![big_image.clone()],
                },
            );
            push(
                &mut events,
                Event::AssistantMessage {
                    content: vec![json!({ "type": "text", "text": format!("finished turn {turn}") })],
                    stop_reason: Some("end_turn".into()),
                    model: "deepseek-v4-pro".into(),
                    usage: json!({}),
                    tokens: Some(Tokens {
                        input: 25_000 + turn * 100,
                        cached: 12_000,
                        output: 80,
                    }),
                },
            );
            push(
                &mut events,
                Event::TurnEnd {
                    turn,
                    reason: "completed".into(),
                    error: None,
                },
            );
        }

        // Case 1: `kept_from_turn: Some(119)` + provider switch in tail + steering message.
        push(&mut events, Event::TurnStart { turn: 121 });
        push(
            &mut events,
            Event::UserMessage {
                text: "inspect tail".into(),
                steering: false,
                mode: Mode::Plan,
                images: vec![],
                files: vec![],
            },
        );
        push(
            &mut events,
            Event::AssistantMessage {
                content: vec![
                    json!({ "type": "tool_use", "id": "tail:1", "name": "read_file", "input": { "path": "Cargo.toml" } }),
                ],
                stop_reason: Some("tool_use".into()),
                model: "deepseek-v4-pro".into(),
                usage: json!({}),
                tokens: Some(Tokens {
                    input: 45_000,
                    cached: 20_000,
                    output: 60,
                }),
            },
        );
        push(
            &mut events,
            Event::ToolResult {
                tool_use_id: "tail:1".into(),
                tool: "read_file".into(),
                content: "[package]".into(),
                is_error: false,
                meta: Value::Null,
                images: vec![],
            },
        );
        push(
            &mut events,
            Event::UserMessage {
                text: "keep going".into(),
                steering: true,
                mode: Mode::Plan,
                images: vec![],
                files: vec![],
            },
        );
        push(
            &mut events,
            Event::ContextCompacted {
                summary: "Compacted 1..118.".into(),
                tokens_before: 45_000,
                focus: None,
                kept_from_turn: Some(119),
                tokens: None,
            },
        );
        push(
            &mut events,
            Event::ModelChanged {
                model: "claude-sonnet-5-5".into(),
                provider: Some("anthropic".into()),
            },
        );

        assert_eq!(derive_messages(&events), naive_derive_messages_for_test(&events));
        assert_eq!(context_estimate(&events), naive_context_estimate_for_test(&events));

        // Verify speed of O(tail) replay across 50 iterations (avoids cloning ~70 MB of prefix images per call).
        let started = std::time::Instant::now();
        for _ in 0..20 {
            let m = derive_messages(&events);
            let c = context_estimate(&events);
            assert!(!m.is_empty() && c > 0);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "tail replay took {:?}",
            started.elapsed()
        );

        // Case 2: Full compaction (`kept_from_turn: None`) with unanswered steering user message.
        push(
            &mut events,
            Event::ContextCompacted {
                summary: "Compacted everything.".into(),
                tokens_before: 12_000,
                focus: None,
                kept_from_turn: None,
                tokens: None,
            },
        );
        assert_eq!(derive_messages(&events), naive_derive_messages_for_test(&events));
        assert_eq!(context_estimate(&events), naive_context_estimate_for_test(&events));
    }

    #[test]
    fn session_store_summary_cache_hits_unchanged_and_updates_on_append() {
        let (_directory, store) = store();
        let mut log = store
            .create(Path::new("/work"), "anthropic", "claude-sonnet-5-5")
            .unwrap();
        let id = log.id().to_string();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage {
            text: "initial title".into(),
            steering: false,
            mode: Mode::Auto,
            images: vec![],
            files: vec![],
        })
        .unwrap();

        let s1 = store.peek(&id).unwrap();
        assert_eq!(s1.title, "initial title");
        assert!(s1.turn_open);
        let idle = store.peek_idle(&id).unwrap();
        assert!(!idle.turn_open && !idle.awaiting_approval);
        // Repeated peek hits cache and returns identical summary.
        assert_eq!(store.peek(&id).unwrap(), s1);

        // Appending changes file size, invalidating the `(mtime, len)` cache entry automatically.
        log.append(Event::SessionRenamed {
            title: "renamed title".into(),
        })
        .unwrap();
        log.append(Event::TurnEnd {
            turn: 1,
            reason: "completed".into(),
            error: None,
        })
        .unwrap();
        let s2 = store.peek(&id).unwrap();
        assert_eq!(s2.title, "renamed title");
        assert!(!s2.turn_open);
    }

    #[test]
    fn summarize_empty_slice_returns_safe_default_without_panicking() {
        let empty = summarize(&[]);
        assert_eq!(empty.id, "");
        assert_eq!(empty.title, "");
        assert_eq!(empty.workspace, "");
        assert_eq!(empty.provider, DEFAULT_PROVIDER);
        assert!(!empty.turn_open && !empty.awaiting_approval);
    }

    #[cfg(unix)]
    #[test]
    fn session_store_enforces_0700_dir_and_0600_log_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let sessions_root = dir.path().join("sessions");
        fs::create_dir_all(&sessions_root).unwrap();
        fs::set_permissions(&sessions_root, fs::Permissions::from_mode(0o755)).unwrap();

        let store = SessionStore::new(&sessions_root);
        let log = store
            .create(Path::new("/work"), "anthropic", "claude-opus-5-5")
            .unwrap();
        let id = log.id().to_string();
        let log_path = log.path().to_path_buf();
        drop(log);

        let mode_of = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_of(&sessions_root), 0o700);
        assert_eq!(mode_of(log_path.parent().unwrap()), 0o700);
        assert_eq!(mode_of(&log_path), 0o600);

        let forked = store.fork(&store.read(&id).unwrap(), None).unwrap();
        let fork_path = forked.path().to_path_buf();
        drop(forked);
        assert_eq!(mode_of(fork_path.parent().unwrap()), 0o700);
        assert_eq!(mode_of(&fork_path), 0o600);
    }
}
