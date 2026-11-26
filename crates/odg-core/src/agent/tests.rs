use super::*;
use crate::llm::AssistantTurn;
use crate::session::{NewSession, SessionStore};
use std::path::Path;
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
    /// The guardian and how long a request for approval waits in auto mode; neither unless a test sets them.
    guard: Arc<Mutex<(Option<String>, Option<Duration>)>>,
    workspace: tempfile::TempDir,
    session: Session<ScriptedLlm>,
    llm: Arc<ScriptedLlm>,
    events: UnboundedReceiver<UiEvent>,
}

fn fixture(llm: Arc<ScriptedLlm>) -> Fixture {
    fixture_started_by(llm, None)
}

/// A session the user started, or one started by the schedule of the given name.
fn fixture_started_by(llm: Arc<ScriptedLlm>, schedule: Option<&str>) -> Fixture {
    fixture_with(llm, schedule, false)
}

/// The same, with the command sandbox switched on when `sandbox`.
fn fixture_with(llm: Arc<ScriptedLlm>, schedule: Option<&str>, sandbox: bool) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let new = NewSession { workspace: workspace.path(), provider: "anthropic", model: "scripted", parent: None, worktree: None, schedule, aside: None };
    let log = SessionStore::new(home.path()).create_with(new).unwrap();
    let (sender, events) = unbounded_channel();
    let limits = Arc::new(Mutex::new((u64::MAX, u64::MAX)));
    let guard: Arc<Mutex<(Option<String>, Option<Duration>)>> = Arc::default();
    let (read, read_guard) = (limits.clone(), guard.clone());
    let tuning = move |_: &str, _: &str| {
        let (context_limit, context_window) = *read.lock().unwrap();
        let (guardian, approval_wait) = read_guard.lock().unwrap().clone();
        Tuning { effort: "high".to_string(), context_limit, context_window, guardian, approval_wait }
    };
    let kept = Arc::new(GrantStore::new(home.path().join("grants.json")));
    let services = Services { grants: kept, mcp: None, skills: Some(home.path().join("skills")), search: None, sandbox: sandbox.then(|| Arc::new(|| Some(Vec::new())) as crate::sandbox::SandboxSource), chat: None, runtime: None, painter: None, lsp: None };
    let session = Session::new(log, llm.clone(), Arc::new(tuning), services, sender, tokio::runtime::Handle::current()).unwrap();
    Fixture { _home: home, limits, guard, workspace, session, llm, events }
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test(flavor = "multi_thread")]
async fn with_the_sandbox_on_commands_run_confined_and_leaving_it_asks() {
    if let Err(why) = crate::sandbox::support() {
        eprintln!("not run: {why}");
        return;
    }
    let script = vec![calls(&[("t1", "run_command", json!({ "command": "ls" })), ("t2", "run_command", json!({ "command": "ls", "unsandboxed": true }))]), says("done")];
    let mut fixture = fixture_with(ScriptedLlm::new(script, None), None, true);
    // Auto mode runs `ls` without asking. The same command asking to leave the sandbox is the user's to decide.
    fixture.session.set_mode(Mode::Auto);
    fixture.session.send("list the files".into());
    assert_eq!(fixture.approval().await, "t2");
    fixture.session.resolve_approval("t2", Decision::AllowOnce);
    fixture.idle().await;
    let log = fixture.session.snapshot();
    let asked: Vec<&str> = log.iter().filter_map(|envelope| if let Event::ApprovalRequested { reason, .. } = &envelope.event { Some(reason.as_str()) } else { None }).collect();
    assert_eq!(asked, ["unsandboxed"]);
    let confined: Vec<(&str, bool)> = log.iter().filter_map(|envelope| if let Event::ToolResult { tool_use_id, meta, .. } = &envelope.event { Some((tool_use_id.as_str(), meta["sandboxed"] == true)) } else { None }).collect();
    assert_eq!(confined, [("t1", true), ("t2", false)]);
}

/// A command auto mode would ask about: it writes its output to a file.
fn writes() -> Result<AssistantTurn, LlmError> {
    calls(&[("t1", "run_command", json!({ "command": "echo hi > out.txt" }))])
}

fn reviews(fixture: &Fixture) -> Vec<(bool, String)> {
    fixture.session.snapshot().into_iter().filter_map(|envelope| if let Event::GuardianReviewed { allowed, reason, .. } = envelope.event { Some((allowed, reason)) } else { None }).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn in_auto_mode_the_guardian_lets_a_command_run_without_asking() {
    let script = vec![writes(), says("ALLOW\n只写一个测试文件。"), says("done")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    *fixture.guard.lock().unwrap() = (Some("guard-model".into()), None);
    fixture.session.set_mode(Mode::Auto);
    fixture.session.send("write hi to out.txt".into());
    fixture.idle().await;
    assert!(!fixture.kinds().contains(&"approval/requested".to_string()));
    assert_eq!(reviews(&fixture), [(true, "只写一个测试文件。".to_string())]);
    assert!(fixture.workspace.path().join("out.txt").is_file());
    // It was asked as a guardian: its own model and prompt, no tools, and the user's words and the command.
    let requests = fixture.llm.requests.lock().unwrap();
    let review = &requests[1];
    let shown = review.messages[0]["content"][0]["text"].as_str().unwrap();
    assert_eq!((review.model.as_str(), review.system.as_str(), review.tools.len()), ("guard-model", GUARDIAN, 0));
    assert!(shown.contains("write hi to out.txt") && shown.contains("echo hi > out.txt"), "{shown}");
}

#[test]
fn the_guardian_verdict_is_read_however_it_is_written() {
    let read = |written: &str| verdict(written);
    let ok = |reason: &str| (true, reason.to_string());
    let ask = |reason: &str| (false, reason.to_string());
    // As told, and the ways a model strays from it while still meaning ALLOW.
    assert_eq!(read("ALLOW\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("allow.\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("**ALLOW**\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("`ALLOWED`\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("\n\n  ALLOW  \n\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("允许\n只读检索。"), ok("只读检索。"));
    assert_eq!(read("放行。\n只读检索。"), ok("只读检索。"));
    // The reason on the verdict's own line.
    assert_eq!(read("ALLOW — 只读检索。"), ok("只读检索。"));
    assert_eq!(read("允许：只读检索。"), ok("只读检索。"));
    // Anything else is left to the user.
    assert_eq!(read("ASK\n要推送。"), ask("要推送。"));
    assert_eq!(read("不允许\n要推送。"), ask("要推送。"));
    assert_eq!(read("ALLOWING\n要推送。"), ask("要推送。"));
    assert_eq!(read("Verdict: ALLOW\n只读。"), ask("只读。"));
    assert_eq!(read(""), ask(""));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guardian_that_answers_in_chinese_still_lets_the_command_run() {
    let script = vec![writes(), says("允许\n只写一个测试文件。"), says("done")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    *fixture.guard.lock().unwrap() = (Some("guard-model".into()), None);
    fixture.session.set_mode(Mode::Auto);
    fixture.session.send("write hi to out.txt".into());
    fixture.idle().await;
    assert!(!fixture.kinds().contains(&"approval/requested".to_string()));
    assert!(fixture.workspace.path().join("out.txt").is_file());
    // The verdict line is kept as it was written.
    let kept: Vec<(bool, String)> = fixture.session.snapshot().into_iter().filter_map(|envelope| if let Event::GuardianReviewed { allowed, verdict, .. } = envelope.event { Some((allowed, verdict)) } else { None }).collect();
    assert_eq!(kept, [(true, "允许".to_string())]);
    // And it is told to write the verdict in English whatever the user's language.
    assert!(GUARDIAN.contains("in English capitals whatever language the user writes in"));
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_guardian_will_not_pass_goes_to_the_user_and_is_declined_in_time() {
    let script = vec![writes(), says("ASK\n要写文件，先问问。"), says("ok, without it")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    *fixture.guard.lock().unwrap() = (Some("guard-model".into()), Some(Duration::from_millis(300)));
    fixture.session.set_mode(Mode::Auto);
    fixture.session.send("write hi to out.txt".into());
    fixture.approval().await;
    fixture.idle().await;
    assert_eq!(reviews(&fixture), [(false, "要写文件，先问问。".to_string())]);
    let log = fixture.session.snapshot();
    assert!(log.iter().any(|envelope| matches!(&envelope.event, Event::ApprovalRequested { expires_at: Some(_), .. })));
    assert!(log.iter().any(|envelope| matches!(envelope.event, Event::ApprovalResolved { decision: Decision::Deny, timed_out: true, .. })));
    let (_, failed, told) = fixture.results().remove(0);
    assert!(failed && told.contains("Nobody answered"), "{told}");
    assert!(!fixture.workspace.path().join("out.txt").exists());
}

/// An `ask_user` call of two questions: which of two plans, and anything to add in words.
fn asks() -> Result<AssistantTurn, LlmError> {
    calls(&[(
        "q1",
        "ask_user",
        json!({ "questions": [
            { "header": "方案", "question": "用哪个方案？", "options": [{ "label": "方案一 (推荐)", "description": "改得少" }, { "label": "方案二" }] },
            { "header": "补充", "question": "还有什么要求？", "kind": "text" }
        ] }),
    )])
}

async fn question(fixture: &mut Fixture) -> (String, Vec<crate::events::Question>, Option<String>) {
    match fixture.until(|event| matches!(event, UiEvent::Log { envelope, .. } if matches!(envelope.event, Event::QuestionAsked { .. }))).await {
        UiEvent::Log { envelope: Envelope { event: Event::QuestionAsked { tool_use_id, questions, expires_at }, .. }, .. } => (tool_use_id, questions, expires_at),
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_the_user_and_hands_the_answers_back_in_the_same_turn() {
    let mut fixture = fixture(ScriptedLlm::new(vec![asks(), says("就用方案一。")], None));
    fixture.session.send("改一下解析器".into());
    let (asked, questions, expires_at) = question(&mut fixture).await;
    // Shown as the user will see them, tidied, and waited on for good outside auto mode.
    assert_eq!(asked, "q1");
    assert_eq!(questions.iter().map(|question| (question.header.as_str(), question.kind)).collect::<Vec<_>>(), [("方案", crate::events::QuestionKind::Single), ("补充", crate::events::QuestionKind::Text)]);
    assert_eq!(questions[0].options[0].description, "改得少");
    assert!(expires_at.is_none());
    fixture.until(|event| matches!(event, UiEvent::Status { status: Status::WaitingApproval, .. })).await;
    assert!(fixture.session.summary().awaiting_approval);
    // The answer goes to the call it was asked by, and only once.
    let answers = vec![Reply { chosen: vec!["方案一 (推荐)".into()], text: String::new() }, Reply { chosen: Vec::new(), text: "别动公共接口".into() }];
    assert!(!fixture.session.answer_questions("other", Some(answers.clone())));
    assert!(fixture.session.answer_questions("q1", Some(answers.clone())));
    assert!(!fixture.session.answer_questions("q1", Some(answers.clone())));
    fixture.idle().await;
    let log = fixture.session.snapshot();
    assert!(log.iter().any(|envelope| envelope.event == Event::QuestionAnswered { tool_use_id: "q1".into(), answers: answers.clone(), declined: false, timed_out: false }));
    let (id, failed, told) = fixture.results().remove(0);
    assert_eq!((id.as_str(), failed), ("q1", false));
    assert!(told.contains("用哪个方案？\n   → 方案一 (推荐)") && told.contains("还有什么要求？\n   → 别动公共接口"), "{told}");
    // The model was told within the same turn, and the session is no longer waiting.
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "turn/end").count(), 1);
    assert!(!fixture.session.summary().awaiting_approval);
}

#[tokio::test(flavor = "multi_thread")]
async fn questions_left_to_the_agent_or_unanswered_in_time_let_it_decide() {
    // The user leaves it to the agent.
    let mut fixture = fixture(ScriptedLlm::new(vec![asks(), says("我自己定了方案一。")], None));
    fixture.session.send("改一下解析器".into());
    let (asked, ..) = question(&mut fixture).await;
    assert!(fixture.session.answer_questions(&asked, None));
    fixture.idle().await;
    assert!(fixture.session.snapshot().iter().any(|envelope| matches!(&envelope.event, Event::QuestionAnswered { declined: true, timed_out: false, answers, .. } if answers.is_empty())));
    let (_, failed, told) = fixture.results().remove(0);
    assert!(!failed && told.contains("chose not to answer"), "{told}");

    // In auto mode nobody may be there: the questions wait a while, then the agent decides.
    let mut auto = self::fixture(ScriptedLlm::new(vec![asks(), says("没人答，我选了方案一。")], None));
    *auto.guard.lock().unwrap() = (None, Some(Duration::from_millis(300)));
    auto.session.set_mode(Mode::Auto);
    auto.session.send("改一下解析器".into());
    let (_, _, expires_at) = question(&mut auto).await;
    assert!(expires_at.is_some());
    auto.idle().await;
    assert!(auto.session.snapshot().iter().any(|envelope| matches!(&envelope.event, Event::QuestionAnswered { declined: false, timed_out: true, .. })));
    let (_, failed, told) = auto.results().remove(0);
    assert!(!failed && told.starts_with("Nobody answered within 0 seconds"), "{told}");
}

#[tokio::test(flavor = "multi_thread")]
async fn questions_that_cannot_be_shown_are_sent_back_without_asking() {
    let script = vec![calls(&[("q1", "ask_user", json!({ "questions": [{ "question": "哪个？", "options": [{ "label": "只有一个" }] }] }))]), says("ok")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("hi".into());
    fixture.idle().await;
    assert!(!fixture.kinds().contains(&"question/asked".to_string()));
    let (_, failed, told) = fixture.results().remove(0);
    assert!(failed && told.contains("offers 1 options"), "{told}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_turn_while_it_asks_ends_the_wait() {
    let mut fixture = fixture(ScriptedLlm::new(vec![asks()], None));
    fixture.session.send("改一下解析器".into());
    let (asked, ..) = question(&mut fixture).await;
    fixture.session.cancel();
    fixture.idle().await;
    assert!(!fixture.session.answer_questions(&asked, None));
    assert!(!fixture.session.summary().awaiting_approval);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guardian_and_the_wait_are_auto_modes_only() {
    // In edit mode the user is asked, and waited for: no guardian, no time limit.
    let mut fixture = fixture(ScriptedLlm::new(vec![writes(), says("done")], None));
    *fixture.guard.lock().unwrap() = (Some("guard-model".into()), Some(Duration::from_millis(50)));
    fixture.session.send("write hi".into());
    let asked = fixture.approval().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fixture.session.resolve_approval(&asked, Decision::AllowOnce), "still waiting after the time auto mode would give");
    fixture.idle().await;
    assert!(reviews(&fixture).is_empty() && fixture.llm.requests.lock().unwrap().len() == 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_a_schedule_started_is_told_nobody_may_be_there() {
    let mut fixture = fixture_started_by(ScriptedLlm::new(vec![says("done")], None), Some("nightly"));
    fixture.session.send("run the tests".into());
    fixture.idle().await;
    let requests = fixture.llm.requests.lock().unwrap();
    assert!(requests[0].system.contains("A schedule the user set up started this session") && !requests[0].system.contains("You are a subagent"));
    // It is still a session of the user's own, so it may hand work to subagents, ask the user and present a plan;
    // and on Anthropic it may search the web through it, with no GLM key saved.
    assert_eq!(requests[0].tools.len(), tools::specs().len() + 4);
    assert_eq!(fixture.session.summary().schedule.as_deref(), Some("nightly"));
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
    // The built-in tools; `delegate`, `ask_user` and `present_plan`, which only a session the user started is
    // offered; and `web_search`, which Anthropic answers with no GLM key saved.
    assert_eq!(requests[0].tools.len(), tools::specs().len() + 4);
    assert!(requests[0].system.contains(&fixture.workspace.path().display().to_string()) && !requests[0].system.contains("A schedule"));
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
    // The request is logged a moment before the status changes: wait for that too.
    fixture.until(|event| matches!(event, UiEvent::Status { status: Status::WaitingApproval, .. })).await;
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
async fn a_command_of_several_allowed_for_good_keeps_each_of_them() {
    let run = |id: &str, command: &str| calls(&[(id, "run_command", json!({ "command": command }))]);
    let script = vec![run("t1", "echo ok; git status"), says("done"), run("t2", "git status"), says("done")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("run it".into());
    let asked = fixture.approval().await;
    assert!(fixture.session.resolve_approval(&asked, Decision::AllowAlways));
    fixture.idle().await;
    let store = GrantStore::new(fixture._home.path().join("grants.json"));
    assert!(store.allows(fixture.workspace.path(), "command:echo ok") && store.allows(fixture.workspace.path(), "command:git status"));
    // One of them on its own needs no approval now.
    fixture.session.send("again".into());
    fixture.idle().await;
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "approval/requested").count(), 1);
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
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let services = Services { grants: Arc::new(GrantStore::new(home.join("grants.json"))), mcp: None, skills: None, search: None, sandbox: None, chat: None, runtime: None, painter: None, lsp: None };
    let later = Session::new(log, fixture.llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();
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
    fixture.session.set_project_trust(true).unwrap();
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
    assert!(!fixture.session.send_with("what is this?".into(), vec![image("Zmlyc3Q=")], vec![]));
    while fixture.llm.requests.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // A second picture sent while the first request is in flight joins at the next step.
    assert!(fixture.session.send_with(String::new(), vec![image("c2Vjb25k")], vec![]));
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
async fn a_file_the_user_attached_is_read_without_asking_wherever_it_is() {
    // Two files outside the workspace: one attached to the message, one not.
    let outside = tempfile::tempdir().unwrap();
    let notes = outside.path().join("notes.md");
    let other = outside.path().join("other.md");
    std::fs::write(&notes, "attached words\n").unwrap();
    std::fs::write(&other, "private words\n").unwrap();
    let read = |id: &str, path: &std::path::Path| calls(&[(id, "read_file", json!({ "path": path.to_string_lossy() }))]);
    let mut fixture = fixture(ScriptedLlm::new(vec![read("t1", &notes), read("t2", &other), says("done")], None));
    let attached = crate::events::attach(&notes).unwrap();
    fixture.session.send_with("see the notes".into(), vec![], vec![attached]);
    // The first read runs by itself; the second, of a file nobody attached, asks.
    let asked = fixture.approval().await;
    assert_eq!(fixture.results()[0].0, "t1");
    assert!(!fixture.results()[0].1 && fixture.results()[0].2.contains("attached words"));
    assert!(fixture.session.resolve_approval(&asked, Decision::Deny));
    fixture.idle().await;
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "approval/requested").count(), 1);
    // The model was shown the file's text with the message, ahead of the words.
    let requests = fixture.llm.requests.lock().unwrap();
    let content = &requests[0].messages[0]["content"];
    assert!(content[0]["text"].as_str().unwrap().contains("attached words") && content[1]["text"] == "see the notes", "{content}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_skills_in_the_prompt_and_loads_one_on_request() {
    let load = calls(&[("t1", "load_skill", json!({ "name": "release" })), ("t2", "load_skill", json!({ "name": "missing" }))]);
    let mut fixture = fixture(ScriptedLlm::new(vec![says("no skills yet"), load, says("released"), says("again")], None));
    fixture.session.set_project_trust(true).unwrap();
    fixture.session.send("hello".into());
    fixture.idle().await;
    assert!(!fixture.llm.requests.lock().unwrap()[0].system.contains("<skills>"));

    // One skill of the user's and one of the workspace's, found as the next turn starts.
    let user = fixture._home.path().join("skills/release");
    let project = fixture.workspace.path().join(".odg/skills/style");
    for (directory, text) in [(&user, "---\nname: release\ndescription: How to cut a release.\n---\nTag, then build.\n"), (&project, "---\nname: style\ndescription: House style.\n---\nTabs.\n")] {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("SKILL.md"), text).unwrap();
    }
    fixture.session.send("cut a release".into());
    fixture.idle().await;
    fixture.session.send("and again".into());
    fixture.idle().await;

    let requests = fixture.llm.requests.lock().unwrap();
    assert!(requests[1].system.ends_with("<skills>\n- release: How to cut a release.\n- style: House style.\n</skills>"), "{}", requests[1].system);
    // The list is logged once while it stays the same, so the prompt and its cache hold.
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "skills/listed").count(), 1);
    assert_eq!(requests[1].system, requests[3].system);
    let results = fixture.results();
    assert!(!results[0].1 && results[0].2.starts_with("Tag, then build.\n"), "{:?}", results[0]);
    assert_eq!(results[1], ("t2".to_string(), true, "There is no skill `missing`. The skills are: release, style.".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn untrusted_workspace_withholds_project_instructions_and_skills_until_trusted() {
    let mut fixture = fixture(ScriptedLlm::new(vec![says("untrusted"), says("trusted"), says("revoked")], None));
    std::fs::write(fixture.workspace.path().join("AGENTS.md"), "Secret repo instructions.\n").unwrap();
    let user_skill = fixture._home.path().join("skills/global-doc");
    let proj_skill = fixture.workspace.path().join(".odg/skills/repo-hook");
    std::fs::create_dir_all(&user_skill).unwrap();
    std::fs::create_dir_all(&proj_skill).unwrap();
    std::fs::write(user_skill.join("SKILL.md"), "---\nname: global-doc\ndescription: Global skill.\n---\nGlobal.\n").unwrap();
    std::fs::write(proj_skill.join("SKILL.md"), "---\nname: repo-hook\ndescription: Repo skill.\n---\nRepo.\n").unwrap();

    assert!(!fixture.session.is_project_trusted());
    let pending_resources = fixture.session.untrusted_project_resources();
    assert_eq!(pending_resources.len(), 2);
    assert!(pending_resources[0].contains("AGENTS.md"));
    assert!(pending_resources[1].contains(".odg/skills"));

    // Turn 1 (untrusted): global skill loads, but repo AGENTS.md and repo skill are withheld!
    fixture.session.send("check".into());
    fixture.idle().await;
    {
        let reqs = fixture.llm.requests.lock().unwrap();
        assert!(!reqs[0].system.contains("Secret repo instructions."));
        assert!(reqs[0].system.contains("global-doc: Global skill."));
        assert!(!reqs[0].system.contains("repo-hook"));
    }

    // Grant trust -> Turn 2 loads both AGENTS.md and repo skill.
    fixture.session.set_project_trust(true).unwrap();
    assert!(fixture.session.is_project_trusted());
    assert!(fixture.session.untrusted_project_resources().is_empty());
    fixture.session.send("now trusted".into());
    fixture.idle().await;
    {
        let reqs = fixture.llm.requests.lock().unwrap();
        assert!(reqs[1].system.contains("Secret repo instructions."));
        assert!(reqs[1].system.contains("repo-hook: Repo skill."));
    }

    // Revoke trust -> Turn 3 clears AGENTS.md and drops repo skill again.
    fixture.session.set_project_trust(false).unwrap();
    fixture.session.send("revoked".into());
    fixture.idle().await;
    {
        let reqs = fixture.llm.requests.lock().unwrap();
        assert!(!reqs[2].system.contains("Secret repo instructions."));
        assert!(!reqs[2].system.contains("repo-hook"));
        assert!(reqs[2].system.contains("global-doc: Global skill."));
    }
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
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let kept = Arc::new(GrantStore::new(home.path().join("grants.json")));
    // Searching is set up, so the model is offered `web_search` as well.
    let search: crate::web::SearchSource = Arc::new(|| Some(crate::web::SearchConfig { url: "http://127.0.0.1:9/web_search".into(), key: "k".into(), engine: "search_std".into() }));
    let services = Services { grants: kept.clone(), mcp: Some(hub), skills: None, search: Some(search), sandbox: None, chat: None, runtime: None, painter: None, lsp: None };
    let session = Session::new(log, llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();
    // Auto mode lets safe commands through, but never a server's tool.
    session.set_mode(Mode::Auto);
    let mut fixture = Fixture { _home: home, limits: Arc::default(), guard: Arc::default(), workspace, session, llm, events };
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
    assert_eq!(names[names.len() - 3..], ["web_search", "mcp__echo__echo", "mcp__echo__fail_now"]);
    assert_eq!(names.len(), tools::specs().len() + 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_to_another_model_between_turns_and_keeps_the_conversation() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let thinking = json!({ "type": "thinking", "thinking": "hm", "signature": "sig-from-scripted" });
    let first = Ok(AssistantTurn { content: vec![thinking.clone(), json!({ "type": "text", "text": "one" })], stop_reason: Some("end_turn".into()), model: "scripted".into(), ..Default::default() });
    let mut fixture = fixture(ScriptedLlm::new(vec![first, says("two"), says("three")], Some(gate.clone())));
    fixture.session.send("first".into());
    fixture.idle().await;

    fixture.session.set_model("anthropic", " scripted-large ").unwrap();
    // Choosing the model it is already on changes nothing and logs nothing.
    fixture.session.set_model("anthropic", "scripted-large").unwrap();
    assert!(fixture.session.set_model("anthropic", "  ").is_err());
    assert_eq!(fixture.session.summary().model, "scripted-large");
    gate.add_permits(1);
    fixture.session.send("second".into());
    fixture.idle().await;

    // Not while a turn runs: that turn stays on the model it started with.
    fixture.session.send("third".into());
    while fixture.llm.requests.lock().unwrap().len() < 3 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(fixture.session.set_model("anthropic", "scripted-small").unwrap_err().contains("is working"));
    gate.add_permits(1);
    fixture.idle().await;

    let requests = fixture.llm.requests.lock().unwrap();
    assert_eq!(requests.iter().map(|request| request.model.as_str()).collect::<Vec<_>>(), ["scripted", "scripted-large", "scripted-large"]);
    // The earlier model's reasoning goes along untouched: the provider drops what the new model cannot read.
    assert_eq!(requests[1].messages[1]["content"][0], thinking);
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "model/changed").count(), 1);
}

#[test]
fn tells_a_conversation_it_is_one_and_a_project_nothing_of_the_kind() {
    let workspace = Path::new("/home/me/.odg/闲谈");
    let chat = system_prompt(workspace, None, &[], Origin::User, true);
    let project = system_prompt(workspace, None, &[], Origin::User, false);
    assert!(chat.contains("This session is a conversation, not work on a project") && chat.contains("Answer the user directly"));
    assert!(!project.contains("This session is a conversation"));
    // However the folder is written, it is the same folder.
    let directory = tempfile::tempdir().unwrap();
    assert!(same_folder(directory.path(), &directory.path().join(".")));
    assert!(!same_folder(directory.path(), &directory.path().join("elsewhere")));
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_to_another_provider_with_what_it_can_read_and_works_at_the_effort_set() {
    // A first turn on Anthropic: signed reasoning, words, and a call whose id another provider gave it a shape of its own.
    let thinking = json!({ "type": "thinking", "thinking": "hm", "signature": "sig-from-anthropic" });
    let call = json!({ "type": "tool_use", "id": "functions.read_file:0", "name": "read_file", "input": { "path": "a.txt" }, "item": { "kept": "by its provider" } });
    let first = Ok(AssistantTurn { content: vec![thinking, json!({ "type": "text", "text": "reading" }), call], stop_reason: Some("tool_use".into()), model: "scripted".into(), ..Default::default() });
    let mut fixture = fixture(ScriptedLlm::new(vec![first, says("read it"), says("on deepseek now")], None));
    std::fs::write(fixture.workspace.path().join("a.txt"), "hello\n").unwrap();
    fixture.session.send("read a.txt".into());
    fixture.idle().await;

    assert!(fixture.session.set_model("nowhere", "m").unwrap_err().contains("no provider"));
    fixture.session.set_model("deepseek", "deepseek-chat").unwrap();
    assert!(fixture.session.set_effort("as hard as it likes").is_err());
    fixture.session.set_effort("low").unwrap();
    let summary = fixture.session.summary();
    assert_eq!((summary.provider.as_str(), summary.model.as_str(), summary.effort.as_deref()), ("deepseek", "deepseek-chat", Some("low")));
    fixture.session.send("go on".into());
    fixture.idle().await;

    let requests = fixture.llm.requests.lock().unwrap();
    assert_eq!(requests.iter().map(|request| (request.provider.as_str(), request.effort.as_str())).collect::<Vec<_>>(), [("anthropic", "high"), ("anthropic", "high"), ("deepseek", "low")]);
    // On the new provider, what Anthropic answered is its words and its call, plainly, the reasoning left out;
    // the call's id takes only characters every provider allows, and its result follows it.
    let moved = &requests[2].messages;
    assert_eq!(moved[1]["content"], json!([{ "type": "text", "text": "reading" }, { "type": "tool_use", "id": "functions_read_file_0", "name": "read_file", "input": { "path": "a.txt" } }]));
    assert_eq!(moved[2]["content"][0]["tool_use_id"], "functions_read_file_0");
    // Before the move the same conversation went as it was.
    assert_eq!(requests[1].messages[1]["content"][0]["signature"], "sig-from-anthropic");
    assert_eq!(requests[1].messages[2]["content"][0]["tool_use_id"], "functions.read_file:0");
    drop(requests);
    // The log says where it moved, and the effort set, so that a session opened again reads both back.
    let kinds = fixture.kinds();
    assert!(kinds.contains(&"model/changed".to_string()) && kinds.contains(&"effort/changed".to_string()));
    let events = fixture.session.snapshot();
    assert_eq!((crate::session::provider(&events), crate::session::effort(&events)), ("deepseek".to_string(), Some("low".to_string())));
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
    assert_eq!(requests[1].tools.len(), tools::specs().len() + 4);
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
async fn compacts_the_conversation_when_asked_between_turns() {
    let script = vec![says("first answer"), says("<summary>The user said one.</summary>"), says("second answer")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    // Nothing has been said yet, so there is nothing to summarize.
    assert!(fixture.session.compact_now().await.unwrap_err().contains("nothing to summarize"));
    fixture.session.send("one".into());
    fixture.idle().await;

    // Well below any limit, asked for by hand: the summary replaces the conversation, outside any turn.
    let before = fixture.session.compact_now().await.unwrap();
    assert!(before > 0);
    // The session counted as working while it summarized, and is idle again.
    fixture.until(|event| matches!(event, UiEvent::Status { status: Status::Running, .. })).await;
    fixture.idle().await;
    assert_eq!(fixture.kinds()[4..], ["context/compacted"]);
    assert_eq!(fixture.session.status(), Status::Idle);
    assert!(fixture.session.compact_now().await.unwrap_err().contains("nothing to summarize"), "a summary with nothing said after it is not summarized again");

    // The next turn starts from the summary.
    fixture.session.send("two".into());
    fixture.idle().await;
    let requests = fixture.llm.requests.lock().unwrap();
    let summary = format!("{}\n\nThe user said one.", crate::session::COMPACTED);
    assert_eq!(texts(&requests[2]), [vec![summary, "two".to_string()]]);
}

#[tokio::test(flavor = "multi_thread")]
async fn compacts_with_custom_focus_and_retains_most_recent_completed_turn() {
    let script = vec![
        says("turn 1 answer"),
        says("turn 2 exact code details"),
        says("<summary>Summary of turn 1 preserving DB schema.</summary>"),
        says("turn 3 answer"),
    ];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("turn 1 question".into());
    fixture.idle().await;
    fixture.session.send("turn 2 question".into());
    fixture.idle().await;

    fixture.session.compact_now_with(Some("保留 DB schema 细节".into())).await.unwrap();
    fixture.until(|event| matches!(event, UiEvent::Status { status: Status::Running, .. })).await;
    fixture.idle().await;

    // Next turn: summary covers turn 1, turn 2 is retained verbatim, followed by turn 3 question!
    fixture.session.send("turn 3 question".into());
    fixture.idle().await;

    let requests = fixture.llm.requests.lock().unwrap();
    // Compaction request (requests[2]) summarized only turn 1 and appended the focus instruction.
    let compact_req_texts = texts(&requests[2]);
    assert_eq!(compact_req_texts.len(), 3);
    assert_eq!(compact_req_texts[0], vec!["turn 1 question".to_string()]);
    assert_eq!(compact_req_texts[1], vec!["turn 1 answer".to_string()]);
    assert!(compact_req_texts[2][0].contains("保留 DB schema 细节"));

    // Post-compaction request (requests[3]) starts with summary + turn 2 user, then turn 2 assistant, then turn 3 user.
    let summary_head = format!("{}\n\nSummary of turn 1 preserving DB schema.", crate::session::COMPACTED);
    assert_eq!(
        texts(&requests[3]),
        [
            vec![summary_head, "turn 2 question".to_string()],
            vec!["turn 2 exact code details".to_string()],
            vec!["turn 3 question".to_string()],
        ]
    );
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
async fn with_no_glm_key_the_sessions_provider_searches_in_a_request_of_its_own() {
    let found = Ok(AssistantTurn {
        content: vec![
            json!({ "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": { "query": "tokio 1.54" } }),
            json!({ "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [{ "type": "web_search_result", "url": "https://tokio.rs/blog/1-54", "title": "Tokio 1.54", "encrypted_content": "x" }] }),
            json!({ "type": "text", "text": "Tokio 1.54 adds a thing." }),
        ],
        stop_reason: Some("end_turn".into()),
        model: "scripted".into(),
        usage: json!({ "input_tokens": 900, "output_tokens": 40 }),
        ..Default::default()
    });
    let script = vec![calls(&[("t1", "web_search", json!({ "query": "tokio 1.54", "site": "tokio.rs" }))]), found, says("Found it.")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("what is new in tokio?".into());
    // Each search is still asked about first, as Zhipu's is.
    let asked = fixture.approval().await;
    assert!(fixture.session.resolve_approval(&asked, Decision::AllowOnce));
    fixture.idle().await;
    let requests = fixture.llm.requests.lock().unwrap();
    // The search is a request of its own: the provider's tool, its own prompt, and the query alone.
    let search = &requests[1];
    assert_eq!((search.system.as_str(), search.effort.as_str(), search.messages.len()), (crate::provider_search::SEARCHER, "low", 1));
    assert_eq!(search.tools[0]["allowed_domains"], json!(["tokio.rs"]));
    assert!(search.tools[0]["type"].as_str().unwrap().starts_with("web_search_"));
    let (_, failed, told) = fixture.results().remove(0);
    assert!(!failed && told.starts_with("Searched the web for `tokio 1.54` through anthropic") && told.contains("https://tokio.rs/blog/1-54"), "{told}");
    // What the search spent is kept with its result.
    let spent = fixture.session.snapshot().into_iter().find_map(|envelope| if let Event::ToolResult { meta, .. } = envelope.event { Some(meta["tokens"].clone()) } else { None });
    assert_eq!(spent.unwrap()["output"], 40);
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_into_a_job_is_asked_about_showing_the_command_it_goes_to() {
    let reading = if cfg!(windows) { "while (($line = [Console]::In.ReadLine()) -ne $null) { \"got $line\" }" } else { "while read line; do echo \"got $line\"; done" };
    let script = vec![
        calls(&[("t1", "run_command", json!({ "command": reading, "background": true }))]),
        calls(&[("t2", "command_input", json!({ "job": "b1", "text": "hi", "wait_seconds": 10 }))]),
        says("done"),
    ];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("start it and say hi".into());
    let started = fixture.approval().await;
    assert!(fixture.session.resolve_approval(&started, Decision::AllowOnce));
    let typed = fixture.approval().await;
    let asked = fixture.session.snapshot().into_iter().find_map(|envelope| match envelope.event {
        Event::ApprovalRequested { tool_use_id, reason, detail, grant, .. } if tool_use_id == typed => Some((reason, detail, grant)),
        _ => None,
    });
    assert_eq!(asked, Some(("input".to_string(), format!("{reading}\n← hi"), Some("input:b1".to_string()))));
    assert!(fixture.session.resolve_approval(&typed, Decision::AllowOnce));
    fixture.idle().await;
    let (_, failed, told) = fixture.results().remove(1);
    assert!(!failed && told.contains("got hi"), "{told}");
    fixture.session.kill_jobs();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_presented_in_plan_mode_ends_the_turn_with_it() {
    let plan = json!({ "title": "Faster parser", "plan": "1. Cache the tokens in `lexer.rs`.\n2. Run `cargo test`." });
    // One reply only: asking the model again after the plan would find the script empty.
    let mut fixture = fixture(ScriptedLlm::new(vec![calls(&[("p1", "present_plan", plan)])], None));
    fixture.session.set_mode(Mode::Plan);
    fixture.session.send("plan a faster parser".into());
    fixture.idle().await;
    assert_eq!(fixture.llm.requests.lock().unwrap().len(), 1);
    assert!(fixture.llm.requests.lock().unwrap()[0].tools.iter().any(|tool| tool["name"] == "present_plan"));
    let (_, failed, told) = fixture.results().remove(0);
    assert!(!failed && told.starts_with("The plan is shown to the user"), "{told}");
    let ended = fixture.session.snapshot().into_iter().find_map(|envelope| if let Event::TurnEnd { reason, .. } = envelope.event { Some(reason) } else { None });
    assert_eq!(ended.as_deref(), Some("completed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_plan_mode_a_plan_is_not_presented_and_the_turn_goes_on() {
    let script = vec![calls(&[("p1", "present_plan", json!({ "plan": "1. Do it." }))]), calls(&[("p2", "present_plan", json!({ "plan": " " }))]), says("doing it then")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("speed up the parser".into());
    fixture.idle().await;
    let results = fixture.results();
    assert!(results[0].1 && results[0].2.starts_with("Plan mode is off"), "{:?}", results[0]);
    assert!(results[1].1 && results[1].2.starts_with("Plan mode is off"), "{:?}", results[1]);
    assert_eq!(fixture.llm.requests.lock().unwrap().len(), 3);
    // In plan mode an empty plan is refused, and the model asked again.
    let mut planning = self::fixture(ScriptedLlm::new(vec![calls(&[("p1", "present_plan", json!({ "plan": "  " }))]), says("here it is in words")], None));
    planning.session.set_mode(Mode::Plan);
    planning.session.send("plan it".into());
    planning.idle().await;
    let (_, failed, told) = planning.results().remove(0);
    assert!(failed && told.contains("`plan` is empty"), "{told}");
}

#[tokio::test(flavor = "multi_thread")]
async fn undoing_a_turn_brings_back_what_it_deleted_and_moved() {
    let script = vec![
        calls(&[("t1", "delete_file", json!({ "path": "gone.txt" })), ("t2", "move_file", json!({ "path": "a.txt", "to": "lib/b.txt" }))]),
        says("tidied"),
        says("understood"),
    ];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    let file = |name: &str| fixture.workspace.path().join(name);
    let (gone, a, b) = (file("gone.txt"), file("a.txt"), file("lib/b.txt"));
    std::fs::write(&gone, "keep me\n").unwrap();
    std::fs::write(&a, "move me\n").unwrap();
    // In 府内自便 both run without asking, as edits in the workspace do.
    fixture.session.send("tidy up".into());
    fixture.idle().await;
    assert!(!fixture.kinds().contains(&"approval/requested".to_string()));
    assert!(!gone.exists() && !a.exists() && std::fs::read_to_string(&b).unwrap() == "move me\n");

    let outcome = fixture.session.undo(1).unwrap();
    assert_eq!(outcome, undo::Outcome { restored: vec!["gone.txt".into(), "lib/b.txt".into(), "a.txt".into()], skipped: vec![] });
    assert_eq!(std::fs::read_to_string(&gone).unwrap(), "keep me\n");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "move me\n");
    assert!(!b.exists());
    // Undone once, nothing is left to undo.
    assert_eq!(fixture.session.undo(1).unwrap_err(), "turn 1 and those after it have no edits to undo");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_file_put_back_by_someone_else_is_left_alone() {
    let mut fixture = fixture(ScriptedLlm::new(vec![calls(&[("t1", "delete_file", json!({ "path": "x.txt" }))]), says("deleted")], None));
    let x = fixture.workspace.path().join("x.txt");
    std::fs::write(&x, "agent's\n").unwrap();
    fixture.session.send("delete it".into());
    fixture.idle().await;
    std::fs::write(&x, "mine\n").unwrap();
    assert_eq!(fixture.session.undo(1).unwrap(), undo::Outcome { restored: vec![], skipped: vec!["x.txt".into()] });
    assert_eq!(std::fs::read_to_string(&x).unwrap(), "mine\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_read_from_a_file_reaches_the_model_and_the_log() {
    let mut fixture = fixture(ScriptedLlm::new(vec![calls(&[("t1", "read_file", json!({ "path": "shot.png" }))]), says("a red square")], None));
    let png = [&b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"[..], &1u32.to_be_bytes(), &1u32.to_be_bytes(), &[8, 6, 0, 0, 0]].concat();
    std::fs::write(fixture.workspace.path().join("shot.png"), &png).unwrap();
    fixture.session.send("what is in shot.png?".into());
    fixture.idle().await;
    let data = crate::events::base64(&png);
    assert!(fixture.session.snapshot().iter().any(|envelope| matches!(&envelope.event, Event::ToolResult { images, .. } if images.len() == 1 && images[0].data == data)));
    let requests = fixture.llm.requests.lock().unwrap();
    let result = &requests[1].messages.last().unwrap()["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["content"][1], json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": data } }));
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
async fn deliver_queued_interrupts_a_running_step_only_while_messages_are_waiting() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut fixture = fixture(ScriptedLlm::new(vec![says("heard right away")], Some(gate.clone())));
    assert!(!fixture.session.deliver_queued(), "idle session has nothing queued");
    fixture.session.send("start".into());
    while fixture.llm.requests.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!fixture.session.deliver_queued(), "running session with empty inbox is not cancelled");
    assert!(fixture.session.send("deliver now".into()));
    assert!(fixture.session.deliver_queued(), "running session with queued message is interrupted immediately");
    while fixture.llm.requests.lock().unwrap().len() < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    gate.add_permits(1);
    fixture.idle().await;

    assert_eq!(fixture.kinds(), [
        "turn/start", "user/message", "turn/end", "turn/start", "user/message", "assistant/message", "turn/end"
    ]);
    assert_eq!(fixture.end().0, "completed");
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

fn delegates(id: &str, tasks: &[(&str, &str)]) -> Result<AssistantTurn, LlmError> {
    let tasks: Vec<Value> = tasks.iter().map(|(name, task)| json!({ "name": name, "task": task })).collect();
    calls(&[(id, "delegate", json!({ "tasks": tasks }))])
}

#[tokio::test(flavor = "multi_thread")]
async fn delegates_tasks_to_subagents_and_hands_back_their_reports() {
    // The two subagents ask the model at the same time, so they are scripted the same answer.
    let script = vec![delegates("d1", &[("parser", "Read the parser."), ("lexer", "Read the lexer.")]), says("Found it."), says("Found it."), says("Both are fine.")];
    let fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.set_mode(Mode::Auto);
    fixture.session.send("look at both".into());
    fixture.session.wait_idle().await;

    // The call's result is what the subagents said, each under its name.
    let results = fixture.results();
    assert_eq!(results.len(), 1);
    assert!(!results[0].1, "{}", results[0].2);
    assert_eq!(results[0].2, "<subagent name=\"parser\">\nFound it.\n</subagent>\n\n<subagent name=\"lexer\">\nFound it.\n</subagent>");
    assert_eq!(fixture.kinds().iter().filter(|kind| *kind == "subagent/started").count(), 2);
    assert_eq!(fixture.end().0, "completed");

    // Each subagent is a session of its own under this one, named for its task, in this one's mode.
    let mut children = fixture.session.children();
    children.sort_by_key(|child| child.summary().title);
    assert_eq!(children.iter().map(|child| child.summary().title).collect::<Vec<_>>(), ["lexer", "parser"]);
    for child in &children {
        assert_eq!(child.parent(), Some(fixture.session.id()));
        assert_eq!(child.summary().parent.as_deref(), Some(fixture.session.id()));
        assert_eq!((child.status(), child.mode()), (Status::Idle, Mode::Auto));
        assert_eq!(fixture.session.child(child.id()).map(|found| found.id().to_string()), Some(child.id().to_string()));
    }
    assert!(fixture.session.parent().is_none());

    let requests = fixture.llm.requests.lock().unwrap();
    let named = |request: &LlmRequest, name: &str| request.tools.iter().any(|tool| tool["name"] == name);
    let own: Vec<&LlmRequest> = requests.iter().filter(|request| request.session == fixture.session.id()).collect();
    let theirs: Vec<&LlmRequest> = requests.iter().filter(|request| request.session != fixture.session.id()).collect();
    assert_eq!((own.len(), theirs.len()), (2, 2));
    // The agent the user started may delegate and is not told it is a subagent; a subagent is the reverse.
    assert!(named(own[0], "delegate") && named(own[0], "ask_user") && own[0].system.contains("`delegate`") && !own[0].system.contains("You are a subagent"));
    for request in &theirs {
        // Nor may it ask the user, present a plan, or manage global setup / user-facing delivery.
        for withheld in ["delegate", "ask_user", "present_plan", "install_skill", "add_mcp_server", "language_server", "document_runtime", "deliver_files"] {
            assert!(!named(request, withheld), "subagent should not be offered {withheld}");
        }
        assert!(named(request, "read_file") && named(request, "code_nav") && named(request, "edit_file"));
        assert!(request.system.contains("You are a subagent") && !request.system.contains("`delegate`") && !request.system.contains("`deliver_files`") && !request.system.contains("`install_skill`"));
        assert_eq!(request.messages.len(), 1);
    }
    let mut tasks: Vec<&str> = theirs.iter().filter_map(|request| request.messages[0]["content"][0]["text"].as_str()).collect();
    tasks.sort();
    assert_eq!(tasks, ["Read the lexer.", "Read the parser."]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_asks_for_approval_in_its_own_session() {
    let script = vec![delegates("d1", &[("runner", "Run it.")]), calls(&[("t1", "run_command", json!({ "command": "echo ok" }))]), says("It printed ok."), says("Done.")];
    let mut fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("run it".into());

    let asked = fixture.approval().await;
    let [child] = fixture.session.children().try_into().ok().expect("one subagent");
    // `Event::ApprovalRequested` is emitted just before `set_status(WaitingApproval)`, so wait briefly for the status flip on multi-threaded runtimes.
    for _ in 0..50 {
        if child.status() == Status::WaitingApproval {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // The subagent waits; the agent that delegated to it is still at work, on the call.
    assert_eq!((child.status(), fixture.session.status()), (Status::WaitingApproval, Status::Running));
    assert!(!fixture.session.resolve_approval(&asked, Decision::AllowOnce));
    assert!(child.resolve_approval(&asked, Decision::AllowOnce));
    fixture.session.wait_idle().await;

    assert_eq!(fixture.results()[0].2, "<subagent name=\"runner\">\nIt printed ok.\n</subagent>");
    let ran: Vec<bool> = child.snapshot().into_iter().filter_map(|envelope| match envelope.event {
        Event::ToolResult { is_error, .. } => Some(is_error),
        _ => None,
    }).collect();
    assert_eq!(ran, [false]);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_turn_stops_its_subagents() {
    // One request passes: the one that delegates. The subagent's waits at the gate until it is stopped.
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let mut fixture = fixture(ScriptedLlm::new(vec![delegates("d1", &[("slow", "Take your time.")])], Some(gate)));
    fixture.session.send("go".into());
    fixture.until(|event| matches!(event, UiEvent::Log { envelope, .. } if matches!(envelope.event, Event::SubagentStarted { .. }))).await;
    let [child] = fixture.session.children().try_into().ok().expect("one subagent");
    while fixture.llm.requests.lock().unwrap().len() < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    fixture.session.cancel();
    fixture.session.wait_idle().await;
    assert_eq!(child.status(), Status::Idle);
    assert_eq!(fixture.end().0, "cancelled");
    let results = fixture.results();
    assert!(results[0].1 && results[0].2.contains("[This subagent stopped before finishing (cancelled)"), "{}", results[0].2);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_list_of_tasks_it_cannot_run() {
    let none = calls(&[("d1", "delegate", json!({ "tasks": [] }))]);
    let many: Vec<(String, String)> = (0..=tools::SUBAGENTS).map(|index| (format!("n{index}"), "t".to_string())).collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(name, task)| (name.as_str(), task.as_str())).collect();
    let fixture = fixture(ScriptedLlm::new(vec![none, delegates("d2", &many), says("ok")], None));
    fixture.session.send("go".into());
    fixture.session.wait_idle().await;

    let results = fixture.results();
    assert!(results[0].1 && results[0].2.contains("at least one task"), "{}", results[0].2);
    assert!(results[1].1 && results[1].2.contains("too many"), "{}", results[1].2);
    assert!(fixture.session.children().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_is_refused_withheld_tools_without_asking() {
    let script = vec![
        delegates("d1", &[("worker", "Try installing a skill.")]),
        calls(&[("t1", "install_skill", json!({ "source": "https://example.com/skill" }))]),
        says("Cannot install skills."),
        says("Understood."),
    ];
    let fixture = fixture(ScriptedLlm::new(script, None));
    fixture.session.send("go".into());
    fixture.session.wait_idle().await;

    let [child] = fixture.session.children().try_into().ok().expect("one subagent");
    let log = child.snapshot();
    assert!(!log.iter().any(|envelope| matches!(envelope.event, Event::ApprovalRequested { .. })));
    let result = log.iter().find_map(|envelope| match &envelope.event {
        Event::ToolResult { is_error, content, meta, .. } => Some((*is_error, content.clone(), meta["refused"].as_bool())),
        _ => None,
    });
    assert!(matches!(result, Some((true, ref msg, Some(true))) if msg.contains("install_skill")), "{result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_side_conversation_is_read_only_and_briefed_on_the_main_session_before_each_new_message() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("notes.txt"), "keep\n").unwrap();
    let script = vec![
        calls(&[("e1", "edit_file", json!({ "path": "notes.txt", "old_string": "keep", "new_string": "gone" }))]),
        says("It is fixing the lexer."),
        says("Still the same."),
        says("Now it runs the tests."),
    ];
    let llm = ScriptedLlm::new(script, None);
    let new = NewSession { workspace: workspace.path(), provider: "anthropic", model: "scripted", parent: None, worktree: None, schedule: None, aside: Some("main-1") };
    let log = SessionStore::new(home.path()).create_with(new).unwrap();
    let (sender, _events) = unbounded_channel();
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let services = Services { grants: Arc::new(GrantStore::new(home.path().join("grants.json"))), mcp: None, skills: None, search: None, sandbox: None, chat: None, runtime: None, painter: None, lsp: None };
    let session = Session::new(log, llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();
    assert_eq!(session.aside(), Some("main-1"));
    let briefing = Arc::new(Mutex::new("[Briefing] fixing the lexer".to_string()));
    let read = briefing.clone();
    session.set_brief_source(Arc::new(move || Some(read.lock().unwrap().clone())));
    // Even in bypass mode a side conversation only looks.
    session.set_mode(Mode::Bypass);

    session.send("what is it doing?".into());
    session.wait_idle().await;
    session.send("and now?".into());
    session.wait_idle().await;
    *briefing.lock().unwrap() = "[Briefing] running the tests".to_string();
    session.send("now?".into());
    session.wait_idle().await;

    assert_eq!(std::fs::read_to_string(workspace.path().join("notes.txt")).unwrap(), "keep\n", "a side conversation changes no file");
    let log = session.snapshot();
    let briefs: Vec<&str> = log.iter().filter_map(|envelope| if let Event::AsideBriefed { brief } = &envelope.event { Some(brief.as_str()) } else { None }).collect();
    // The same briefing is not logged twice in a row.
    assert_eq!(briefs, ["[Briefing] fixing the lexer", "[Briefing] running the tests"]);
    assert!(log.iter().all(|envelope| !matches!(envelope.event, Event::FilesMade { .. })));

    let requests = llm.requests.lock().unwrap();
    let named = |name: &str| requests[0].tools.iter().any(|tool| tool["name"] == name);
    for withheld in ["edit_file", "write_file", "delete_file", "move_file", "delegate", "present_plan", "deliver_files", "install_skill", "add_mcp_server"] {
        assert!(!named(withheld), "a side conversation should not be offered {withheld}");
    }
    for kept in ["read_file", "search", "find_files", "run_command", "ask_user"] {
        assert!(named(kept), "a side conversation should still be offered {kept}");
    }
    assert!(requests[0].system.contains("This session is a side conversation") && !requests[0].system.contains("You are a subagent"));
    // The briefing goes ahead of the user's words, in the same message.
    let first_user = &requests[0].messages[0]["content"];
    assert_eq!(first_user[0]["text"], "[Briefing] fixing the lexer");
    assert_eq!(first_user[1]["text"], "what is it doing?");
    let last = requests.last().unwrap();
    let said: Vec<String> = last.messages.iter().filter(|message| message["role"] == "user").flat_map(|message| message["content"].as_array().cloned().unwrap_or_default()).filter_map(|block| block["text"].as_str().map(str::to_string)).collect();
    assert!(said.ends_with(&["[Briefing] running the tests".to_string(), "now?".to_string()]), "{said:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_session_trims_project_only_tools_and_prompt_references() {
    let home = tempfile::tempdir().unwrap();
    let chat_dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![says("hello")], None);
    let new = NewSession { workspace: chat_dir.path(), provider: "anthropic", model: "scripted", parent: None, worktree: None, schedule: None, aside: None };
    let log = SessionStore::new(home.path()).create_with(new).unwrap();
    let (sender, _events) = unbounded_channel();
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let services = Services { grants: Arc::new(GrantStore::new(home.path().join("grants.json"))), mcp: None, skills: None, search: None, sandbox: None, chat: Some(chat_dir.path().to_path_buf()), runtime: None, painter: None, lsp: None };
    let session = Session::new(log, llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();

    session.send("hi".into());
    session.wait_idle().await;

    let requests = llm.requests.lock().unwrap();
    let named = |name: &str| requests[0].tools.iter().any(|tool| tool["name"] == name);
    for withheld in ["code_nav", "language_server", "present_plan", "delegate"] {
        assert!(!named(withheld), "chat session should not be offered {withheld}");
    }
    for kept in ["read_file", "write_file", "run_command", "fetch_url", "ask_user", "deliver_files", "install_skill", "add_mcp_server", "web_search"] {
        assert!(named(kept), "chat session should still be offered {kept}");
    }
    assert!(requests[0].system.contains("This session is a conversation"));
    assert!(!requests[0].system.contains("`code_nav`") && !requests[0].system.contains("`delegate`"));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_subagent_omits_write_tools_and_refuses_mutations_even_in_bypass_mode() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("hello.txt"), "world\n").unwrap();
    let hub = Arc::new(McpHub::new(home.path().join("mcp.json"), tokio::runtime::Handle::current()));
    hub.insert("echo", crate::mcp::testing::echo_server().await);

    let listing = if cfg!(windows) { "Get-ChildItem" } else { "ls" };
    let script = vec![
        calls(&[(
            "d1",
            "delegate",
            json!({ "tasks": [{ "name": "inspector", "task": "Analyze hello.txt.", "read_only": true }] }),
        )]),
        calls(&[
            ("c1", "edit_file", json!({ "path": "hello.txt", "old_string": "world", "new_string": "hacked" })),
            ("c2", "run_command", json!({ "command": "echo hacked > out.txt" })),
            ("c3", "run_command", json!({ "command": "cargo fmt" })),
            ("c4", "mcp__echo__echo", json!({ "text": "hi" })),
            ("c5", "run_command", json!({ "command": listing })),
        ]),
        says("Inspected."),
        says("All done."),
    ];
    let llm = ScriptedLlm::new(script, None);
    let worktree = crate::events::Worktree { origin: workspace.path().to_string_lossy().into_owned(), checkout: workspace.path().to_string_lossy().into_owned(), branch: "odg/test".into() };
    let new = NewSession { workspace: workspace.path(), provider: "anthropic", model: "scripted", parent: None, worktree: Some(worktree), schedule: None, aside: None };
    let log = SessionStore::new(home.path()).create_with(new).unwrap();
    let (sender, _events) = unbounded_channel();
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let services = Services { grants: Arc::new(GrantStore::new(home.path().join("grants.json"))), mcp: Some(hub), skills: None, search: None, sandbox: None, chat: None, runtime: None, painter: None, lsp: None };
    let session = Session::new(log, llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();
    // Even with Bypass enabled on the parent worktree session, the read_only subagent stays strictly read-only.
    session.set_mode(Mode::Bypass);
    session.send("analyze".into());
    session.wait_idle().await;

    let [child] = session.children().try_into().ok().expect("one read-only subagent");
    let requests = llm.requests.lock().unwrap();
    let child_req = requests.iter().find(|req| req.session == child.id()).expect("child request");
    let named = |name: &str| child_req.tools.iter().any(|tool| tool["name"] == name);
    for withheld in ["edit_file", "write_file", "delete_file", "move_file", "generate_image", "command_input", "command_output", "stop_command", "mcp__echo__echo", "install_skill"] {
        assert!(!named(withheld), "read_only subagent should not be offered {withheld}");
    }
    for kept in ["read_file", "search", "find_files", "code_nav", "run_command"] {
        assert!(named(kept), "read_only subagent should still be offered {kept}");
    }
    assert!(child_req.system.contains("restricted to read-only inspection"));
    drop(requests);

    // Every mutating call was refused with `refusal: "read_only"`, while `ls` succeeded; workspace files are untouched.
    let child_results: Vec<(String, bool, Option<String>)> = child
        .snapshot()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::ToolResult { tool_use_id, is_error, meta, .. } => Some((tool_use_id, is_error, meta["refusal"].as_str().map(str::to_string))),
            _ => None,
        })
        .collect();
    assert_eq!(
        child_results,
        [
            ("c1".into(), true, Some("read_only".into())),
            ("c2".into(), true, Some("read_only".into())),
            ("c3".into(), true, Some("read_only".into())),
            ("c4".into(), true, Some("read_only".into())),
            ("c5".into(), false, None),
        ]
    );
    assert_eq!(std::fs::read_to_string(workspace.path().join("hello.txt")).unwrap(), "world\n");
    assert!(!workspace.path().join("out.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn deliverable_versions_are_snapshotted_restored_with_backup_and_saved_as_copies() {
    let script = vec![
        calls(&[
            ("w1", "write_file", json!({ "path": "report.md", "content": "# Draft v1\n" })),
            ("d1", "deliver_files", json!({ "paths": ["deck.pdf"] })),
        ]),
        says("first version ready"),
        calls(&[
            ("w2", "edit_file", json!({ "path": "report.md", "old_string": "Draft v1", "new_string": "Final v2" })),
            ("d2", "deliver_files", json!({ "paths": ["deck.pdf"] })),
        ]),
        says("second version ready"),
        says("noted the restore"),
    ];
    let fixture = fixture(ScriptedLlm::new(script, None));
    let deck = fixture.workspace.path().join("deck.pdf");
    let deck_str = deck.to_string_lossy().into_owned();
    let report = fixture.workspace.path().join("report.md");
    let report_str = report.to_string_lossy().into_owned();

    // Turn 1: v1 of both report.md and deck.pdf.
    std::fs::write(&deck, b"%PDF-1.4 v1").unwrap();
    fixture.session.send("make v1".into());
    fixture.session.wait_idle().await;

    // Turn 2: v2 of both report.md and deck.pdf.
    std::fs::write(&deck, b"%PDF-1.4 v2 updated").unwrap();
    fixture.session.send("make v2".into());
    fixture.session.wait_idle().await;

    let tool_meta = |id: &str| {
        fixture
            .session
            .snapshot()
            .into_iter()
            .find_map(|envelope| match envelope.event {
                Event::ToolResult { tool_use_id, meta, .. } if tool_use_id == id => Some(meta),
                _ => None,
            })
            .unwrap()
    };
    let report_v1 = tool_meta("w1")["snapshot"].as_str().unwrap().to_string();
    let report_v2 = tool_meta("w2")["snapshot"].as_str().unwrap().to_string();
    let deck_v1 = tool_meta("d1")["snapshots"][&deck_str].as_str().unwrap().to_string();
    let deck_v2 = tool_meta("d2")["snapshots"][&deck_str].as_str().unwrap().to_string();
    assert_ne!(report_v1, report_v2);
    assert_ne!(deck_v1, deck_v2);
    assert!(fixture.session.has_artifact_snapshot(&report_str, &report_v1));
    assert!(fixture.session.has_artifact_snapshot(&deck_str, &deck_v1));
    assert_eq!(fixture.session.snapshot_bytes(&report_v1).unwrap(), b"# Draft v1\n");
    assert_eq!(fixture.session.snapshot_bytes(&report_v2).unwrap(), b"# Final v2\n");
    assert_eq!(fixture.session.snapshot_bytes(&deck_v1).unwrap(), b"%PDF-1.4 v1");
    assert_eq!(fixture.session.snapshot_bytes(&deck_v2).unwrap(), b"%PDF-1.4 v2 updated");

    // Manual edit on disk before restoring v1 is backed up automatically so nothing is lost.
    std::fs::write(&deck, b"%PDF-1.4 manual edit").unwrap();
    let restored = fixture.session.restore_artifact(&deck_str, &deck_v1, 1).unwrap();
    assert_eq!((restored.path.as_str(), restored.version, restored.backed_up), ("deck.pdf", 1, true));
    assert_eq!(std::fs::read(&deck).unwrap(), b"%PDF-1.4 v1");
    let backup_snap = fixture
        .session
        .snapshot()
        .into_iter()
        .find_map(|envelope| match envelope.event {
            Event::ArtifactRestored { previous, .. } => previous,
            _ => None,
        })
        .unwrap();
    assert!(fixture.session.has_artifact_snapshot(&deck_str, &backup_snap));
    assert_eq!(fixture.session.snapshot_bytes(&backup_snap).unwrap(), b"%PDF-1.4 manual edit");

    // Saving a copy creates non-clobbering sibling files `deck.v2.pdf` and `deck.v2-2.pdf`.
    let copy1 = fixture.session.save_artifact_copy(&deck_str, &deck_v2, 2).unwrap();
    let copy2 = fixture.session.save_artifact_copy(&deck_str, &deck_v2, 2).unwrap();
    assert_eq!((copy1.as_str(), copy2.as_str()), ("deck.v2.pdf", "deck.v2-2.pdf"));
    assert_eq!(std::fs::read(fixture.workspace.path().join(&copy1)).unwrap(), b"%PDF-1.4 v2 updated");
    assert_eq!(std::fs::read(fixture.workspace.path().join(&copy2)).unwrap(), b"%PDF-1.4 v2 updated");

    // The next turn tells the model that deck.pdf was restored to v1.
    fixture.session.send("revise from v1".into());
    fixture.session.wait_idle().await;
    let requests = fixture.llm.requests.lock().unwrap();
    let told = texts(requests.last().unwrap()).pop().unwrap().remove(0);
    assert!(told.contains("restored deck.pdf to version v1"), "{told}");
    drop(requests);

    // A forked session can still read snapshots kept by its source session.
    let store = SessionStore::new(fixture._home.path());
    let forked_log = store.fork(&fixture.session.snapshot(), None).unwrap();
    let (sender, _events) = unbounded_channel();
    let tuning = Arc::new(|_: &str, _: &str| Tuning { effort: "high".to_string(), context_limit: u64::MAX, context_window: u64::MAX, guardian: None, approval_wait: None });
    let services = Services { grants: Arc::new(GrantStore::new(fixture._home.path().join("grants.json"))), mcp: None, skills: None, search: None, sandbox: None, chat: None, runtime: None, painter: None, lsp: None };
    let forked = Session::new(forked_log, fixture.llm.clone(), tuning, services, sender, tokio::runtime::Handle::current()).unwrap();
    assert_eq!(forked.snapshot_bytes(&deck_v1).unwrap(), b"%PDF-1.4 v1");
}
