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

use crate::events::{Decision, Envelope, Event};
use crate::llm::{Llm, LlmError, LlmRequest, StreamDelta};
use crate::policy::{self, Mode, Verdict};
use crate::session::{SessionLog, SessionSummary, derive_messages, summarize, unanswered_tool_uses};
use crate::tools::{self, ToolContext};

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
pub type EffortSource = Arc<dyn Fn() -> String + Send + Sync>;

struct State {
    status: Status,
    cancel: CancellationToken,
    /// Messages sent while a turn runs; they join the conversation at the next step.
    inbox: VecDeque<String>,
    approvals: HashMap<String, oneshot::Sender<Decision>>,
    /// Grants the user allowed for the rest of this run of the app.
    grants: HashSet<String>,
    /// What runs without asking; read at every tool call, so a switch applies to the next one.
    mode: Mode,
}

struct Inner<L: Llm> {
    id: String,
    workspace: PathBuf,
    provider: String,
    model: String,
    llm: Arc<L>,
    effort: EffortSource,
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

/// The system prompt. It is the same for every request of a session, so the cached prefix stays valid.
pub fn system_prompt(workspace: &std::path::Path) -> String {
    let (_, _, shell) = tools::shell();
    format!(
        "You are OmniDoppelganger (odg), a coding agent that works in the user's workspace on their own machine.\n\
         \n\
         Workspace: {}\n\
         Operating system: {}\n\
         Shell for run_command: {shell}\n\
         \n\
         How you work:\n\
         - Read and search before you change code, and keep each change as small as the task allows.\n\
         - The user sees every tool call, and may have to approve one before it runs. Before a call that changes something, say in a sentence what you are about to do and why.\n\
         - After changing code, verify it when the project gives you a way to: build it, run its tests.\n\
         - When something fails, or you could not verify a change, say so plainly.\n\
         - Reply in the language the user writes in.",
        workspace.display(),
        std::env::consts::OS,
    )
}

impl<L: Llm> Session<L> {
    /// Wrap a stored log. A turn that a crash left open is closed first. The
    /// session resumes the mode of its last message.
    pub fn new(
        mut log: SessionLog,
        llm: Arc<L>,
        effort: EffortSource,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
    ) -> std::io::Result<Self> {
        log.repair_tail()?;
        let (workspace, provider, model) = log.header();
        let mode = summarize(log.events()).mode;
        Ok(Self {
            inner: Arc::new(Inner {
                id: log.id().to_string(),
                workspace,
                provider,
                model,
                llm,
                effort,
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

    /// Switch modes. The next tool call is decided by the new mode, and the
    /// next message records it, which is when the model learns of plan mode.
    pub fn set_mode(&self, mode: Mode) {
        self.inner.state.lock().unwrap().mode = mode;
    }

    /// Give the agent a message. When idle it starts a turn; during a turn the
    /// message is queued and joins at the next step without interrupting the
    /// current one. Returns true when it was queued.
    pub fn send(&self, text: String) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        if state.status != Status::Idle {
            state.inbox.push_back(text);
            return true;
        }
        state.status = Status::Running;
        state.cancel = CancellationToken::new();
        let cancel = state.cancel.clone();
        drop(state);
        self.inner.emit_status(Status::Running);
        let inner = self.inner.clone();
        self.inner.runtime.spawn(async move { inner.run_turns(text, cancel).await });
        false
    }

    /// Answer a pending approval. Returns false when nothing was waiting for it.
    pub fn resolve_approval(&self, tool_use_id: &str, decision: Decision) -> bool {
        let sender = self.inner.state.lock().unwrap().approvals.remove(tool_use_id);
        sender.is_some_and(|sender| sender.send(decision).is_ok())
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

    fn tool_result(&self, tool_use_id: &str, tool: &str, content: String, is_error: bool, meta: Value) -> Result<(), String> {
        self.append(Event::ToolResult { tool_use_id: tool_use_id.to_string(), tool: tool.to_string(), content, is_error, meta })
    }

    /// Run turns until no message is waiting. A message that arrives after the
    /// last step of a turn, or while a turn is being stopped, opens the next
    /// turn instead of being dropped. The inbox is checked and the status set
    /// under the same lock `send` takes, so no message falls between the two.
    async fn run_turns(self: Arc<Self>, mut text: String, mut cancel: CancellationToken) {
        loop {
            self.run_turn(text, &cancel).await;
            let mut state = self.state.lock().unwrap();
            state.approvals.clear();
            let Some(next) = state.inbox.pop_front() else {
                state.status = Status::Idle;
                drop(state);
                self.emit_status(Status::Idle);
                return;
            };
            text = next;
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

    async fn run_turn(&self, text: String, cancel: &CancellationToken) {
        let turn = 1 + self.log.lock().unwrap().events().iter().filter(|envelope| matches!(envelope.event, Event::TurnStart { .. })).count() as u64;
        let opened = self.append(Event::TurnStart { turn }).and_then(|()| self.append(Event::UserMessage { text, steering: false, mode: self.mode() }));
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

    /// Run steps until the turn is over. Returns the reason it ended, or what went wrong.
    async fn run_steps(&self, cancel: &CancellationToken) -> Result<&'static str, String> {
        for _ in 0..MAX_STEPS {
            let queued: Vec<String> = self.state.lock().unwrap().inbox.drain(..).collect();
            for text in queued {
                self.append(Event::UserMessage { text, steering: true, mode: self.mode() })?;
            }
            if cancel.is_cancelled() {
                return Ok("cancelled");
            }

            let request = LlmRequest {
                provider: self.provider.clone(),
                session: self.id.clone(),
                model: self.model.clone(),
                system: system_prompt(&self.workspace),
                messages: derive_messages(self.log.lock().unwrap().events()),
                tools: tools::specs(),
                effort: (self.effort)(),
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
                Err(error) => return Err(error.to_string()),
            };

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
                    && !grant.as_ref().is_some_and(|grant| self.state.lock().unwrap().grants.contains(grant))
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
                        if later_name == "edit_file" {
                            break;
                        }
                        let reads = matches!(later_name.as_str(), "read_file" | "search") && !message.invalid_tool_inputs.contains_key(later_id);
                        if !reads || policy::check(self.mode(), later_name, later_input, &self.workspace) != Verdict::Allow {
                            continue;
                        }
                        let context = ToolContext { workspace: self.workspace.clone(), cancel: cancel.clone() };
                        let output = tools::execute(later_name, later_input, &context).await;
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
                        Decision::AllowSession => {
                            if let Some(grant) = grant {
                                self.state.lock().unwrap().grants.insert(grant);
                            }
                        }
                        Decision::AllowOnce => {}
                    }
                }
                let context = ToolContext { workspace: self.workspace.clone(), cancel: cancel.clone() };
                let output = tools::execute(&name, &input, &context).await;
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
        let session = Session::new(log, llm.clone(), Arc::new(|| "high".to_string()), sender, tokio::runtime::Handle::current()).unwrap();
        Fixture { _home: home, workspace, session, llm, events }
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
        assert_eq!(requests[0].tools.len(), 4);
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
