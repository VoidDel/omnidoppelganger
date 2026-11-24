//! Unified headless streaming runner and drivers for external coding-agent CLIs:
//! - Google Antigravity (`agy`) — [`CliKind::Antigravity`] (see [`crate::antigravity`])
//! - OpenAI Codex (`codex`) — [`CliKind::Codex`]
//! - Anthropic Claude Code (`claude`) — [`CliKind::ClaudeCode`]
//! - Cursor Agent (`cursor-agent`) — [`CliKind::Cursor`]
//! - xAI Grok CLI (`grok`) — [`CliKind::Grok`]
//!
//! Each driver maps OmniDoppelganger's prompt, resumed session id, model, effort (`境界`),
//! permission `Mode`, and sandbox flag onto the CLI's headless streaming command, then parses
//! its NDJSON stdout stream into normalized [`CliEvent`] items for the session log and UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::events::{Attached, Event};
use crate::llm::{StreamDelta, Tokens};
use crate::policy::Mode;
use crate::providers::Listed;

/// Which local coding-agent CLI backs a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliKind {
    Antigravity,
    Codex,
    ClaudeCode,
    Cursor,
    Grok,
}

impl CliKind {
    /// Key stored on `Event::ExternalSession { backend, session_id }`.
    pub fn backend_key(self) -> &'static str {
        match self {
            Self::Antigravity => "antigravity",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::Cursor => "cursor",
            Self::Grok => "grok",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Antigravity => "Antigravity (agy)",
            Self::Codex => "Codex CLI (codex)",
            Self::ClaudeCode => "Claude Code (claude)",
            Self::Cursor => "Cursor Agent (cursor-agent)",
            Self::Grok => "Grok CLI (grok)",
        }
    }

    /// Locate this CLI's executable on the current machine.
    pub fn find_binary(self) -> Option<PathBuf> {
        match self {
            Self::Antigravity => crate::antigravity::find_binary(),
            Self::Codex => find_named_binary("ODG_CODEX_BIN", "codex", &[]),
            Self::ClaudeCode => find_named_binary("ODG_CLAUDE_BIN", "claude", &[".claude/local"]),
            Self::Cursor => find_named_binary("ODG_CURSOR_AGENT_BIN", "cursor-agent", &[".cursor/bin"]),
            Self::Grok => find_named_binary("ODG_GROK_BIN", "grok", &[".grok/bin"]),
        }
    }

    /// Query models available for this CLI backend. CLIs without a model-listing subcommand
    /// return their curated model list from [`crate::providers`].
    pub async fn list_models(self, fallback: &'static [(&'static str, &'static str)]) -> Result<Vec<Listed>, String> {
        if self == Self::Antigravity {
            return crate::antigravity::list_models().await;
        }
        if self.find_binary().is_none() {
            return Err(format!("未找到本机 {}，请先安装并完成登录。", self.display_name()));
        }
        Ok(fallback
            .iter()
            .map(|(id, name)| Listed { id: (*id).to_string(), name: (*name).to_string(), context_window: Some(context_window(self)) })
            .collect())
    }
}

pub fn context_window(kind: CliKind) -> u64 {
    match kind {
        CliKind::Antigravity | CliKind::ClaudeCode => 1_000_000,
        CliKind::Codex => 400_000,
        CliKind::Cursor => 500_000,
        CliKind::Grok => 500_000,
    }
}

/// Locate `bin_name` via an override env var, `PATH`, or common per-user / package-manager folders
/// (so macOS / Linux GUI launches from Finder/Dock still find `~/.local/bin`, Homebrew, npm/nvm/pnpm/bun, Cargo).
pub fn find_named_binary(env_var: &str, bin_name: &str, extra_home_rel: &[&str]) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(env_var) {
        let path = PathBuf::from(explicit.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = if cfg!(windows) { format!("{bin_name}.exe") } else { bin_name.to_string() };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            for ext in ["cmd", "bat"] {
                let script = dir.join(format!("{bin_name}.{ext}"));
                if script.is_file() {
                    return Some(script);
                }
            }
        }
    }
    let mut fallbacks = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for rel in extra_home_rel {
            fallbacks.push(home.join(rel).join(&exe));
        }
        fallbacks.push(home.join(".local").join("bin").join(&exe));
        fallbacks.push(home.join(".npm-global").join("bin").join(&exe));
        fallbacks.push(home.join(".bun").join("bin").join(&exe));
        fallbacks.push(home.join(".cargo").join("bin").join(&exe));
        fallbacks.push(home.join("Library").join("pnpm").join(&exe));
    }
    #[cfg(not(windows))]
    {
        fallbacks.push(PathBuf::from(format!("/opt/homebrew/bin/{bin_name}")));
        fallbacks.push(PathBuf::from(format!("/usr/local/bin/{bin_name}")));
    }
    fallbacks.into_iter().find(|path| path.is_file())
}

/// Normalized event emitted by any CLI driver's NDJSON parser.
#[derive(Debug, Clone, PartialEq)]
pub enum CliEvent {
    Init {
        conversation_id: Option<String>,
    },
    AgentText {
        conversation_id: Option<String>,
        delta: String,
        done: bool,
        usage: Value,
        tokens: Option<Tokens>,
    },
    ToolStart {
        conversation_id: Option<String>,
        call_key: String,
        name: String,
        input: Value,
    },
    ToolDone {
        conversation_id: Option<String>,
        call_key: String,
        name: Option<String>,
        input: Value,
        output: String,
        is_error: bool,
    },
    Result {
        conversation_id: Option<String>,
        success: bool,
        response: String,
        error: Option<String>,
        usage: Value,
        tokens: Option<Tokens>,
    },
}

/// Map external CLI tool names and parameter keys onto OmniDoppelganger's built-in tool shapes so
/// `ToolCard` displays native seals (`读`, `写`, `改`, `跑`, `搜`, `寻`, `谋`) and readable targets.
pub fn normalize_tool(raw_name: &str, params: &Value) -> (String, Value) {
    let lower = raw_name.to_ascii_lowercase();
    let mapped_name = match lower.as_str() {
        "read_file" | "view_file" | "read" | "readtoolcall" => "read_file",
        "write_file" | "write_to_file" | "write" | "writetoolcall" | "create_file" => "write_file",
        "edit_file" | "replace_file_content" | "multi_replace_file_content" | "sed_file" | "edit" | "multiedit" | "edittoolcall" | "file_change" => "edit_file",
        "run_command" | "bash" | "shell" | "command_execution" | "shelltoolcall" | "bashtoolcall" | "runterminalcommand" => "run_command",
        "grep_search" | "search" | "grep" | "greptoolcall" => "search",
        "find_by_name" | "list_dir" | "find_files" | "glob" | "ls" | "globtoolcall" | "lstoolcall" => "find_files",
        "search_web" | "web_search" | "websearch" => "web_search",
        "read_url_content" | "fetch_url" | "webfetch" => "fetch_url",
        "generate_image" => "generate_image",
        "update_steps" | "todowrite" | "todo_write" | "update_todos" => "update_todos",
        _ => raw_name,
    };
    let mut normalized = params.as_object().cloned().unwrap_or_default();
    if mapped_name == "update_todos"
        && !normalized.contains_key("todos")
        && let Some(raw_steps) = normalized.get("steps")
    {
        let parsed_arr = raw_steps
            .as_array()
            .cloned()
            .or_else(|| raw_steps.as_str().and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok()));
        if let Some(items) = parsed_arr {
            let todos: Vec<Value> = items
                .into_iter()
                .filter_map(|item| {
                    let text = item["title"]
                        .as_str()
                        .or_else(|| item["text"].as_str())
                        .or_else(|| item["content"].as_str())?
                        .trim()
                        .to_string();
                    if text.is_empty() {
                        return None;
                    }
                    let status = match item["status"].as_str().unwrap_or("pending") {
                        "done" | "completed" | "skipped" => "done",
                        "in_progress" | "active" => "in_progress",
                        _ => "pending",
                    };
                    Some(json!({ "text": text, "status": status }))
                })
                .collect();
            normalized.insert("todos".to_string(), Value::Array(todos));
        }
    } else if mapped_name == "update_todos"
        && let Some(items) = normalized.get("todos").and_then(Value::as_array).cloned()
    {
        // Normalize Claude Code's `TodoWrite` (`content` field) to OmniDoppelganger's `text` field.
        let todos: Vec<Value> = items
            .into_iter()
            .filter_map(|item| {
                let text = item["text"]
                    .as_str()
                    .or_else(|| item["content"].as_str())
                    .or_else(|| item["title"].as_str())?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return None;
                }
                let status = match item["status"].as_str().unwrap_or("pending") {
                    "done" | "completed" | "skipped" => "done",
                    "in_progress" | "active" => "in_progress",
                    _ => "pending",
                };
                Some(json!({ "text": text, "status": status }))
            })
            .collect();
        normalized.insert("todos".to_string(), Value::Array(todos));
    }
    if !normalized.contains_key("path")
        && let Some(path) = ["file_path", "Path", "TargetFile", "AbsolutePath", "DirectoryPath", "SearchDirectory", "SearchPath", "target_file"]
            .iter()
            .find_map(|key| normalized.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    {
        normalized.insert("path".to_string(), Value::String(path));
    }
    if !normalized.contains_key("command")
        && let Some(cmd) = ["CommandLine", "cmd"].iter().find_map(|key| normalized.get(*key).and_then(Value::as_str)).map(str::to_string)
    {
        normalized.insert("command".to_string(), Value::String(cmd));
    }
    if !normalized.contains_key("pattern")
        && let Some(query) = ["Query", "Pattern", "query"].iter().find_map(|key| normalized.get(*key).and_then(Value::as_str)).map(str::to_string)
    {
        normalized.insert("pattern".to_string(), Value::String(query));
    }
    if !normalized.contains_key("url")
        && let Some(url) = ["Url", "URI"].iter().find_map(|key| normalized.get(*key).and_then(Value::as_str)).map(str::to_string)
    {
        normalized.insert("url".to_string(), Value::String(url));
    }
    (mapped_name.to_string(), Value::Object(normalized))
}

/// Build the prompt string passed to an external CLI, prepending any side-conversation briefing and attached file paths.
pub fn build_prompt(text: &str, files: &[Attached], briefing: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(brief) = briefing
        && !brief.trim().is_empty()
    {
        parts.push(brief.trim().to_string());
    }
    if !files.is_empty() {
        let mut list = String::from("Attached files:");
        for file in files {
            list.push_str("\n- ");
            list.push_str(&file.path);
        }
        parts.push(list);
    }
    if !text.trim().is_empty() {
        parts.push(text.to_string());
    }
    parts.join("\n\n")
}

/// Build the CLI argument vector for one headless turn of `kind`.
#[allow(clippy::too_many_arguments)]
pub fn build_args(
    kind: CliKind,
    prompt: &str,
    workspace: Option<&Path>,
    conversation_id: Option<&str>,
    model: &str,
    effort: &str,
    mode: Mode,
    sandboxed: bool,
    read_only: bool,
) -> Vec<String> {
    let resume = conversation_id.map(str::trim).filter(|id| !id.is_empty());
    let model = model.trim();
    let effort = effort.trim();
    match kind {
        CliKind::Antigravity => crate::antigravity::build_args(prompt, workspace, resume, model, effort, mode, sandboxed, read_only),
        CliKind::Codex => {
            let mut args = vec!["exec".to_string()];
            if let Some(thread_id) = resume {
                args.push("resume".to_string());
                args.push(thread_id.to_string());
            }
            args.push("--json".to_string());
            args.push("--skip-git-repo-check".to_string());
            if !model.is_empty() {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if !effort.is_empty() {
                let mapped = match effort {
                    "max" => "xhigh",
                    "low" | "medium" | "high" | "xhigh" => effort,
                    _ => "high",
                };
                args.push("-c".to_string());
                args.push(format!("model_reasoning_effort=\"{mapped}\""));
            }
            // `codex exec resume` inherits sandbox/bypass flags on newer builds, while fresh `exec` takes `--sandbox`.
            if resume.is_none() {
                if read_only || mode == Mode::Plan {
                    args.push("--sandbox".to_string());
                    args.push("read-only".to_string());
                } else if mode == Mode::Bypass && !sandboxed {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                } else {
                    args.push("--sandbox".to_string());
                    args.push("workspace-write".to_string());
                }
            } else if mode == Mode::Bypass && !sandboxed && !read_only {
                args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
            }
            args.push("--".to_string());
            args.push(prompt.to_string());
            args
        }
        CliKind::ClaudeCode => {
            let mut args = vec![
                "--print".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--verbose".to_string(),
                "--include-partial-messages".to_string(),
            ];
            if let Some(session_id) = resume {
                args.push("--resume".to_string());
                args.push(session_id.to_string());
            }
            if !model.is_empty() {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if !effort.is_empty() {
                args.push("--effort".to_string());
                args.push(effort.to_string());
            }
            let perm = if read_only {
                "plan"
            } else {
                match mode {
                    Mode::Plan => "plan",
                    Mode::Edit => "acceptEdits",
                    Mode::Auto => "auto",
                    Mode::Bypass => "bypassPermissions",
                    Mode::Manual => "acceptEdits",
                }
            };
            args.push("--permission-mode".to_string());
            args.push(perm.to_string());
            if !read_only && mode == Mode::Bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            args.push("--".to_string());
            args.push(prompt.to_string());
            args
        }
        CliKind::Cursor => {
            let mut args = vec![
                "--print".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--stream-partial-output".to_string(),
            ];
            if let Some(chat_id) = resume {
                args.push("--resume".to_string());
                args.push(chat_id.to_string());
            }
            if !model.is_empty() && model != "auto" {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if read_only || mode == Mode::Plan {
                args.push("--mode".to_string());
                args.push("plan".to_string());
            } else if matches!(mode, Mode::Edit | Mode::Auto | Mode::Bypass) {
                args.push("--force".to_string());
            }
            args.push("--".to_string());
            args.push(prompt.to_string());
            args
        }
        CliKind::Grok => {
            let mut args = vec![
                "--no-auto-update".to_string(),
                "--output-format".to_string(),
                "streaming-json".to_string(),
            ];
            if let Some(session_id) = resume {
                args.push("--resume".to_string());
                args.push(session_id.to_string());
            }
            if !model.is_empty() {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if read_only || mode == Mode::Plan {
                args.push("--sandbox".to_string());
                args.push("read-only".to_string());
            } else if matches!(mode, Mode::Edit | Mode::Auto | Mode::Bypass) {
                if sandboxed {
                    args.push("--sandbox".to_string());
                    args.push("workspace-write".to_string());
                }
                args.push("--always-approve".to_string());
            }
            args.push("-p".to_string());
            args.push(prompt.to_string());
            args
        }
    }
}

/// Stateful per-turn NDJSON parser for a CLI backend (needed where items stream cumulative text or partial blocks).
#[derive(Default)]
pub struct StreamParser {
    /// Tracks how many bytes of cumulative text were already emitted for an item id (used by Codex `item.updated`).
    seen_item_bytes: HashMap<String, usize>,
    /// Tracks whether Claude Code / Cursor already streamed `stream_event` deltas for the current assistant message.
    saw_partial_delta: bool,
    step_seq: u64,
}

impl StreamParser {
    pub fn parse_line(&mut self, kind: CliKind, line: &str) -> Vec<CliEvent> {
        match kind {
            CliKind::Antigravity => crate::antigravity::parse_ndjson_line(line).into_iter().map(crate::antigravity::into_cli_event).collect(),
            CliKind::Codex => self.parse_codex(line),
            CliKind::ClaudeCode => self.parse_claude(line),
            CliKind::Cursor => self.parse_cursor(line),
            CliKind::Grok => self.parse_grok(line),
        }
    }

    fn next_key(&mut self, prefix: &str) -> String {
        self.step_seq += 1;
        format!("{prefix}-{}", self.step_seq)
    }

    fn parse_codex(&mut self, line: &str) -> Vec<CliEvent> {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else { return Vec::new() };
        let Some(ty) = v["type"].as_str() else { return Vec::new() };
        match ty {
            "thread.started" => vec![CliEvent::Init { conversation_id: v["thread_id"].as_str().map(str::to_string) }],
            "item.started" | "item.updated" | "item.completed" => {
                let item = &v["item"];
                let item_id = item["id"].as_str().unwrap_or("item").to_string();
                let done = ty == "item.completed";
                match item["type"].as_str().unwrap_or("") {
                    "agent_message" => {
                        let full = item["text"].as_str().unwrap_or("");
                        let prev = self.seen_item_bytes.get(&item_id).copied().unwrap_or(0);
                        let delta = if full.len() > prev && full.is_char_boundary(prev) { full[prev..].to_string() } else { String::new() };
                        self.seen_item_bytes.insert(item_id, full.len());
                        if delta.is_empty() && !done {
                            Vec::new()
                        } else {
                            vec![CliEvent::AgentText { conversation_id: None, delta, done, usage: Value::Null, tokens: None }]
                        }
                    }
                    "command_execution" => {
                        let cmd = item["command"].as_str().unwrap_or("");
                        let (_, input) = normalize_tool("run_command", &json!({ "command": cmd }));
                        if done {
                            let output = item["aggregated_output"].as_str().unwrap_or("").to_string();
                            let exit_code = item["exit_code"].as_i64().unwrap_or(0);
                            let failed = exit_code != 0 || item["status"].as_str() == Some("failed");
                            vec![CliEvent::ToolDone { conversation_id: None, call_key: item_id, name: Some("run_command".into()), input, output, is_error: failed }]
                        } else if ty == "item.started" {
                            vec![CliEvent::ToolStart { conversation_id: None, call_key: item_id, name: "run_command".into(), input }]
                        } else {
                            Vec::new()
                        }
                    }
                    "file_change" if done => {
                        let first_path = item["changes"]
                            .as_array()
                            .and_then(|arr| arr.first())
                            .and_then(|c| c["path"].as_str())
                            .unwrap_or("");
                        let summary = item["changes"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| Some(format!("{}: {}", c["kind"].as_str().unwrap_or("edit"), c["path"].as_str()?)))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        let input = json!({ "path": first_path });
                        let failed = item["status"].as_str() == Some("failed");
                        vec![CliEvent::ToolDone { conversation_id: None, call_key: item_id, name: Some("edit_file".into()), input, output: summary, is_error: failed }]
                    }
                    "mcp_tool_call" => {
                        let raw = format!("mcp__{}__{}", item["server"].as_str().unwrap_or("mcp"), item["tool"].as_str().unwrap_or("call"));
                        let input = item["arguments"].clone();
                        if done {
                            let err = item["error"]["message"].as_str();
                            let output = err
                                .map(str::to_string)
                                .or_else(|| (!item["result"].is_null()).then(|| item["result"].to_string()))
                                .unwrap_or_default();
                            vec![CliEvent::ToolDone { conversation_id: None, call_key: item_id, name: Some(raw), input, output, is_error: err.is_some() }]
                        } else if ty == "item.started" {
                            vec![CliEvent::ToolStart { conversation_id: None, call_key: item_id, name: raw, input }]
                        } else {
                            Vec::new()
                        }
                    }
                    _ => Vec::new(),
                }
            }
            "turn.completed" => {
                let usage = v["usage"].clone();
                let cached = usage["cached_input_tokens"].as_u64().unwrap_or(0);
                let input = usage["input_tokens"].as_u64().unwrap_or(0);
                let output = usage["output_tokens"].as_u64().unwrap_or(0);
                let tokens = (input > 0 || output > 0).then_some(Tokens { input, cached, output });
                vec![CliEvent::Result { conversation_id: None, success: true, response: String::new(), error: None, usage, tokens }]
            }
            "turn.failed" => {
                let msg = v["error"]["message"].as_str().unwrap_or("Codex turn failed").to_string();
                vec![CliEvent::Result { conversation_id: None, success: false, response: String::new(), error: Some(msg), usage: Value::Null, tokens: None }]
            }
            "error" => {
                let msg = v["message"].as_str().unwrap_or("Codex error").to_string();
                vec![CliEvent::Result { conversation_id: None, success: false, response: String::new(), error: Some(msg), usage: Value::Null, tokens: None }]
            }
            _ => Vec::new(),
        }
    }

    fn parse_claude(&mut self, line: &str) -> Vec<CliEvent> {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else { return Vec::new() };
        let Some(ty) = v["type"].as_str() else { return Vec::new() };
        let conv = v["session_id"].as_str().map(str::to_string);
        match ty {
            "system" => vec![CliEvent::Init { conversation_id: conv }],
            "stream_event" => {
                let ev = &v["event"];
                if ev["type"].as_str() == Some("content_block_delta") && ev["delta"]["type"].as_str() == Some("text_delta") {
                    let text = ev["delta"]["text"].as_str().unwrap_or("").to_string();
                    if !text.is_empty() {
                        self.saw_partial_delta = true;
                        return vec![CliEvent::AgentText { conversation_id: conv, delta: text, done: false, usage: Value::Null, tokens: None }];
                    }
                }
                Vec::new()
            }
            "assistant" => {
                let mut out = Vec::new();
                let msg = &v["message"];
                let usage = msg["usage"].clone();
                let tokens = anthropic_like_tokens(&usage);
                if let Some(blocks) = msg["content"].as_array() {
                    let mut full_text = String::new();
                    for block in blocks {
                        match block["type"].as_str().unwrap_or("") {
                            "text" => {
                                if let Some(t) = block["text"].as_str() {
                                    full_text.push_str(t);
                                }
                            }
                            "tool_use" => {
                                let call_key = block["id"].as_str().map(str::to_string).unwrap_or_else(|| self.next_key("claude"));
                                let raw_name = block["name"].as_str().unwrap_or("tool");
                                let (name, input) = normalize_tool(raw_name, &block["input"]);
                                out.push(CliEvent::ToolStart { conversation_id: conv.clone(), call_key, name, input });
                            }
                            _ => {}
                        }
                    }
                    let delta = if self.saw_partial_delta { String::new() } else { full_text };
                    if self.saw_partial_delta || !delta.is_empty() {
                        out.insert(0, CliEvent::AgentText { conversation_id: conv, delta, done: true, usage, tokens });
                    }
                }
                self.saw_partial_delta = false;
                out
            }
            "tool_use" => {
                let call_key = v["id"].as_str().map(str::to_string).unwrap_or_else(|| self.next_key("claude"));
                let raw_name = v["name"].as_str().unwrap_or("tool");
                let (name, input) = normalize_tool(raw_name, &v["input"]);
                vec![CliEvent::ToolStart { conversation_id: conv, call_key, name, input }]
            }
            "user" => {
                let mut out = Vec::new();
                if let Some(blocks) = v["message"]["content"].as_array() {
                    for block in blocks {
                        if block["type"].as_str() == Some("tool_result") {
                            let call_key = block["tool_use_id"].as_str().unwrap_or("").to_string();
                            let output = extract_content_text(&block["content"]);
                            let is_error = block["is_error"].as_bool().unwrap_or(false);
                            out.push(CliEvent::ToolDone { conversation_id: conv.clone(), call_key, name: None, input: Value::Null, output, is_error });
                        }
                    }
                }
                out
            }
            "tool_result" => {
                let call_key = v["tool_use_id"].as_str().unwrap_or("").to_string();
                let output = extract_content_text(&v["content"]);
                let is_error = v["is_error"].as_bool().unwrap_or(false);
                vec![CliEvent::ToolDone { conversation_id: conv, call_key, name: None, input: Value::Null, output, is_error }]
            }
            "result" => {
                let is_error = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s.starts_with("error"));
                let response = v["result"].as_str().unwrap_or("").to_string();
                let error = is_error.then(|| {
                    if !response.is_empty() {
                        response.clone()
                    } else {
                        v["error"].as_str().unwrap_or("Claude Code turn failed").to_string()
                    }
                });
                let usage = v["usage"].clone();
                let tokens = anthropic_like_tokens(&usage);
                vec![CliEvent::Result { conversation_id: conv, success: !is_error, response, error, usage, tokens }]
            }
            _ => Vec::new(),
        }
    }

    fn parse_cursor(&mut self, line: &str) -> Vec<CliEvent> {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else { return Vec::new() };
        let Some(ty) = v["type"].as_str() else { return Vec::new() };
        let conv = v["session_id"].as_str().or_else(|| v["chatId"].as_str()).map(str::to_string);
        match ty {
            "system" | "init" => vec![CliEvent::Init { conversation_id: conv }],
            "assistant" => {
                let text = v["message"]["content"]
                    .as_array()
                    .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<String>())
                    .or_else(|| v["text"].as_str().map(str::to_string))
                    .unwrap_or_default();
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![CliEvent::AgentText { conversation_id: conv, delta: text, done: false, usage: Value::Null, tokens: None }]
                }
            }
            "tool_call" => {
                let call_key = v["call_id"].as_str().map(str::to_string).unwrap_or_else(|| self.next_key("cursor"));
                let subtype = v["subtype"].as_str().unwrap_or("");
                // Cursor nests `{"readToolCall": {"args": {...}}}` inside `tool_call`.
                let (raw_name, raw_args) = v["tool_call"]
                    .as_object()
                    .and_then(|map| map.iter().next())
                    .map(|(k, val)| (k.as_str(), val.get("args").unwrap_or(val)))
                    .unwrap_or(("tool", &v["args"]));
                let (name, input) = normalize_tool(raw_name, raw_args);
                if subtype == "completed" {
                    let res = &v["result"];
                    let is_error = res["success"].as_bool() == Some(false) || res["is_error"].as_bool() == Some(true) || !res["error"].is_null();
                    let output = res["content"]
                        .as_str()
                        .or_else(|| res["stdout"].as_str())
                        .or_else(|| res["error"].as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| if res.is_null() { String::new() } else { res.to_string() });
                    vec![CliEvent::ToolDone { conversation_id: conv, call_key, name: Some(name), input, output, is_error }]
                } else {
                    vec![CliEvent::ToolStart { conversation_id: conv, call_key, name, input }]
                }
            }
            "result" => {
                let is_error = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str() == Some("error");
                let response = v["result"].as_str().unwrap_or("").to_string();
                let error = is_error.then(|| if !response.is_empty() { response.clone() } else { "Cursor Agent turn failed".to_string() });
                vec![CliEvent::Result { conversation_id: conv, success: !is_error, response, error, usage: Value::Null, tokens: None }]
            }
            _ => Vec::new(),
        }
    }

    fn parse_grok(&mut self, line: &str) -> Vec<CliEvent> {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else { return Vec::new() };
        let ty = v["type"].as_str().or_else(|| v["event"].as_str()).unwrap_or("");
        let conv = v["session_id"].as_str().or_else(|| v["conversation_id"].as_str()).map(str::to_string);
        match ty {
            "init" | "system" | "step_start" => {
                if conv.is_some() {
                    vec![CliEvent::Init { conversation_id: conv }]
                } else {
                    Vec::new()
                }
            }
            "text" | "text_delta" | "assistant" => {
                let delta = v["text"]
                    .as_str()
                    .or_else(|| v["delta"].as_str())
                    .or_else(|| v["content"].as_str())
                    .unwrap_or("")
                    .to_string();
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![CliEvent::AgentText { conversation_id: conv, delta, done: false, usage: Value::Null, tokens: None }]
                }
            }
            "tool_use" | "tool_call" => {
                let call_key = v["id"].as_str().or_else(|| v["tool_use_id"].as_str()).map(str::to_string).unwrap_or_else(|| self.next_key("grok"));
                let raw_name = v["name"].as_str().or_else(|| v["tool"].as_str()).unwrap_or("tool");
                let args = if !v["input"].is_null() { &v["input"] } else { &v["arguments"] };
                let (name, input) = normalize_tool(raw_name, args);
                vec![CliEvent::ToolStart { conversation_id: conv, call_key, name, input }]
            }
            "tool_result" => {
                let call_key = v["tool_use_id"].as_str().or_else(|| v["id"].as_str()).unwrap_or("").to_string();
                let output = extract_content_text(&v["content"]);
                let is_error = v["is_error"].as_bool().unwrap_or(false);
                vec![CliEvent::ToolDone { conversation_id: conv, call_key, name: None, input: Value::Null, output, is_error }]
            }
            "step_finish" => {
                let usage = v["usage"].clone();
                let tokens = anthropic_like_tokens(&usage);
                vec![CliEvent::AgentText { conversation_id: conv, delta: String::new(), done: true, usage, tokens }]
            }
            "result" => {
                let is_error = v["is_error"].as_bool().unwrap_or(false) || v["status"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("error"));
                let response = v["result"].as_str().or_else(|| v["response"].as_str()).unwrap_or("").to_string();
                let error = v["error"].as_str().map(str::to_string).or_else(|| is_error.then(|| response.clone()));
                let usage = v["usage"].clone();
                let tokens = anthropic_like_tokens(&usage);
                vec![CliEvent::Result { conversation_id: conv, success: !is_error, response, error, usage, tokens }]
            }
            "error" => {
                let msg = v["message"].as_str().or_else(|| v["error"].as_str()).unwrap_or("Grok CLI error").to_string();
                vec![CliEvent::Result { conversation_id: conv, success: false, response: String::new(), error: Some(msg), usage: Value::Null, tokens: None }]
            }
            _ => Vec::new(),
        }
    }
}

fn extract_content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|item| item["text"].as_str().or_else(|| item.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    if content.is_null() { String::new() } else { content.to_string() }
}

fn anthropic_like_tokens(usage: &Value) -> Option<Tokens> {
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let cached = count("cache_read_input_tokens").max(count("cache_read_tokens")).max(count("cached_tokens"));
    let input = count("input_tokens").max(count("prompt_tokens")) + count("cache_read_input_tokens") + count("cache_creation_input_tokens");
    let output = count("output_tokens").max(count("completion_tokens"));
    (input > 0 || output > 0).then_some(Tokens { input, cached, output })
}

/// Strip `agy`'s leading `"Created At: ...\nCompleted At: ...\n"` envelope and synthesize
/// the [`Event::ToolResult`] `meta` fields that `ToolCard`, `changedFiles`, and `todos` read.
pub fn clean_tool_result(tool_name: &str, input: &Value, raw_output: &str, is_error: bool) -> (String, Value) {
    let mut cleaned = raw_output.replace("\r\n", "\n");
    loop {
        let trimmed = cleaned.trim_start_matches('\n');
        if let Some(rest) = trimmed.strip_prefix("Created At:")
            && let Some((_, after)) = rest.split_once('\n')
        {
            cleaned = after.to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Completed At:")
            && let Some((_, after)) = rest.split_once('\n')
        {
            cleaned = after.to_string();
            continue;
        }
        break;
    }
    let cleaned = cleaned.trim_start_matches('\n').to_string();

    match tool_name {
        "run_command" => {
            let mut exit_code: Option<i64> = None;
            let mut body = cleaned.as_str();
            if let Some(rest) = body.strip_prefix("Command exited with code ")
                && let Some((code_part, after_line)) = rest.split_once('.')
            {
                exit_code = code_part.trim().parse::<i64>().ok();
                let rem = after_line.trim_start_matches(['\r', '\n']);
                if let Some(cmd_out) = rem.strip_prefix("Command output:\n") {
                    body = cmd_out;
                } else if rem.trim() == "No output from the command." {
                    body = "";
                } else {
                    body = rem;
                }
            }
            let code = exit_code.unwrap_or(if is_error { 1 } else { 0 });
            let out_str = body.to_string();
            let meta = json!({
                "command": input["command"].as_str().unwrap_or(""),
                "output": out_str,
                "exit_code": code,
            });
            (out_str, meta)
        }
        "read_file" => {
            let lines = parse_leading_line_count(&cleaned).unwrap_or_else(|| if cleaned.is_empty() { 0 } else { cleaned.lines().count() });
            let meta = json!({
                "path": input["path"].as_str().unwrap_or(""),
                "lines": lines,
            });
            (cleaned, meta)
        }
        "write_file" | "edit_file" => {
            let path = input["path"].as_str().unwrap_or("");
            let (added, removed) = parse_plus_minus_lines(&cleaned).unwrap_or_else(|| estimate_edit_lines(tool_name, input, path));
            let meta = json!({
                "path": path,
                "file": path,
                "created": tool_name == "write_file",
                "added": added,
                "removed": removed,
            });
            (cleaned, meta)
        }
        "update_todos" => {
            let todos = input.get("todos").cloned().unwrap_or_else(|| json!([]));
            (cleaned, json!({ "todos": todos }))
        }
        "find_files" => {
            let entries = cleaned.lines().filter(|l| !l.trim().is_empty()).count();
            (cleaned, json!({ "entries": entries }))
        }
        "search" => {
            let matches = cleaned.lines().filter(|l| !l.trim().is_empty()).count();
            (cleaned, json!({ "matches": matches }))
        }
        _ => (cleaned, Value::Null),
    }
}

fn parse_leading_line_count(text: &str) -> Option<usize> {
    let first = text.lines().next()?.trim();
    // Matches `"1413 lines, 79900 bytes"` or `"40 lines"`.
    if let Some((num, _)) = first.split_once(" lines")
        && let Ok(n) = num.trim().parse::<usize>()
    {
        return Some(n);
    }
    // Matches `"Showing lines 1-80 of 1413"`.
    if let Some(rest) = first.strip_prefix("Showing lines ")
        && let Some((range, _)) = rest.split_once(" of ")
        && let Some((start, end)) = range.split_once('-')
        && let (Ok(s), Ok(e)) = (start.trim().parse::<usize>(), end.trim().parse::<usize>())
        && e >= s
    {
        return Some(e - s + 1);
    }
    None
}

fn parse_plus_minus_lines(text: &str) -> Option<(usize, usize)> {
    // Matches `"(+23/-7 lines)"` in `agy` edit summaries.
    let open = text.find("(+")?;
    let rest = &text[open + 2..];
    let (plus, after_slash) = rest.split_once("/-")?;
    let (minus, _) = after_slash.split_once(" lines)")?;
    Some((plus.trim().parse().ok()?, minus.trim().parse().ok()?))
}

fn estimate_edit_lines(tool_name: &str, input: &Value, path: &str) -> (usize, usize) {
    let count_lines = |s: &str| if s.is_empty() { 0 } else { s.lines().count() };
    if tool_name == "write_file" {
        if let Some(content) = ["Content", "content", "file_text"].iter().find_map(|k| input.get(*k).and_then(Value::as_str)) {
            return (count_lines(content), 0);
        }
        if !path.is_empty()
            && let Ok(disk) = std::fs::read_to_string(path)
        {
            return (count_lines(&disk), 0);
        }
        return (0, 0);
    }
    let added = ["Replacement", "new_string", "new_str"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map_or(0, count_lines);
    let removed = ["Target", "old_string", "old_str"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map_or(0, count_lines);
    (added, removed)
}

/// Synchronous guard that kills the CLI process group if the async task is dropped abruptly
/// (e.g. on app exit or hot-reload), preventing orphan CLI instances from mutating the same session.
struct ProcessGroupGuard(Option<u32>);

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let Some(pid) = self.0.take() else { return };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).output();
        }
    }
}

/// Parameters for executing a single headless CLI turn.
pub struct TurnParams<'a> {
    pub kind: CliKind,
    pub workspace: &'a Path,
    pub turn: u64,
    pub prompt: String,
    pub conversation_id: Option<String>,
    pub model: String,
    pub effort: String,
    pub mode: Mode,
    pub sandboxed: bool,
    pub read_only: bool,
}

/// Callbacks bridging external CLI stream events into the owning `Session`.
pub trait Sink {
    fn log(&self, event: Event) -> Result<(), String>;
    fn stream(&self, delta: StreamDelta);
}

/// Execute one headless streaming turn on any supported [`CliKind`].
pub async fn run_turn<S: Sink>(params: TurnParams<'_>, sink: &S, cancel: &CancellationToken) -> Result<&'static str, String> {
    let bin = params
        .kind
        .find_binary()
        .ok_or_else(|| format!("未找到本机 {}，请先安装并完成登录。", params.kind.display_name()))?;
    let args = build_args(
        params.kind,
        &params.prompt,
        Some(params.workspace),
        params.conversation_id.as_deref(),
        &params.model,
        &params.effort,
        params.mode,
        params.sandboxed,
        params.read_only,
    );
    let mut cmd = Command::new(&bin);
    cmd.args(&args)
        .current_dir(params.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|error| format!("无法启动 {}：{error}", params.kind.display_name()))?;
    let mut group_guard = ProcessGroupGuard(child.id());
    let stdout = child.stdout.take().ok_or("CLI stdout pipe unavailable")?;
    let mut stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr.take() {
            let _ = pipe.read_to_string(&mut buf).await;
        }
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut parser = StreamParser::default();
    let mut logged_conv = params.conversation_id.clone();
    let mut active_text = String::new();
    let mut emitted_any_text = false;
    let mut started_tools: HashMap<String, (String, String, Value)> = HashMap::new();
    let mut result_error: Option<String> = None;

    let backend_key = params.kind.backend_key();
    let mut record_conv = |conv: Option<String>| -> Result<(), String> {
        if let Some(id) = conv
            && !id.is_empty()
            && logged_conv.as_deref() != Some(id.as_str())
        {
            sink.log(Event::ExternalSession { backend: backend_key.to_string(), session_id: id.clone() })?;
            logged_conv = Some(id);
        }
        Ok(())
    };

    let flush_text = |text: &mut String, usage: Value, tokens: Option<Tokens>, model: &str, emitted: &mut bool| -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let body = std::mem::take(text);
        *emitted = true;
        sink.log(Event::AssistantMessage {
            content: vec![json!({ "type": "text", "text": body })],
            stop_reason: Some("end_turn".to_string()),
            model: model.to_string(),
            usage,
            tokens,
        })
    };

    let cancelled = loop {
        let next_line = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                crate::tools::kill_tree(&mut child).await;
                break true;
            }
            res = lines.next_line() => res,
        };
        let line = match next_line {
            Ok(Some(line)) => line,
            Ok(None) => break false,
            Err(error) => {
                crate::tools::kill_tree(&mut child).await;
                group_guard.0 = None;
                return Err(format!("读取 {} 输出失败：{error}", params.kind.display_name()));
            }
        };
        for ev in parser.parse_line(params.kind, &line) {
            match ev {
                CliEvent::Init { conversation_id } => {
                    record_conv(conversation_id)?;
                }
                CliEvent::AgentText { conversation_id, delta, done, usage, tokens } => {
                    record_conv(conversation_id)?;
                    if !delta.is_empty() {
                        active_text.push_str(&delta);
                        sink.stream(StreamDelta::TextDelta { index: 0, text: delta });
                    }
                    if done {
                        flush_text(&mut active_text, usage, tokens, &params.model, &mut emitted_any_text)?;
                    }
                }
                CliEvent::ToolStart { conversation_id, call_key, name, input } => {
                    record_conv(conversation_id)?;
                    flush_text(&mut active_text, Value::Null, None, &params.model, &mut emitted_any_text)?;
                    let tool_use_id = format!("{backend_key}-{}-{call_key}", params.turn);
                    if let std::collections::hash_map::Entry::Vacant(slot) = started_tools.entry(call_key) {
                        slot.insert((tool_use_id.clone(), name.clone(), input.clone()));
                        sink.log(Event::AssistantMessage {
                            content: vec![json!({ "type": "tool_use", "id": tool_use_id, "name": name, "input": input })],
                            stop_reason: Some("tool_use".to_string()),
                            model: params.model.clone(),
                            usage: Value::Null,
                            tokens: None,
                        })?;
                    }
                }
                CliEvent::ToolDone { conversation_id, call_key, name, input, output, is_error } => {
                    record_conv(conversation_id)?;
                    let (tool_use_id, tool_name, tool_input) = match started_tools.remove(&call_key) {
                        Some(existing) => existing,
                        None => {
                            flush_text(&mut active_text, Value::Null, None, &params.model, &mut emitted_any_text)?;
                            let resolved_name = name.unwrap_or_else(|| "tool".to_string());
                            let id = format!("{backend_key}-{}-{call_key}", params.turn);
                            sink.log(Event::AssistantMessage {
                                content: vec![json!({ "type": "tool_use", "id": id, "name": resolved_name, "input": input })],
                                stop_reason: Some("tool_use".to_string()),
                                model: params.model.clone(),
                                usage: Value::Null,
                                tokens: None,
                            })?;
                            (id, resolved_name, input)
                        }
                    };
                    let (clean_output, meta) = clean_tool_result(&tool_name, &tool_input, &output, is_error);
                    sink.log(Event::ToolResult { tool_use_id, tool: tool_name, content: clean_output, is_error, meta, images: Vec::new() })?;
                }
                CliEvent::Result { conversation_id, success, response, error, usage, tokens } => {
                    record_conv(conversation_id)?;
                    if !active_text.is_empty() {
                        flush_text(&mut active_text, usage, tokens, &params.model, &mut emitted_any_text)?;
                    } else if !emitted_any_text && !response.trim().is_empty() {
                        sink.log(Event::AssistantMessage {
                            content: vec![json!({ "type": "text", "text": response })],
                            stop_reason: Some("end_turn".to_string()),
                            model: params.model.clone(),
                            usage,
                            tokens,
                        })?;
                        emitted_any_text = true;
                    }
                    if !success {
                        result_error = error;
                    }
                }
            }
        }
    };

    if cancelled {
        group_guard.0 = None;
        let _ = child.wait().await;
        return Ok("cancelled");
    }

    flush_text(&mut active_text, Value::Null, None, &params.model, &mut emitted_any_text)?;
    let status = child.wait().await.map_err(|error| format!("等待 {} 结束失败：{error}", params.kind.display_name()))?;
    group_guard.0 = None;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if let Some(err) = result_error {
        return Err(err);
    }
    if !status.success() {
        let trimmed = stderr_text.trim();
        return Err(if trimmed.is_empty() {
            format!("{} 异常退出（{status}）。", params.kind.display_name())
        } else {
            trimmed.to_string()
        });
    }
    Ok("completed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_codex_claude_cursor_and_grok_cli_arguments() {
        let codex = build_args(CliKind::Codex, "fix bug", None, None, "gpt-5.4", "max", Mode::Edit, false, false);
        assert_eq!(
            codex,
            ["exec", "--json", "--skip-git-repo-check", "--model", "gpt-5.4", "-c", "model_reasoning_effort=\"xhigh\"", "--sandbox", "workspace-write", "--", "fix bug"]
        );
        let codex_resume = build_args(CliKind::Codex, "next", None, Some("thr-1"), "gpt-5.4", "high", Mode::Edit, false, false);
        assert_eq!(
            codex_resume,
            ["exec", "resume", "thr-1", "--json", "--skip-git-repo-check", "--model", "gpt-5.4", "-c", "model_reasoning_effort=\"high\"", "--", "next"]
        );

        let claude = build_args(CliKind::ClaudeCode, "review", None, Some("ses-9"), "opus", "xhigh", Mode::Bypass, false, false);
        assert_eq!(
            claude,
            ["--print", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--resume", "ses-9", "--model", "opus", "--effort", "xhigh", "--permission-mode", "bypassPermissions", "--dangerously-skip-permissions", "--", "review"]
        );

        let cursor = build_args(CliKind::Cursor, "explain", None, Some("chat-2"), "auto", "high", Mode::Plan, false, false);
        assert_eq!(
            cursor,
            ["--print", "--output-format", "stream-json", "--stream-partial-output", "--resume", "chat-2", "--mode", "plan", "--", "explain"]
        );

        let grok = build_args(CliKind::Grok, "hello", None, Some("g-1"), "grok-4.7", "high", Mode::Auto, true, false);
        assert_eq!(
            grok,
            ["--no-auto-update", "--output-format", "streaming-json", "--resume", "g-1", "--model", "grok-4.7", "--sandbox", "workspace-write", "--always-approve", "-p", "hello"]
        );
    }

    #[test]
    fn normalizes_update_steps_and_synthesizes_tool_result_meta() {
        let (name, input) = normalize_tool(
            "update_steps",
            &json!({
                "steps": [
                    { "title": "构建引擎", "status": "done" },
                    { "title": "渲染质检", "status": "in_progress" },
                    { "title": "交付文件", "status": "pending" }
                ]
            }),
        );
        assert_eq!(name, "update_todos");
        let (_, todo_meta) = clean_tool_result(&name, &input, "Created At: 2026-11-24T14:00:00+08:00\nCompleted At: 2026-11-24T14:00:01+08:00\nOK", false);
        assert_eq!(
            todo_meta["todos"],
            json!([
                { "text": "构建引擎", "status": "done" },
                { "text": "渲染质检", "status": "in_progress" },
                { "text": "交付文件", "status": "pending" }
            ])
        );

        let (cmd_out, cmd_meta) = clean_tool_result(
            "run_command",
            &json!({ "command": "echo hi" }),
            "Created At: 2026-11-24T14:00:00+08:00\nCompleted At: 2026-11-24T14:00:01+08:00\nCommand exited with code 0.\n\nCommand output:\nhi\n",
            false,
        );
        assert_eq!(cmd_out, "hi\n");
        assert_eq!(cmd_meta["exit_code"], 0);

        let (_, read_meta) = clean_tool_result("read_file", &json!({ "path": "a.py" }), "1413 lines, 79900 bytes", false);
        assert_eq!(read_meta["lines"], 1413);

        let (_, edit_meta) = clean_tool_result("edit_file", &json!({ "path": "a.py" }), "Made 1 replacement in a.py starting at line 10 (+23/-7 lines).", false);
        assert_eq!((edit_meta["added"].as_u64(), edit_meta["removed"].as_u64()), (Some(23), Some(7)));
    }

    #[test]
    fn parses_codex_ndjson_stream_with_incremental_agent_message() {
        let mut p = StreamParser::default();
        assert_eq!(
            p.parse_line(CliKind::Codex, r#"{"type":"thread.started","thread_id":"t-42"}"#),
            vec![CliEvent::Init { conversation_id: Some("t-42".into()) }]
        );
        assert_eq!(
            p.parse_line(CliKind::Codex, r#"{"type":"item.updated","item":{"id":"m1","type":"agent_message","text":"Hel"}}"#),
            vec![CliEvent::AgentText { conversation_id: None, delta: "Hel".into(), done: false, usage: Value::Null, tokens: None }]
        );
        assert_eq!(
            p.parse_line(CliKind::Codex, r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Hello!"}}"#),
            vec![CliEvent::AgentText { conversation_id: None, delta: "lo!".into(), done: true, usage: Value::Null, tokens: None }]
        );
        let done = p.parse_line(CliKind::Codex, r#"{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":50,"output_tokens":12}}"#);
        assert_eq!(done[0], CliEvent::Result {
            conversation_id: None,
            success: true,
            response: String::new(),
            error: None,
            usage: json!({"input_tokens":200,"cached_input_tokens":50,"output_tokens":12}),
            tokens: Some(Tokens { input: 200, cached: 50, output: 12 }),
        });
    }

    #[test]
    fn parses_claude_cursor_and_grok_ndjson_events() {
        let mut claude = StreamParser::default();
        assert_eq!(
            claude.parse_line(CliKind::ClaudeCode, r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#),
            vec![CliEvent::AgentText { conversation_id: Some("s1".into()), delta: "Hi".into(), done: false, usage: Value::Null, tokens: None }]
        );
        // Subsequent full `assistant` message does not duplicate the already-streamed `"Hi"` text!
        let msg = claude.parse_line(CliKind::ClaudeCode, r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":10,"output_tokens":4}}}"#);
        assert_eq!(msg.len(), 2);
        assert_eq!(msg[0], CliEvent::AgentText {
            conversation_id: Some("s1".into()),
            delta: String::new(),
            done: true,
            usage: json!({"input_tokens":10,"output_tokens":4}),
            tokens: Some(Tokens { input: 10, cached: 0, output: 4 }),
        });
        assert_eq!(msg[1], CliEvent::ToolStart {
            conversation_id: Some("s1".into()),
            call_key: "tu1".into(),
            name: "run_command".into(),
            input: json!({"command": "ls"}),
        });

        let mut cursor = StreamParser::default();
        let tc = cursor.parse_line(CliKind::Cursor, r#"{"type":"tool_call","subtype":"started","call_id":"c1","session_id":"ch1","tool_call":{"readToolCall":{"args":{"path":"README.md"}}}}"#);
        assert_eq!(tc, vec![CliEvent::ToolStart {
            conversation_id: Some("ch1".into()),
            call_key: "c1".into(),
            name: "read_file".into(),
            input: json!({"path": "README.md"}),
        }]);

        let mut grok = StreamParser::default();
        let gt = grok.parse_line(CliKind::Grok, r#"{"type":"text","session_id":"g9","text":"Done"}"#);
        assert_eq!(gt, vec![CliEvent::AgentText { conversation_id: Some("g9".into()), delta: "Done".into(), done: false, usage: Value::Null, tokens: None }]);
    }
}
