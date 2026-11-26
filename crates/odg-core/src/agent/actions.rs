//! Agent loop operations that run separate LLM requests or wait on subagents and user answers:
//! subagent delegation, `ask_user`, provider-backed search, context compaction, and guardian review.

use serde_json::{Value, json};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::events::{Event, Reply};
use crate::llm::{Llm, LlmError, LlmRequest, StreamDelta};
use crate::policy::Mode;
use crate::providers;
use crate::session::{NewSession, derive_messages, transcript};
use crate::tools::{self, ToolOutput};

use super::prompts::{GUARDIAN, SUMMARIZE, verdict};
use super::{Inner, Session, Status, Tuning};

/// How long a stopped turn waits for its subagents to stop too before it ends without them.
const SUBAGENT_STOP: std::time::Duration = std::time::Duration::from_secs(5);

/// Pick the last completed turn to keep verbatim across compaction when there are at least two
/// completed turns since the previous compaction and that last turn fits comfortably (`<= window / 4`).
/// Returns `(event_index_of_kept_turn_start, kept_turn_number)`.
fn choose_retained_turn(events: &[crate::events::Envelope], context_window: u64) -> Option<(usize, u64)> {
    let since = events
        .iter()
        .rposition(|envelope| matches!(envelope.event, Event::ContextCompacted { .. }))
        .map_or(0, |idx| idx + 1);
    let mut completed: Vec<(u64, usize, u64)> = Vec::new();
    let mut active: Option<(u64, usize, bool, u64)> = None;
    let guess = |s: &str| s.len() as u64 / 3;
    for (offset, env) in events[since..].iter().enumerate() {
        let abs_idx = since + offset;
        match &env.event {
            Event::TurnStart { turn } => {
                active = Some((*turn, abs_idx, false, 0));
            }
            Event::UserMessage { text, images, files, .. } => {
                if let Some((_, _, _, ref mut est)) = active {
                    *est += guess(text) + 1500 * images.len() as u64 + files.iter().map(|f| guess(&crate::events::attached_text(f))).sum::<u64>();
                }
            }
            Event::AssistantMessage { content, .. } => {
                if let Some((_, _, ref mut spoke, ref mut est)) = active {
                    *spoke = true;
                    *est += guess(&Value::Array(content.clone()).to_string());
                }
            }
            Event::ToolResult { content, images, .. } => {
                if let Some((_, _, _, ref mut est)) = active {
                    *est += guess(content) + 1500 * images.len() as u64;
                }
            }
            Event::TurnEnd { reason, .. } => {
                if let Some((turn, start_idx, spoke, est)) = active.take()
                    && spoke
                    && reason == "completed"
                {
                    completed.push((turn, start_idx, est));
                }
            }
            _ => {}
        }
    }
    if completed.len() < 2 {
        return None;
    }
    let &(turn, start_idx, est) = completed.last()?;
    (est <= context_window / 4).then_some((start_idx, turn))
}

impl<L: Llm> Inner<L> {
    /// Start a subagent: a session of its own in this workspace, on this
    /// session's provider, model, and effort, in its mode and with what the
    /// user has allowed it so far.
    fn spawn(&self, name: &str, read_only: bool) -> Result<Session<L>, String> {
        let provider = self.provider();
        let new = NewSession { workspace: &self.workspace, provider: &provider, model: &self.model(), parent: Some(&self.id), worktree: self.worktree.clone(), schedule: None, aside: None };
        let log = self.store.create_with(new).map_err(|error| error.to_string())?;
        let child = Session::new_with_read_only(log, self.llm.clone(), self.tuning.clone(), self.services.clone(), self.events.clone(), self.runtime.clone(), read_only).map_err(|error| error.to_string())?;
        let (mode, grants) = {
            let state = self.state.lock().unwrap();
            (state.mode, state.grants.clone())
        };
        {
            let mut state = child.inner.state.lock().unwrap();
            state.mode = mode;
            state.grants = grants;
        }
        child.rename(name)?;
        if let Some(effort) = self.effort.lock().unwrap().clone() {
            child.set_effort(&effort)?;
        }
        self.children.lock().unwrap().insert(child.id().to_string(), child.clone());
        Ok(child)
    }

    /// Put the agent's questions to the user and wait for the answers: for good where the user is
    /// taken to be there, for `approval_wait` in auto and bypass mode, where nobody may be. `None` when
    /// the turn was stopped meanwhile.
    pub(super) async fn ask_user(&self, tool_use_id: &str, input: &Value, tuning: &Tuning, cancel: &CancellationToken) -> Result<Option<ToolOutput>, String> {
        if self.parent.is_some() {
            return Ok(Some(ToolOutput::error("A subagent cannot ask the user. Decide yourself, and say in your report what you decided and why.")));
        }
        let questions = match tools::questions(input) {
            Ok(questions) => questions,
            Err(message) => return Ok(Some(ToolOutput::error(message))),
        };
        let (sender, receiver) = oneshot::channel();
        self.state.lock().unwrap().questions.insert(tool_use_id.to_string(), sender);
        let wait = tuning.approval_wait.filter(|_| matches!(self.mode(), Mode::Auto | Mode::Bypass));
        let expires_at = wait.map(|wait| (chrono::Utc::now() + wait).to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        self.append(Event::QuestionAsked { tool_use_id: tool_use_id.to_string(), questions: questions.clone(), expires_at })?;
        self.set_status(Status::WaitingApproval);
        let (answers, timed_out) = tokio::select! {
            answers = receiver => (answers.unwrap_or(None), false),
            () = cancel.cancelled() => return Ok(None),
            () = async { match wait { Some(wait) => tokio::time::sleep(wait).await, None => std::future::pending().await } } => (None, true),
        };
        if timed_out {
            // An answer arriving now finds nothing waiting for it.
            self.state.lock().unwrap().questions.remove(tool_use_id);
        }
        self.set_status(Status::Running);
        let declined = answers.is_none() && !timed_out;
        let mut answers = answers.unwrap_or_default();
        answers.resize(questions.len(), Reply::default());
        let answers = if answers.iter().all(Reply::is_empty) { Vec::new() } else { answers };
        self.append(Event::QuestionAnswered { tool_use_id: tool_use_id.to_string(), answers: answers.clone(), declined, timed_out })?;
        let content = tools::answered(&questions, &answers, declined, timed_out.then(|| wait.map_or(0, |wait| wait.as_secs())));
        Ok(Some(ToolOutput { content, is_error: false, meta: Value::Null, images: Vec::new() }))
    }

    /// Search the web through the session's own provider and model, with the provider's search tool, in a request of
    /// its own; see [`crate::provider_search`]. What it cost is kept with the result. `None` when the turn was stopped.
    pub(super) async fn search_with_provider(&self, input: &Value, cancel: &CancellationToken) -> Option<ToolOutput> {
        let (provider, model) = (self.provider(), self.model());
        let query = input["query"].as_str().unwrap_or_default();
        let request = match crate::provider_search::request(&provider, &self.id, &model, query, input) {
            Ok(request) => request,
            Err(message) => return Some(ToolOutput::error(message)),
        };
        let kind = providers::kind_id(&provider);
        let (sender, mut receiver) = unbounded_channel::<StreamDelta>();
        // What the searching model writes is not the agent's: it is not shown as it comes.
        let discard = async { while receiver.recv().await.is_some() {} };
        let (result, ()) = tokio::join!(self.llm.stream(request, sender, cancel.clone()), discard);
        match result {
            Err(LlmError::Cancelled) => None,
            Err(error) => Some(ToolOutput::error(format!("The search through {kind} failed: {error}"))),
            Ok(turn) => {
                let mut output = crate::provider_search::results(&turn, query.trim(), input, kind, &model);
                if let Some(tokens) = providers::tokens(&provider, &turn.usage) {
                    output.meta["tokens"] = json!(tokens);
                }
                Some(output)
            }
        }
    }

    /// Run `delegate`: set off a subagent for each task, wait until all have
    /// finished, and hand back what each reported. Stopping this turn stops them.
    pub(super) async fn delegate(&self, tool_use_id: &str, input: &Value, cancel: &CancellationToken) -> Result<ToolOutput, String> {
        if self.parent.is_some() {
            return Ok(ToolOutput::error("A subagent cannot delegate. Do the task yourself."));
        }
        let tasks = match tools::delegate_tasks(input) {
            Ok(tasks) => tasks,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        // Each task with its subagent, or why it could not be started.
        let mut started: Vec<(String, Result<Session<L>, String>)> = Vec::new();
        for task in tasks {
            let child = self.spawn(&task.name, task.read_only);
            if let Ok(child) = &child {
                self.append(Event::SubagentStarted { tool_use_id: tool_use_id.to_string(), session: child.id().to_string(), name: task.name.clone() })?;
                child.send(task.task);
            }
            started.push((task.name, child));
        }
        let children: Vec<&Session<L>> = started.iter().filter_map(|(_, child)| child.as_ref().ok()).collect();
        let all_idle = async {
            for child in &children {
                child.wait_idle().await;
            }
        };
        tokio::select! {
            () = all_idle => {}
            () = cancel.cancelled() => {
                for child in &children {
                    child.cancel();
                }
                // Each stops once what it is doing returns; one that will not is left to finish on its own.
                let stopped = async {
                    for child in &children {
                        child.wait_idle().await;
                    }
                };
                let _ = tokio::time::timeout(SUBAGENT_STOP, stopped).await;
            }
        }

        let mut parts = Vec::new();
        let mut agents = Vec::new();
        let mut finished = 0;
        for (name, child) in &started {
            match child {
                Ok(child) => {
                    let (said, reason, error) = crate::session::report(&child.snapshot());
                    let note = match reason.as_str() {
                        "completed" => {
                            finished += 1;
                            String::new()
                        }
                        "running" => "[This subagent was still working when the turn was stopped.]\n".to_string(),
                        other => format!("[This subagent stopped before finishing ({}); what follows is the last thing it said.]\n", error.as_deref().unwrap_or(other)),
                    };
                    let said = if said.trim().is_empty() { "(it said nothing)".to_string() } else { said };
                    parts.push(format!("<subagent name=\"{name}\">\n{note}{said}\n</subagent>"));
                    agents.push(json!({ "session": child.id(), "name": name, "reason": reason }));
                }
                Err(error) => {
                    parts.push(format!("<subagent name=\"{name}\">\n[This subagent could not be started: {error}]\n</subagent>"));
                    agents.push(json!({ "session": Value::Null, "name": name, "reason": "error" }));
                }
            }
        }
        Ok(ToolOutput { content: parts.join("\n\n"), is_error: finished == 0, meta: json!({ "agents": agents }), images: Vec::new() })
    }

    /// Replace the conversation by a summary the model writes of it. The
    /// request is the conversation itself with the question added, which the
    /// provider's cache makes cheap; when that would not fit the context
    /// window (`from_transcript`, or the provider says so), it is a shortened
    /// plain-text transcript instead.
    pub(super) async fn compact(
        &self,
        tuning: &Tuning,
        tokens_before: u64,
        mut from_transcript: bool,
        focus: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<(), LlmError> {
        let focus_clean = focus.map(str::trim).filter(|s| !s.is_empty());
        let question = match focus_clean {
            Some(extra) => format!("{SUMMARIZE}\n\nAdditional user instructions — be sure to preserve the following details in your summary:\n{extra}"),
            None => SUMMARIZE.to_string(),
        };
        loop {
            let text = |text: String| json!({ "type": "text", "text": text });
            let (messages, kept_from_turn) = {
                let log = self.log.lock().unwrap();
                let all_events = log.events();
                let retained = if from_transcript {
                    None
                } else {
                    choose_retained_turn(all_events, tuning.context_window)
                };
                let slice = match retained {
                    Some((split_idx, _)) => &all_events[..split_idx],
                    None => all_events,
                };
                let kept_turn = retained.map(|(_, t)| t);
                if from_transcript {
                    // Half the window, at about three bytes a token.
                    let budget = usize::try_from((tuning.context_window / 2).saturating_mul(3)).unwrap_or(usize::MAX);
                    (
                        vec![json!({ "role": "user", "content": [text(format!("{}\n\n{question}", transcript(slice, budget)))] })],
                        None,
                    )
                } else {
                    let mut messages = derive_messages(slice);
                    // A step starts with the user's turn to speak, so the question joins that message.
                    match messages.last_mut().filter(|message| message["role"] == "user").and_then(|message| message["content"].as_array_mut()) {
                        Some(content) => content.push(text(question.clone())),
                        None => messages.push(json!({ "role": "user", "content": [text(question.clone())] })),
                    }
                    (messages, kept_turn)
                }
            };
            let request = LlmRequest {
                provider: self.provider(),
                session: self.id.clone(),
                model: self.model(),
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
            let event = Event::ContextCompacted {
                summary: summary.to_string(),
                tokens_before,
                focus: focus_clean.map(str::to_string),
                kept_from_turn,
                tokens: providers::tokens(&self.provider(), &turn.usage),
            };
            return self.append(event).map_err(LlmError::Protocol);
        }
    }

    /// Ask the guardian whether a command can run without the user. What it is shown: the user's
    /// messages, what the agent said before the call, the command; never a tool's output. Its verdict
    /// is logged. `Ok(false)` leaves the command to the user, as does a guardian that cannot be
    /// asked; `Err` when the turn was stopped meanwhile.
    pub(super) async fn review(&self, tool_use_id: &str, command: &str, said: &[Value], guardian: &str, cancel: &CancellationToken) -> Result<bool, ()> {
        let clip = |text: &str, most: usize| -> String {
            let count = text.chars().count();
            if count <= most { text.to_string() } else { format!("…{}", text.chars().skip(count - most).collect::<String>()) }
        };
        let asked: Vec<String> = {
            let log = self.log.lock().unwrap();
            let messages: Vec<String> = log.events().iter().filter_map(|envelope| if let Event::UserMessage { text, .. } = &envelope.event { Some(clip(text, 2_000)) } else { None }).collect();
            messages[messages.len().saturating_sub(8)..].to_vec()
        };
        let intent: Vec<&str> = said.iter().filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str()).collect();
        let (_, _, shell) = tools::shell();
        let prompt = format!(
            "<workspace>{}</workspace>\n<shell>{shell}</shell>\n<user-messages>\n{}\n</user-messages>\n<agent-said>\n{}\n</agent-said>\n<command>\n{command}\n</command>",
            self.workspace.display(),
            asked.join("\n---\n"),
            clip(&intent.join("\n"), 2_000),
        );
        let request = LlmRequest {
            provider: self.provider(),
            session: self.id.clone(),
            model: guardian.to_string(),
            system: GUARDIAN.to_string(),
            messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": prompt }] })],
            tools: Vec::new(),
            effort: "low".to_string(),
        };
        let (sender, mut receiver) = unbounded_channel::<StreamDelta>();
        let discard = async { while receiver.recv().await.is_some() {} };
        let (result, ()) = tokio::join!(self.llm.stream(request, sender, cancel.clone()), discard);
        let (allowed, reason, said, failed, tokens) = match result {
            Err(LlmError::Cancelled) => return Err(()),
            Err(error) => (false, error.to_string(), String::new(), true, None),
            Ok(turn) => {
                let written: Vec<&str> = turn.content.iter().filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str()).collect();
                let written = written.join("\n");
                let (allowed, reason) = verdict(&written);
                // The first line as written, so a verdict read wrongly can be seen in the log.
                let said: String = written.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().chars().take(200).collect();
                (allowed, reason, said, false, providers::tokens(&self.provider(), &turn.usage))
            }
        };
        let event = Event::GuardianReviewed { tool_use_id: tool_use_id.to_string(), model: guardian.to_string(), allowed, reason, verdict: said, failed, tokens };
        if self.append(event).is_err() {
            return Ok(false);
        }
        Ok(allowed)
    }

    /// Whether the model has said anything since the last compaction: without that there is nothing to summarize.
    pub(super) fn compactable(&self) -> bool {
        let log = self.log.lock().unwrap();
        let since = log.events().iter().rposition(|envelope| matches!(envelope.event, Event::ContextCompacted { .. })).unwrap_or(0);
        log.events()[since..].iter().any(|envelope| matches!(envelope.event, Event::AssistantMessage { .. }))
    }
}
