//! The agent loop for one session.
//!
//! A turn starts with a user message and runs steps until the model stops
//! calling tools. A step is one model request plus the tools it calls. Every
//! durable fact is appended to the session log before anything depends on it,
//! and each model request is derived from the log alone.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::events::{Decision, Envelope, Event, Image};
use crate::grants::GrantStore;
use crate::llm::{Llm, LlmError, LlmRequest, StreamDelta};
use crate::mcp::McpHub;
use crate::policy::{self, Mode, Verdict};
use crate::providers;
use crate::session::{SessionLog, SessionSummary, context_estimate, derive_messages, instructions, read_instructions, summarize, transcript, unanswered_tool_uses};
use crate::tools::{self, ToolContext};
use crate::undo;

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
pub enum UiEvent {
    Log { session_id: String, envelope: Envelope },
    Stream { session_id: String, delta: StreamDelta },
    Status { session_id: String, status: Status },
}

/// Settings read at the start of every model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuning {
    pub effort: String,
    /// The conversation is compacted once it holds about this many tokens.
    pub context_limit: u64,
    /// How many tokens the model's context window holds. A conversation
    /// close to it is summarized from a shortened transcript, since asking
    /// for the summary would not fit either.
    pub context_window: u64,
}

pub type TuningSource = Arc<dyn Fn() -> Tuning + Send + Sync>;

/// What the model is asked when the conversation is compacted. It follows
/// Anthropic's guidance for client-side compaction; the last sentence matters
/// because the request still carries the tools.
pub const SUMMARIZE: &str = "The conversation has grown too long to continue as it is. Summarize it inside <summary></summary> tags, so that the work can go on from the summary alone, without redoing anything or asking the user again. Be sure to preserve: (1) what the user asked for, decided, ruled out, or set as a preference or constraint, close to their own words; (2) the files you read or changed, by path, and what matters in each; the commands you ran and what they showed; (3) problems that came up and how they were handled; (4) approaches that were tried or set aside, and why; (5) exactly where things stand now and what is done; (6) what is still open or expected next; (7) details that would be hard to reconstruct: names, numbers, exact wording, error messages. Be complete on these even at the cost of length, and keep everything else short. Do not call any tools while writing this summary; respond with text only.";

struct State {
    status: Status,
    cancel: CancellationToken,
    /// Messages sent while a turn runs, each with its pictures; they join the conversation at the next step.
    inbox: VecDeque<(String, Vec<Image>)>,
    approvals: HashMap<String, oneshot::Sender<Decision>>,
    /// Grants the user allowed for the rest of this run of the app.
    grants: HashSet<String>,
    /// What runs without asking; read at every tool call, so a switch applies to the next one.
    mode: Mode,
}

struct Inner<L: Llm> {
    id: String,
    workspace: PathBuf,
    /// Where `edit_file` saves what files held, beside the log.
    snapshots: PathBuf,
    /// Commands left running in the background.
    jobs: Arc<crate::jobs::Jobs>,
    /// Approvals given for good, shared by every session.
    kept: Arc<GrantStore>,
    /// The MCP servers whose tools join the built-in ones; shared by every session.
    mcp: Option<Arc<McpHub>>,
    provider: String,
    model: String,
    llm: Arc<L>,
    tuning: TuningSource,
    log: Mutex<SessionLog>,
    state: Mutex<State>,
    events: UnboundedSender<UiEvent>,
    runtime: tokio::runtime::Handle,
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

/// The system prompt. It changes only when the project's instructions do,
/// so the cached prefix stays valid from one request to the next.
/// `instructions` is the project's own file for agents, as `(name, contents)`.
pub fn system_prompt(workspace: &std::path::Path, instructions: Option<(&str, &str)>) -> String {
    let (_, _, shell) = tools::shell();
    let project = match instructions {
        Some((name, content)) => format!(
            "\n\nThe project keeps instructions for agents in {name}, at the top of the workspace. Follow them; where they and the user disagree, the user decides.\n\n<project-instructions file=\"{name}\">\n{content}\n</project-instructions>"
        ),
        None => String::new(),
    };
    let general = format!(
        "You are OmniDoppelganger (odg), a coding agent that works in the user's workspace on their own machine.\n\
         \n\
         Workspace: {}\n\
         Operating system: {}\n\
         Shell for run_command: {shell}\n\
         \n\
         How you work:\n\
         - Read and search before you change code, and keep each change as small as the task allows.\n\
         - The user sees every tool call, and may have to approve one before it runs. Before a call that changes something, say in a sentence what you are about to do and why.\n\
         - For work of several steps, keep a to-do list with `update_todos` and mark items as you go; the user follows it beside the conversation.\n\
         - After changing code, verify it when the project gives you a way to: build it, run its tests.\n\
         - When something fails, or you could not verify a change, say so plainly.\n\
         - Reply in the language the user writes in.",
        workspace.display(),
        std::env::consts::OS,
    );
    general + &project
}

impl<L: Llm> Session<L> {
    /// Wrap a stored log. A turn that a crash left open is closed first. The
    /// session resumes the mode of its last message.
    pub fn new(
        mut log: SessionLog,
        llm: Arc<L>,
        tuning: TuningSource,
        kept: Arc<GrantStore>,
        mcp: Option<Arc<McpHub>>,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
    ) -> std::io::Result<Self> {
        log.repair_tail()?;
        let (workspace, provider, model) = log.header();
        let mode = summarize(log.events()).mode;
        let snapshots = log.path().with_file_name("snapshots");
        Ok(Self {
            inner: Arc::new(Inner {
                id: log.id().to_string(),
                workspace,
                snapshots,
                jobs: Arc::default(),
                kept,
                mcp,
                provider,
                model,
                llm,
                tuning,
                log: Mutex::new(log),
                state: Mutex::new(State {
                    status: Status::Idle,
                    cancel: CancellationToken::new(),
                    inbox: VecDeque::new(),
                    approvals: HashMap::new(),
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
        self.send_with(text, Vec::new())
    }

    /// [`Session::send`] with pictures attached to the message. The caller
    /// has checked them with [`crate::events::check_images`].
    pub fn send_with(&self, text: String, images: Vec<Image>) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            state.inbox.push_back((text, images));
            return true;
        }
        state.status = Status::Running;
        state.cancel = CancellationToken::new();
        let cancel = state.cancel.clone();
        drop(state);
        self.inner.emit_status(Status::Running);
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move { inner.run_turns((text, images), cancel).await });
        false
    }

    /// Answer a pending approval. Returns false when nothing was waiting for it.
    pub fn resolve_approval(&self, tool_use_id: &str, decision: Decision) -> bool {
        let sender = self.inner.state.lock().unwrap().approvals.remove(tool_use_id);
        sender.is_some_and(|sender| sender.send(decision).is_ok())
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

    /// The ids and command lines of the background commands still running.
    pub fn running_jobs(&self) -> Vec<(String, String)> {
        self.inner.jobs.running()
    }

    /// End every background command now. For when the app exits or the session is deleted.
    pub fn kill_jobs(&self) {
        self.inner.jobs.kill_all();
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

    fn mode(&self) -> Mode {
        self.state.lock().unwrap().mode
    }

    /// The system prompt of the next request: the general part and the instructions the log holds.
    fn system(&self) -> String {
        system_prompt(&self.workspace, instructions(self.log.lock().unwrap().events()))
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

    fn tool_context(&self, cancel: &CancellationToken) -> ToolContext {
        ToolContext {
            workspace: self.workspace.clone(),
            cancel: cancel.clone(),
            snapshots: Some(self.snapshots.clone()),
            jobs: Some(self.jobs.clone()),
            mcp: self.mcp.clone(),
        }
    }

    /// The tools of the next request: the built-in ones, then those of the MCP servers connected right now.
    fn tool_specs(&self) -> Vec<Value> {
        let mut specs = tools::specs();
        if let Some(hub) = &self.mcp {
            specs.extend(hub.specs());
        }
        specs
    }

    fn tool_result(&self, tool_use_id: &str, tool: &str, content: String, is_error: bool, meta: Value) -> Result<(), String> {
        self.append(Event::ToolResult { tool_use_id: tool_use_id.to_string(), tool: tool.to_string(), content, is_error, meta })
    }

    /// Run turns until no message is waiting. A message that arrives after the
    /// last step of a turn, or while a turn is being stopped, opens the next
    /// turn instead of being dropped. The inbox is checked and the status set
    /// under the same lock `send` takes, so no message falls between the two.
    async fn run_turns(self: Arc<Self>, mut message: (String, Vec<Image>), mut cancel: CancellationToken) {
        loop {
            self.run_turn(message, &cancel).await;
            let mut state = self.state.lock().unwrap();
            state.approvals.clear();
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

    async fn run_turn(&self, (text, images): (String, Vec<Image>), cancel: &CancellationToken) {
        let turn = 1 + self.log.lock().unwrap().events().iter().filter(|envelope| matches!(envelope.event, Event::TurnStart { .. })).count() as u64;
        let opened = self
            .append(Event::TurnStart { turn })
            .and_then(|()| self.append(Event::UserMessage { text, steering: false, mode: self.mode(), images }))
            .and_then(|()| self.refresh_instructions());
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
        let _ = self.append(Event::TurnEnd { turn, reason: reason.to_string(), error });
    }

    /// Replace the conversation by a summary the model writes of it. The
    /// request is the conversation itself with the question added, which the
    /// provider's cache makes cheap; when that would not fit the context
    /// window (`from_transcript`, or the provider says so), it is a shortened
    /// plain-text transcript instead.
    async fn compact(&self, tuning: &Tuning, tokens_before: u64, mut from_transcript: bool, cancel: &CancellationToken) -> Result<(), LlmError> {
        loop {
            let text = |text: String| json!({ "type": "text", "text": text });
            let messages = {
                let log = self.log.lock().unwrap();
                if from_transcript {
                    // Half the window, at about three bytes a token.
                    let budget = usize::try_from((tuning.context_window / 2).saturating_mul(3)).unwrap_or(usize::MAX);
                    vec![json!({ "role": "user", "content": [text(format!("{}\n\n{SUMMARIZE}", transcript(log.events(), budget)))] })]
                } else {
                    let mut messages = derive_messages(log.events());
                    // A step starts with the user's turn to speak, so the question joins that message.
                    match messages.last_mut().filter(|message| message["role"] == "user").and_then(|message| message["content"].as_array_mut()) {
                        Some(content) => content.push(text(SUMMARIZE.to_string())),
                        None => messages.push(json!({ "role": "user", "content": [text(SUMMARIZE.to_string())] })),
                    }
                    messages
                }
            };
            let request = LlmRequest {
                provider: self.provider.clone(),
                session: self.id.clone(),
                model: self.model.clone(),
                system: self.system(),
                messages,
                // The same tools keep the cached prefix valid; a transcript has no use for them.
                tools: if from_transcript { Vec::new() } else { self.tool_specs() },
                effort: tuning.effort.clone(),
            };
            // The summary is not the agent speaking to the user, so its fragments are not shown.
            let (sender, mut receiver) = unbounded_channel::<StreamDelta>();
            let discard = async { while receiver.recv().await.is_some() {} };
            let (result, ()) = tokio::join!(self.llm.stream(request, sender, cancel.clone()), discard);
            let turn = match result {
                Err(error) if error.context_overflow() && !from_transcript => {
                    from_transcript = true;
                    continue;
                }
                other => other?,
            };
            let written: Vec<&str> = turn.content.iter().filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str()).collect();
            let written = written.join("\n");
            let summary = match (written.find("<summary>"), written.rfind("</summary>")) {
                (Some(start), Some(end)) if start < end => &written[start + "<summary>".len()..end],
                _ => written.as_str(),
            }
            .trim();
            if summary.is_empty() {
                return Err(LlmError::Protocol("the model wrote no summary".to_string()));
            }
            let event = Event::ContextCompacted { summary: summary.to_string(), tokens_before, tokens: providers::tokens(&self.provider, &turn.usage) };
            return self.append(event).map_err(LlmError::Protocol);
        }
    }

    /// Whether the model has said anything since the last compaction: without that there is nothing to summarize.
    fn compactable(&self) -> bool {
        let log = self.log.lock().unwrap();
        let since = log.events().iter().rposition(|envelope| matches!(envelope.event, Event::ContextCompacted { .. })).unwrap_or(0);
        log.events()[since..].iter().any(|envelope| matches!(envelope.event, Event::AssistantMessage { .. }))
    }

    /// Run steps until the turn is over. Returns the reason it ended, or what went wrong.
    async fn run_steps(&self, cancel: &CancellationToken) -> Result<&'static str, String> {
        // The provider refused the last request as too long, and the conversation was compacted for it.
        let mut overflowed = false;
        for _ in 0..MAX_STEPS {
            let queued: Vec<(String, Vec<Image>)> = self.state.lock().unwrap().inbox.drain(..).collect();
            for (text, images) in queued {
                self.append(Event::UserMessage { text, steering: true, mode: self.mode(), images })?;
            }
            if cancel.is_cancelled() {
                return Ok("cancelled");
            }

            let tuning = (self.tuning)();
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
                provider: self.provider.clone(),
                session: self.id.clone(),
                model: self.model.clone(),
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
                tokens: providers::tokens(&self.provider, &message.usage),
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
                let verdict = policy::check(self.mode(), &name, &input, &self.workspace);
                if let Verdict::Refuse(message) = verdict {
                    self.tool_result(&id, &name, message.to_string(), true, json!({ "refused": true }))?;
                    continue;
                }
                if let Verdict::Ask { reason, detail, grant } = verdict
                    && !grant.as_ref().is_some_and(|grant| self.state.lock().unwrap().grants.contains(grant) || self.kept.allows(&self.workspace, grant))
                {
                    let (sender, receiver) = oneshot::channel();
                    self.state.lock().unwrap().approvals.insert(id.clone(), sender);
                    self.append(Event::ApprovalRequested {
                        tool_use_id: id.clone(),
                        tool: name.clone(),
                        reason: reason.to_string(),
                        detail,
                        grant: grant.clone(),
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
                        self.tool_result(later_id, later_name, output.content, output.is_error, output.meta)?;
                        ran_early.insert(later_id.clone());
                    }
                    let decision = tokio::select! {
                        decision = receiver => decision.unwrap_or(Decision::Deny),
                        () = cancel.cancelled() => return Ok("cancelled"),
                    };
                    self.set_status(Status::Running);
                    self.append(Event::ApprovalResolved { tool_use_id: id.clone(), decision })?;
                    match decision {
                        Decision::Deny => {
                            self.tool_result(&id, &name, "The user declined this action.".to_string(), true, Value::Null)?;
                            continue;
                        }
                        Decision::AllowSession | Decision::AllowAlways => {
                            if let Some(grant) = grant {
                                // Kept for good when asked and possible; for this run of the app either
                                // way, so a grant that could not be written still holds until restart.
                                if decision == Decision::AllowAlways && GrantStore::keeps(&grant) {
                                    let _ = self.kept.add(&self.workspace, &grant);
                                }
                                self.state.lock().unwrap().grants.insert(grant);
                            }
                        }
                        Decision::AllowOnce => {}
                    }
                }
                let output = tools::execute(&name, &input, &self.tool_context(cancel)).await;
                self.tool_result(&id, &name, output.content, output.is_error, output.meta)?;
            }
        }
        Ok("step_limit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::AssistantTurn;
    use crate::session::SessionStore;
    use std::time::Duration;
    use tokio::sync::mpsc::UnboundedReceiver;

    /// Answers each request with the next scripted message and records what it was asked.
    struct ScriptedLlm {
        script: Mutex<VecDeque<Result<AssistantTurn, LlmError>>>,
        requests: Mutex<Vec<LlmRequest>>,
        /// When set, each request waits here until the test lets it through.
        gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl ScriptedLlm {
        fn new(script: Vec<Result<AssistantTurn, LlmError>>, gate: Option<Arc<tokio::sync::Semaphore>>) -> Arc<Self> {
            Arc::new(Self { script: Mutex::new(script.into()), requests: Mutex::new(Vec::new()), gate })
        }
    }

    impl Llm for ScriptedLlm {
        async fn stream(&self, request: LlmRequest, deltas: UnboundedSender<StreamDelta>, cancel: CancellationToken) -> Result<AssistantTurn, LlmError> {
            self.requests.lock().unwrap().push(request);
            if let Some(gate) = &self.gate {
                tokio::select! {
                    permit = gate.acquire() => permit.unwrap().forget(),
                    () = cancel.cancelled() => return Err(LlmError::Cancelled),
                }
            }
            let _ = deltas.send(StreamDelta::TextDelta { index: 0, text: "…".into() });
            self.script.lock().unwrap().pop_front().expect("the script has a message for every request")
        }
    }

    fn says(text: &str) -> Result<AssistantTurn, LlmError> {
        Ok(AssistantTurn { content: vec![json!({ "type": "text", "text": text })], stop_reason: Some("end_turn".into()), model: "scripted".into(), ..Default::default() })
    }

    fn calls(calls: &[(&str, &str, Value)]) -> Result<AssistantTurn, LlmError> {
        Ok(AssistantTurn {
            content: calls.iter().map(|(id, name, input)| json!({ "type": "tool_use", "id": id, "name": name, "input": input })).collect(),
            stop_reason: Some("tool_use".into()),
            model: "scripted".into(),
            ..Default::default()
        })
    }

    struct Fixture {
        _home: tempfile::TempDir,
        /// The compaction limit and context window the session reads; none unless a test sets them.
        limits: Arc<Mutex<(u64, u64)>>,
        workspace: tempfile::TempDir,
        session: Session<ScriptedLlm>,
        llm: Arc<ScriptedLlm>,
        events: UnboundedReceiver<UiEvent>,
    }

    fn fixture(llm: Arc<ScriptedLlm>) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let log = SessionStore::new(home.path()).create(workspace.path(), "anthropic", "scripted").unwrap();
        let (sender, events) = unbounded_channel();
        let limits = Arc::new(Mutex::new((u64::MAX, u64::MAX)));
        let read = limits.clone();
        let tuning = move || {
            let (context_limit, context_window) = *read.lock().unwrap();
            Tuning { effort: "high".to_string(), context_limit, context_window }
        };
        let kept = Arc::new(GrantStore::new(home.path().join("grants.json")));
        let session = Session::new(log, llm.clone(), Arc::new(tuning), kept, None, sender, tokio::runtime::Handle::current()).unwrap();
        Fixture { _home: home, limits, workspace, session, llm, events }
    }

    impl Fixture {
        /// Wait for the first event the predicate accepts.
        async fn until(&mut self, mut accept: impl FnMut(&UiEvent) -> bool) -> UiEvent {
            loop {
                let event = tokio::time::timeout(Duration::from_secs(20), self.events.recv()).await.expect("the event arrives in time").expect("the session is alive");
                if accept(&event) {
                    return event;
                }
            }
        }

        async fn idle(&mut self) {
            self.until(|event| matches!(event, UiEvent::Status { status: Status::Idle, .. })).await;
        }

        async fn approval(&mut self) -> String {
            match self.until(|event| matches!(event, UiEvent::Log { envelope, .. } if matches!(envelope.event, Event::ApprovalRequested { .. }))).await {
                UiEvent::Log { envelope: Envelope { event: Event::ApprovalRequested { tool_use_id, .. }, .. }, .. } => tool_use_id,
                _ => unreachable!(),
            }
        }

        /// Event type names in log order, without the header.
        fn kinds(&self) -> Vec<String> {
            self.session.snapshot().iter().skip(1).map(|envelope| serde_json::to_value(envelope).unwrap()["type"].as_str().unwrap().to_string()).collect()
        }

        fn results(&self) -> Vec<(String, bool, String)> {
            self.session
                .snapshot()
                .into_iter()
                .filter_map(|envelope| match envelope.event {
                    Event::ToolResult { tool_use_id, is_error, content, .. } => Some((tool_use_id, is_error, content)),
                    _ => None,
                })
                .collect()
        }

        fn end(&self) -> (String, Option<String>) {
            match self.session.snapshot().pop().map(|envelope| envelope.event) {
                Some(Event::TurnEnd { reason, error, .. }) => (reason, error),
                other => panic!("the log ends with {other:?}, not turn/end"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_allowed_tools_and_feeds_results_back() {
        let mut fixture = fixture(ScriptedLlm::new(vec![calls(&[("t1", "read_file", json!({ "path": "a.txt" }))]), says("done")], None));
        std::fs::write(fixture.workspace.path().join("a.txt"), "hello\n").unwrap();
        assert!(!fixture.session.send("read a.txt".into()));
        fixture.idle().await;

        assert_eq!(fixture.kinds(), ["turn/start", "user/message", "assistant/message", "tool/result", "assistant/message", "turn/end"]);
        assert_eq!(fixture.results(), [("t1".to_string(), false, "     1\thello".to_string())]);
        assert_eq!(fixture.end(), ("completed".to_string(), None));

        let requests = fixture.llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].effort, "high");
        assert_eq!((requests[0].provider.as_str(), requests[0].session.as_str()), ("anthropic", fixture.session.id()));
        assert_eq!(requests[0].tools.len(), tools::specs().len());
        assert!(requests[0].system.contains(&fixture.workspace.path().display().to_string()));
        // The second request carries the whole conversation: user, assistant tool call, tool result.
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(requests[1].messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(requests[0].system, requests[1].system);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn waits_for_approval_and_honours_each_decision() {
        let echo = |id: &str| (id.to_string(), "run_command".to_string(), json!({ "command": "echo ok" }));
        let script = ["t1", "t2", "t3"].map(|id| {
            let (id, name, input) = echo(id);
            calls(&[(id.as_str(), name.as_str(), input)])
        });
        let mut fixture = fixture(ScriptedLlm::new(script.into_iter().chain([says("done")]).collect(), None));
        fixture.session.send("run it three times".into());

        // First call: declined, so the command never runs.
        let first = fixture.approval().await;
        assert_eq!(fixture.session.status(), Status::WaitingApproval);
        assert!(fixture.session.summary().awaiting_approval);
        assert!(fixture.session.resolve_approval(&first, Decision::Deny));
        // Second call: allowed for the session, which also covers the third.
        let second = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&second, Decision::AllowSession));
        assert!(!fixture.session.resolve_approval("unknown", Decision::AllowOnce));
        fixture.idle().await;

        let results = fixture.results();
        assert_eq!(results[0], ("t1".to_string(), true, "The user declined this action.".to_string()));
        assert!(!results[1].1 && results[1].2.trim() == "ok");
        assert!(!results[2].1 && results[2].2.trim() == "ok");
        assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "approval/requested").count(), 2);
        assert_eq!(fixture.end().0, "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_command_allowed_for_good_never_asks_again_in_that_workspace() {
        let echo = |id: &str| calls(&[(id, "run_command", json!({ "command": "echo ok" }))]);
        let edit = calls(&[("t3", "edit_file", json!({ "path": "../outside.txt", "old_string": "", "new_string": "x" }))]);
        let mut fixture = fixture(ScriptedLlm::new(vec![echo("t1"), says("done"), echo("t2"), edit, says("done")], None));
        fixture.session.send("run it".into());
        let asked = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&asked, Decision::AllowAlways));
        fixture.idle().await;
        let store = GrantStore::new(fixture._home.path().join("grants.json"));
        assert!(store.allows(fixture.workspace.path(), "command:echo ok"));

        // A session opened later on the same workspace, as after a restart, does not ask for it.
        let home = fixture._home.path().to_path_buf();
        let log = SessionStore::new(&home).create(fixture.workspace.path(), "anthropic", "scripted").unwrap();
        let (sender, events) = unbounded_channel();
        let tuning = Arc::new(|| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX });
        let later = Session::new(log, fixture.llm.clone(), tuning, Arc::new(GrantStore::new(home.join("grants.json"))), None, sender, tokio::runtime::Handle::current()).unwrap();
        fixture.session = later;
        fixture.events = events;
        fixture.session.send("again".into());
        // The command runs; the edit outside the workspace asks, and "always" cannot be kept for it.
        let outside = fixture.approval().await;
        assert_eq!(fixture.results()[0], ("t2".to_string(), false, "ok".to_string()));
        assert!(fixture.session.resolve_approval(&outside, Decision::AllowAlways));
        fixture.idle().await;
        assert_eq!(store.all().values().map(|kept| kept.len()).sum::<usize>(), 1);
        let _ = std::fs::remove_file(fixture.workspace.path().parent().unwrap().join("outside.txt"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn allowing_a_write_into_git_for_the_session_still_asks_next_time() {
        let hook = |id: &str| calls(&[(id, "edit_file", json!({ "path": ".git/hooks/pre-commit", "old_string": "", "new_string": "echo hi\n" }))]);
        let mut fixture = fixture(ScriptedLlm::new(vec![hook("t1"), hook("t2"), says("done")], None));
        fixture.session.send("add a hook".into());

        let first = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&first, Decision::AllowSession));
        // The second write asks again: a session-wide approval does not cover `.git`.
        let second = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&second, Decision::Deny));
        fixture.idle().await;

        let results = fixture.results();
        assert!(!results[0].1, "{}", results[0].2);
        assert_eq!(results[1], ("t2".to_string(), true, "The user declined this action.".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_mode_decides_what_asks_and_each_message_records_it() {
        let edit = |id: &str| calls(&[(id, "edit_file", json!({ "path": "a.txt", "old_string": "", "new_string": "hi\n" }))]);
        let echo = calls(&[("t3", "run_command", json!({ "command": "echo ok" }))]);
        let mut fixture = fixture(ScriptedLlm::new(vec![edit("t1"), says("my plan"), edit("t2"), says("asked"), echo, says("checked")], None));

        // Plan: the edit is refused without asking, and the model is told plan mode is on.
        fixture.session.set_mode(Mode::Plan);
        fixture.session.send("plan it".into());
        fixture.idle().await;
        // Manual: the same edit waits for the user, who declines.
        fixture.session.set_mode(Mode::Manual);
        fixture.session.send("do it".into());
        let asked = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&asked, Decision::Deny));
        fixture.idle().await;
        // Auto: a command that passes the safety check runs without asking.
        fixture.session.set_mode(Mode::Auto);
        fixture.session.send("check".into());
        fixture.idle().await;

        let results = fixture.results();
        assert_eq!(results[0], ("t1".to_string(), true, policy::PLAN_REFUSAL.to_string()));
        assert_eq!(results[1], ("t2".to_string(), true, "The user declined this action.".to_string()));
        assert_eq!((results[2].0.as_str(), results[2].1, results[2].2.trim()), ("t3", false, "ok"));
        assert!(!fixture.workspace.path().join("a.txt").exists());
        assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "approval/requested").count(), 1);

        let modes: Vec<Mode> = fixture
            .session
            .snapshot()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::UserMessage { mode, .. } => Some(mode),
                _ => None,
            })
            .collect();
        assert_eq!(modes, [Mode::Plan, Mode::Manual, Mode::Auto]);
        assert_eq!(fixture.llm.requests.lock().unwrap()[0].messages[0]["content"][0]["text"], crate::session::PLAN_ON);
        assert_eq!(fixture.session.summary().mode, Mode::Auto);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_run_while_an_earlier_call_waits_for_approval_but_not_past_an_edit() {
        let mut fixture = fixture(ScriptedLlm::new(
            vec![
                calls(&[
                    ("t1", "run_command", json!({ "command": "echo one" })),
                    ("t2", "read_file", json!({ "path": "a.txt" })),
                    ("t3", "search", json!({ "pattern": "hello" })),
                    ("t4", "edit_file", json!({ "path": "a.txt", "old_string": "hello", "new_string": "changed" })),
                    ("t5", "read_file", json!({ "path": "a.txt" })),
                ]),
                says("done"),
            ],
            None,
        ));
        std::fs::write(fixture.workspace.path().join("a.txt"), "hello\n").unwrap();
        fixture.session.send("go".into());
        let asked = fixture.approval().await;

        // The command still waits, and the two reads behind it have their results already.
        fixture.until(|event| matches!(event, UiEvent::Log { envelope, .. } if matches!(&envelope.event, Event::ToolResult { tool_use_id, .. } if tool_use_id == "t3"))).await;
        assert_eq!(fixture.session.status(), Status::WaitingApproval);
        let early: Vec<String> = fixture.results().into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(early, ["t2", "t3"]);

        assert!(fixture.session.resolve_approval(&asked, Decision::AllowOnce));
        fixture.idle().await;
        let results = fixture.results();
        assert_eq!(results.iter().map(|(id, is_error, _)| (id.as_str(), *is_error)).collect::<Vec<_>>(), [
            ("t2", false),
            ("t3", false),
            ("t1", false),
            ("t4", false),
            ("t5", false),
        ]);
        // The read behind the edit waited for it, and saw what it wrote.
        assert!(results[0].2.contains("hello") && results[4].2.contains("changed"), "{results:?}");
        assert_eq!(fixture.end().0, "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn follows_the_projects_instructions_file_and_notices_when_it_changes() {
        let mut fixture = fixture(ScriptedLlm::new(vec![says("one"), says("two"), says("three"), says("four")], None));
        let file = fixture.workspace.path().join("AGENTS.md");
        let loaded = |fixture: &Fixture| -> Vec<(String, String)> {
            let events = fixture.session.snapshot().into_iter();
            events.filter_map(|envelope| match envelope.event { Event::InstructionsLoaded { path, content } => Some((path, content)), _ => None }).collect()
        };

        // No file: nothing is logged, and the prompt says nothing of instructions.
        fixture.session.send("first".into());
        fixture.idle().await;
        assert!(loaded(&fixture).is_empty());
        assert!(!fixture.llm.requests.lock().unwrap()[0].system.contains("project-instructions"));

        // The file is read as the next turn starts, and logged once while it stays the same.
        std::fs::write(&file, "Run `cargo fmt` before finishing.\r\n").unwrap();
        fixture.session.send("second".into());
        fixture.idle().await;
        fixture.session.send("third".into());
        fixture.idle().await;
        assert_eq!(loaded(&fixture), [("AGENTS.md".to_string(), "Run `cargo fmt` before finishing.".to_string())]);
        assert_eq!(fixture.kinds()[4..7], ["turn/start", "user/message", "instructions/loaded"]);
        {
            let requests = fixture.llm.requests.lock().unwrap();
            let expected = "<project-instructions file=\"AGENTS.md\">\nRun `cargo fmt` before finishing.\n</project-instructions>";
            assert!(requests[1].system.ends_with(expected), "{}", requests[1].system);
            // Unchanged instructions leave the prompt, and so the provider's cache, as they were.
            assert_eq!(requests[1].system, requests[2].system);
        }

        // Gone again: that is logged too, and the prompt drops them.
        std::fs::remove_file(&file).unwrap();
        fixture.session.send("fourth".into());
        fixture.idle().await;
        assert_eq!(loaded(&fixture).last(), Some(&("AGENTS.md".to_string(), String::new())));
        assert!(!fixture.llm.requests.lock().unwrap()[3].system.contains("project-instructions"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_carries_its_images_to_the_model_whether_it_opens_a_turn_or_joins_one() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut fixture = fixture(ScriptedLlm::new(vec![says("I see a login form."), says("And a chart.")], Some(gate.clone())));
        let image = |data: &str| Image { media_type: "image/png".into(), data: data.into() };
        assert!(!fixture.session.send_with("what is this?".into(), vec![image("Zmlyc3Q=")]));
        while fixture.llm.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // A second picture sent while the first request is in flight joins at the next step.
        assert!(fixture.session.send_with(String::new(), vec![image("c2Vjb25k")]));
        gate.add_permits(2);
        fixture.idle().await;

        let requests = fixture.llm.requests.lock().unwrap();
        let block = |data: &str| json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": data } });
        assert_eq!(requests[0].messages[0]["content"], json!([block("Zmlyc3Q="), { "type": "text", "text": "what is this?" }]));
        // The picture sent alone is a message of that one block.
        assert_eq!(requests[1].messages[2]["content"], json!([block("c2Vjb25k")]));
        assert_eq!(fixture.end().0, "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_mcp_servers_tools_join_the_request_and_ask_before_each_call() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let hub = Arc::new(McpHub::new(home.path().join("mcp.json"), tokio::runtime::Handle::current()));
        hub.insert("echo", crate::mcp::testing::echo_server().await);
        let echo = |id: &str, text: &str| calls(&[(id, "mcp__echo__echo", json!({ "text": text }))]);
        let llm = ScriptedLlm::new(vec![echo("t1", "one"), echo("t2", "two"), echo("t3", "three"), says("done")], None);
        let log = SessionStore::new(home.path()).create(workspace.path(), "anthropic", "scripted").unwrap();
        let (sender, events) = unbounded_channel();
        let tuning = Arc::new(|| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX });
        let kept = Arc::new(GrantStore::new(home.path().join("grants.json")));
        let session = Session::new(log, llm.clone(), tuning, kept.clone(), Some(hub), sender, tokio::runtime::Handle::current()).unwrap();
        // Auto mode lets safe commands through, but never a server's tool.
        session.set_mode(Mode::Auto);
        let mut fixture = Fixture { _home: home, limits: Arc::default(), workspace, session, llm, events };
        fixture.session.send("use the echo tool".into());

        // Declined: the server is never called. Then allowed for good, which covers the third call.
        let first = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&first, Decision::Deny));
        let second = fixture.approval().await;
        assert!(fixture.session.resolve_approval(&second, Decision::AllowAlways));
        fixture.idle().await;

        let results = fixture.results();
        assert_eq!(results[0], ("t1".to_string(), true, "The user declined this action.".to_string()));
        assert!(!results[1].1 && results[1].2.starts_with("two\n"), "{:?}", results[1]);
        assert!(!results[2].1 && results[2].2.starts_with("three\n"));
        assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "approval/requested").count(), 2);
        assert!(kept.allows(fixture.workspace.path(), "mcp:mcp__echo__echo"));

        // The model was offered the server's tools after the built-in ones.
        let requests = fixture.llm.requests.lock().unwrap();
        let names: Vec<&str> = requests[0].tools.iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(names[names.len() - 2..], ["mcp__echo__echo", "mcp__echo__fail_now"]);
        assert_eq!(names.len(), tools::specs().len() + 2);
    }

    /// The texts of a request's messages, one list per message.
    fn texts(request: &LlmRequest) -> Vec<Vec<String>> {
        let of = |message: &Value| message["content"].as_array().unwrap().iter().filter_map(|block| block["text"].as_str().map(str::to_string)).collect();
        request.messages.iter().map(of).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compacts_the_conversation_once_it_passes_the_limit() {
        let mut first = says("first answer").unwrap();
        first.usage = json!({ "input_tokens": 500, "output_tokens": 20 });
        let mut fixture = fixture(ScriptedLlm::new(vec![Ok(first), says("<summary>The user said one; I answered.</summary>"), says("second answer")], None));
        fixture.session.send("one".into());
        fixture.idle().await;
        assert_eq!(fixture.session.snapshot().iter().filter(|envelope| matches!(envelope.event, Event::ContextCompacted { .. })).count(), 0);

        // The conversation holds 520 tokens, which is now past the limit.
        *fixture.limits.lock().unwrap() = (100, 1_000_000);
        fixture.session.send("two".into());
        fixture.idle().await;

        assert_eq!(fixture.kinds()[4..], ["turn/start", "user/message", "context/compacted", "assistant/message", "turn/end"]);
        let requests = fixture.llm.requests.lock().unwrap();
        // The summary is asked for at the end of the conversation as it was, tools included.
        assert_eq!(texts(&requests[1]), [vec!["one".to_string()], vec!["first answer".into()], vec!["two".into(), SUMMARIZE.into()]]);
        assert_eq!(requests[1].tools.len(), tools::specs().len());
        // The step then starts over from the summary and the message not yet answered.
        let summary = format!("{}\n\nThe user said one; I answered.", crate::session::COMPACTED);
        assert_eq!(texts(&requests[2]), [vec![summary, "two".to_string()]]);
        let compacted = fixture.session.snapshot().into_iter().find_map(|envelope| match envelope.event {
            Event::ContextCompacted { summary, tokens_before, .. } => Some((summary, tokens_before)),
            _ => None,
        });
        assert_eq!(compacted, Some(("The user said one; I answered.".to_string(), 521)));
        assert_eq!(fixture.end().0, "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compacts_from_a_transcript_and_retries_when_the_provider_says_the_conversation_is_too_long() {
        let too_long = || Err(LlmError::Api { status: Some(400), kind: "invalid_request_error".into(), message: "prompt is too long: 210000 tokens > 200000 maximum".into(), retryable: false });
        let mut fixture = fixture(ScriptedLlm::new(vec![too_long(), says("first answer"), too_long(), says("The user said one."), says("second answer"), too_long(), says("More was said."), too_long()], None));
        // Nothing to summarize yet: the error is the turn's.
        fixture.session.send("one".into());
        fixture.idle().await;
        assert_eq!(fixture.end().0, "error");
        fixture.session.send("one again".into());
        fixture.idle().await;

        fixture.session.send("two".into());
        fixture.idle().await;
        assert_eq!(fixture.end().0, "completed");
        {
            let requests = fixture.llm.requests.lock().unwrap();
            // The summary request is a transcript in one message, with no tools.
            let asked = &texts(&requests[3])[0][0];
            assert!(requests[3].tools.is_empty() && requests[3].messages.len() == 1);
            assert!(asked.starts_with("USER:\none\n\nUSER:\none again\n\nASSISTANT:\nfirst answer\n\nUSER:\ntwo") && asked.ends_with(SUMMARIZE), "{asked}");
            assert_eq!(texts(&requests[4]), [vec![format!("{}\n\nThe user said one.", crate::session::COMPACTED), "two".to_string()]]);
        }

        // Still too long after compacting for it: the step is retried once, not forever.
        fixture.session.send("three".into());
        fixture.idle().await;
        assert_eq!(fixture.end().0, "error");
        assert_eq!(fixture.llm.requests.lock().unwrap().len(), 8);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn undoes_edits_from_a_turn_on_and_leaves_files_changed_since_alone() {
        let edit = |id: &str, path: &str, old: &str, new: &str| (id.to_string(), "edit_file".to_string(), json!({ "path": path, "old_string": old, "new_string": new }));
        let turn = |edits: Vec<(String, String, Value)>| calls(&edits.iter().map(|(id, name, input)| (id.as_str(), name.as_str(), input.clone())).collect::<Vec<_>>());
        let mut fixture = fixture(ScriptedLlm::new(
            vec![
                turn(vec![edit("t1", "a.txt", "one", "two")]),
                says("edited a"),
                turn(vec![edit("t2", "a.txt", "two", "three"), edit("t3", "new.txt", "", "made\n"), edit("t4", "b.txt", "x", "y")]),
                says("edited more"),
                says("understood"),
            ],
            None,
        ));
        let file = |name: &str| fixture.workspace.path().join(name);
        let (a, b, new) = (file("a.txt"), file("b.txt"), file("new.txt"));
        std::fs::write(&a, "one\n").unwrap();
        std::fs::write(&b, "x\n").unwrap();
        fixture.session.send("first".into());
        fixture.idle().await;
        fixture.session.send("second".into());
        fixture.idle().await;
        assert_eq!((std::fs::read_to_string(&a).unwrap().as_str(), new.exists()), ("three\n", true));

        // The user changes b.txt after the agent did; undoing turn 2 must not overwrite that.
        std::fs::write(&b, "mine\n").unwrap();
        let outcome = fixture.session.undo(2).unwrap();
        assert_eq!(outcome, undo::Outcome { restored: vec!["a.txt".into(), "new.txt".into()], skipped: vec!["b.txt".into()] });
        // a.txt is back to what turn 1 left, the created file is gone, b.txt is the user's.
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "two\n");
        assert!(!new.exists());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "mine\n");

        // What is undone is not undone twice; turn 1's edit still can be. b.txt stays the user's.
        assert_eq!(fixture.session.undo(1).unwrap(), undo::Outcome { restored: vec!["a.txt".into()], skipped: vec!["b.txt".into()] });
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one\n");
        assert_eq!(fixture.session.undo(1).unwrap(), undo::Outcome { restored: vec![], skipped: vec!["b.txt".into()] });

        // The model hears of it with the next message.
        fixture.session.send("third".into());
        fixture.idle().await;
        let requests = fixture.llm.requests.lock().unwrap();
        let told = texts(requests.last().unwrap()).pop().unwrap().remove(0);
        assert!(told.starts_with("[The user undid your file changes from turn 2 on.") && told.contains("a.txt, new.txt") && told.contains("after your last edit: b.txt"), "{told}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_during_an_approval_answers_every_tool_call() {
        let mut fixture = fixture(ScriptedLlm::new(
            vec![calls(&[("t1", "run_command", json!({ "command": "echo one" })), ("t2", "read_file", json!({ "path": "a.txt" }))])],
            None,
        ));
        fixture.session.send("go".into());
        fixture.approval().await;
        fixture.session.cancel();
        fixture.idle().await;

        assert_eq!(fixture.end().0, "cancelled");
        let results = fixture.results();
        // The read ran while the command waited (a.txt does not exist); the command never ran.
        assert_eq!(results.iter().map(|(id, is_error, _)| (id.as_str(), *is_error)).collect::<Vec<_>>(), [("t2", true), ("t1", true)]);
        assert!(!fixture.session.summary().turn_open);
        // The log is a valid conversation again: the next turn can start.
        assert_eq!(derive_messages(&fixture.session.snapshot()).len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_sent_during_a_turn_joins_at_the_next_step() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut fixture = fixture(ScriptedLlm::new(vec![says("first answer"), says("second answer")], Some(gate.clone())));
        assert!(!fixture.session.send("start".into()));
        while fixture.llm.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The first request is in flight; this message must not start a second turn.
        assert!(fixture.session.send("and one more thing".into()));
        gate.add_permits(2);
        fixture.idle().await;

        assert_eq!(fixture.kinds(), ["turn/start", "user/message", "assistant/message", "user/message", "assistant/message", "turn/end"]);
        let steering: Vec<bool> = fixture
            .session
            .snapshot()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::UserMessage { steering, .. } => Some(steering),
                _ => None,
            })
            .collect();
        assert_eq!(steering, [false, true]);
        assert_eq!(fixture.llm.requests.lock().unwrap()[1].messages.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_queued_when_the_turn_stops_opens_the_next_turn() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut fixture = fixture(ScriptedLlm::new(vec![says("on it")], Some(gate.clone())));
        fixture.session.send("start".into());
        while fixture.llm.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(fixture.session.send("do this instead".into()));
        fixture.session.cancel();
        while fixture.llm.requests.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        gate.add_permits(1);
        fixture.idle().await;

        assert_eq!(fixture.kinds(), [
            "turn/start", "user/message", "turn/end", "turn/start", "user/message", "assistant/message", "turn/end"
        ]);
        let texts: Vec<String> = fixture
            .session
            .snapshot()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::UserMessage { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["start", "do this instead"]);
        assert_eq!(fixture.end().0, "completed");
        assert_eq!(fixture.session.status(), Status::Idle);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn never_runs_tools_of_a_cut_off_or_malformed_message() {
        let cut_off = Ok(AssistantTurn {
            content: vec![json!({ "type": "tool_use", "id": "t1", "name": "edit_file", "input": {} })],
            stop_reason: Some("max_tokens".into()),
            model: "scripted".into(),
            ..Default::default()
        });
        let mut fixture = fixture(ScriptedLlm::new(vec![cut_off], None));
        fixture.session.send("write a big file".into());
        fixture.idle().await;
        assert_eq!(fixture.end().0, "max_tokens");
        assert_eq!(fixture.results(), [("t1".to_string(), true, "The turn stopped before this tool ran.".to_string())]);

        let mut malformed = calls(&[("t2", "edit_file", json!({}))]).unwrap();
        malformed.invalid_tool_inputs.insert("t2".into(), "{\"path\": \"a".into());
        let mut fixture = self::fixture(ScriptedLlm::new(vec![Ok(malformed), says("retrying")], None));
        fixture.session.send("edit".into());
        fixture.idle().await;
        assert_eq!(fixture.results(), [("t2".to_string(), true, "{\"INVALID_JSON\":\"{\\\"path\\\": \\\"a\"}".to_string())]);
        assert_eq!(fixture.end().0, "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_request_ends_the_turn_with_the_error_and_the_next_turn_works() {
        let mut fixture = fixture(ScriptedLlm::new(vec![Err(LlmError::MissingKey), says("hello")], None));
        fixture.session.send("hi".into());
        fixture.idle().await;
        assert_eq!(fixture.end(), ("error".to_string(), Some("no API key is configured".to_string())));

        fixture.session.send("hi again".into());
        fixture.idle().await;
        assert_eq!(fixture.end().0, "completed");
        let turns: Vec<u64> = fixture
            .session
            .snapshot()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::TurnStart { turn } => Some(turn),
                _ => None,
            })
            .collect();
        assert_eq!(turns, [1, 2]);
    }
}
