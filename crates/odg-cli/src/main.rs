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
use odg_core::session::SessionStore;
use odg_core::settings::{self, SettingsStore};

const USAGE: &str = "\
odg: run one task with the OmniDoppelganger agent, without the window.

Usage: odg [options] <task>

Options:
  -C, --workspace <dir>   The folder to work in. Default: the current folder.
      --provider <id>     The model provider. Default: the one chosen in settings.
      --model <id>        The model. Default: the one chosen in settings.
      --mode <mode>       manual, edit, plan, or auto. Default: the one chosen in settings.
      --yes               Approve everything the agent asks for. It will run any command.
      --no                Decline everything the agent asks for. The default when
                          standard input is not a terminal.
  -h, --help              Show this.

The agent's words go to standard output; its tool calls and questions to
standard error. Exit status: 0 when the turn completed, 1 when it did not,
2 when the arguments are wrong.";

/// How a request for approval is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Ask on the terminal.
    Ask,
    Yes,
    No,
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    task: String,
    workspace: PathBuf,
    provider: Option<String>,
    model: Option<String>,
    mode: Option<Mode>,
    answer: Option<Answer>,
}

/// Read the arguments. `Err` holds what to print: the usage text for `--help`, or what is wrong.
fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options { task: String::new(), workspace: PathBuf::from("."), provider: None, model: None, mode: None, answer: None };
    let mut words: Vec<String> = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or(format!("{name} needs a value"));
        match argument.as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "-C" | "--workspace" => options.workspace = PathBuf::from(value("--workspace")?),
            "--provider" => options.provider = Some(value("--provider")?),
            "--model" => options.model = Some(value("--model")?),
            "--mode" => {
                options.mode = Some(match value("--mode")?.as_str() {
                    "manual" => Mode::Manual,
                    "edit" => Mode::Edit,
                    "plan" => Mode::Plan,
                    "auto" => Mode::Auto,
                    other => return Err(format!("--mode is manual, edit, plan, or auto, not `{other}`")),
                });
            }
            "--yes" => options.answer = Some(Answer::Yes),
            "--no" => options.answer = Some(Answer::No),
            // Everything after `--` is the task, whatever it looks like.
            "--" => words.extend(arguments.by_ref()),
            flag if flag.starts_with('-') && flag.len() > 1 => return Err(format!("unknown option `{flag}`; see odg --help")),
            _ => words.push(argument),
        }
    }
    options.task = words.join(" ").trim().to_string();
    if options.task.is_empty() {
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
        "language_server" => format!("{}{}", text("language"), if input["install"] == true { " (install)" } else { "" }),
        "code_nav" => {
            let line = input["line"].as_u64().map(|line| format!(":{line}")).unwrap_or_default();
            let what = [text("symbol"), text("query")].into_iter().find(|what| !what.is_empty()).map(|what| format!(" {what}")).unwrap_or_default();
            format!("{} {}{line}{what}", text("operation"), text("path"))
        }
        "search" => text("pattern"),
        "find_files" => input["glob"].as_str().or(input["path"].as_str()).unwrap_or(".").to_string(),
        "run_command" => {
            let cwd = text("cwd");
            let command = text("command");
            if cwd.is_empty() { command } else { format!("[{cwd}] {command}") }
        }
        "fetch_url" => text("url"),
        "web_search" => text("query"),
        "load_skill" => text("name"),
        "command_output" | "stop_command" => text("job"),
        "list_mcp_resources" => text("server"),
        "read_mcp_resource" => format!("{} {}", text("server"), text("uri")),
        "present_plan" => text("title"),
        "command_input" => format!("{} ← {}{}", text("job"), text("text"), if input["close"] == true { " (end of input)" } else { "" }),
        "ask_user" => input["questions"].as_array().map(|questions| questions.iter().filter_map(|question| question["question"].as_str()).collect::<Vec<_>>().join(" / ")).unwrap_or_default(),
        _ => String::new(),
    };
    let subject: String = subject.lines().next().unwrap_or_default().chars().take(120).collect();
    if subject.is_empty() { name.to_string() } else { format!("{name}  {subject}") }
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
    let choices = if session_wide { "[y] yes  [a] yes, and the like for this run  [n] no" } else { "[y] yes  [n] no" };
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
    let parts: Vec<&str> = line.split(|c: char| c == ',' || c == '，' || c.is_whitespace()).filter(|part| !part.is_empty()).collect();
    let numbers: Option<Vec<usize>> = parts.iter().map(|part| part.parse().ok().filter(|number| (1..=question.options.len()).contains(number))).collect();
    match numbers {
        Some(numbers) if !question.options.is_empty() && (question.kind == QuestionKind::Multiple || numbers.len() == 1) => {
            let mut chosen: Vec<String> = Vec::new();
            for number in numbers {
                let label = &question.options[number - 1].label;
                if !chosen.contains(label) {
                    chosen.push(label.clone());
                }
            }
            Reply { chosen, text: String::new() }
        }
        _ => Reply { chosen: Vec::new(), text: line.to_string() },
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
                let description = if option.description.is_empty() { String::new() } else { format!(" — {}", option.description) };
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

async fn run(options: Options) -> Result<bool, String> {
    let workspace = options.workspace.canonicalize().map_err(|error| format!("{} is not a folder to work in: {error}", options.workspace.display()))?;
    // Windows gives the canonical path a `\\?\` prefix that most programs do not expect.
    let workspace = PathBuf::from(workspace.to_string_lossy().trim_start_matches(r"\\?\").to_string());
    if !workspace.is_dir() {
        return Err(format!("{} is not a folder", workspace.display()));
    }
    let home = settings::home();
    let store = Arc::new(SettingsStore::at_home());
    let saved = store.load();
    let provider_id = options.provider.unwrap_or_else(|| saved.provider.clone());
    if provider(&provider_id).is_none() {
        return Err(format!("there is no provider `{provider_id}`"));
    }
    // A model named here goes with the provider named here; otherwise the settings' own pair is used.
    let model = match options.model {
        Some(model) => model,
        None if provider_id == saved.provider => saved.model.clone(),
        None => provider(&provider_id).and_then(|known| known.models.first()).map(|(id, _)| id.to_string()).unwrap_or_default(),
    };
    if model.is_empty() {
        return Err(format!("name a model for `{provider_id}` with --model"));
    }

    let keys = store.clone();
    let base_url = std::env::var("ANTHROPIC_BASE_URL").ok().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let anthropic = AnthropicClient::new(base_url, Arc::new(move || keys.api_key("anthropic"))).map_err(|error| error.to_string())?;
    let endpoints = store.clone();
    let llm = Arc::new(Router::new(anthropic, Arc::new(move |provider: &str| endpoints.endpoint(provider))).map_err(|error| error.to_string())?);

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
    for server in mcp.statuses().iter().filter(|server| server.state != "ready") {
        eprintln!("! MCP server {} is not connected: {}", server.name, server.error.as_deref().unwrap_or("it is still starting"));
    }

    let log = SessionStore::new(home.join("sessions")).create(&workspace, &provider_id, &model).map_err(|error| error.to_string())?;
    let tuned = store.clone();
    let tuning = move |provider: &str, model: &str| {
        let settings = tuned.load();
        let window = tuned.context_window(&settings, provider, model).tokens;
        let (guardian, approval_wait) = (settings.guardian_model(provider, model), settings.approval_wait());
        Tuning { context_limit: settings.context_limit_within(window), context_window: window, effort: settings.effort, guardian, approval_wait }
    };
    let searching = store.clone();
    let painting = store.clone();
    let confining = store.clone();
    let services = Services {
        grants: Arc::new(GrantStore::new(home.join("grants.json"))),
        mcp: Some(mcp.clone()),
        skills: Some(home.join("skills")),
        search: Some(Arc::new(move || searching.search())),
        sandbox: Some(Arc::new(move || confining.sandbox())),
        chat: Some(home.join("闲谈")),
        runtime: Some(home.join("runtime")),
        painter: Some(Arc::new(move || painting.painter())),
        lsp: Some(lsp.clone()),
    };
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    let session = Session::new(log, llm, Arc::new(tuning), services, sender, tokio::runtime::Handle::current()).map_err(|error| error.to_string())?;
    session.set_mode(options.mode.unwrap_or(saved.mode));
    let answer = options.answer.unwrap_or(if std::io::stdin().is_terminal() { Answer::Ask } else { Answer::No });
    let untrusted = session.untrusted_project_resources();
    if !untrusted.is_empty() {
        if answer == Answer::Yes {
            let _ = session.set_project_trust(true);
        } else {
            eprintln!(
                "! untrusted project configuration skipped ({}): pass --yes or trust this workspace in the app",
                untrusted.join(", ")
            );
        }
    }
    eprintln!("odg: {provider_id} {model}, in {} (session {})", workspace.display(), session.id());
    session.send(options.task);

    // Whether the agent's words on standard output still lack their closing newline.
    let mut mid_line = false;
    let mut completed = false;
    let mut stdout = std::io::stdout();
    let close_line = |mid_line: &mut bool| {
        if std::mem::take(mid_line) {
            println!();
        }
    };
    // The names of the subagents the agent has set off, by session id: their events arrive on the same channel.
    let mut subagents: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    while let Some(event) = events.recv().await {
        let from = match &event {
            UiEvent::Log { session_id, .. } | UiEvent::Stream { session_id, .. } | UiEvent::Status { session_id, .. } => session_id.clone(),
        };
        // A subagent's work is shown under its name on standard error; its words are for the agent, not the output.
        if from != session.id() {
            let name = subagents.get(&from).cloned().unwrap_or_else(|| "subagent".to_string());
            if let UiEvent::Log { envelope, .. } = event {
                match envelope.event {
                    Event::AssistantMessage { content, .. } => {
                        close_line(&mut mid_line);
                        for block in content.iter().filter(|block| block["type"] == "tool_use") {
                            eprintln!("  [{name}] > {}", describe(block["name"].as_str().unwrap_or_default(), &block["input"]));
                        }
                    }
                    Event::ApprovalRequested { tool_use_id, reason, detail, grant, .. } => {
                        close_line(&mut mid_line);
                        eprintln!("  [{name}] asks:");
                        let decision = match answer {
                            Answer::Yes => Decision::AllowOnce,
                            Answer::No => Decision::Deny,
                            Answer::Ask => tokio::task::spawn_blocking(move || ask(&reason, &detail, grant.is_some())).await.unwrap_or(Decision::Deny),
                        };
                        if let Some(child) = session.child(&from) {
                            child.resolve_approval(&tool_use_id, decision);
                        }
                    }
                    Event::TurnEnd { reason, error, .. } if reason != "completed" => eprintln!("  [{name}] ! stopped: {}", error.unwrap_or(reason)),
                    _ => {}
                }
            }
            continue;
        }
        match event {
            UiEvent::Stream { delta: StreamDelta::TextDelta { text, .. }, .. } => {
                print!("{text}");
                let _ = stdout.flush();
                mid_line = !text.ends_with('\n');
            }
            // The request is being sent again: what was printed of it is void.
            UiEvent::Stream { delta: StreamDelta::Reset, .. } => {
                close_line(&mut mid_line);
                eprintln!("! the connection dropped; asking the model again");
            }
            UiEvent::Stream { .. } => {}
            UiEvent::Log { envelope, .. } => match envelope.event {
                Event::AssistantMessage { content, .. } => {
                    close_line(&mut mid_line);
                    for block in content.iter().filter(|block| block["type"] == "tool_use") {
                        eprintln!("> {}", describe(block["name"].as_str().unwrap_or_default(), &block["input"]));
                        // A plan presented is the turn's answer: it goes to the output, as the agent's words do.
                        if block["name"] == "present_plan"
                            && let Some(plan) = block["input"]["plan"].as_str()
                        {
                            println!("{}", plan.trim_end());
                        }
                    }
                }
                Event::ApprovalRequested { tool_use_id, reason, detail, grant, .. } => {
                    let decision = match answer {
                        Answer::Yes => Decision::AllowOnce,
                        Answer::No => Decision::Deny,
                        Answer::Ask => tokio::task::spawn_blocking(move || ask(&reason, &detail, grant.is_some())).await.unwrap_or(Decision::Deny),
                    };
                    if answer != Answer::Ask {
                        eprintln!("  {} (--{})", if decision == Decision::Deny { "declined" } else { "approved" }, if answer == Answer::Yes { "yes" } else { "no" });
                    }
                    session.resolve_approval(&tool_use_id, decision);
                }
                Event::QuestionAsked { tool_use_id, questions, .. } => {
                    let answers = if answer == Answer::Ask {
                        tokio::task::spawn_blocking(move || ask_questions(&questions)).await.ok()
                    } else {
                        eprintln!("  nobody to answer (--{}); the agent decides", if answer == Answer::Yes { "yes" } else { "no" });
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
                Event::ContextCompacted { tokens_before, .. } => eprintln!("! the conversation reached about {tokens_before} tokens and was summarized"),
                Event::TurnEnd { reason, error, .. } => {
                    completed = reason == "completed";
                    if !completed {
                        eprintln!("! the turn ended: {}", error.unwrap_or(reason));
                    }
                }
                _ => {}
            },
            UiEvent::Status { status: Status::Idle, .. } => break,
            UiEvent::Status { .. } => {}
        }
    }
    close_line(&mut mid_line);
    // Nothing the run started outlives it.
    session.kill_jobs();
    mcp.shutdown();
    lsp.shutdown();
    Ok(completed)
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
        Err(message) if message == USAGE => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("odg: {message}");
            return ExitCode::from(2);
        }
    };
    match run(options).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
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
            options: ["一", "二", "三"].iter().map(|label| Choice { label: label.to_string(), description: String::new() }).collect(),
        };
        let (single, multiple) = (question(QuestionKind::Single), question(QuestionKind::Multiple));
        let chosen = |labels: &[&str]| Reply { chosen: labels.iter().map(|label| label.to_string()).collect(), text: String::new() };
        let words = |text: &str| Reply { chosen: Vec::new(), text: text.to_string() };
        assert_eq!(reply(&single, " 2\n"), chosen(&["二"]));
        assert_eq!(reply(&multiple, "3, 1，3 "), chosen(&["三", "一"]));
        // Several numbers for one choice, a number out of range, or words: taken as words.
        assert_eq!(reply(&single, "1 2"), words("1 2"));
        assert_eq!(reply(&single, "4"), words("4"));
        assert_eq!(reply(&single, "都不要"), words("都不要"));
        assert_eq!(reply(&Question { kind: QuestionKind::Text, options: Vec::new(), ..single.clone() }, "2"), words("2"));
        assert_eq!(reply(&single, "  \n"), Reply::default());
    }

    use super::*;

    fn parsed(arguments: &[&str]) -> Result<Options, String> {
        parse(arguments.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn reads_the_task_and_its_options() {
        let options = parsed(&["-C", "proj", "--mode", "auto", "--yes", "fix", "the", "lexer"]).unwrap();
        assert_eq!(options, Options { task: "fix the lexer".into(), workspace: "proj".into(), provider: None, model: None, mode: Some(Mode::Auto), answer: Some(Answer::Yes) });
        let named = parsed(&["--provider", "zhipu", "--model", "glm-5.3", "--no", "hello"]).unwrap();
        assert_eq!((named.provider.as_deref(), named.model.as_deref(), named.answer), (Some("zhipu"), Some("glm-5.3"), Some(Answer::No)));
        // After `--` everything is the task, even what looks like an option.
        assert_eq!(parsed(&["--", "explain", "--mode"]).unwrap().task, "explain --mode");

        assert_eq!(parsed(&["--help"]).unwrap_err(), USAGE);
        assert!(parsed(&[]).unwrap_err().contains("give the task"));
        assert!(parsed(&["--mode", "fast", "x"]).unwrap_err().contains("manual, edit, plan, or auto"));
        assert!(parsed(&["--model"]).unwrap_err().contains("needs a value"));
        assert!(parsed(&["--bogus", "x"]).unwrap_err().contains("unknown option"));
    }

    #[test]
    fn describes_a_tool_call_in_one_line() {
        let json = |text: &str| -> serde_json::Value { text.parse().unwrap() };
        assert_eq!(describe("run_command", &json(r#"{"command": "cargo test\n# second line"}"#)), "run_command  cargo test");
        assert_eq!(describe("run_command", &json(r#"{"command": "cargo test", "cwd": "crates/odg-core"}"#)), "run_command  [crates/odg-core] cargo test");
        assert_eq!(describe("find_files", &json(r#"{"glob": "**/*.rs"}"#)), "find_files  **/*.rs");
        assert_eq!(describe("update_todos", &json("{}")), "update_todos");
        assert_eq!(describe("move_file", &json(r#"{"path": "a.txt", "to": "lib/a.txt"}"#)), "move_file  a.txt → lib/a.txt");
        assert_eq!(describe("code_nav", &json(r#"{"operation": "definition", "path": "src/a.rs", "line": 12, "symbol": "helper"}"#)), "code_nav  definition src/a.rs:12 helper");
    }
}
