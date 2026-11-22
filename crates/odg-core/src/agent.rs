//! The agent loop for one session.
//!
//! A turn starts with a user message and runs steps until the model stops
//! calling tools. A step is one model request plus the tools it calls. Every
//! durable fact is appended to the session log before anything depends on it,
//! and each model request is derived from the log alone.

mod actions;
mod prompts;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::events::{Attached, Decision, Envelope, Event, Image, Reply};
use crate::grants::GrantStore;
use crate::llm::{Llm, LlmError, LlmRequest, StreamDelta};
use crate::mcp::McpHub;
use crate::policy::{self, Mode, Verdict};
use crate::providers;
use crate::session::{SessionLog, SessionStore, SessionSummary, context_estimate, derive_messages, instructions, read_instructions, skills, summarize, unanswered_tool_uses};
use crate::tools::{self, ToolContext, ToolOutput};
use crate::undo;

pub use prompts::{GUARDIAN, Origin, SUMMARIZE, system_prompt};
use prompts::same_folder;
#[cfg(test)]
use prompts::verdict;

/// A turn that needs more model requests than this is stopped: it is looping.
const MAX_STEPS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Running,
    WaitingApproval,
}

/// What a client observes. `Log` events are durable and arrive in log order;
/// `Stream` fragments are transient and belong to the message being generated.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// Nearly every event sent is a log event, so a smaller enum with the envelope boxed would save nothing.
#[allow(clippy::large_enum_variant)]
pub enum UiEvent {
    Log { session_id: String, envelope: Envelope },
    Stream { session_id: String, delta: StreamDelta },
    Status { session_id: String, status: Status },
}

/// Settings read at the start of every model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuning {
    /// How hard the model works, unless the session was set to work otherwise.
    pub effort: String,
    /// The conversation is compacted once it holds about this many tokens.
    pub context_limit: u64,
    /// How many tokens the model's context window holds. A conversation
    /// close to it is summarized from a shortened transcript, since asking
    /// for the summary would not fit either.
    pub context_window: u64,
    /// In auto mode, the model that judges a command the rules cannot pass before the user is asked;
    /// `None` asks the user straight away.
    pub guardian: Option<String>,
    /// In auto mode, how long a request for approval waits for an answer; `None` waits for good.
    pub approval_wait: Option<std::time::Duration>,
}

/// Gives the settings for a request to the provider and model named: the window and limit depend on them.
pub type TuningSource = Arc<dyn Fn(&str, &str) -> Tuning + Send + Sync>;

/// What a session shares with every other session of the app.
#[derive(Clone)]
pub struct Services {
    /// Approvals given for good.
    pub grants: Arc<GrantStore>,
    /// The MCP servers whose tools join the built-in ones.
    pub mcp: Option<Arc<McpHub>>,
    /// The user's own skills directory; the workspace's `.odg/skills` is added to it.
    pub skills: Option<PathBuf>,
    /// Where a web search goes, asked at every request so that saving a key switches searching on.
    pub search: Option<crate::web::SearchSource>,
    /// Whether commands are confined, asked at every command; `None` means never. See [`crate::sandbox`].
    pub sandbox: Option<crate::sandbox::SandboxSource>,
    /// The folder kept for conversations (闲谈): a session there is told it is one, not work on a project.
    pub chat: Option<PathBuf>,
    /// Where the document runtime is kept; see [`crate::runtime`]. `None` means it is not offered.
    pub runtime: Option<PathBuf>,
    /// Where `generate_image` paints, asked at every request so that saving a key switches painting on.
    pub painter: Option<crate::imagegen::ImageSource>,
    /// The language servers `code_nav` asks, shared by every session. `None` means code is not navigated.
    pub lsp: Option<Arc<crate::lsp::LspHub>>,
}

struct State {
    status: Status,
    cancel: CancellationToken,
    /// Messages sent while a turn runs, each with what was attached; they join the conversation at the next step.
    inbox: VecDeque<Said>,
    approvals: HashMap<String, oneshot::Sender<Decision>>,
    /// Questions `ask_user` waits on the user's answers to; `None` when the user would rather the agent decided.
    questions: HashMap<String, oneshot::Sender<Option<Vec<Reply>>>>,
    /// Grants the user allowed for the rest of this run of the app.
    grants: HashSet<String>,
    /// What runs without asking; read at every tool call, so a switch applies to the next one.
    mode: Mode,
}

/// A message as it is sent: its words, its pictures, its attached files.
type Said = (String, Vec<Image>, Vec<Attached>);

struct Inner<L: Llm> {
    id: String,
    /// The session whose agent set this one off as a subagent.
    parent: Option<String>,
    /// The subagents this session's agent has set off since the app started, by session id.
    children: Mutex<HashMap<String, Session<L>>>,
    /// Where this session's log is kept, and its subagents' logs are created.
    store: SessionStore,
    /// What a subagent is given to share, as this session was.
    services: Services,
    /// The status as last told to the client, for whoever waits for the session to go idle.
    watch: tokio::sync::watch::Sender<Status>,
    workspace: PathBuf,
    /// The folder approvals given for good are kept under: the workspace, or for
    /// a session in a worktree the folder it stands for, so they hold there too.
    granted_in: PathBuf,
    /// The worktree the workspace is in, which a subagent works in too.
    worktree: Option<crate::events::Worktree>,
    /// Where `edit_file` saves what files held, beside the log.
    snapshots: PathBuf,
    /// Commands left running in the background.
    jobs: Arc<crate::jobs::Jobs>,
    /// Approvals given for good, shared by every session.
    kept: Arc<GrantStore>,
    /// The MCP servers whose tools join the built-in ones; shared by every session.
    mcp: Option<Arc<McpHub>>,
    /// Where skills are looked for: the user's directory, then the workspace's, which wins on a shared name.
    skill_roots: Vec<PathBuf>,
    search: Option<crate::web::SearchSource>,
    sandbox: Option<crate::sandbox::SandboxSource>,
    /// Where the document runtime is kept; `None` means it is not offered.
    documents: Option<PathBuf>,
    /// Where a picture is painted, asked at every request so that saving a key switches painting on.
    painter: Option<crate::imagegen::ImageSource>,
    /// The language servers `code_nav` asks; shared by every session.
    lsp: Option<Arc<crate::lsp::LspHub>>,
    /// Where Git keeps the workspace's repository, found when a command is first confined.
    git_directory: std::sync::OnceLock<Option<PathBuf>>,
    /// The session works in the folder kept for conversations.
    chatting: bool,
    /// When true (set on `delegate` tasks with `read_only: true`), file edits, image generation,
    /// external MCP tool calls, and mutating commands are withheld and refused even in Bypass mode.
    read_only: bool,
    /// The provider and model requests go to; the user can move the session to another of either.
    provider: Mutex<String>,
    model: Mutex<String>,
    /// How hard the model works, when the user set it for this session; else the settings' default.
    effort: Mutex<Option<String>>,
    llm: Arc<L>,
    tuning: TuningSource,
    log: Mutex<SessionLog>,
    state: Mutex<State>,
    events: UnboundedSender<UiEvent>,
    runtime: tokio::runtime::Handle,
}

/// What restoring a deliverable version did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoredArtifact {
    /// Display path relative to the workspace.
    pub path: String,
    /// Absolute path restored in the workspace.
    pub file: String,
    /// 1-based version number restored to.
    pub version: u32,
    /// Size in bytes written back.
    pub bytes: u64,
    /// Whether the live file held unrecorded edits that were snapshotted before overwriting.
    pub backed_up: bool,
}

/// A live session: the log plus the agent that appends to it.
pub struct Session<L: Llm> {
    inner: Arc<Inner<L>>,
}

impl<L: Llm> Clone for Session<L> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<L: Llm> Session<L> {
    /// Wrap a stored log. A turn that a crash left open is closed first. The
    /// session resumes the mode of its last message.
    pub fn new(
        log: SessionLog,
        llm: Arc<L>,
        tuning: TuningSource,
        services: Services,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
    ) -> std::io::Result<Self> {
        Self::new_with_read_only(log, llm, tuning, services, events, runtime, false)
    }

    pub(super) fn new_with_read_only(
        mut log: SessionLog,
        llm: Arc<L>,
        tuning: TuningSource,
        services: Services,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
        read_only: bool,
    ) -> std::io::Result<Self> {
        log.repair_tail()?;
        let (workspace, provider, model) = log.header();
        let mode = summarize(log.events()).mode;
        let snapshots = log.path().with_file_name("snapshots");
        let parent = crate::session::parent(log.events()).map(str::to_string);
        let worktree = crate::session::worktree(log.events()).cloned();
        let granted_in = worktree.as_ref().map_or_else(|| workspace.clone(), |worktree| PathBuf::from(&worktree.origin));
        let store = SessionStore::of(&log);
        let Services { grants: kept, mcp, skills, search, sandbox, chat, runtime: documents, painter, lsp } = services.clone();
        let chatting = chat.is_some_and(|folder| same_folder(&folder, &workspace));
        let skill_roots: Vec<PathBuf> = skills.into_iter().chain([workspace.join(".odg").join("skills")]).collect();
        Ok(Self {
            inner: Arc::new(Inner {
                id: log.id().to_string(),
                parent,
                children: Mutex::default(),
                store,
                services,
                watch: tokio::sync::watch::Sender::new(Status::Idle),
                workspace,
                granted_in,
                worktree,
                snapshots,
                jobs: Arc::default(),
                kept,
                mcp,
                skill_roots,
                search,
                sandbox,
                documents,
                painter,
                lsp,
                git_directory: std::sync::OnceLock::new(),
                chatting,
                read_only,
                provider: Mutex::new(provider),
                model: Mutex::new(model),
                effort: Mutex::new(crate::session::effort(log.events())),
                llm,
                tuning,
                log: Mutex::new(log),
                state: Mutex::new(State {
                    status: Status::Idle,
                    cancel: CancellationToken::new(),
                    inbox: VecDeque::new(),
                    approvals: HashMap::new(),
                    questions: HashMap::new(),
                    grants: HashSet::new(),
                    mode,
                }),
                events,
                runtime,
            }),
        })
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn status(&self) -> Status {
        self.inner.state.lock().unwrap().status
    }

    /// Id of the session whose agent set this one off as a subagent.
    pub fn parent(&self) -> Option<&str> {
        self.inner.parent.as_deref()
    }

    /// The subagents this session's agent has set off since the app started.
    /// Each is a session of its own, to be shown, messaged, and stopped like any other.
    pub fn children(&self) -> Vec<Session<L>> {
        self.inner.children.lock().unwrap().values().cloned().collect()
    }

    /// The subagent with this session id, if this session's agent set it off.
    pub fn child(&self, id: &str) -> Option<Session<L>> {
        self.inner.children.lock().unwrap().get(id).cloned()
    }

    /// Returns once no turn is running: at once when none is.
    pub async fn wait_idle(&self) {
        // The sender lives as long as the session, so this cannot fail.
        let _ = self.inner.watch.subscribe().wait_for(|status| *status == Status::Idle).await;
    }

    /// Every event logged so far.
    pub fn snapshot(&self) -> Vec<Envelope> {
        self.inner.log.lock().unwrap().events().to_vec()
    }

    /// The log's summary, with the mode as it is now rather than as the last message recorded it.
    pub fn summary(&self) -> SessionSummary {
        let summary = summarize(self.inner.log.lock().unwrap().events());
        SessionSummary { mode: self.mode(), ..summary }
    }

    pub fn mode(&self) -> Mode {
        self.inner.mode()
    }

    /// Move the session to another model, of its provider or of another.
    /// Only between turns: a turn runs on the model it started with. Within
    /// a provider the conversation carries over as it is; to another, what
    /// earlier providers answered goes in a form it takes (see [`crate::session::derive_messages`]).
    pub fn set_model(&self, provider: &str, model: &str) -> Result<(), String> {
        let (provider, model) = (provider.trim(), model.trim());
        if model.is_empty() {
            return Err("name a model".to_string());
        }
        if providers::provider(provider).is_none() {
            return Err(format!("there is no provider `{provider}`"));
        }
        // Held to the end: a turn cannot start between the check and the change.
        let state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            return Err("the agent is working; stop it or wait before changing the model".to_string());
        }
        let moved = *self.inner.provider.lock().unwrap() != provider;
        if moved || *self.inner.model.lock().unwrap() != model {
            self.inner.append(Event::ModelChanged { model: model.to_string(), provider: moved.then(|| provider.to_string()) })?;
            *self.inner.provider.lock().unwrap() = provider.to_string();
            *self.inner.model.lock().unwrap() = model.to_string();
        }
        drop(state);
        Ok(())
    }

    /// Set how hard the model works on this session's requests, from the next one on, in place of the settings' default.
    pub fn set_effort(&self, effort: &str) -> Result<(), String> {
        if !crate::settings::EFFORTS.contains(&effort) {
            return Err(format!("`{effort}` is not one of {}", crate::settings::EFFORTS.join(", ")));
        }
        if self.inner.effort.lock().unwrap().as_deref() != Some(effort) {
            self.inner.append(Event::EffortChanged { effort: effort.to_string() })?;
            *self.inner.effort.lock().unwrap() = Some(effort.to_string());
        }
        Ok(())
    }

    /// Name the session. An empty name gives it its first message back as title.
    pub fn rename(&self, title: &str) -> Result<(), String> {
        let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect();
        self.inner.append(Event::SessionRenamed { title })
    }

    /// Switch modes. The next tool call is decided by the new mode, and the
    /// next message records it, which is when the model learns of plan mode.
    pub fn set_mode(&self, mode: Mode) {
        self.inner.state.lock().unwrap().mode = mode;
    }

    /// Give the agent a message. When idle it starts a turn; during a turn the
    /// message is queued and joins at the next step without interrupting the
    /// current one. Returns true when it was queued.
    pub fn send(&self, text: String) -> bool {
        self.send_with(text, Vec::new(), Vec::new())
    }

    /// [`Session::send`] with pictures and files attached to the message. The
    /// caller has checked the pictures with [`crate::events::check_images`],
    /// and read each file with [`crate::events::attach`].
    pub fn send_with(&self, text: String, images: Vec<Image>, files: Vec<Attached>) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            state.inbox.push_back((text, images, files));
            return true;
        }
        state.status = Status::Running;
        state.cancel = CancellationToken::new();
        let cancel = state.cancel.clone();
        drop(state);
        self.inner.emit_status(Status::Running);
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move { inner.run_turns((text, images, files), cancel).await });
        false
    }

    /// Answer a pending approval. Returns false when nothing was waiting for it.
    pub fn resolve_approval(&self, tool_use_id: &str, decision: Decision) -> bool {
        let sender = self.inner.state.lock().unwrap().approvals.remove(tool_use_id);
        sender.is_some_and(|sender| sender.send(decision).is_ok())
    }

    /// Answer the questions an `ask_user` call is waiting on: a reply to each, in their order, or `None` to
    /// leave it to the agent. Returns false when nothing was waiting for it.
    pub fn answer_questions(&self, tool_use_id: &str, answers: Option<Vec<Reply>>) -> bool {
        let sender = self.inner.state.lock().unwrap().questions.remove(tool_use_id);
        sender.is_some_and(|sender| sender.send(answers).is_ok())
    }

    /// Undo the agent's edits made in `turn` and every later one: each file
    /// goes back to what it held before the first of them. Only between
    /// turns, so no edit is in flight. See [`crate::undo`].
    pub fn undo(&self, turn: u64) -> Result<undo::Outcome, String> {
        // Held to the end: a turn cannot start while files are being restored.
        let state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            return Err("the agent is working; stop it or wait before undoing".to_string());
        }
        let plan = undo::plan(self.inner.log.lock().unwrap().events(), turn);
        if plan.is_empty() {
            return Err(format!("turn {turn} and those after it have no edits to undo"));
        }
        let outcome = undo::apply(&plan, &self.inner.snapshots);
        self.inner.append(Event::FilesRestored { turn, restored: outcome.restored.clone(), skipped: outcome.skipped.clone() })?;
        drop(state);
        Ok(outcome)
    }

    /// Whether `(path, snapshot)` is a recorded deliverable snapshot of this session.
    pub fn has_artifact_snapshot(&self, path: &str, snapshot: &str) -> bool {
        if !undo::valid_snapshot_name(snapshot) {
            return false;
        }
        self.inner.log.lock().unwrap().events().iter().any(|envelope| match &envelope.event {
            Event::ToolResult { tool, is_error: false, meta, .. } => match tool.as_str() {
                "deliver_files" => meta["snapshots"].as_object().and_then(|map| map.get(path)).and_then(Value::as_str) == Some(snapshot),
                "generate_image" | "write_file" | "edit_file" => meta["file"].as_str() == Some(path) && meta["snapshot"].as_str() == Some(snapshot),
                _ => false,
            },
            Event::FilesMade { snapshots, .. } => snapshots.get(path).map(String::as_str) == Some(snapshot),
            Event::ArtifactRestored { file, snapshot: restored, previous, .. } => {
                file == path && (restored == snapshot || previous.as_deref() == Some(snapshot))
            }
            _ => false,
        })
    }

    /// Read the raw bytes of a snapshot belonging to this session (or to an ancestor session when forked).
    pub fn snapshot_bytes(&self, snapshot: &str) -> Result<Vec<u8>, String> {
        if !undo::valid_snapshot_name(snapshot) {
            return Err("invalid snapshot name".to_string());
        }
        match undo::read_snapshot(&self.inner.snapshots, snapshot) {
            Ok(bytes) => return Ok(bytes),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(format!("cannot read snapshot: {error}")),
            Err(_) => {}
        }
        // Forked sessions reference snapshots kept by the session they were forked from.
        let mut source = {
            let log = self.inner.log.lock().unwrap();
            crate::session::forked_from(log.events()).map(|fork| fork.session.clone())
        };
        for _ in 0..16 {
            let Some(id) = source else { break };
            if let Some(dir) = self.inner.store.snapshots_dir(&id) {
                match undo::read_snapshot(&dir, snapshot) {
                    Ok(bytes) => return Ok(bytes),
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(format!("cannot read snapshot: {error}")),
                    Err(_) => {}
                }
            }
            source = self.inner.store.fork_source(&id);
        }
        Err("这个版本的快照文件已经找不到了。".to_string())
    }

    /// Restore a deliverable in the workspace back to one of its snapshotted versions.
    /// If the live file holds unrecorded edits, those edits are snapshotted first so nothing is lost.
    pub fn restore_artifact(&self, path: &str, snapshot: &str, version: u32) -> Result<RestoredArtifact, String> {
        let state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            return Err("分身正在运功，先等它停下来再恢复历史版本。".to_string());
        }
        if !self.has_artifact_snapshot(path, snapshot) {
            return Err("这个历史版本不属于当前会话，不能恢复。".to_string());
        }
        let target = PathBuf::from(path);
        if !tools::inside_workspace(&self.inner.workspace, &target) || tools::in_git_directory(&self.inner.workspace, &target) {
            return Err("只能恢复当前洞府内的交付物（且不能写入 .git 目录）。".to_string());
        }
        let bytes = self.snapshot_bytes(snapshot)?;
        // Snapshot the current live file when it differs from the target and is not already in history.
        let (previous, previous_bytes) = match undo::save_file(&self.inner.snapshots, &target, undo::ARTIFACT_SNAPSHOT_BYTES) {
            Ok(Some((live_snap, live_len))) if live_snap != snapshot && !self.has_artifact_snapshot(path, &live_snap) => (Some(live_snap), Some(live_len)),
            _ => (None, None),
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("无法创建目录： {error}"))?;
        }
        crate::fsutil::write_atomically(&target, &bytes).map_err(|error| format!("无法恢复文件：{error}"))?;
        let shown = tools::display(&self.inner.workspace, &target);
        let backed_up = previous.is_some();
        self.inner.append(Event::ArtifactRestored {
            file: path.to_string(),
            path: shown.clone(),
            version,
            snapshot: snapshot.to_string(),
            bytes: bytes.len() as u64,
            previous,
            previous_bytes,
        })?;
        drop(state);
        Ok(RestoredArtifact { path: shown, file: path.to_string(), version, bytes: bytes.len() as u64, backed_up })
    }

    /// Save a historical deliverable version as a sibling copy in the workspace (e.g. `report.v1.docx`),
    /// never overwriting an existing file. Returns the workspace-relative display path of the created copy.
    pub fn save_artifact_copy(&self, path: &str, snapshot: &str, version: u32) -> Result<String, String> {
        if !self.has_artifact_snapshot(path, snapshot) {
            return Err("这个历史版本不属于当前会话，不能另存。".to_string());
        }
        let target = PathBuf::from(path);
        if !tools::inside_workspace(&self.inner.workspace, &target) || tools::in_git_directory(&self.inner.workspace, &target) {
            return Err("只能将当前洞府内的交付物另存为副本。".to_string());
        }
        let bytes = self.snapshot_bytes(snapshot)?;
        let parent = target.parent().unwrap_or(&self.inner.workspace);
        let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| "file".to_string());
        let ext = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let mut copy = parent.join(format!("{stem}.v{version}{ext}"));
        let mut attempt = 2u32;
        while copy.exists() {
            copy = parent.join(format!("{stem}.v{version}-{attempt}{ext}"));
            attempt += 1;
        }
        if !tools::inside_workspace(&self.inner.workspace, &copy) || tools::in_git_directory(&self.inner.workspace, &copy) {
            return Err("副本路径超出了当前洞府范围。".to_string());
        }
        std::fs::create_dir_all(parent).map_err(|error| format!("无法创建目录：{error}"))?;
        crate::fsutil::write_atomically(&copy, &bytes).map_err(|error| format!("无法保存副本：{error}"))?;
        Ok(tools::display(&self.inner.workspace, &copy))
    }

    /// The ids and command lines of the background commands still running.
    pub fn running_jobs(&self) -> Vec<(String, String)> {
        self.inner.jobs.running()
    }

    /// End every background command now, this session's and its subagents'.
    /// For when the app exits or the session is deleted.
    pub fn kill_jobs(&self) {
        self.inner.jobs.kill_all();
        for child in self.children() {
            child.kill_jobs();
        }
    }

    /// Have the model summarize the conversation now, between turns, as it
    /// does by itself once the conversation passes the limit: the next
    /// request starts from the summary. The session counts as working while
    /// it does, so it can be stopped, and a message sent meanwhile opens a
    /// turn once it is done. Returns how many tokens the conversation held.
    pub async fn compact_now(&self) -> Result<u64, String> {
        let cancel = {
            let mut state = self.inner.state.lock().unwrap();
            if state.status != Status::Idle {
                return Err("the agent is working; stop it or wait before compacting".to_string());
            }
            if !self.inner.compactable() {
                return Err("there is nothing to summarize: the agent has said nothing since the conversation began or was last summarized".to_string());
            }
            state.status = Status::Running;
            state.cancel = CancellationToken::new();
            state.cancel.clone()
        };
        self.inner.emit_status(Status::Running);
        let tuning = self.inner.tuning();
        let estimate = context_estimate(self.inner.log.lock().unwrap().events());
        // Near the window, the conversation itself would not fit beside the question: summarize a transcript of it.
        let near_window = estimate >= tuning.context_window / 10 * 9;
        let outcome = self.inner.compact(&tuning, estimate, near_window, &cancel).await;
        // What was sent meanwhile is answered now; otherwise the session is idle again.
        let next = {
            let mut state = self.inner.state.lock().unwrap();
            let next = state.inbox.pop_front();
            if next.is_none() {
                state.status = Status::Idle;
            } else {
                state.cancel = CancellationToken::new();
            }
            next.map(|message| (message, state.cancel.clone()))
        };
        match next {
            Some((message, cancel)) => {
                let inner = self.inner.clone();
                self.inner.runtime.spawn(async move { inner.run_turns(message, cancel).await });
            }
            None => self.inner.emit_status(Status::Idle),
        }
        match outcome {
            Ok(()) => Ok(estimate),
            Err(LlmError::Cancelled) => Err("stopped before the summary was written; the conversation is as it was".to_string()),
            Err(error) => Err(format!("the conversation could not be summarized, and is as it was: {error}")),
        }
    }

    /// Stop the running turn. The turn ends as `cancelled` once the current operation returns.
    pub fn cancel(&self) {
        self.inner.state.lock().unwrap().cancel.cancel();
    }
}

impl<L: Llm> Inner<L> {
    fn emit(&self, event: UiEvent) {
        // No receiver means no client is attached; the log is still the record.
        let _ = self.events.send(event);
    }

    fn emit_status(&self, status: Status) {
        self.watch.send_replace(status);
        self.emit(UiEvent::Status { session_id: self.id.clone(), status });
    }

    fn set_status(&self, status: Status) {
        self.state.lock().unwrap().status = status;
        self.emit_status(status);
    }

    /// Append to the log and tell the client. A log that cannot be written ends the turn.
    fn append(&self, event: Event) -> Result<(), String> {
        let envelope = self.log.lock().unwrap().append(event).map_err(|error| format!("cannot write the session log: {error}"))?;
        self.emit(UiEvent::Log { session_id: self.id.clone(), envelope });
        Ok(())
    }

    /// The mode the next tool call is decided by. Asking about nothing holds only in a worktree of the
    /// session's own: anywhere else, as in a log left in it and opened where no worktree is, it is auto.
    fn mode(&self) -> Mode {
        match self.state.lock().unwrap().mode {
            Mode::Bypass if self.worktree.is_none() => Mode::Auto,
            mode => mode,
        }
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn provider(&self) -> String {
        self.provider.lock().unwrap().clone()
    }

    /// The settings for a request now: for the provider and model the session is on, at the effort it was set to, if it was.
    fn tuning(&self) -> Tuning {
        let mut tuning = (self.tuning)(&self.provider(), &self.model());
        if let Some(effort) = self.effort.lock().unwrap().clone() {
            tuning.effort = effort;
        }
        tuning
    }

    /// The system prompt of the next request: the general part and the instructions the log holds.
    fn system(&self) -> String {
        let log = self.log.lock().unwrap();
        // A fork of a session a schedule started is the user's: they are there, having just made it.
        let scheduled = crate::session::schedule(log.events()).is_some() && crate::session::forked_from(log.events()).is_none();
        let origin = match (&self.parent, scheduled) {
            (Some(_), _) => Origin::Agent,
            (None, true) => Origin::Schedule,
            (None, false) => Origin::User,
        };
        prompts::build_system_prompt(&self.workspace, instructions(log.events()), skills(log.events()), origin, self.chatting, self.read_only)
    }

    /// Log the project's instructions when they are new, changed, or gone
    /// since they were last logged. Done as a turn starts: an edit to the
    /// file takes effect at the next turn, not in the middle of one.
    fn refresh_instructions(&self) -> Result<(), String> {
        let found = read_instructions(&self.workspace);
        let event = {
            let log = self.log.lock().unwrap();
            let logged = instructions(log.events());
            match (&found, logged) {
                (Some((path, content)), logged) if logged != Some((path.as_str(), content.as_str())) => Some(Event::InstructionsLoaded { path: path.clone(), content: content.clone() }),
                (None, Some((path, _))) => Some(Event::InstructionsLoaded { path: path.to_string(), content: String::new() }),
                _ => None,
            }
        };
        event.map_or(Ok(()), |event| self.append(event))
    }

    /// Log the skills on offer when they differ from the list last logged, as a turn starts.
    fn refresh_skills(&self) -> Result<(), String> {
        let found = crate::skills::listing(&crate::skills::discover(&self.skill_roots));
        let changed = skills(self.log.lock().unwrap().events()) != found.as_slice();
        if changed { self.append(Event::SkillsListed { skills: found }) } else { Ok(()) }
    }

    fn tool_context(&self, cancel: &CancellationToken) -> ToolContext {
        ToolContext {
            workspace: self.workspace.clone(),
            cancel: cancel.clone(),
            snapshots: Some(self.snapshots.clone()),
            jobs: Some(self.jobs.clone()),
            mcp: self.mcp.clone(),
            skills: self.skill_roots.clone(),
            search: self.search_config(),
            sandbox: self.sandbox_now(),
            runtime: self.documents.clone(),
            painter: self.painter_config(),
            vision: providers::vision(&self.provider(), &self.model()),
            lsp: self.lsp.clone(),
        }
    }

    /// Where a picture would be painted right now; `None` when painting is not set up.
    fn painter_config(&self) -> Option<crate::imagegen::ImageConfig> {
        self.painter.as_ref().and_then(|source| source())
    }

    /// Whether this session's commands are confined right now: the user has switched that on, and the system can.
    fn sandboxed(&self) -> bool {
        self.sandbox.as_ref().is_some_and(|source| source().is_some()) && crate::sandbox::support().is_ok()
    }

    /// What confines this session's commands right now, if anything does.
    fn sandbox_now(&self) -> Option<crate::sandbox::Sandbox> {
        let mut extra = self.sandbox.as_ref().and_then(|source| source())?;
        crate::sandbox::support().ok()?;
        // Git writes to the repository's own directory, which a worktree's checkout, or a workspace below the top, does not hold.
        extra.extend(self.git_directory.get_or_init(|| crate::worktree::git_directory(&self.workspace)).clone());
        Some(crate::sandbox::Sandbox { writable: crate::sandbox::roots(&self.workspace, &extra) })
    }

    /// Where a web search would go right now; `None` when searching is not set up.
    fn search_config(&self) -> Option<crate::web::SearchConfig> {
        self.search.as_ref().and_then(|source| source())
    }

    /// Built-in tools withheld from a subagent: setup/configuration, user-facing delivery,
    /// and delegation/user-interaction tools belong to the main session.
    fn withheld_from_subagent(name: &str) -> bool {
        matches!(
            name,
            "delegate"
                | tools::ASK_USER
                | tools::PRESENT_PLAN
                | "install_skill"
                | "add_mcp_server"
                | "language_server"
                | "document_runtime"
                | "deliver_files"
        )
    }

    /// Built-in tools withheld from a conversation session (闲谈): project-engineering tools
    /// have no repository to work on there.
    fn withheld_from_chat(name: &str) -> bool {
        matches!(name, "code_nav" | "language_server" | tools::PRESENT_PLAN | "delegate")
    }

    /// Built-in tools withheld from a `read_only` subagent: anything that writes files,
    /// paints pictures, or manages interactive background processes.
    fn withheld_from_read_only(name: &str) -> bool {
        tools::edits_files(name) || matches!(name, "generate_image" | "command_input" | "command_output" | "stop_command")
    }

    /// Why a `read_only` subagent is refused a tool call, if it is. Runs before `policy::check_in`
    /// so even `Mode::Bypass` cannot override `read_only: true`.
    fn read_only_refusal(&self, name: &str, input: &Value) -> Option<String> {
        if !self.read_only {
            return None;
        }
        if Self::withheld_from_read_only(name) {
            return Some(format!("This subagent is read-only and cannot use `{name}`. Inspect files and report your findings to the main agent."));
        }
        if crate::mcp::split_name(name).is_some() {
            return Some(format!("This subagent is read-only and cannot call external MCP tool `{name}`."));
        }
        if name == "run_command" {
            if input["background"] == true {
                return Some("This subagent is read-only and cannot start background commands.".to_string());
            }
            if input["unsandboxed"] == true {
                return Some("This subagent is read-only and cannot run commands outside the sandbox.".to_string());
            }
            if let Some(dir) = tools::command_cwd(input, &self.workspace)
                && (!tools::inside_workspace(&self.workspace, &dir) || tools::in_git_directory(&self.workspace, &dir))
            {
                return Some("This subagent is read-only and cannot run commands outside the workspace or inside .git.".to_string());
            }
            let command = input["command"].as_str().unwrap_or_default();
            if let Err(why) = crate::safety::check_read_only(command, crate::safety::Dialect::of_this_machine(), &self.workspace) {
                return Some(format!("This subagent is read-only and cannot run this command ({why}). Use read_file, search, find_files, code_nav, or read-only inspection commands."));
            }
        }
        None
    }

    /// The tools of the next request: the built-in ones, then those of the MCP servers connected right now.
    fn tool_specs(&self) -> Vec<Value> {
        let mut specs = tools::specs();
        // Subagents do not set off subagents of their own.
        // Nor do they ask the user: they report to the agent that set them off.
        if self.parent.is_none() {
            specs.push(tools::delegate_spec());
            specs.push(tools::ask_user_spec());
            specs.push(tools::present_plan_spec());
        }
        // Through Zhipu with a GLM key, or else through the session's own provider where it can search.
        if self.search_config().is_some() || crate::provider_search::can_search(&self.provider()) {
            specs.push(tools::search_spec());
        }
        if !self.read_only
            && let Some(config) = self.painter_config()
        {
            specs.push(tools::image_spec(config.painter));
        }
        if let Some(hub) = &self.mcp {
            if !self.read_only {
                specs.extend(hub.specs());
            }
            // What servers offer to be read, while one does.
            let servers: Vec<String> = hub.resource_servers().into_iter().map(|(name, _)| name).collect();
            if !servers.is_empty() {
                specs.extend(tools::mcp_resource_specs(&servers));
            }
        }
        if self.parent.is_some() {
            specs.retain(|spec| !spec["name"].as_str().is_some_and(Self::withheld_from_subagent));
        }
        if self.read_only {
            specs.retain(|spec| !spec["name"].as_str().is_some_and(Self::withheld_from_read_only));
        }
        if self.chatting {
            specs.retain(|spec| !spec["name"].as_str().is_some_and(Self::withheld_from_chat));
        }
        specs
    }

    /// Log what a tool returned, pictures included.
    fn tool_output(&self, tool_use_id: &str, tool: &str, output: ToolOutput) -> Result<(), String> {
        let ToolOutput { content, is_error, meta, images } = output;
        self.append(Event::ToolResult { tool_use_id: tool_use_id.to_string(), tool: tool.to_string(), content, is_error, meta, images })
    }

    fn tool_result(&self, tool_use_id: &str, tool: &str, content: String, is_error: bool, meta: Value) -> Result<(), String> {
        self.append(Event::ToolResult { tool_use_id: tool_use_id.to_string(), tool: tool.to_string(), content, is_error, meta, images: Vec::new() })
    }

    /// Whether the user attached the file at `path` to a message of this conversation.
    fn attached(&self, path: &str) -> bool {
        // As the system names each, so that two spellings of one path are one file.
        let real = |path: &str| std::path::Path::new(path).canonicalize().unwrap_or_else(|_| PathBuf::from(path));
        let wanted = real(path);
        self.log.lock().unwrap().events().iter().any(|envelope| match &envelope.event {
            Event::UserMessage { files, .. } => files.iter().any(|file| real(&file.path) == wanted),
            _ => false,
        })
    }

    /// Run turns until no message is waiting. A message that arrives after the
    /// last step of a turn, or while a turn is being stopped, opens the next
    /// turn instead of being dropped. The inbox is checked and the status set
    /// under the same lock `send` takes, so no message falls between the two.
    async fn run_turns(self: Arc<Self>, mut message: Said, mut cancel: CancellationToken) {
        loop {
            self.run_turn(message, &cancel).await;
            let mut state = self.state.lock().unwrap();
            state.approvals.clear();
            state.questions.clear();
            let Some(next) = state.inbox.pop_front() else {
                state.status = Status::Idle;
                drop(state);
                self.emit_status(Status::Idle);
                return;
            };
            message = next;
            state.cancel = CancellationToken::new();
            cancel = state.cancel.clone();
            let resumed = state.status != Status::Running;
            state.status = Status::Running;
            drop(state);
            if resumed {
                self.emit_status(Status::Running);
            }
        }
    }

    async fn run_turn(&self, (text, images, files): Said, cancel: &CancellationToken) {
        let turn = 1 + self.log.lock().unwrap().events().iter().filter(|envelope| matches!(envelope.event, Event::TurnStart { .. })).count() as u64;
        let started = std::time::SystemTime::now();
        let opened = self
            .append(Event::TurnStart { turn })
            .and_then(|()| self.append(Event::UserMessage { text, steering: false, mode: self.mode(), images, files }))
            .and_then(|()| self.refresh_instructions())
            .and_then(|()| self.refresh_skills());
        let (reason, error) = match opened {
            Ok(()) => match self.run_steps(cancel).await {
                Ok(reason) => (reason, None),
                Err(error) => ("error", Some(error)),
            },
            Err(error) => ("error", Some(error)),
        };
        // The API rejects a conversation in which a tool call has no result, whatever stopped the turn.
        let unanswered = unanswered_tool_uses(self.log.lock().unwrap().events());
        for (tool_use_id, tool) in unanswered {
            let _ = self.tool_result(&tool_use_id, &tool, "The turn stopped before this tool ran.".to_string(), true, Value::Null);
        }
        // What the turn left for the user that no tool told of, such as a document a script wrote: looked for once,
        // off the async threads, since a workspace can be large. Snapshot each found file for deliverable history.
        let root = self.workspace.clone();
        let snapshot_dir = self.snapshots.clone();
        let (files, snapshots, sizes) = tokio::task::spawn_blocking(move || {
            let made = crate::made::since(&root, started);
            let mut files = Vec::with_capacity(made.len());
            let mut snapshots = std::collections::BTreeMap::new();
            let mut sizes = std::collections::BTreeMap::new();
            for path in made {
                let key = path.display().to_string();
                if let Ok(Some((snap, len))) = undo::save_file(&snapshot_dir, &path, undo::ARTIFACT_SNAPSHOT_BYTES) {
                    snapshots.insert(key.clone(), snap);
                    sizes.insert(key.clone(), len);
                }
                files.push(key);
            }
            (files, snapshots, sizes)
        })
        .await
        .unwrap_or_default();
        if !files.is_empty() {
            let _ = self.append(Event::FilesMade { turn, files, snapshots, sizes });
        }
        let _ = self.append(Event::TurnEnd { turn, reason: reason.to_string(), error });
    }

    /// Run steps until the turn is over. Returns the reason it ended, or what went wrong.
    async fn run_steps(&self, cancel: &CancellationToken) -> Result<&'static str, String> {
        // The provider refused the last request as too long, and the conversation was compacted for it.
        let mut overflowed = false;
        for _ in 0..MAX_STEPS {
            let queued: Vec<Said> = self.state.lock().unwrap().inbox.drain(..).collect();
            for (text, images, files) in queued {
                self.append(Event::UserMessage { text, steering: true, mode: self.mode(), images, files })?;
            }
            if cancel.is_cancelled() {
                return Ok("cancelled");
            }

            let tuning = self.tuning();
            let estimate = context_estimate(self.log.lock().unwrap().events());
            if estimate >= tuning.context_limit && self.compactable() {
                let near_window = estimate >= tuning.context_window / 10 * 9;
                // Any other failure is let pass: past the limit is not yet past the window, so
                // the request below may still fit, and the next step tries to compact again.
                if let Err(LlmError::Cancelled) = self.compact(&tuning, estimate, near_window, cancel).await {
                    return Ok("cancelled");
                }
            }

            let request = LlmRequest {
                provider: self.provider(),
                session: self.id.clone(),
                model: self.model(),
                system: self.system(),
                messages: derive_messages(self.log.lock().unwrap().events()),
                tools: self.tool_specs(),
                effort: tuning.effort.clone(),
            };
            let (sender, mut receiver) = unbounded_channel::<StreamDelta>();
            let forward = async {
                while let Some(delta) = receiver.recv().await {
                    self.emit(UiEvent::Stream { session_id: self.id.clone(), delta });
                }
            };
            // `stream` owns the sender, so `forward` ends once the request is over and every fragment is out.
            let (result, ()) = tokio::join!(self.llm.stream(request, sender, cancel.clone()), forward);
            let message = match result {
                Ok(message) => message,
                Err(LlmError::Cancelled) => return Ok("cancelled"),
                // The window is smaller than the limit assumed: compact, and send the step again, once.
                Err(error) if error.context_overflow() && !overflowed && self.compactable() => {
                    overflowed = true;
                    match self.compact(&tuning, estimate, true, cancel).await {
                        Ok(()) => continue,
                        Err(LlmError::Cancelled) => return Ok("cancelled"),
                        Err(failure) => return Err(format!("{error}; and the conversation could not be summarized to fit: {failure}")),
                    }
                }
                Err(error) => return Err(error.to_string()),
            };
            overflowed = false;

            let calls: Vec<(String, String, Value)> = message
                .content
                .iter()
                .filter(|block| block["type"] == "tool_use")
                .map(|block| (block["id"].as_str().unwrap_or_default().to_string(), block["name"].as_str().unwrap_or_default().to_string(), block["input"].clone()))
                .collect();
            self.append(Event::AssistantMessage {
                content: message.content.clone(),
                stop_reason: message.stop_reason.clone(),
                model: message.model.clone(),
                usage: message.usage.clone(),
                tokens: providers::tokens(&self.provider(), &message.usage),
            })?;

            match message.stop_reason.as_deref() {
                // A refusal or a cut-off message can end in the middle of a tool call, so its tools never run.
                Some("refusal") => return Ok("refusal"),
                Some("max_tokens") => return Ok("max_tokens"),
                _ => {}
            }
            if calls.is_empty() {
                if self.state.lock().unwrap().inbox.is_empty() {
                    return Ok("completed");
                }
                continue;
            }

            // Reads that ran while an earlier call of this message waited for approval.
            let mut ran_early: HashSet<String> = HashSet::new();
            // A plan presented in plan mode ends the turn once this message's calls are done.
            let mut presented = false;
            for (index, (id, name, input)) in calls.iter().enumerate() {
                let (id, name, input) = (id.clone(), name.clone(), input.clone());
                if ran_early.contains(&id) {
                    continue;
                }
                if cancel.is_cancelled() {
                    return Ok("cancelled");
                }
                if let Some(raw) = message.invalid_tool_inputs.get(&id) {
                    self.tool_result(&id, &name, json!({ "INVALID_JSON": raw }).to_string(), true, Value::Null)?;
                    continue;
                }
                if self.parent.is_some() && Self::withheld_from_subagent(&name) {
                    let message = match name.as_str() {
                        "delegate" => "A subagent cannot delegate. Do the task yourself.".to_string(),
                        tools::ASK_USER => "A subagent cannot ask the user. Decide yourself, and say in your report what you decided and why.".to_string(),
                        _ => format!("A subagent cannot use `{name}`. Report back to the main agent if it is needed."),
                    };
                    self.tool_result(&id, &name, message, true, json!({ "refused": true, "refusal": "subagent" }))?;
                    continue;
                }
                if let Some(message) = self.read_only_refusal(&name, &input) {
                    self.tool_result(&id, &name, message, true, json!({ "refused": true, "refusal": "read_only" }))?;
                    continue;
                }
                let mut verdict = policy::check_in(self.mode(), &name, &input, &self.workspace, self.sandboxed());
                // Input to a job is shown with the command it goes to, which only the session's jobs know.
                if let Verdict::Ask { reason: "input", detail, .. } = &mut verdict
                    && let Some(command) = self.jobs.command(input["job"].as_str().unwrap_or_default())
                {
                    *detail = format!("{command}\n{detail}");
                }
                if let Verdict::Refuse(message) = verdict {
                    self.tool_result(&id, &name, message.to_string(), true, json!({ "refused": true }))?;
                    continue;
                }
                // A grant of several commands, one per line, holds when each of them is granted.
                let granted = |grant: &String| {
                    grant.lines().all(|part| self.state.lock().unwrap().grants.contains(part) || self.kept.allows(&self.granted_in, part))
                };
                // In auto mode a command the rules cannot pass goes to the guardian first, when there is one.
                let auto = self.mode() == Mode::Auto;
                let verdict = match verdict {
                    Verdict::Ask { grant, .. } if grant.as_ref().is_some_and(granted) => Verdict::Allow,
                    // A file the user attached is read without asking, wherever it is: attaching it said so.
                    Verdict::Ask { reason: "outside_read", ref detail, .. } if name == "read_file" && self.attached(detail) => Verdict::Allow,
                    Verdict::Ask { reason: "command", detail, grant } if auto && tuning.guardian.is_some() => {
                        let guardian = tuning.guardian.clone().unwrap_or_default();
                        match self.review(&id, &detail, &message.content, &guardian, cancel).await {
                            Ok(true) => Verdict::Allow,
                            Ok(false) => Verdict::Ask { reason: "command", detail, grant },
                            Err(()) => return Ok("cancelled"),
                        }
                    }
                    other => other,
                };
                if let Verdict::Ask { reason, detail, grant } = verdict {
                    let (sender, receiver) = oneshot::channel();
                    self.state.lock().unwrap().approvals.insert(id.clone(), sender);
                    // In auto mode nobody may be there to answer: the request is declined by itself in time.
                    let wait = tuning.approval_wait.filter(|_| auto);
                    let expires_at = wait.map(|wait| (chrono::Utc::now() + wait).to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                    self.append(Event::ApprovalRequested {
                        tool_use_id: id.clone(),
                        tool: name.clone(),
                        reason: reason.to_string(),
                        detail,
                        grant: grant.clone(),
                        expires_at,
                    })?;
                    self.set_status(Status::WaitingApproval);
                    // The reads behind this call need no answer from the user, so they run
                    // while the user decides. They stop at an edit: a read after one may
                    // be there to read what the edit wrote.
                    for (later_id, later_name, later_input) in &calls[index + 1..] {
                        if tools::edits_files(later_name) {
                            break;
                        }
                        let reads = matches!(later_name.as_str(), "read_file" | "search" | "find_files") && !message.invalid_tool_inputs.contains_key(later_id);
                        if !reads || policy::check(self.mode(), later_name, later_input, &self.workspace) != Verdict::Allow {
                            continue;
                        }
                        let output = tools::execute(later_name, later_input, &self.tool_context(cancel)).await;
                        self.tool_output(later_id, later_name, output)?;
                        ran_early.insert(later_id.clone());
                    }
                    let (decision, timed_out) = tokio::select! {
                        decision = receiver => (decision.unwrap_or(Decision::Deny), false),
                        () = cancel.cancelled() => return Ok("cancelled"),
                        () = async { match wait { Some(wait) => tokio::time::sleep(wait).await, None => std::future::pending().await } } => (Decision::Deny, true),
                    };
                    if timed_out {
                        // An answer arriving now finds nothing waiting for it.
                        self.state.lock().unwrap().approvals.remove(&id);
                    }
                    self.set_status(Status::Running);
                    self.append(Event::ApprovalResolved { tool_use_id: id.clone(), decision, timed_out })?;
                    match decision {
                        Decision::Deny if timed_out => {
                            let seconds = wait.map_or(0, |wait| wait.as_secs());
                            let content = format!("Nobody answered within {seconds} seconds, so this was declined. Do not try to get around it: carry on without it, and say in your reply what you need the user to approve.");
                            self.tool_result(&id, &name, content, true, Value::Null)?;
                            continue;
                        }
                        Decision::Deny => {
                            self.tool_result(&id, &name, "The user declined this action.".to_string(), true, Value::Null)?;
                            continue;
                        }
                        Decision::AllowSession | Decision::AllowAlways => {
                            // Kept for good when asked and possible; for this run of the app either way,
                            // so a grant that could not be written still holds until restart.
                            for part in grant.iter().flat_map(|grant| grant.lines()) {
                                if decision == Decision::AllowAlways && GrantStore::keeps(part) {
                                    let _ = self.kept.add(&self.granted_in, part);
                                }
                                self.state.lock().unwrap().grants.insert(part.to_string());
                            }
                        }
                        Decision::AllowOnce => {}
                    }
                }
                // `delegate` starts sessions and `ask_user` waits on the user, which only the loop can; every other tool is the executor's.
                let output = match name.as_str() {
                    "delegate" => self.delegate(&id, &input, cancel).await?,
                    tools::ASK_USER => match self.ask_user(&id, &input, &tuning, cancel).await? {
                        Some(output) => output,
                        None => return Ok("cancelled"),
                    },
                    tools::PRESENT_PLAN => {
                        let plan = input["plan"].as_str().map(str::trim).unwrap_or_default();
                        if self.mode() != Mode::Plan {
                            ToolOutput::error("Plan mode is off, so there is no plan to present: carry out the work, or say what you propose in your reply.")
                        } else if plan.is_empty() {
                            ToolOutput::error("`plan` is empty: give the whole plan.")
                        } else {
                            presented = true;
                            ToolOutput::ok("The plan is shown to the user, who decides whether to have it carried out.", json!({ "presented": true }))
                        }
                    }
                    // Without Zhipu's search, the session's own provider searches, in a request of its own.
                    "web_search" if self.search_config().is_none() => match self.search_with_provider(&input, cancel).await {
                        Some(output) => output,
                        None => return Ok("cancelled"),
                    },
                    _ => tools::execute(&name, &input, &self.tool_context(cancel)).await,
                };
                self.tool_output(&id, &name, output)?;
            }
            // The plan is the turn's answer: nothing more is asked of the model, unless the user said something meanwhile.
            if presented && self.state.lock().unwrap().inbox.is_empty() {
                return Ok("completed");
            }
        }
        Ok("step_limit")
    }
}
