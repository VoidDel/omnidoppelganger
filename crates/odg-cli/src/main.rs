//! `odg`: run one task from the command line, without the window.
//!
//! It is the desktop app's agent on the same home directory: the same
//! settings, keys, kept approvals, MCP servers, and skills, and the session
//! it runs shows up in the app afterwards. What the agent says goes to
//! standard output; what it does, and what it asks, to standard error.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use odg_core::agent::{Services, Session, Status, Tuning, UiEvent};
use odg_core::anthropic::{AnthropicClient, DEFAULT_BASE_URL};
use odg_core::events::{Decision, Event, Question, QuestionKind, Reply};
use odg_core::grants::GrantStore;
use odg_core::llm::StreamDelta;
use odg_core::mcp::McpHub;
use odg_core::policy::Mode;
use odg_core::providers::{Router, provider};
use odg_core::session::{NewSession, SessionStore};
use odg_core::settings::{self, SettingsStore};
use serde_json::json;

const VERSION: &str = concat!("odg ", env!("CARGO_PKG_VERSION"));

const USAGE: &str = "\
odg: run tasks with the OmniDoppelganger agent from the command line.

Usage:
  odg [options] <task>
  odg --export <session-id>
  odg --land <session-id>

Options:
  -C, --workspace <dir>   The folder to work in. Default: the current folder.
  -c, --continue          Continue the most recent session in this workspace
                          (starts a new one when none exists yet).
  -r, --resume <id>       Resume an existing session by id (or unique prefix).
  -w, --worktree          Run a new session in an isolated Git worktree.
      --land <id>         Merge a worktree session's branch back into its origin.
      --json              Stream structured NDJSON events to standard output.
      --export <id>       Export a stored session as Markdown to standard output.
      --provider <id>     The model provider. Default: the one chosen in settings.
      --model <id>        The model. Default: the one chosen in settings.
      --mode <mode>       manual, edit, plan, auto, or bypass (-w). Default: from settings.
      --yes               Approve everything the agent asks for. It will run any command.
      --no                Decline everything the agent asks for. The default when
                          standard input is not a terminal.
  -V, --version           Show the version.
  -h, --help              Show this.

The agent's words go to standard output (or NDJSON events with --json); its
tool calls and questions go to standard error. Exit status: 0 when the turn
completed, 1 when it did not, 2 when the arguments are wrong.";

/// How a request for approval is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Ask on the terminal.
    Ask,
    Yes,
    No,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    task: String,
    workspace: PathBuf,
    continue_latest: bool,
    resume: Option<String>,
    worktree: bool,
    land: Option<String>,
    json: bool,
    export: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    mode: Option<Mode>,
    answer: Option<Answer>,
}

/// Read the arguments. `Err` holds what to print: the usage text for `--help`, or what is wrong.
fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        workspace: PathBuf::from("."),
        ..Options::default()
    };
    let mut words: Vec<String> = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or(format!("{name} needs a value"));
        match argument.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "-V" | "--version" => return Err(VERSION.to_string()),
            "-C" | "--workspace" => options.workspace = PathBuf::from(value("--workspace")?),
            "-c" | "--continue" => options.continue_latest = true,
            "-r" | "--resume" => options.resume = Some(value("--resume")?),
            "-w" | "--worktree" => options.worktree = true,
            "--land" => options.land = Some(value("--land")?),
            "--json" => options.json = true,
            "--export" => options.export = Some(value("--export")?),
            "--provider" => options.provider = Some(value("--provider")?),
            "--model" => options.model = Some(value("--model")?),
            "--mode" => {
                options.mode = Some(match value("--mode")?.as_str() {
                    "manual" => Mode::Manual,
                    "edit" => Mode::Edit,
                    "plan" => Mode::Plan,
                    "auto" => Mode::Auto,
                    "bypass" => Mode::Bypass,
                    other => return Err(format!("--mode is manual, edit, plan, auto, or bypass, not `{other}`")),
                });
            }
            "--yes" => options.answer = Some(Answer::Yes),
            "--no" => options.answer = Some(Answer::No),
            // Everything after `--` is the task, whatever it looks like.
            "--" => words.extend(arguments.by_ref()),
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option `{flag}`; see odg --help"));
            }
            _ => words.push(argument),
        }
    }
    if options.land.is_some() && options.export.is_some() {
        return Err("--land and --export cannot be used together".to_string());
    }
    if options.land.is_some()
        && (options.continue_latest || options.resume.is_some() || options.worktree || !words.is_empty())
    {
        return Err(
            "--land takes only a session id (and optional --json), without a task or session flags".to_string(),
        );
    }
    if options.continue_latest && options.resume.is_some() {
        return Err("--continue and --resume cannot be used together".to_string());
    }
    if options.worktree && (options.continue_latest || options.resume.is_some()) {
        return Err("--worktree starts a new session and cannot be combined with --continue or --resume".to_string());
    }
    if options.mode == Some(Mode::Bypass) && !options.worktree && !options.continue_latest && options.resume.is_none() {
        return Err("--mode bypass requires --worktree (-w) or resuming a worktree session".to_string());
    }
    options.task = words.join(" ").trim().to_string();
    if options.export.is_none() && options.land.is_none() && options.task.is_empty() {
        return Err("give the task to run; see odg --help".to_string());
    }
    Ok(options)
}

/// One line saying what a tool call does, for standard error.
fn describe(name: &str, input: &serde_json::Value) -> String {
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    let subject = match name {
        "read_file" | "edit_file" | "write_file" | "delete_file" => text("path"),
        "move_file" => format!("{} → {}", text("path"), text("to")),
        "language_server" => format!(
            "{}{}",
            text("language"),
            if input["install"] == true { " (install)" } else { "" }
        ),
        "code_nav" => {
            let line = input["line"]
                .as_u64()
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            let what = [text("symbol"), text("query")]
                .into_iter()
                .find(|what| !what.is_empty())
                .map(|what| format!(" {what}"))
                .unwrap_or_default();
            format!("{} {}{line}{what}", text("operation"), text("path"))
        }
        "search" => text("pattern"),
        "find_files" => input["glob"]
            .as_str()
            .or(input["path"].as_str())
            .unwrap_or(".")
            .to_string(),
        "batch_inspect" => format!("{} queries", input["queries"].as_array().map_or(0, Vec::len)),
        "run_command" => {
            let cwd = text("cwd");
            let command = text("command");
            if cwd.is_empty() {
                command
            } else {
                format!("[{cwd}] {command}")
            }
        }
        "fetch_url" => text("url"),
        "web_search" => text("query"),
        "load_skill" => text("name"),
        "command_output" | "stop_command" => text("job"),
        "list_mcp_resources" => text("server"),
        "read_mcp_resource" => format!("{} {}", text("server"), text("uri")),
        "present_plan" => text("title"),
        "command_input" => format!(
            "{} ← {}{}",
            text("job"),
            text("text"),
            if input["close"] == true { " (end of input)" } else { "" }
        ),
        "ask_user" => input["questions"]
            .as_array()
            .map(|questions| {
                questions
                    .iter()
                    .filter_map(|question| question["question"].as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    let subject: String = subject.lines().next().unwrap_or_default().chars().take(120).collect();
    if subject.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {subject}")
    }
}

/// Ask on the terminal whether to allow what the agent wants to do.
fn ask(reason: &str, detail: &str, session_wide: bool) -> Decision {
    let what = match reason {
        "command" => "run this command",
        "outside_command" => "run a command outside the workspace (or in .git)",
        "edit" => "change this file",
        "web" => "fetch this address",
        "search" => "search the web for",
        "mcp" => "call this MCP tool",
        "git_write" => "change this file in .git",
        "outside_read" => "read outside the workspace",
        "lsp" => "start a language server that loads the project's own code",
        "install_lsp" => "download a language server for code navigation",
        "input" => "type into a background command",
        "mcp_resource" => "read a resource of an MCP server",
        _ => "change a file outside the workspace",
    };
    let mut stderr = std::io::stderr();
    let choices = if session_wide {
        "[y] yes  [a] yes, and the like for this run  [n] no"
    } else {
        "[y] yes  [n] no"
    };
    let _ = write!(stderr, "\n? The agent wants to {what}:\n    {detail}\n  {choices}: ");
    let _ = stderr.flush();
    let mut line = String::new();
    // No answer (the input ended) is a no.
    let _ = std::io::stdin().read_line(&mut line);
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => Decision::AllowOnce,
        "a" | "all" if session_wide => Decision::AllowSession,
        _ => Decision::Deny,
    }
}

/// The answer to one question, from a line typed on the terminal: the numbers of options for a question
/// that has them (one, or for `multiple` several, split by commas or spaces), and anything else as words.
/// An empty line leaves it unanswered.
fn reply(question: &Question, line: &str) -> Reply {
    let line = line.trim();
    if line.is_empty() {
        return Reply::default();
    }
    let parts: Vec<&str> = line
        .split(|c: char| c == ',' || c == '，' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let numbers: Option<Vec<usize>> = parts
        .iter()
        .map(|part| {
            part.parse()
                .ok()
                .filter(|number| (1..=question.options.len()).contains(number))
        })
        .collect();
    match numbers {
        Some(numbers)
            if !question.options.is_empty() && (question.kind == QuestionKind::Multiple || numbers.len() == 1) =>
        {
            let mut chosen: Vec<String> = Vec::new();
            for number in numbers {
                let label = &question.options[number - 1].label;
                if !chosen.contains(label) {
                    chosen.push(label.clone());
                }
            }
            Reply {
                chosen,
                text: String::new(),
            }
        }
        _ => Reply {
            chosen: Vec::new(),
            text: line.to_string(),
        },
    }
}

/// Ask the agent's questions on the terminal, one after another.
fn ask_questions(questions: &[Question]) -> Vec<Reply> {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "\n? The agent asks:");
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let _ = writeln!(stderr, "  {}. [{}] {}", index + 1, question.header, question.question);
            for (number, option) in question.options.iter().enumerate() {
                let description = if option.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", option.description)
                };
                let _ = writeln!(stderr, "     {}) {}{description}", number + 1, option.label);
            }
            let how = match question.kind {
                QuestionKind::Single => "a number, or your own words",
                QuestionKind::Multiple => "numbers split by commas, or your own words",
                QuestionKind::Text => "your answer",
            };
            let _ = write!(stderr, "  {how} (empty to leave it to the agent): ");
            let _ = stderr.flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            reply(question, &line)
        })
        .collect()
}

fn normalize_dir(path: &std::path::Path) -> PathBuf {
    path.canonicalize()
        .map(|p| PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\").to_string()))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn resolve_session_id(store: &SessionStore, target: &str) -> Result<String, String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err("session id cannot be empty".to_string());
    }
    if store.peek(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }
    let matches: Vec<String> = store.ids().into_iter().filter(|id| id.starts_with(trimmed)).collect();
    match matches.len() {
        0 => Err(format!("session `{trimmed}` does not exist")),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(format!(
            "session prefix `{trimmed}` matches multiple sessions: {}",
            matches.join(", ")
        )),
    }
}

enum ExitOutcome {
    Completed,
    Failed,
    Interrupted,
}

enum RunError {
    Usage(String),
    Runtime(String),
}

impl From<String> for RunError {
    fn from(msg: String) -> Self {
        Self::Runtime(msg)
    }
}

struct ShutdownSignals {
    #[cfg(unix)]
    sigint: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    sigterm: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                sigint: signal(SignalKind::interrupt()).ok(),
                sigterm: signal(SignalKind::terminate()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            let wait_int = async {
                match &mut self.sigint {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let wait_term = async {
                match &mut self.sigterm {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = wait_int => {}
                _ = wait_term => {}
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {}
                Err(_) => std::future::pending::<()>().await,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_effective_mode(
    explicit_mode: Option<Mode>,
    answer: Option<Answer>,
    effective_answer: Answer,
    has_worktree: bool,
    is_cli_provider: bool,
    is_resumed: bool,
    current_session_mode: Mode,
    saved_mode: Mode,
) -> Option<Mode> {
    if let Some(mode) = explicit_mode {
        return Some(mode);
    }
    let base = if is_resumed { current_session_mode } else { saved_mode };
    if is_cli_provider {
        if answer == Some(Answer::Yes) {
            return Some(if has_worktree { Mode::Bypass } else { Mode::Auto });
        }
        if effective_answer == Answer::No && base == Mode::Manual {
            return Some(Mode::Plan);
        }
    } else if answer == Some(Answer::Yes) && base == Mode::Plan {
        return Some(if has_worktree { Mode::Bypass } else { Mode::Edit });
    }
    if !is_resumed { Some(saved_mode) } else { None }
}

async fn run(options: Options) -> Result<ExitOutcome, RunError> {
    tokio::task::spawn_blocking(|| {
        odg_core::fsutil::prune_stale_temp_files(Duration::from_secs(86_400));
    });
    let home = settings::home();
    let session_store = SessionStore::new(home.join("sessions"));
    if let Some(target) = &options.export {
        let id = resolve_session_id(&session_store, target)?;
        let events = session_store.read(&id).map_err(|e| e.to_string())?;
        print!("{}", odg_core::export::to_markdown(&events, None));
        return Ok(ExitOutcome::Completed);
    }
    if let Some(target) = &options.land {
        let id = resolve_session_id(&session_store, target)?;
        let summary = session_store.peek(&id).map_err(|e| e.to_string())?;
        let made = summary
            .worktree
            .clone()
            .ok_or_else(|| format!("session `{id}` has no isolated worktree to land"))?;
        if summary.turn_open || summary.awaiting_approval {
            return Err(RunError::Runtime(format!(
                "session `{id}` is not idle; stop or finish its running turn before landing"
            )));
        }
        if let Some(busy) = session_store.list().into_iter().find(|other| {
            other.id != id
                && (other.turn_open || other.awaiting_approval)
                && other.worktree.as_ref().is_some_and(|wt| wt.checkout == made.checkout)
        }) {
            return Err(RunError::Runtime(format!(
                "another session (`{}`) sharing worktree {} is not idle; wait before landing",
                busy.id, made.checkout
            )));
        }
        let message = format!(
            "odg: {}",
            if summary.title.is_empty() {
                "work of a session"
            } else {
                summary.title.as_str()
            }
        );
        let branch = made.branch.clone();
        let landed = tokio::task::spawn_blocking(move || odg_core::worktree::land(&made, &message))
            .await
            .map_err(|e| e.to_string())??;
        if options.json {
            println!(
                "{}",
                json!({
                    "kind": "landed",
                    "session_id": id,
                    "branch": branch,
                    "into": landed.into,
                    "commits": landed.commits,
                })
            );
        } else if landed.commits == 0 {
            println!("Nothing new to land from {branch} into {}.", landed.into);
        } else {
            println!(
                "Landed {} commit{} from {branch} into {}.",
                landed.commits,
                if landed.commits == 1 { "" } else { "s" },
                landed.into
            );
        }
        return Ok(ExitOutcome::Completed);
    }

    let workspace = options
        .workspace
        .canonicalize()
        .map_err(|error| format!("{} is not a folder to work in: {error}", options.workspace.display()))?;
    // Windows gives the canonical path a `\\?\` prefix that most programs do not expect.
    let workspace = PathBuf::from(workspace.to_string_lossy().trim_start_matches(r"\\?\").to_string());
    if !workspace.is_dir() {
        return Err(RunError::Runtime(format!("{} is not a folder", workspace.display())));
    }
    let store = Arc::new(SettingsStore::at_home());
    let saved = store.load();

    // Locate an existing session to resume if `--resume` or `--continue` was passed.
    let resumed_id = if let Some(target) = &options.resume {
        Some(resolve_session_id(&session_store, target)?)
    } else if options.continue_latest {
        session_store
            .list()
            .into_iter()
            .find(|s| {
                s.parent.is_none()
                    && s.aside.is_none()
                    && (normalize_dir(std::path::Path::new(&s.workspace)) == workspace
                        || s.worktree
                            .as_ref()
                            .is_some_and(|wt| normalize_dir(std::path::Path::new(&wt.origin)) == workspace))
            })
            .map(|s| s.id)
    } else {
        None
    };

    let existing_summary = match &resumed_id {
        Some(id) => Some(session_store.peek(id).map_err(|e| e.to_string())?),
        None => None,
    };
    if options.mode == Some(Mode::Bypass)
        && !options.worktree
        && existing_summary.as_ref().and_then(|s| s.worktree.as_ref()).is_none()
    {
        return Err(RunError::Usage(
            "--mode bypass requires --worktree (-w) or resuming a worktree session".to_string(),
        ));
    }

    let provider_id = options
        .provider
        .clone()
        .or_else(|| existing_summary.as_ref().map(|s| s.provider.clone()))
        .unwrap_or_else(|| saved.provider.clone());
    if provider(&provider_id).is_none() {
        return Err(RunError::Runtime(format!("there is no provider `{provider_id}`")));
    }
    // A model named here goes with the provider named here; otherwise keep the resumed session's or settings' pair.
    let model = match options.model.clone() {
        Some(model) => model,
        None if existing_summary.as_ref().is_some_and(|s| s.provider == provider_id) => {
            existing_summary.as_ref().unwrap().model.clone()
        }
        None if provider_id == saved.provider => saved.model.clone(),
        None => provider(&provider_id)
            .and_then(|known| known.models.first())
            .map(|(id, _)| id.to_string())
            .unwrap_or_default(),
    };
    if model.is_empty() {
        return Err(RunError::Runtime(format!(
            "name a model for `{provider_id}` with --model"
        )));
    }

    let keys = store.clone();
    let base_url = std::env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let anthropic = AnthropicClient::new(base_url, Arc::new(move || keys.api_key("anthropic")))
        .map_err(|error| error.to_string())?;
    let endpoints = store.clone();
    let llm = Arc::new(
        Router::new(anthropic, Arc::new(move |provider: &str| endpoints.endpoint(provider)))
            .map_err(|error| error.to_string())?,
    );

    // The model can only be offered the tools of servers that are connected, so they get a moment to.
    let mcp = Arc::new(McpHub::new(home.join("mcp.json"), tokio::runtime::Handle::current()));
    let lsp = Arc::new(odg_core::lsp::LspHub::new(home.join("lsp")));
    mcp.reload();
    for _ in 0..100 {
        if mcp.statuses().iter().all(|server| server.state != "connecting") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !options.json {
        for server in mcp.statuses().iter().filter(|server| server.state != "ready") {
            eprintln!(
                "! MCP server {} is not connected: {}",
                server.name,
                server.error.as_deref().unwrap_or("it is still starting")
            );
        }
    }

    let (log, session_workspace, created_worktree) = if let Some(id) = &resumed_id {
        let log = session_store.open(id).map_err(|e| e.to_string())?;
        let ws = existing_summary
            .as_ref()
            .map(|s| PathBuf::from(&s.workspace))
            .unwrap_or_else(|| workspace.clone());
        let wt = existing_summary.as_ref().and_then(|s| s.worktree.clone());
        (log, ws, wt)
    } else {
        let (session_ws, made_wt) = if options.worktree {
            let (checkout_ws, wt) = odg_core::worktree::create(&workspace, &home.join("worktrees"))?;
            (checkout_ws, Some(wt))
        } else {
            (workspace.clone(), None)
        };
        let new = NewSession {
            workspace: &session_ws,
            provider: &provider_id,
            model: &model,
            parent: None,
            worktree: made_wt.clone(),
            schedule: None,
            aside: None,
        };
        let log = session_store.create_with(new).map_err(|error| {
            if let Some(wt) = &made_wt {
                let _ = odg_core::worktree::remove(wt);
            }
            error.to_string()
        })?;
        (log, session_ws, made_wt)
    };

    let tuned = store.clone();
    let tuning = move |provider: &str, model: &str| {
        let settings = tuned.load();
        let window = tuned.context_window(&settings, provider, model).tokens;
        let (guardian, approval_wait) = (settings.guardian_model(provider, model), settings.approval_wait());
        Tuning {
            context_limit: settings.context_limit_within(window),
            context_window: window,
            effort: settings.effort,
            guardian,
            approval_wait,
        }
    };
    let searching = store.clone();
    let painting = store.clone();
    let confining = store.clone();
    let grants = Arc::new(GrantStore::new(home.join("grants.json")));
    let services = Services {
        grants: grants.clone(),
        mcp: Some(mcp.clone()),
        skills: Some(home.join("skills")),
        search: Some(Arc::new(move || searching.search_mode())),
        sandbox: Some(Arc::new(move || confining.sandbox())),
        chat: Some(home.join("闲谈")),
        runtime: Some(home.join("runtime")),
        painter: Some(Arc::new(move || painting.painter())),
        lsp: Some(lsp.clone()),
    };
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    let session = Session::new(
        log,
        llm,
        Arc::new(tuning),
        services,
        sender,
        tokio::runtime::Handle::current(),
    )
    .map_err(|error| error.to_string())?;
    if let Some(prev) = &existing_summary
        && (prev.provider != provider_id || prev.model != model)
    {
        session.set_model(&provider_id, &model)?;
    }
    let answer = options.answer.unwrap_or(if std::io::stdin().is_terminal() {
        Answer::Ask
    } else {
        Answer::No
    });
    let is_cli_provider = matches!(
        provider(&provider_id).map(|p| p.protocol),
        Some(odg_core::providers::Protocol::Cli(_))
    );
    if let Some(effective_mode) = resolve_effective_mode(
        options.mode,
        options.answer,
        answer,
        created_worktree.is_some(),
        is_cli_provider,
        existing_summary.is_some(),
        session.mode(),
        saved.mode,
    ) {
        session.set_mode(effective_mode);
    }
    let mut untrusted = session.untrusted_project_resources();
    if !session.is_project_trusted() {
        for cfg in odg_core::cli_agents::detect_untrusted_cli_configs(&session_workspace) {
            if !untrusted.contains(&cfg) {
                untrusted.push(cfg);
            }
        }
    }
    if !untrusted.is_empty() {
        if answer == Answer::Yes {
            let _ = session.set_project_trust(true);
        } else if !options.json {
            eprintln!(
                "! untrusted project configuration skipped ({}): pass --yes or trust this workspace in the app",
                untrusted.join(", ")
            );
        }
    }

    // Expand `/template args` if `task` invokes an installed prompt template.
    let task_to_send = if let Some(rest) = options.task.strip_prefix('/').filter(|r| !r.starts_with('/')) {
        let (tpl_name, tpl_args) = match rest.find(char::is_whitespace) {
            Some(pos) => (&rest[..pos], rest[pos..].trim()),
            None => (rest, ""),
        };
        let grant_root = created_worktree
            .as_ref()
            .map_or_else(|| session_workspace.clone(), |wt| PathBuf::from(&wt.origin));
        let proj_prompts = grants
            .is_trusted_workspace(&grant_root)
            .then(|| session_workspace.join(".odg").join("prompts"));
        odg_core::templates::expand_named(&home.join("prompts"), proj_prompts.as_deref(), tpl_name, tpl_args)
            .unwrap_or_else(|_| options.task.clone())
    } else {
        options.task.clone()
    };

    if options.json {
        println!(
            "{}",
            json!({
                "kind": "session",
                "session_id": session.id(),
                "resumed": resumed_id.is_some(),
                "provider": provider_id,
                "model": model,
                "workspace": session_workspace.display().to_string(),
                "worktree": created_worktree,
            })
        );
    } else {
        let action = if resumed_id.is_some() { "resumed" } else { "session" };
        eprintln!(
            "odg: {provider_id} {model}, in {} ({action} {})",
            session_workspace.display(),
            session.id()
        );
    }
    let mut signals = ShutdownSignals::new();
    session.send(task_to_send);

    // Whether the agent's words on standard output still lack their closing newline.
    let mut mid_line = false;
    let mut completed = false;
    let mut interrupted = false;
    let mut force_exit_deadline: Option<tokio::time::Instant> = None;
    let mut stdout = std::io::stdout();
    let close_line = |mid_line: &mut bool| {
        if std::mem::take(mid_line) {
            println!();
        }
    };
    // The names of the subagents the agent has set off, by session id: their events arrive on the same channel.
    let mut subagents: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    loop {
        let wait_deadline = async {
            match force_exit_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        let event = tokio::select! {
            biased;
            _ = signals.recv() => {
                close_line(&mut mid_line);
                if !interrupted {
                    interrupted = true;
                    eprintln!("\n! stopping the turn...");
                    session.cancel();
                    force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                    continue;
                } else {
                    break;
                }
            }
            _ = wait_deadline => {
                break;
            }
            maybe_event = events.recv() => {
                match maybe_event {
                    Some(ev) => ev,
                    None => break,
                }
            }
        };
        if options.json {
            if let Ok(line) = serde_json::to_string(&event) {
                println!("{line}");
                let _ = stdout.flush();
            }
            match &event {
                UiEvent::Log { session_id, envelope } if session_id != session.id() => {
                    if let Event::ApprovalRequested {
                        tool_use_id,
                        reason,
                        detail,
                        grant,
                        ..
                    } = &envelope.event
                    {
                        let (r, d, g) = (reason.clone(), detail.clone(), grant.is_some());
                        let decision = if interrupted {
                            Decision::Deny
                        } else {
                            match answer {
                                Answer::Yes => Decision::AllowOnce,
                                Answer::No => Decision::Deny,
                                Answer::Ask => {
                                    let prompt_fut = tokio::task::spawn_blocking(move || ask(&r, &d, g));
                                    tokio::select! {
                                        biased;
                                        _ = signals.recv() => {
                                            interrupted = true;
                                            eprintln!("\n! stopping the turn...");
                                            session.cancel();
                                            force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                            Decision::Deny
                                        }
                                        res = prompt_fut => res.unwrap_or(Decision::Deny),
                                    }
                                }
                            }
                        };
                        if let Some(child) = session.child(session_id) {
                            child.resolve_approval(tool_use_id, decision);
                        }
                    }
                }
                UiEvent::Log { envelope, .. } => match &envelope.event {
                    Event::ApprovalRequested {
                        tool_use_id,
                        reason,
                        detail,
                        grant,
                        ..
                    } => {
                        let (r, d, g) = (reason.clone(), detail.clone(), grant.is_some());
                        let decision = if interrupted {
                            Decision::Deny
                        } else {
                            match answer {
                                Answer::Yes => Decision::AllowOnce,
                                Answer::No => Decision::Deny,
                                Answer::Ask => {
                                    let prompt_fut = tokio::task::spawn_blocking(move || ask(&r, &d, g));
                                    tokio::select! {
                                        biased;
                                        _ = signals.recv() => {
                                            interrupted = true;
                                            eprintln!("\n! stopping the turn...");
                                            session.cancel();
                                            force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                            Decision::Deny
                                        }
                                        res = prompt_fut => res.unwrap_or(Decision::Deny),
                                    }
                                }
                            }
                        };
                        session.resolve_approval(tool_use_id, decision);
                    }
                    Event::QuestionAsked {
                        tool_use_id, questions, ..
                    } => {
                        let qs = questions.clone();
                        let answers = if interrupted || answer != Answer::Ask {
                            None
                        } else {
                            let prompt_fut = tokio::task::spawn_blocking(move || ask_questions(&qs));
                            tokio::select! {
                                biased;
                                _ = signals.recv() => {
                                    interrupted = true;
                                    eprintln!("\n! stopping the turn...");
                                    session.cancel();
                                    force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                    None
                                }
                                res = prompt_fut => res.ok(),
                            }
                        };
                        session.answer_questions(tool_use_id, answers);
                    }
                    Event::TurnEnd { reason, .. } => {
                        completed = reason == "completed";
                    }
                    _ => {}
                },
                UiEvent::Status {
                    session_id,
                    status: Status::Idle,
                } if session_id == session.id() => break,
                _ => {}
            }
            continue;
        }
        let from = match &event {
            UiEvent::Log { session_id, .. }
            | UiEvent::Stream { session_id, .. }
            | UiEvent::Status { session_id, .. } => session_id.clone(),
        };
        // A subagent's work is shown under its name on standard error; its words are for the agent, not the output.
        if from != session.id() {
            let name = subagents.get(&from).cloned().unwrap_or_else(|| "subagent".to_string());
            if let UiEvent::Log { envelope, .. } = event {
                match envelope.event {
                    Event::AssistantMessage { content, .. } => {
                        close_line(&mut mid_line);
                        for block in content.iter().filter(|block| block["type"] == "tool_use") {
                            eprintln!(
                                "  [{name}] > {}",
                                describe(block["name"].as_str().unwrap_or_default(), &block["input"])
                            );
                        }
                    }
                    Event::ApprovalRequested {
                        tool_use_id,
                        reason,
                        detail,
                        grant,
                        ..
                    } => {
                        close_line(&mut mid_line);
                        let decision = if interrupted {
                            Decision::Deny
                        } else {
                            eprintln!("  [{name}] asks:");
                            match answer {
                                Answer::Yes => Decision::AllowOnce,
                                Answer::No => Decision::Deny,
                                Answer::Ask => {
                                    let prompt_fut =
                                        tokio::task::spawn_blocking(move || ask(&reason, &detail, grant.is_some()));
                                    tokio::select! {
                                        biased;
                                        _ = signals.recv() => {
                                            interrupted = true;
                                            eprintln!("\n! stopping the turn...");
                                            session.cancel();
                                            force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                            Decision::Deny
                                        }
                                        res = prompt_fut => res.unwrap_or(Decision::Deny),
                                    }
                                }
                            }
                        };
                        if let Some(child) = session.child(&from) {
                            child.resolve_approval(&tool_use_id, decision);
                        }
                    }
                    Event::TurnEnd { reason, error, .. } if reason != "completed" => {
                        eprintln!("  [{name}] ! stopped: {}", error.unwrap_or(reason))
                    }
                    _ => {}
                }
            }
            continue;
        }
        match event {
            UiEvent::Stream {
                delta: StreamDelta::TextDelta { text, .. },
                ..
            } => {
                print!("{text}");
                let _ = stdout.flush();
                mid_line = !text.ends_with('\n');
            }
            // The request is being sent again: what was printed of it is void.
            UiEvent::Stream {
                delta: StreamDelta::Reset,
                ..
            } => {
                close_line(&mut mid_line);
                eprintln!("! the connection dropped; asking the model again");
            }
            UiEvent::Stream { .. } => {}
            UiEvent::Log { envelope, .. } => match envelope.event {
                Event::AssistantMessage { content, .. } => {
                    close_line(&mut mid_line);
                    for block in content.iter().filter(|block| block["type"] == "tool_use") {
                        eprintln!(
                            "> {}",
                            describe(block["name"].as_str().unwrap_or_default(), &block["input"])
                        );
                        // A plan presented is the turn's answer: it goes to the output, as the agent's words do.
                        if block["name"] == "present_plan"
                            && let Some(plan) = block["input"]["plan"].as_str()
                        {
                            println!("{}", plan.trim_end());
                        }
                    }
                }
                Event::ApprovalRequested {
                    tool_use_id,
                    reason,
                    detail,
                    grant,
                    ..
                } => {
                    let decision = if interrupted {
                        Decision::Deny
                    } else {
                        match answer {
                            Answer::Yes => Decision::AllowOnce,
                            Answer::No => Decision::Deny,
                            Answer::Ask => {
                                let prompt_fut =
                                    tokio::task::spawn_blocking(move || ask(&reason, &detail, grant.is_some()));
                                tokio::select! {
                                    biased;
                                    _ = signals.recv() => {
                                        interrupted = true;
                                        eprintln!("\n! stopping the turn...");
                                        session.cancel();
                                        force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                        Decision::Deny
                                    }
                                    res = prompt_fut => res.unwrap_or(Decision::Deny),
                                }
                            }
                        }
                    };
                    if !interrupted && answer != Answer::Ask {
                        eprintln!(
                            "  {} (--{})",
                            if decision == Decision::Deny {
                                "declined"
                            } else {
                                "approved"
                            },
                            if answer == Answer::Yes { "yes" } else { "no" }
                        );
                    }
                    session.resolve_approval(&tool_use_id, decision);
                }
                Event::QuestionAsked {
                    tool_use_id, questions, ..
                } => {
                    let answers = if interrupted {
                        None
                    } else if answer == Answer::Ask {
                        let prompt_fut = tokio::task::spawn_blocking(move || ask_questions(&questions));
                        tokio::select! {
                            biased;
                            _ = signals.recv() => {
                                interrupted = true;
                                eprintln!("\n! stopping the turn...");
                                session.cancel();
                                force_exit_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                                None
                            }
                            res = prompt_fut => res.ok(),
                        }
                    } else {
                        eprintln!(
                            "  nobody to answer (--{}); the agent decides",
                            if answer == Answer::Yes { "yes" } else { "no" }
                        );
                        None
                    };
                    session.answer_questions(&tool_use_id, answers);
                }
                Event::ToolResult { content, is_error, .. } => {
                    let first: String = content.lines().next().unwrap_or_default().chars().take(160).collect();
                    eprintln!("  {} {first}", if is_error { "✗" } else { "✓" });
                }
                Event::SubagentStarted { session, name, .. } => {
                    eprintln!("  [{name}] set off");
                    subagents.insert(session, name);
                }
                Event::ContextCompacted { tokens_before, .. } => {
                    eprintln!("! the conversation reached about {tokens_before} tokens and was summarized")
                }
                Event::TurnEnd { reason, error, .. } => {
                    completed = reason == "completed";
                    if !completed && !interrupted {
                        eprintln!("! the turn ended: {}", error.unwrap_or(reason));
                    }
                }
                _ => {}
            },
            UiEvent::Status {
                status: Status::Idle, ..
            } => break,
            UiEvent::Status { .. } => {}
        }
    }
    close_line(&mut mid_line);
    // Nothing the run started outlives it.
    session.kill_jobs();
    mcp.shutdown();
    lsp.shutdown();
    if interrupted {
        return Ok(ExitOutcome::Interrupted);
    }
    Ok(if completed {
        ExitOutcome::Completed
    } else {
        ExitOutcome::Failed
    })
}

fn main() -> ExitCode {
    // Before the runtime, which starts threads of its own: the `PATH` this was started with may leave out where the
    // programs the agent starts live.
    odg_core::env::adopt_shell_path();
    runtime_main()
}

#[tokio::main]
async fn runtime_main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) if message == USAGE || message == VERSION => {
            println!("{message}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("odg: {message}");
            return ExitCode::from(2);
        }
    };
    match run(options).await {
        Ok(ExitOutcome::Completed) => ExitCode::SUCCESS,
        Ok(ExitOutcome::Failed) => ExitCode::FAILURE,
        Ok(ExitOutcome::Interrupted) => ExitCode::from(130),
        Err(RunError::Usage(message)) => {
            eprintln!("odg: {message}");
            ExitCode::from(2)
        }
        Err(RunError::Runtime(message)) => {
            eprintln!("odg: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_an_answer_typed_on_the_terminal() {
        use odg_core::events::Choice;
        let question = |kind: QuestionKind| Question {
            header: "方案".into(),
            question: "用哪个？".into(),
            kind,
            options: ["一", "二", "三"]
                .iter()
                .map(|label| Choice {
                    label: label.to_string(),
                    description: String::new(),
                })
                .collect(),
        };
        let (single, multiple) = (question(QuestionKind::Single), question(QuestionKind::Multiple));
        let chosen = |labels: &[&str]| Reply {
            chosen: labels.iter().map(|label| label.to_string()).collect(),
            text: String::new(),
        };
        let words = |text: &str| Reply {
            chosen: Vec::new(),
            text: text.to_string(),
        };
        assert_eq!(reply(&single, " 2\n"), chosen(&["二"]));
        assert_eq!(reply(&multiple, "3, 1，3 "), chosen(&["三", "一"]));
        // Several numbers for one choice, a number out of range, or words: taken as words.
        assert_eq!(reply(&single, "1 2"), words("1 2"));
        assert_eq!(reply(&single, "4"), words("4"));
        assert_eq!(reply(&single, "都不要"), words("都不要"));
        assert_eq!(
            reply(
                &Question {
                    kind: QuestionKind::Text,
                    options: Vec::new(),
                    ..single.clone()
                },
                "2"
            ),
            words("2")
        );
        assert_eq!(reply(&single, "  \n"), Reply::default());
    }

    use super::*;

    fn parsed(arguments: &[&str]) -> Result<Options, String> {
        parse(arguments.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn reads_the_task_and_its_options() {
        let options = parsed(&["-C", "proj", "--mode", "auto", "--yes", "fix", "the", "lexer"]).unwrap();
        assert_eq!(
            options,
            Options {
                task: "fix the lexer".into(),
                workspace: "proj".into(),
                mode: Some(Mode::Auto),
                answer: Some(Answer::Yes),
                ..Options::default()
            }
        );
        let named = parsed(&["--provider", "zhipu", "--model", "glm-5.3", "--no", "hello"]).unwrap();
        assert_eq!(
            (named.provider.as_deref(), named.model.as_deref(), named.answer),
            (Some("zhipu"), Some("glm-5.3"), Some(Answer::No))
        );
        let resumed = parsed(&["-c", "--json", "follow", "up"]).unwrap();
        assert!(resumed.continue_latest && resumed.json && resumed.task == "follow up");
        let worktree = parsed(&["-w", "try", "refactor"]).unwrap();
        assert!(worktree.worktree && worktree.task == "try refactor");
        let export = parsed(&["--export", "0190abc"]).unwrap();
        assert_eq!(export.export.as_deref(), Some("0190abc"));
        let land = parsed(&["--land", "0190def", "--json"]).unwrap();
        assert_eq!((land.land.as_deref(), land.json), (Some("0190def"), true));
        assert!(parsed(&["--land", "0190def", "--export", "0190def"]).is_err());
        assert!(parsed(&["--land", "0190def", "extra"]).is_err());
        assert!(parsed(&["--land"]).unwrap_err().contains("needs a value"));
        // After `--` everything is the task, even what looks like an option.
        assert_eq!(parsed(&["--", "explain", "--mode"]).unwrap().task, "explain --mode");

        assert_eq!(parsed(&["--help"]).unwrap_err(), USAGE);
        assert_eq!(parsed(&["--version"]).unwrap_err(), VERSION);
        assert_eq!(parsed(&["-V"]).unwrap_err(), VERSION);
        assert!(parsed(&[]).unwrap_err().contains("give the task"));
        assert!(
            parsed(&["-c", "-r", "abc", "hi"])
                .unwrap_err()
                .contains("cannot be used together")
        );
        assert!(parsed(&["-w", "-c", "hi"]).unwrap_err().contains("cannot be combined"));
        assert!(
            parsed(&["--mode", "fast", "x"])
                .unwrap_err()
                .contains("manual, edit, plan, auto, or bypass")
        );
        assert!(
            parsed(&["--mode", "bypass", "x"])
                .unwrap_err()
                .contains("requires --worktree")
        );
        let wt_bypass = parsed(&["-w", "--mode", "bypass", "ship", "it"]).unwrap();
        assert_eq!((wt_bypass.worktree, wt_bypass.mode), (true, Some(Mode::Bypass)));
        assert_eq!(
            parsed(&["-r", "ses1", "--mode", "bypass", "cont"]).unwrap().mode,
            Some(Mode::Bypass)
        );
        assert!(parsed(&["--model"]).unwrap_err().contains("needs a value"));
        assert!(parsed(&["--bogus", "x"]).unwrap_err().contains("unknown option"));
    }

    #[test]
    fn promotes_effective_mode_for_external_clis_when_yes_is_passed() {
        // External CLI + `--yes` without explicit `--mode`: Auto without worktree, Bypass with worktree.
        assert_eq!(
            resolve_effective_mode(
                None,
                Some(Answer::Yes),
                Answer::Yes,
                false,
                true,
                false,
                Mode::Edit,
                Mode::Manual
            ),
            Some(Mode::Auto)
        );
        assert_eq!(
            resolve_effective_mode(
                None,
                Some(Answer::Yes),
                Answer::Yes,
                true,
                true,
                false,
                Mode::Edit,
                Mode::Edit
            ),
            Some(Mode::Bypass)
        );
        // Explicit `--mode` always wins over `--yes`.
        assert_eq!(
            resolve_effective_mode(
                Some(Mode::Plan),
                Some(Answer::Yes),
                Answer::Yes,
                true,
                true,
                false,
                Mode::Edit,
                Mode::Edit
            ),
            Some(Mode::Plan)
        );
        // External CLI + `--no` on a Manual default avoids hanging for interactive approval.
        assert_eq!(
            resolve_effective_mode(
                None,
                Some(Answer::No),
                Answer::No,
                false,
                true,
                false,
                Mode::Manual,
                Mode::Manual
            ),
            Some(Mode::Plan)
        );
        // Native API provider keeps `saved_mode` (`Edit`) so `--yes` still answers `ApprovalRequested` directly.
        assert_eq!(
            resolve_effective_mode(
                None,
                Some(Answer::Yes),
                Answer::Yes,
                false,
                false,
                false,
                Mode::Edit,
                Mode::Edit
            ),
            Some(Mode::Edit)
        );
    }

    #[test]
    fn describes_a_tool_call_in_one_line() {
        let json = |text: &str| -> serde_json::Value { text.parse().unwrap() };
        assert_eq!(
            describe("run_command", &json(r#"{"command": "cargo test\n# second line"}"#)),
            "run_command  cargo test"
        );
        assert_eq!(
            describe(
                "run_command",
                &json(r#"{"command": "cargo test", "cwd": "crates/odg-core"}"#)
            ),
            "run_command  [crates/odg-core] cargo test"
        );
        assert_eq!(
            describe("find_files", &json(r#"{"glob": "**/*.rs"}"#)),
            "find_files  **/*.rs"
        );
        assert_eq!(describe("update_todos", &json("{}")), "update_todos");
        assert_eq!(
            describe("move_file", &json(r#"{"path": "a.txt", "to": "lib/a.txt"}"#)),
            "move_file  a.txt → lib/a.txt"
        );
        assert_eq!(
            describe(
                "code_nav",
                &json(r#"{"operation": "definition", "path": "src/a.rs", "line": 12, "symbol": "helper"}"#)
            ),
            "code_nav  definition src/a.rs:12 helper"
        );
    }
}
