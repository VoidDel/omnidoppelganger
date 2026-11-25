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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    #[serde(rename = "opencode")]
    OpenCode,
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
            Self::OpenCode => "opencode",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Antigravity => "Antigravity (agy)",
            Self::Codex => "Codex CLI (codex)",
            Self::ClaudeCode => "Claude Code (claude)",
            Self::Cursor => "Cursor Agent (cursor-agent)",
            Self::Grok => "Grok CLI (grok)",
            Self::OpenCode => "OpenCode (opencode)",
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
            Self::OpenCode => find_named_binary("ODG_OPENCODE_BIN", "opencode", &[".opencode/bin"]),
        }
    }

    /// Query models available for this CLI backend. CLIs without a model-listing subcommand
    /// return their curated model list from [`crate::providers`].
    pub async fn list_models(self, fallback: &'static [(&'static str, &'static str)]) -> Result<Vec<Listed>, String> {
        if self == Self::Antigravity {
            return crate::antigravity::list_models().await;
        }
        let Some(bin) = self.find_binary() else {
            return Err(format!("未找到本机 {}，请先安装并完成登录。", self.display_name()));
        };
        if self == Self::OpenCode {
            let mut cmd = Command::new(&bin);
            cmd.arg("models")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000);
            let out = tokio::time::timeout(std::time::Duration::from_secs(12), cmd.output())
                .await
                .map_err(|_| "查询 opencode models 超时（12s）。".to_string())?
                .map_err(|e| format!("运行 opencode models 失败：{e}"))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(if err.is_empty() { "opencode models 执行失败。".to_string() } else { format!("opencode models 失败：{err}") });
            }
            let parsed = parse_opencode_models_output(&String::from_utf8_lossy(&out.stdout));
            if parsed.is_empty() {
                return Err("opencode models 未返回任何可用模型，请先检查 OpenCode 配置。".to_string());
            }
            return Ok(parsed);
        }
        Ok(fallback
            .iter()
            .map(|(id, name)| Listed { id: (*id).to_string(), name: (*name).to_string(), context_window: Some(context_window(self)) })
            .collect())
    }
}

fn parse_opencode_models_output(raw: &str) -> Vec<Listed> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.contains(char::is_whitespace) || !trimmed.contains('/') {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(Listed {
                id: trimmed.to_string(),
                name: trimmed.to_string(),
                context_window: Some(context_window(CliKind::OpenCode)),
            });
        }
    }
    out
}

pub fn context_window(kind: CliKind) -> u64 {
    match kind {
        CliKind::Antigravity | CliKind::ClaudeCode => 1_000_000,
        CliKind::Codex => 400_000,
        CliKind::Cursor => 500_000,
        CliKind::Grok => 500_000,
        CliKind::OpenCode => 500_000,
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
        "write_file" | "write_to_file" | "write" | "writetoolcall" | "create_file" | "createfiletoolcall" => "write_file",
        "edit_file"
        | "replace_file_content"
        | "multi_replace_file_content"
        | "sed_file"
        | "edit"
        | "multiedit"
        | "notebookedit"
        | "edittoolcall"
        | "strreplace"
        | "str_replace"
        | "strreplacetoolcall"
        | "str_replace_editor"
        | "str_replace_based_edit_tool"
        | "apply_patch"
        | "applypatch"
        | "applypatchtoolcall"
        | "file_change" => "edit_file",
        "delete_file" | "remove_file" | "delete" | "deletetoolcall" | "deletefiletoolcall" => "delete_file",
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
        && let Some(path) = [
            "file_path",
            "Path",
            "TargetFile",
            "AbsolutePath",
            "DirectoryPath",
            "SearchDirectory",
            "SearchPath",
            "target_file",
            "notebook_path",
            "relative_workspace_path",
        ]
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

/// Build a concise cross-engine handoff briefing from prior turns in the session log when a user
/// switches mid-session to a CLI engine that has no existing external session id yet.
pub fn build_handoff_brief(events: &[crate::events::Envelope], current_turn: u64) -> Option<String> {
    if current_turn <= 1 {
        return None;
    }
    let mut turn = 0u64;
    let mut latest_summary: Option<String> = None;
    let mut exchanges: Vec<(&'static str, String)> = Vec::new();
    let mut edited_files: Vec<String> = Vec::new();

    let clip_text = |s: &str, max_chars: usize| -> String {
        let trimmed = s.trim();
        if trimmed.chars().count() <= max_chars {
            trimmed.to_string()
        } else {
            format!("{}…", trimmed.chars().take(max_chars).collect::<String>())
        }
    };

    for env in events {
        match &env.event {
            Event::TurnStart { turn: t } => turn = *t,
            _ if turn >= current_turn => break,
            Event::ContextCompacted { summary, .. } if !summary.trim().is_empty() => {
                latest_summary = Some(clip_text(summary, 1200));
                exchanges.clear();
            }
            Event::UserMessage { text, .. } if !text.trim().is_empty() => {
                exchanges.push(("用户", clip_text(text, 500)));
            }
            Event::AssistantMessage { content, .. } => {
                let text: String = content
                    .iter()
                    .filter(|b| b["type"].as_str() == Some("text"))
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    exchanges.push(("助手", clip_text(&text, 600)));
                }
            }
            Event::ToolResult { tool, is_error: false, meta, .. } if crate::tools::edits_files(tool) => {
                if let Some(path) = meta["path"].as_str().filter(|p| !p.is_empty())
                    && !edited_files.iter().any(|existing| existing == path)
                {
                    edited_files.push(path.to_string());
                }
            }
            _ => {}
        }
    }

    if latest_summary.is_none() && exchanges.is_empty() && edited_files.is_empty() {
        return None;
    }

    let mut out = String::from("[会话交接上下文 / Cross-Engine Handoff]\n你正在接手同一个会话的后续任务。以下是此前由其他模型/引擎完成的对话与文件改动摘要，请在此基础上无缝继续：\n");
    if let Some(summary) = latest_summary {
        out.push_str("\n前情摘要：\n");
        out.push_str(&summary);
        out.push('\n');
    }
    if !edited_files.is_empty() {
        out.push_str("\n此前已改动的文件：\n");
        for path in edited_files.iter().take(20) {
            out.push_str("- ");
            out.push_str(path);
            out.push('\n');
        }
    }
    if !exchanges.is_empty() {
        out.push_str("\n近期对话记录：\n");
        let start = exchanges.len().saturating_sub(10);
        for (who, msg) in &exchanges[start..] {
            out.push_str(&format!("[{who}] {msg}\n"));
        }
    }
    out.push_str("\n--- 以下是本轮用户的新指令 ---");
    Some(out)
}

/// Build a skills briefing for external CLI turns:
/// - Whenever the user explicitly invokes an `odg` skill (e.g. `按秘籍「pptx」（skill）办` or `/pptx`),
///   inline that skill's full `SKILL.md` instructions so every CLI executes it faithfully.
/// - On the first turn of a CLI session (`include_catalog = true`), also include a compact catalog of
///   available `odg` skills with their `SKILL.md` file paths so the CLI can read any matching skill on demand.
pub fn build_skills_brief(skill_roots: &[PathBuf], user_text: &str, include_catalog: bool) -> Option<String> {
    let skills = crate::skills::discover(skill_roots);
    if skills.is_empty() {
        return None;
    }

    let mut triggered: Vec<&crate::skills::Skill> = Vec::new();
    for skill in &skills {
        let name = skill.name.as_str();
        let slash = format!("/{name}");
        let by_menu = user_text.contains(&format!("「{name}」"));
        let by_slash = user_text.split_whitespace().any(|tok| tok == slash);
        if (by_menu || by_slash) && triggered.len() < 3 {
            triggered.push(skill);
        }
    }

    if triggered.is_empty() && !include_catalog {
        return None;
    }

    let mut sections: Vec<String> = Vec::new();
    for skill in &triggered {
        if let Ok(body) = crate::skills::load(skill_roots, &skill.name) {
            sections.push(format!(
                "[已启用 odg 秘籍：{}（目录：{}）]\n请严格按照以下秘籍说明执行本轮任务：\n\n{}",
                skill.name,
                skill.directory.display(),
                body
            ));
        }
    }

    if include_catalog {
        let mut cat = String::from("[可用 odg 秘籍 / Skills]\n若任务匹配以下专项秘籍，请先读取对应的 SKILL.md 并按其中规范执行：\n");
        for skill in &skills {
            let md = skill.directory.join("SKILL.md");
            cat.push_str(&format!("- {}: {} ({})\n", skill.name, skill.description, md.display()));
        }
        sections.push(cat);
    }

    if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
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
        CliKind::OpenCode => {
            let mut args = vec!["run".to_string(), "--format".to_string(), "json".to_string()];
            if let Some(session_id) = resume {
                args.push("--session".to_string());
                args.push(session_id.to_string());
            }
            if !model.is_empty() && model != "default" {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if read_only || mode == Mode::Plan {
                args.push("--agent".to_string());
                args.push("plan".to_string());
            } else if matches!(mode, Mode::Auto | Mode::Bypass) {
                args.push("--auto".to_string());
            }
            args.push("--".to_string());
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
            CliKind::Grok | CliKind::OpenCode => self.parse_grok(line),
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
                    "file_change" => {
                        let Some(changes) = item["changes"].as_array() else { return Vec::new() };
                        let failed = item["status"].as_str() == Some("failed");
                        let mut events = Vec::new();
                        for (idx, change) in changes.iter().enumerate() {
                            let Some(path) = change["path"].as_str().filter(|p| !p.is_empty()) else { continue };
                            let kind_str = change["kind"].as_str().unwrap_or("update").to_ascii_lowercase();
                            let tool_name = match kind_str.as_str() {
                                "add" | "create" => "write_file",
                                "delete" | "remove" => "delete_file",
                                _ => "edit_file",
                            };
                            let mut input = json!({ "path": path });
                            if let Some(diff_str) = change["diff"].as_str().or_else(|| change["patch"].as_str()) {
                                input["diff"] = Value::String(diff_str.to_string());
                            }
                            let key = if changes.len() == 1 { item_id.clone() } else { format!("{item_id}-{idx}") };
                            if done {
                                let output = format!("{kind_str}: {path}");
                                events.push(CliEvent::ToolDone {
                                    conversation_id: None,
                                    call_key: key,
                                    name: Some(tool_name.into()),
                                    input,
                                    output,
                                    is_error: failed,
                                });
                            } else if ty == "item.started" {
                                events.push(CliEvent::ToolStart {
                                    conversation_id: None,
                                    call_key: key,
                                    name: tool_name.into(),
                                    input,
                                });
                            }
                        }
                        events
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
                let call_key = v["call_id"]
                    .as_str()
                    .or_else(|| v["id"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| self.next_key("cursor"));
                let subtype = v["subtype"].as_str().unwrap_or("");
                // Cursor nests `{"readToolCall": {"args": {...}, "result": {"success": {...}}}}` inside `tool_call`,
                // or emits top-level `{"name": "writeToolCall", "function": {"name": ..., "arguments": ...}}`.
                let nested = v["tool_call"].as_object().and_then(|map| map.iter().next());
                let raw_name = nested
                    .map(|(k, _)| k.as_str())
                    .or_else(|| v["name"].as_str())
                    .or_else(|| v["function"]["name"].as_str())
                    .unwrap_or("tool");
                let parsed_fn_args: Option<Value> = v["function"]["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok());
                let raw_args = nested
                    .map(|(_, val)| val.get("args").unwrap_or(val))
                    .or(parsed_fn_args.as_ref())
                    .or_else(|| (!v["function"]["arguments"].is_null()).then(|| &v["function"]["arguments"]))
                    .unwrap_or(&v["args"]);
                let (name, mut input) = normalize_tool(raw_name, raw_args);
                if subtype == "completed" {
                    let res = nested.and_then(|(_, val)| val.get("result")).unwrap_or(&v["result"]);
                    let is_error = res["success"].as_bool() == Some(false)
                        || res["is_error"].as_bool() == Some(true)
                        || !res["error"].is_null()
                        || !res["rejected"].is_null();
                    let payload = if res["success"].is_object() { &res["success"] } else { res };
                    if let Some(created_lines) = payload["linesCreated"].as_u64()
                        && let Some(obj) = input.as_object_mut()
                    {
                        obj.insert("linesCreated".to_string(), json!(created_lines));
                    }
                    let output = if is_error {
                        res["error"]["message"]
                            .as_str()
                            .or_else(|| res["error"].as_str())
                            .or_else(|| res["rejected"]["reason"].as_str())
                            .or_else(|| payload["stderr"].as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| res.to_string())
                    } else if name == "run_command" && payload["exitCode"].is_i64() {
                        let code = payload["exitCode"].as_i64().unwrap_or(0);
                        let stdout = payload["stdout"].as_str().or_else(|| payload["output"].as_str()).unwrap_or("");
                        format!("Command exited with code {code}.\n\nCommand output:\n{stdout}")
                    } else if name == "read_file"
                        && payload["content"].as_str().is_none_or(str::is_empty)
                        && let Some(total) = payload["totalLines"].as_u64()
                    {
                        format!("{total} lines")
                    } else {
                        payload["content"]
                            .as_str()
                            .or_else(|| payload["stdout"].as_str())
                            .unwrap_or_default()
                            .to_string()
                    };
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
        let conv = v["session_id"]
            .as_str()
            .or_else(|| v["sessionID"].as_str())
            .or_else(|| v["conversation_id"].as_str())
            .or_else(|| v["part"]["sessionID"].as_str())
            .map(str::to_string);
        match ty {
            "init" | "system" | "step_start" => {
                if conv.is_some() {
                    vec![CliEvent::Init { conversation_id: conv }]
                } else {
                    Vec::new()
                }
            }
            "text" | "text_delta" | "assistant" => {
                let delta = v["part"]["text"]
                    .as_str()
                    .or_else(|| v["text"].as_str())
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
                let node = if v["part"].is_object() { &v["part"] } else { &v };
                let call_key = node["call_id"]
                    .as_str()
                    .or_else(|| node["callID"].as_str())
                    .or_else(|| node["id"].as_str())
                    .or_else(|| node["tool_use_id"].as_str())
                    .or_else(|| v["call_id"].as_str())
                    .or_else(|| v["callID"].as_str())
                    .or_else(|| v["id"].as_str())
                    .or_else(|| v["tool_use_id"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| self.next_key("grok"));
                let raw_name = node["name"]
                    .as_str()
                    .or_else(|| node["tool"].as_str())
                    .or_else(|| node["function"]["name"].as_str())
                    .unwrap_or("tool");
                let parsed_fn_args: Option<Value> = node["arguments"]
                    .as_str()
                    .or_else(|| node["function"]["arguments"].as_str())
                    .and_then(|s| serde_json::from_str::<Value>(s).ok());
                let args = if !node["state"]["input"].is_null() {
                    &node["state"]["input"]
                } else if !node["input"].is_null() {
                    &node["input"]
                } else if let Some(ref parsed) = parsed_fn_args {
                    parsed
                } else if !node["arguments"].is_null() {
                    &node["arguments"]
                } else {
                    &node["function"]["arguments"]
                };
                let (name, input) = normalize_tool(raw_name, args);
                let state_status = node["state"]["status"].as_str().unwrap_or("");
                if matches!(state_status, "completed" | "error") {
                    let is_error = state_status == "error" || node["is_error"].as_bool().unwrap_or(false);
                    let output = node["state"]["output"]
                        .as_str()
                        .or_else(|| node["state"]["error"].as_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    vec![
                        CliEvent::ToolStart {
                            conversation_id: conv.clone(),
                            call_key: call_key.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        },
                        CliEvent::ToolDone {
                            conversation_id: conv,
                            call_key,
                            name: Some(name),
                            input,
                            output,
                            is_error,
                        },
                    ]
                } else {
                    vec![CliEvent::ToolStart { conversation_id: conv, call_key, name, input }]
                }
            }
            "tool_result" => {
                let node = if v["part"].is_object() { &v["part"] } else { &v };
                let call_key = node["tool_use_id"]
                    .as_str()
                    .or_else(|| node["call_id"].as_str())
                    .or_else(|| node["callID"].as_str())
                    .or_else(|| node["id"].as_str())
                    .or_else(|| v["tool_use_id"].as_str())
                    .or_else(|| v["call_id"].as_str())
                    .or_else(|| v["callID"].as_str())
                    .or_else(|| v["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let content = if !node["content"].is_null() {
                    &node["content"]
                } else if !node["output"].is_null() {
                    &node["output"]
                } else {
                    &node["result"]
                };
                let output = extract_content_text(content);
                let is_error = node["is_error"].as_bool().or_else(|| v["is_error"].as_bool()).unwrap_or(false);
                vec![CliEvent::ToolDone { conversation_id: conv, call_key, name: None, input: Value::Null, output, is_error }]
            }
            "step_finish" => {
                let usage = if !v["part"]["tokens"].is_null() { v["part"]["tokens"].clone() } else { v["usage"].clone() };
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
                let msg = v["error"]["data"]["message"]
                    .as_str()
                    .or_else(|| v["error"]["message"].as_str())
                    .or_else(|| v["message"].as_str())
                    .or_else(|| v["error"].as_str())
                    .unwrap_or("CLI error")
                    .to_string();
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
    let cache_read = count("cache_read_input_tokens")
        .max(count("cache_read_tokens"))
        .max(count("cached_tokens"))
        .max(usage["cache"]["read"].as_u64().unwrap_or(0));
    let cache_write = count("cache_creation_input_tokens").max(usage["cache"]["write"].as_u64().unwrap_or(0));
    let base_input = count("input_tokens").max(count("prompt_tokens")).max(count("input"));
    let input = base_input + cache_read + cache_write;
    let output = count("output_tokens").max(count("completion_tokens")).max(count("output") + count("reasoning"));
    (input > 0 || output > 0).then_some(Tokens { input, cached: cache_read, output })
}

fn strip_agy_timestamp_envelope(raw_output: &str) -> String {
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
    cleaned.trim_start_matches('\n').to_string()
}

/// Strip `agy`'s leading `"Created At: ...\nCompleted At: ...\n"` envelope and synthesize
/// the [`Event::ToolResult`] `meta` fields that `ToolCard`, `changedFiles`, and `todos` read.
pub fn clean_tool_result(tool_name: &str, input: &Value, raw_output: &str, is_error: bool) -> (String, Value) {
    let cleaned = strip_agy_timestamp_envelope(raw_output);

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
        if let Some(content) = ["Content", "content", "file_text", "fileText"]
            .iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
        {
            return (count_lines(content), 0);
        }
        if let Some(lines) = input["linesCreated"].as_u64() {
            return (lines as usize, 0);
        }
        if !path.is_empty()
            && let Ok(disk) = std::fs::read_to_string(path)
        {
            return (count_lines(&disk), 0);
        }
        return (0, 0);
    }
    if let Some(edits) = input["edits"].as_array() {
        let mut total_added = 0usize;
        let mut total_removed = 0usize;
        for edit in edits {
            total_added += ["new_string", "new_str", "newString", "Replacement"]
                .iter()
                .find_map(|k| edit.get(*k).and_then(Value::as_str))
                .map_or(0, count_lines);
            total_removed += ["old_string", "old_str", "oldString", "Target"]
                .iter()
                .find_map(|k| edit.get(*k).and_then(Value::as_str))
                .map_or(0, count_lines);
        }
        if total_added > 0 || total_removed > 0 {
            return (total_added, total_removed);
        }
    }
    let added = ["Replacement", "new_string", "new_str", "newString", "new_source", "streamContent"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map_or(0, count_lines);
    let removed = ["Target", "old_string", "old_str", "oldString"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map_or(0, count_lines);
    (added, removed)
}

/// Parse a standard unified diff (`@@ -old,len +new,len @@`) emitted by CLIs like Codex `file_change`
/// into a [`crate::tools::LineDiff`] when pre-edit disk state was not captured on `ToolStart`.
fn parse_unified_diff(diff_text: &str) -> Option<crate::tools::LineDiff> {
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut hunks: Vec<Value> = Vec::new();
    let mut current: Option<(usize, usize, Vec<Value>)> = None;
    let mut total_hunk_lines = 0usize;

    let parse_start = |spec: &str, prefix: char| -> Option<usize> {
        let rest = spec.strip_prefix(prefix)?;
        let num = rest.split(',').next()?;
        num.parse::<usize>().ok()
    };

    for raw in diff_text.replace("\r\n", "\n").lines() {
        if let Some(after_at) = raw.strip_prefix("@@ ")
            && let Some((header, _)) = after_at.split_once(" @@")
        {
            if let Some((os, ns, lines)) = current.take()
                && !lines.is_empty()
            {
                hunks.push(json!({ "old_start": os, "new_start": ns, "lines": lines }));
            }
            let mut parts = header.split_whitespace();
            let old_start = parts.next().and_then(|s| parse_start(s, '-')).unwrap_or(1);
            let new_start = parts.next().and_then(|s| parse_start(s, '+')).unwrap_or(1);
            current = Some((old_start, new_start, Vec::new()));
            continue;
        }
        let Some((_, _, ref mut lines)) = current else { continue };
        if raw.starts_with("\\ No newline") {
            continue;
        }
        if let Some(rest) = raw.strip_prefix('+') {
            added += 1;
            total_hunk_lines += 1;
            if total_hunk_lines <= 400 {
                lines.push(json!({ "kind": "+", "text": rest }));
            }
        } else if let Some(rest) = raw.strip_prefix('-') {
            removed += 1;
            total_hunk_lines += 1;
            if total_hunk_lines <= 400 {
                lines.push(json!({ "kind": "-", "text": rest }));
            }
        } else if let Some(rest) = raw.strip_prefix(' ')
            && total_hunk_lines <= 400
        {
            lines.push(json!({ "kind": " ", "text": rest }));
        }
    }
    if let Some((os, ns, lines)) = current.take()
        && !lines.is_empty()
    {
        hunks.push(json!({ "old_start": os, "new_start": ns, "lines": lines }));
    }
    if hunks.is_empty() && added == 0 && removed == 0 {
        return None;
    }
    if total_hunk_lines > 400 {
        return Some(crate::tools::LineDiff { added, removed, hunk: Value::Null, hunks: Vec::new() });
    }
    let hunk = hunks.first().cloned().unwrap_or(Value::Null);
    let multi = if hunks.len() > 1 { hunks } else { Vec::new() };
    Some(crate::tools::LineDiff { added, removed, hunk, hunks: multi })
}

/// Resolve a tool path against `workspace`, returning `(absolute_path, display_path)` where
/// `display_path` is forward-slash workspace-relative when inside `workspace` (even across symlinked
/// roots like `/tmp` -> `/private/tmp`), or the resolved path otherwise.
fn resolve_workspace_path(workspace: &Path, raw: &str) -> (PathBuf, String) {
    let trimmed = raw.trim();
    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let resolved = crate::tools::resolve(workspace, unquoted);
    if let Ok(rel) = resolved.strip_prefix(workspace) {
        let s = rel.to_string_lossy().replace('\\', "/");
        return (resolved, if s.is_empty() { ".".to_string() } else { s });
    }
    if let Some(rel) = canonical_relative_path(workspace, &resolved) {
        return (resolved, rel);
    }
    let shown = crate::tools::display(workspace, &resolved);
    (resolved, shown)
}

fn canonical_relative_path(workspace: &Path, path: &Path) -> Option<String> {
    let canon_ws = workspace.canonicalize().ok()?;
    let mut existing = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let canon_base = loop {
        if let Ok(canon) = existing.canonicalize() {
            break canon;
        }
        missing.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?.to_path_buf();
    };
    let full = missing.iter().rev().fold(canon_base, |acc, part| acc.join(part));
    let rel = full.strip_prefix(&canon_ws).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    Some(if s.is_empty() { ".".to_string() } else { s })
}

fn relativize_tool_input(workspace: &Path, input: &mut Value) -> Option<(PathBuf, String)> {
    let raw = input.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty())?.to_string();
    let (resolved, shown) = resolve_workspace_path(workspace, &raw);
    if let Some(obj) = input.as_object_mut() {
        obj.insert("path".to_string(), Value::String(shown.clone()));
    }
    Some((resolved, shown))
}

fn read_file_bytes_capped(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > crate::tools::READ_FILE_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// Fallback for `agy --output-format stream-json`, which omits `Target`/`Replacement` and `tool_info.output`
/// on `edit_file` stdout events while recording `"Made N replacement(s) in ... (+A/-R lines)."` in its local
/// `transcript.jsonl` under `~/.gemini/antigravity-cli/brain/<conv_id>/.system_generated/logs/transcript.jsonl`.
fn lookup_agy_transcript_edit_in(home: &Path, conv_id: &str, call_key: &str) -> Option<(usize, usize, String)> {
    let conv = conv_id.trim();
    if conv.is_empty() || conv.len() > 128 || !conv.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let step_index = call_key.parse::<u64>().ok()?;
    let needle_tight = format!("\"step_index\":{step_index}");
    let needle_spaced = format!("\"step_index\": {step_index}");
    for root in [".gemini/antigravity-cli/brain", ".gemini/antigravity/brain"] {
        let path = home.join(root).join(conv).join(".system_generated").join("logs").join("transcript.jsonl");
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() || meta.len() > 20 * 1024 * 1024 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        for line in content.lines().rev() {
            if !line.contains(&needle_tight) && !line.contains(&needle_spaced) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["step_index"].as_u64() != Some(step_index) {
                continue;
            }
            let Some(raw_text) = v["content"].as_str() else { continue };
            let cleaned = strip_agy_timestamp_envelope(raw_text);
            if let Some((added, removed)) = parse_plus_minus_lines(&cleaned) {
                return Some((added, removed, cleaned));
            }
        }
    }
    None
}

fn lookup_agy_transcript_edit(conv_id: Option<&str>, call_key: &str) -> Option<(usize, usize, String)> {
    let home = dirs::home_dir()?;
    lookup_agy_transcript_edit_in(&home, conv_id?, call_key)
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
    pub snapshots: Option<&'a Path>,
    pub turn: u64,
    pub prompt: String,
    pub conversation_id: Option<String>,
    pub model: String,
    pub effort: String,
    pub mode: Mode,
    pub sandboxed: bool,
    pub read_only: bool,
    /// Extra readable directories (e.g. `~/.odg/skills`) granted to CLIs that support `--add-dir`.
    pub extra_dirs: Vec<PathBuf>,
    /// Enabled `odg` MCP servers (`~/.odg/mcp.json`) to bridge into CLIs supporting per-turn MCP config.
    pub mcp_servers: BTreeMap<String, crate::mcp::ServerConfig>,
}

/// Callbacks bridging external CLI stream events into the owning `Session`.
pub trait Sink {
    fn log(&self, event: Event) -> Result<(), String>;
    fn stream(&self, delta: StreamDelta);
}

struct StartedTool {
    id: String,
    name: String,
    input: Value,
    target: Option<(PathBuf, String)>,
    before_bytes: Option<Vec<u8>>,
    file_existed_at_start: bool,
}

/// Outcome of a single external CLI turn.
#[derive(Debug, Default)]
pub struct TurnOutcome {
    pub reason: &'static str,
    /// Newly created image/screenshot files outside the workspace observed during the turn
    /// (e.g. in an external CLI's scratch directory) so they still appear in 「产物」.
    pub extra_artifacts: Vec<PathBuf>,
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str())?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Stateful per-turn event recorder translating [`CliEvent`] items into session log [`Event`]s,
/// capturing pre/post file contents across `ToolStart` -> `ToolDone` so external CLI edits get
/// accurate `+added / -removed` counts, unified diff hunks, workspace-relative display paths, and undo snapshots.
struct TurnRecorder<'a, S: Sink> {
    params: &'a TurnParams<'a>,
    sink: &'a S,
    backend_key: &'static str,
    logged_conv: Option<String>,
    active_text: String,
    emitted_any_text: bool,
    started_tools: HashMap<String, StartedTool>,
    known_files: HashMap<PathBuf, Vec<u8>>,
    turn_started: std::time::SystemTime,
    external_artifacts: Vec<PathBuf>,
    result_error: Option<String>,
}

impl<'a, S: Sink> TurnRecorder<'a, S> {
    fn new(params: &'a TurnParams<'a>, sink: &'a S) -> Self {
        Self {
            params,
            sink,
            backend_key: params.kind.backend_key(),
            logged_conv: params.conversation_id.clone(),
            active_text: String::new(),
            emitted_any_text: false,
            started_tools: HashMap::new(),
            known_files: HashMap::new(),
            turn_started: std::time::SystemTime::now(),
            external_artifacts: Vec::new(),
            result_error: None,
        }
    }

    fn record_conv(&mut self, conv: Option<String>) -> Result<(), String> {
        if let Some(id) = conv
            && !id.is_empty()
            && self.logged_conv.as_deref() != Some(id.as_str())
        {
            self.sink.log(Event::ExternalSession {
                backend: self.backend_key.to_string(),
                session_id: id.clone(),
            })?;
            self.logged_conv = Some(id);
        }
        Ok(())
    }

    fn flush_text(&mut self, usage: Value, tokens: Option<Tokens>) -> Result<(), String> {
        if self.active_text.is_empty() {
            return Ok(());
        }
        let body = std::mem::take(&mut self.active_text);
        self.emitted_any_text = true;
        self.sink.log(Event::AssistantMessage {
            content: vec![json!({ "type": "text", "text": body })],
            stop_reason: Some("end_turn".to_string()),
            model: self.params.model.clone(),
            usage,
            tokens,
        })
    }

    fn handle_event(&mut self, ev: CliEvent) -> Result<(), String> {
        match ev {
            CliEvent::Init { conversation_id } => {
                self.record_conv(conversation_id)?;
            }
            CliEvent::AgentText { conversation_id, delta, done, usage, tokens } => {
                self.record_conv(conversation_id)?;
                if !delta.is_empty() {
                    self.active_text.push_str(&delta);
                    self.sink.stream(StreamDelta::TextDelta { index: 0, text: delta });
                }
                if done {
                    self.flush_text(usage, tokens)?;
                }
            }
            CliEvent::ToolStart { conversation_id, call_key, name, mut input } => {
                self.record_conv(conversation_id)?;
                self.flush_text(Value::Null, None)?;
                let target = relativize_tool_input(self.params.workspace, &mut input);
                if name == "read_file"
                    && let Some((ref path, _)) = target
                    && !self.known_files.contains_key(path)
                    && let Some(bytes) = read_file_bytes_capped(path)
                {
                    self.known_files.insert(path.clone(), bytes);
                }
                let (file_existed_at_start, before_bytes) = if matches!(name.as_str(), "edit_file" | "write_file" | "delete_file") {
                    let existed = target.as_ref().is_some_and(|(p, _)| p.is_file());
                    let bytes = target.as_ref().and_then(|(p, _)| read_file_bytes_capped(p));
                    (existed, bytes)
                } else {
                    (false, None)
                };
                let tool_use_id = format!("{}-{}-{call_key}", self.backend_key, self.params.turn);
                if let std::collections::hash_map::Entry::Vacant(slot) = self.started_tools.entry(call_key) {
                    self.sink.log(Event::AssistantMessage {
                        content: vec![json!({ "type": "tool_use", "id": tool_use_id, "name": name, "input": input })],
                        stop_reason: Some("tool_use".to_string()),
                        model: self.params.model.clone(),
                        usage: Value::Null,
                        tokens: None,
                    })?;
                    slot.insert(StartedTool {
                        id: tool_use_id,
                        name,
                        input,
                        target,
                        before_bytes,
                        file_existed_at_start,
                    });
                }
            }
            CliEvent::ToolDone { conversation_id, call_key, name, mut input, output, is_error } => {
                self.record_conv(conversation_id)?;
                let (started, had_start) = match self.started_tools.remove(&call_key) {
                    Some(mut existing) => {
                        if existing.target.is_none() {
                            existing.target = relativize_tool_input(self.params.workspace, &mut input);
                            if existing.target.is_some()
                                && let Some(path_val) = input.get("path").cloned()
                                && let Some(obj) = existing.input.as_object_mut()
                            {
                                obj.insert("path".to_string(), path_val);
                            }
                        }
                        for extra_key in ["diff", "linesCreated"] {
                            if let Some(val) = input.get(extra_key).cloned()
                                && let Some(obj) = existing.input.as_object_mut()
                            {
                                obj.entry(extra_key.to_string()).or_insert(val);
                            }
                        }
                        (existing, true)
                    }
                    None => {
                        self.flush_text(Value::Null, None)?;
                        let resolved_name = name.unwrap_or_else(|| "tool".to_string());
                        let target = relativize_tool_input(self.params.workspace, &mut input);
                        let id = format!("{}-{}-{call_key}", self.backend_key, self.params.turn);
                        self.sink.log(Event::AssistantMessage {
                            content: vec![json!({ "type": "tool_use", "id": id, "name": resolved_name, "input": input })],
                            stop_reason: Some("tool_use".to_string()),
                            model: self.params.model.clone(),
                            usage: Value::Null,
                            tokens: None,
                        })?;
                        (
                            StartedTool {
                                id,
                                name: resolved_name,
                                input,
                                target,
                                before_bytes: None,
                                file_existed_at_start: false,
                            },
                            false,
                        )
                    }
                };
                if started.name == "read_file"
                    && !is_error
                    && let Some((ref path, _)) = started.target
                    && !self.known_files.contains_key(path)
                    && let Some(bytes) = read_file_bytes_capped(path)
                {
                    self.known_files.insert(path.clone(), bytes);
                }
                let (mut clean_output, mut meta) = clean_tool_result(&started.name, &started.input, &output, is_error);
                if started.name == "read_file"
                    && !is_error
                    && let Some((ref path, _)) = started.target
                    && let Some(media_type) = image_media_type(path)
                    && path.is_file()
                {
                    meta["picture"] = json!(media_type);
                    meta["file"] = json!(path.display().to_string());
                    // External CLIs (e.g. agy) often write E2E screenshots into their own scratch directory
                    // outside the workspace. Track newly created images so they also appear in 「产物」.
                    let in_workspace = path.starts_with(self.params.workspace);
                    let created_this_turn = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .is_some_and(|mtime| mtime + std::time::Duration::from_secs(2) >= self.turn_started);
                    if !in_workspace && created_this_turn && !self.external_artifacts.contains(path) {
                        self.external_artifacts.push(path.clone());
                    }
                }
                if matches!(started.name.as_str(), "edit_file" | "write_file") && !is_error {
                    self.enrich_file_edit(&call_key, &started, had_start, &mut clean_output, &mut meta);
                } else if started.name == "delete_file" && !is_error {
                    self.enrich_file_delete(&started, &mut clean_output, &mut meta);
                }
                self.sink.log(Event::ToolResult {
                    tool_use_id: started.id,
                    tool: started.name,
                    content: clean_output,
                    is_error,
                    meta,
                    images: Vec::new(),
                })?;
            }
            CliEvent::Result { conversation_id, success, response, error, usage, tokens } => {
                self.record_conv(conversation_id)?;
                if !self.active_text.is_empty() {
                    self.flush_text(usage, tokens)?;
                } else if !self.emitted_any_text && !response.trim().is_empty() {
                    self.sink.log(Event::AssistantMessage {
                        content: vec![json!({ "type": "text", "text": response })],
                        stop_reason: Some("end_turn".to_string()),
                        model: self.params.model.clone(),
                        usage,
                        tokens,
                    })?;
                    self.emitted_any_text = true;
                }
                if !success {
                    self.result_error = error;
                }
            }
        }
        Ok(())
    }

    fn enrich_file_edit(&mut self, call_key: &str, started: &StartedTool, had_start: bool, clean_output: &mut String, meta: &mut Value) {
        let Some((ref resolved, ref shown)) = started.target else { return };
        let after_bytes = read_file_bytes_capped(resolved);
        let prior_known = self.known_files.get(resolved).cloned();

        let (created, prev_bytes, verified_for_undo): (bool, Option<&[u8]>, bool) = if had_start && !started.file_existed_at_start {
            let exists_now = after_bytes.is_some() || resolved.is_file();
            (exists_now, None, after_bytes.is_some())
        } else if let Some(ref b) = started.before_bytes
            && after_bytes.as_deref() != Some(b.as_slice())
        {
            (false, Some(b.as_slice()), after_bytes.is_some())
        } else if let Some(ref k) = prior_known
            && after_bytes.as_deref() != Some(k.as_slice())
        {
            (false, Some(k.as_slice()), after_bytes.is_some())
        } else {
            let guessed_created = !had_start && started.name == "write_file" && prior_known.is_none();
            (guessed_created, None, false)
        };

        let computed_diff = if created && verified_for_undo {
            after_bytes
                .as_deref()
                .and_then(|after| std::str::from_utf8(after).ok())
                .map(|after_str| crate::tools::line_diff("", &after_str.replace("\r\n", "\n")))
        } else if let (Some(prev), Some(after)) = (prev_bytes, after_bytes.as_deref()) {
            match (std::str::from_utf8(prev), std::str::from_utf8(after)) {
                (Ok(before_str), Ok(after_str)) => Some(crate::tools::line_diff(&before_str.replace("\r\n", "\n"), &after_str.replace("\r\n", "\n"))),
                _ => None,
            }
        } else {
            started.input["diff"]
                .as_str()
                .and_then(parse_unified_diff)
                .or_else(|| clean_output.contains("@@ -").then(|| parse_unified_diff(clean_output)).flatten())
        };

        let mut transcript_summary: Option<String> = None;
        let (added, removed) = if let Some(ref diff) = computed_diff {
            (diff.added, diff.removed)
        } else if let Some(counts) = parse_plus_minus_lines(clean_output) {
            counts
        } else if self.params.kind == CliKind::Antigravity
            && let Some((a, r, summary)) = lookup_agy_transcript_edit(self.logged_conv.as_deref(), call_key)
        {
            transcript_summary = Some(summary);
            (a, r)
        } else {
            estimate_edit_lines(&started.name, &started.input, &resolved.to_string_lossy())
        };

        if let Some(ref after) = after_bytes {
            self.known_files.insert(resolved.clone(), after.clone());
        }

        let abs_file = resolved.to_string_lossy().into_owned();
        let mut built = json!({
            "path": shown,
            "file": abs_file,
            "created": created,
            "added": added,
            "removed": removed,
        });
        if let Some(diff) = computed_diff {
            built["hunk"] = diff.hunk;
            if !diff.hunks.is_empty() {
                built["hunks"] = json!(diff.hunks);
            }
        }
        if !self.params.read_only
            && verified_for_undo
            && let (Some(snap_dir), Some(after)) = (self.params.snapshots, after_bytes.as_deref())
            && crate::tools::inside_workspace(self.params.workspace, resolved)
            && !crate::tools::in_git_directory(self.params.workspace, resolved)
        {
            let before_snap = prev_bytes.and_then(|prev| crate::undo::save(snap_dir, prev).ok());
            if created || before_snap.is_some() {
                built["before"] = json!(before_snap);
                built["after"] = json!(crate::undo::fingerprint(after));
                if (after.len() as u64) <= crate::undo::ARTIFACT_SNAPSHOT_BYTES
                    && let Ok(after_snap) = crate::undo::save(snap_dir, after)
                {
                    built["snapshot"] = json!(after_snap);
                }
                built["bytes"] = json!(after.len());
            }
        }
        *meta = built;

        if clean_output.trim().is_empty() {
            if let Some(summary) = transcript_summary.filter(|s| !s.trim().is_empty()) {
                *clean_output = summary;
            } else if created {
                *clean_output = format!("Created {shown} ({added} lines).");
            } else if added > 0 || removed > 0 {
                *clean_output = format!("Changed {shown}: {added} lines added, {removed} lines removed.");
            }
        }
    }

    fn enrich_file_delete(&mut self, started: &StartedTool, clean_output: &mut String, meta: &mut Value) {
        let Some((ref resolved, ref shown)) = started.target else { return };
        let prior_known = self.known_files.remove(resolved);
        let prev_bytes = started.before_bytes.as_deref().or(prior_known.as_deref());
        let abs_file = resolved.to_string_lossy().into_owned();

        let mut built = json!({
            "path": shown,
            "file": abs_file,
            "deleted": true,
        });
        let mut removed_lines = 0usize;
        if let Some(prev) = prev_bytes {
            built["bytes"] = json!(prev.len());
            if let Ok(text) = std::str::from_utf8(prev)
                && !text.contains('\0')
            {
                let diff = crate::tools::line_diff(&text.replace("\r\n", "\n"), "");
                removed_lines = diff.removed;
                built["removed"] = json!(diff.removed);
                built["hunk"] = diff.hunk;
            }
            if !self.params.read_only
                && !resolved.exists()
                && let Some(snap_dir) = self.params.snapshots
                && crate::tools::inside_workspace(self.params.workspace, resolved)
                && !crate::tools::in_git_directory(self.params.workspace, resolved)
                && let Ok(before_snap) = crate::undo::save(snap_dir, prev)
            {
                built["changes"] = json!([{
                    "file": abs_file,
                    "path": shown,
                    "before": before_snap,
                    "after": Value::Null,
                }]);
            }
        } else if let Some(diff) = started.input["diff"].as_str().and_then(parse_unified_diff) {
            removed_lines = diff.removed;
            built["removed"] = json!(diff.removed);
            built["hunk"] = diff.hunk;
        }
        *meta = built;
        if clean_output.trim().is_empty() {
            *clean_output = if removed_lines > 0 {
                format!("Deleted {shown} ({removed_lines} lines).")
            } else {
                format!("Deleted {shown}.")
            };
        }
    }
}

/// Safe command-line prompt byte budget before spilling to `stdin` (Codex / Claude Code)
/// or a temporary UTF-8 file (Antigravity / Cursor / Grok).
/// Windows `.cmd` wrappers cap command lines at ~8,191 chars (`CreateProcessW` at 32,767 chars,
/// returning `os error 206`); Linux caps each argv entry (`MAX_ARG_STRLEN`) at 128 KB.
#[cfg(windows)]
const MAX_CLI_PROMPT_BYTES: usize = 6_000;
#[cfg(not(windows))]
const MAX_CLI_PROMPT_BYTES: usize = 64_000;

struct TempPromptGuard(Option<PathBuf>);

impl Drop for TempPromptGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct PreparedPrompt {
    cli_prompt: String,
    stdin_bytes: Option<Vec<u8>>,
    extra_dir: Option<PathBuf>,
    _guard: TempPromptGuard,
}

fn prepare_prompt_delivery(kind: CliKind, prompt: &str, turn: u64, limit: usize) -> Result<PreparedPrompt, String> {
    if prompt.len() <= limit {
        return Ok(PreparedPrompt {
            cli_prompt: prompt.to_string(),
            stdin_bytes: None,
            extra_dir: None,
            _guard: TempPromptGuard(None),
        });
    }
    if matches!(kind, CliKind::Codex | CliKind::ClaudeCode) {
        return Ok(PreparedPrompt {
            cli_prompt: String::new(),
            stdin_bytes: Some(prompt.as_bytes().to_vec()),
            extra_dir: None,
            _guard: TempPromptGuard(None),
        });
    }
    let dir = std::env::temp_dir().join("odg-prompts");
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法创建临时提示词目录：{error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("prompt-{}-{turn}-{stamp}.txt", std::process::id()));
    std::fs::write(&path, prompt.as_bytes()).map_err(|error| format!("无法写入临时提示词文件：{error}"))?;
    let cli_prompt = format!(
        "本轮用户的完整输入内容较长，已保存到文件：{}\n请先完整读取该文件的内容，再直接按其中的要求回答或处理。",
        path.display()
    );
    Ok(PreparedPrompt {
        cli_prompt,
        stdin_bytes: None,
        extra_dir: Some(dir),
        _guard: TempPromptGuard(Some(path)),
    })
}

/// Adjust `build_args` output when the prompt is delivered via `stdin` or accompanied by an extra readable directory.
fn apply_prompt_delivery_args(kind: CliKind, args: &mut Vec<String>, prepared: &PreparedPrompt) {
    if prepared.stdin_bytes.is_some() {
        // Both `Codex` and `ClaudeCode` append `["--", prompt]` at the tail of `build_args`.
        if args.len() >= 2 && args[args.len() - 2] == "--" {
            args.truncate(args.len() - 2);
        }
        if kind == CliKind::Codex {
            args.push("-".to_string());
        }
    }
    if kind == CliKind::Antigravity
        && let Some(dir) = prepared.extra_dir.as_deref().and_then(Path::to_str)
    {
        args.push("--add-dir".to_string());
        args.push(dir.to_string());
    }
}

/// Insert CLI flags before the trailing `-- <prompt>` (or `-`) separator when present, or append at the end.
fn insert_before_prompt_tail(args: &mut Vec<String>, extra: Vec<String>) {
    if extra.is_empty() {
        return;
    }
    let pos = if args.len() >= 2 && args[args.len() - 2] == "--" {
        args.len() - 2
    } else if args.last().is_some_and(|a| a == "-") {
        args.len() - 1
    } else {
        args.len()
    };
    args.splice(pos..pos, extra);
}

/// Bridge enabled `odg` MCP servers (`~/.odg/mcp.json`) into CLIs that support per-turn MCP injection:
/// - Claude Code: `--mcp-config <temp.json>`
/// - Codex: `-c mcp_servers.<name>.command=...` / `.args=[...]` / `.url=...`
///
/// Note: Antigravity (`agy`) manages MCP servers via `agy mcp add` rather than `--settings`,
/// and rejects `"mcpServers"` in `--settings`, so we do not pass `--settings` to `agy`.
fn apply_mcp_bridge_args(
    kind: CliKind,
    args: &mut Vec<String>,
    servers: &BTreeMap<String, crate::mcp::ServerConfig>,
    turn: u64,
) -> Option<TempPromptGuard> {
    if servers.is_empty() {
        return None;
    }
    match kind {
        CliKind::ClaudeCode => {
            let mut map = serde_json::Map::new();
            for (name, srv) in servers {
                let mut entry = serde_json::Map::new();
                if let Some(cmd) = srv.command.as_deref().filter(|c| !c.trim().is_empty()) {
                    entry.insert("command".into(), json!(cmd));
                    if !srv.args.is_empty() {
                        entry.insert("args".into(), json!(srv.args));
                    }
                    if !srv.env.is_empty() {
                        entry.insert("env".into(), json!(srv.env));
                    }
                } else if let Some(url) = srv.url.as_deref().filter(|u| !u.trim().is_empty()) {
                    entry.insert("type".into(), json!("http"));
                    entry.insert("url".into(), json!(url));
                    if !srv.headers.is_empty() {
                        entry.insert("headers".into(), json!(srv.headers));
                    }
                } else {
                    continue;
                }
                map.insert(name.clone(), Value::Object(entry));
            }
            if map.is_empty() {
                return None;
            }
            let dir = std::env::temp_dir().join("odg-mcp");
            std::fs::create_dir_all(&dir).ok()?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = dir.join(format!("mcp-{}-{turn}-{stamp}.json", std::process::id()));
            let payload = serde_json::to_vec_pretty(&json!({ "mcpServers": map })).ok()?;
            std::fs::write(&path, payload).ok()?;
            insert_before_prompt_tail(args, vec!["--mcp-config".to_string(), path.to_string_lossy().into_owned()]);
            Some(TempPromptGuard(Some(path)))
        }
        CliKind::Codex => {
            let mut flags = Vec::new();
            for (name, srv) in servers {
                if let Some(cmd) = srv.command.as_deref().filter(|c| !c.trim().is_empty())
                    && let Ok(lit) = serde_json::to_string(cmd)
                {
                    flags.push("-c".to_string());
                    flags.push(format!("mcp_servers.{name}.command={lit}"));
                    if !srv.args.is_empty()
                        && let Ok(arr) = serde_json::to_string(&srv.args)
                    {
                        flags.push("-c".to_string());
                        flags.push(format!("mcp_servers.{name}.args={arr}"));
                    }
                } else if let Some(url) = srv.url.as_deref().filter(|u| !u.trim().is_empty())
                    && let Ok(lit) = serde_json::to_string(url)
                {
                    flags.push("-c".to_string());
                    flags.push(format!("mcp_servers.{name}.url={lit}"));
                }
            }
            insert_before_prompt_tail(args, flags);
            None
        }
        CliKind::Antigravity | CliKind::Cursor | CliKind::Grok | CliKind::OpenCode => None,
    }
}

/// Build `agy mcp add ...` arguments for one enabled `odg` MCP server.
fn build_agy_mcp_add_args(name: &str, srv: &crate::mcp::ServerConfig) -> Option<Vec<String>> {
    if srv.disabled {
        return None;
    }
    let mut args = vec!["mcp".to_string(), "add".to_string()];
    if let Some(cmd) = srv.command.as_deref().filter(|c| !c.trim().is_empty()) {
        args.push("--type".to_string());
        args.push("stdio".to_string());
        for (k, v) in &srv.env {
            args.push("--env".to_string());
            args.push(format!("{k}={v}"));
        }
        args.push(name.to_string());
        args.push("--".to_string());
        args.push(cmd.to_string());
        args.extend(srv.args.iter().cloned());
        Some(args)
    } else if let Some(url) = srv.url.as_deref().filter(|u| !u.trim().is_empty()) {
        args.push("--type".to_string());
        args.push("http".to_string());
        for (k, v) in &srv.headers {
            args.push("--header".to_string());
            args.push(format!("{k}: {v}"));
        }
        args.push(name.to_string());
        args.push(url.to_string());
        Some(args)
    } else {
        None
    }
}

/// Sync enabled `odg` MCP servers (`~/.odg/mcp.json`) into Antigravity CLI via `agy mcp add`.
/// Cached by a serialized snapshot so unchanged configs add zero per-turn latency.
async fn sync_antigravity_mcp(bin: &Path, servers: &std::collections::BTreeMap<String, crate::mcp::ServerConfig>) {
    static LAST_SYNCED: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    let enabled: Vec<(&String, &crate::mcp::ServerConfig)> = servers.iter().filter(|(_, s)| !s.disabled).collect();
    if enabled.is_empty() {
        return;
    }
    let sig = serde_json::to_string(&enabled).unwrap_or_default();
    let cache = LAST_SYNCED.get_or_init(|| std::sync::Mutex::new(None));
    if cache.lock().unwrap().as_deref() == Some(sig.as_str()) {
        return;
    }
    let mut all_ok = true;
    for (name, srv) in enabled {
        let Some(add_args) = build_agy_mcp_add_args(name, srv) else { continue };
        let mut cmd = Command::new(bin);
        cmd.args(&add_args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let ran = tokio::time::timeout(std::time::Duration::from_secs(8), cmd.status()).await;
        if !matches!(ran, Ok(Ok(status)) if status.success()) {
            all_ok = false;
        }
    }
    if all_ok {
        *cache.lock().unwrap() = Some(sig);
    }
}

/// Execute one headless streaming turn on any supported [`CliKind`].
pub async fn run_turn<S: Sink>(params: TurnParams<'_>, sink: &S, cancel: &CancellationToken) -> Result<TurnOutcome, String> {
    let bin = params
        .kind
        .find_binary()
        .ok_or_else(|| format!("未找到本机 {}，请先安装并完成登录。", params.kind.display_name()))?;
    let prepared = prepare_prompt_delivery(params.kind, &params.prompt, params.turn, MAX_CLI_PROMPT_BYTES)?;
    let mut args = build_args(
        params.kind,
        &prepared.cli_prompt,
        Some(params.workspace),
        params.conversation_id.as_deref(),
        &params.model,
        &params.effort,
        params.mode,
        params.sandboxed,
        params.read_only,
    );
    apply_prompt_delivery_args(params.kind, &mut args, &prepared);
    if params.kind == CliKind::Antigravity {
        for dir in &params.extra_dirs {
            if dir.is_dir()
                && let Some(s) = dir.to_str()
            {
                args.push("--add-dir".to_string());
                args.push(s.to_string());
            }
        }
        sync_antigravity_mcp(&bin, &params.mcp_servers).await;
    }
    let _mcp_guard = apply_mcp_bridge_args(params.kind, &mut args, &params.mcp_servers, params.turn);
    let mut cmd = Command::new(&bin);
    cmd.args(&args)
        .current_dir(params.workspace)
        .stdin(if prepared.stdin_bytes.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|error| format!("无法启动 {}：{error}", params.kind.display_name()))?;
    if let Some(bytes) = prepared.stdin_bytes
        && let Some(mut stdin_pipe) = child.stdin.take()
    {
        tokio::spawn(async move {
            let _ = stdin_pipe.write_all(&bytes).await;
            let _ = stdin_pipe.shutdown().await;
        });
    }
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
    let mut recorder = TurnRecorder::new(&params, sink);

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
            recorder.handle_event(ev)?;
        }
    };

    if cancelled {
        group_guard.0 = None;
        let _ = child.wait().await;
        return Ok(TurnOutcome { reason: "cancelled", extra_artifacts: recorder.external_artifacts });
    }

    recorder.flush_text(Value::Null, None)?;
    let status = child.wait().await.map_err(|error| format!("等待 {} 结束失败：{error}", params.kind.display_name()))?;
    group_guard.0 = None;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if let Some(err) = recorder.result_error {
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
    Ok(TurnOutcome { reason: "completed", extra_artifacts: recorder.external_artifacts })
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

        let opencode = build_args(CliKind::OpenCode, "refactor", None, Some("ses_7"), "anthropic/claude-sonnet-4-6", "high", Mode::Plan, false, false);
        assert_eq!(
            opencode,
            ["run", "--format", "json", "--session", "ses_7", "--model", "anthropic/claude-sonnet-4-6", "--agent", "plan", "--", "refactor"]
        );
        let opencode_auto = build_args(CliKind::OpenCode, "fix", None, None, "opencode/big-pickle", "high", Mode::Auto, false, false);
        assert_eq!(
            opencode_auto,
            ["run", "--format", "json", "--model", "opencode/big-pickle", "--auto", "--", "fix"]
        );
        assert_eq!(serde_json::to_value(CliKind::OpenCode).unwrap(), "opencode");
    }

    #[test]
    fn parses_opencode_v2_stream_events_and_models() {
        let mut parser = StreamParser::default();
        let ev1 = parser.parse_line(
            CliKind::OpenCode,
            r#"{"type":"step_start","sessionID":"ses_1","part":{"type":"step-start"}}"#,
        );
        assert!(matches!(ev1.as_slice(), [CliEvent::Init { conversation_id: Some(id) }] if id == "ses_1"));

        let ev3 = parser.parse_line(
            CliKind::OpenCode,
            r#"{"type":"tool_use","sessionID":"ses_1","part":{"type":"tool","id":"call_1","tool":"read","state":{"status":"completed","input":{"path":"package.json"},"output":"ok"}}}"#,
        );
        assert_eq!(ev3.len(), 2);
        assert!(matches!(&ev3[0], CliEvent::ToolStart { name, .. } if name == "read_file"));
        assert!(matches!(&ev3[1], CliEvent::ToolDone { output, is_error: false, .. } if output == "ok"));

        let ev4 = parser.parse_line(
            CliKind::OpenCode,
            r#"{"type":"step_finish","sessionID":"ses_1","part":{"type":"step-finish","reason":"stop","tokens":{"input":788,"output":10,"reasoning":1,"cache":{"read":17256,"write":0}}}}"#,
        );
        assert!(matches!(
            ev4.as_slice(),
            [CliEvent::AgentText { done: true, tokens: Some(Tokens { input: 18044, cached: 17256, output: 11 }), .. }]
        ));

        let listed = parse_opencode_models_output("opencode/big-pickle\nopencode/claude-sonnet-5-5\nopencode/big-pickle\nnot a model\n");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "opencode/big-pickle");
        assert_eq!(listed[1].id, "opencode/claude-sonnet-5-5");
    }

    #[test]
    fn builds_cross_engine_handoff_brief_from_prior_turns() {
        let events = vec![
            crate::events::Envelope { seq: 1, ts: "2026-11-24T00:00:00Z".into(), event: Event::TurnStart { turn: 1 } },
            crate::events::Envelope {
                seq: 2,
                ts: "2026-11-24T00:00:01Z".into(),
                event: Event::UserMessage { text: "修一下 motor.c 的重锁逻辑".into(), steering: false, mode: Mode::Auto, images: vec![], files: vec![] },
            },
            crate::events::Envelope {
                seq: 3,
                ts: "2026-11-24T00:00:02Z".into(),
                event: Event::ToolResult {
                    tool_use_id: "t1".into(),
                    tool: "edit_file".into(),
                    content: "ok".into(),
                    is_error: false,
                    meta: json!({ "path": "peripheral/src/motor.c", "added": 5, "removed": 1 }),
                    images: vec![],
                },
            },
            crate::events::Envelope {
                seq: 4,
                ts: "2026-11-24T00:00:03Z".into(),
                event: Event::AssistantMessage {
                    content: vec![json!({ "type": "text", "text": "已修复 motor.c 停稳判定。" })],
                    stop_reason: Some("end_turn".into()),
                    model: "claude-opus-4-7".into(),
                    usage: Value::Null,
                    tokens: None,
                },
            },
            crate::events::Envelope { seq: 5, ts: "2026-11-24T00:00:04Z".into(), event: Event::TurnStart { turn: 2 } },
        ];
        assert!(build_handoff_brief(&events, 1).is_none());
        let brief = build_handoff_brief(&events, 2).expect("handoff brief");
        assert!(brief.contains("peripheral/src/motor.c"));
        assert!(brief.contains("修一下 motor.c"));
        assert!(brief.contains("已修复 motor.c 停稳判定"));
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

    #[test]
    fn turn_recorder_computes_agy_edit_diffs_relativizes_paths_and_records_undo_snapshots() {
        use std::sync::Mutex;

        struct TestSink(Mutex<Vec<crate::events::Envelope>>);
        impl Sink for TestSink {
            fn log(&self, event: Event) -> Result<(), String> {
                let mut guard = self.0.lock().unwrap();
                let seq = guard.len() as u64 + 1;
                guard.push(crate::events::Envelope {
                    seq,
                    ts: "2026-11-24T08:00:00Z".to_string(),
                    event,
                });
                Ok(())
            }
            fn stream(&self, _delta: StreamDelta) {}
        }

        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("Firmware");
        let snap_dir = tmp.path().join("snapshots");
        let motor_dir = workspace.join("peripheral").join("src");
        std::fs::create_dir_all(&motor_dir).unwrap();
        let motor_c = motor_dir.join("motor.c");
        std::fs::write(&motor_c, "int a = 1;\nint b = 2;\nint c = 3;\n").unwrap();

        let params = TurnParams {
            kind: CliKind::Antigravity,
            workspace: &workspace,
            snapshots: Some(&snap_dir),
            turn: 1,
            prompt: "fix motor".into(),
            conversation_id: Some("35f75987-1c92-4e43-907b-a54a7f66ac2f".into()),
            model: "gemini-4-neon".into(),
            effort: "xhigh".into(),
            mode: Mode::Auto,
            sandboxed: false,
            read_only: false,
            extra_dirs: Vec::new(),
            mcp_servers: BTreeMap::new(),
        };
        let sink = TestSink(Mutex::new(Vec::new()));
        sink.log(Event::TurnStart { turn: 1 }).unwrap();
        let mut recorder = TurnRecorder::new(&params, &sink);

        // 1) `agy` edit_file omits Target/Replacement and sends empty output "".
        let abs_motor = motor_c.to_string_lossy().into_owned();
        let (_, edit_input) = normalize_tool("edit_file", &json!({ "Path": abs_motor }));
        recorder
            .handle_event(CliEvent::ToolStart {
                conversation_id: None,
                call_key: "121".into(),
                name: "edit_file".into(),
                input: edit_input.clone(),
            })
            .unwrap();
        std::fs::write(&motor_c, "int a = 1;\nint b = 20;\nint b2 = 21;\nint c = 3;\n").unwrap();
        recorder
            .handle_event(CliEvent::ToolDone {
                conversation_id: None,
                call_key: "121".into(),
                name: Some("edit_file".into()),
                input: edit_input,
                output: String::new(),
                is_error: false,
            })
            .unwrap();

        // 2) `agy` write_file creates a brand-new test script in workspace.
        let new_test = workspace.join("scripts").join("test_io_park_host.py");
        let (_, write_input) = normalize_tool("write_file", &json!({ "Path": new_test.to_string_lossy() }));
        recorder
            .handle_event(CliEvent::ToolStart {
                conversation_id: None,
                call_key: "137".into(),
                name: "write_file".into(),
                input: write_input.clone(),
            })
            .unwrap();
        std::fs::create_dir_all(new_test.parent().unwrap()).unwrap();
        std::fs::write(&new_test, "def test_park():\n    assert True\n").unwrap();
        recorder
            .handle_event(CliEvent::ToolDone {
                conversation_id: None,
                call_key: "137".into(),
                name: Some("write_file".into()),
                input: write_input,
                output: String::new(),
                is_error: false,
            })
            .unwrap();

        let events = sink.0.lock().unwrap().clone();
        let edit_res = events
            .iter()
            .find_map(|env| match &env.event {
                Event::ToolResult { tool, meta, .. } if tool == "edit_file" => Some(meta.clone()),
                _ => None,
            })
            .expect("edit_file ToolResult logged");
        assert_eq!(edit_res["path"], "peripheral/src/motor.c");
        assert_eq!(edit_res["file"], motor_c.to_string_lossy().as_ref());
        assert_eq!(edit_res["created"], false);
        assert_eq!(edit_res["added"], 2);
        assert_eq!(edit_res["removed"], 1);
        assert!(edit_res["hunk"].is_object(), "expected structured diff hunk");

        let write_res = events
            .iter()
            .find_map(|env| match &env.event {
                Event::ToolResult { tool, meta, .. } if tool == "write_file" => Some(meta.clone()),
                _ => None,
            })
            .expect("write_file ToolResult logged");
        assert_eq!(write_res["path"], "scripts/test_io_park_host.py");
        assert_eq!(write_res["created"], true);
        assert_eq!(write_res["added"], 2);
        assert_eq!(write_res["removed"], 0);

        // Verify undo restores `motor.c` to its original content and deletes the created script!
        let plan = crate::undo::plan(&events, 1);
        assert_eq!(plan.len(), 2);
        let outcome = crate::undo::apply(&plan, &snap_dir);
        assert_eq!(outcome.restored, vec!["peripheral/src/motor.c", "scripts/test_io_park_host.py"]);
        assert_eq!(std::fs::read_to_string(&motor_c).unwrap(), "int a = 1;\nint b = 2;\nint c = 3;\n");
        assert!(!new_test.exists());
    }

    #[test]
    fn extracts_agy_transcript_fallback_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let conv = "35f75987-1c92-4e43-907b-a54a7f66ac2f";
        let log_dir = tmp
            .path()
            .join(".gemini")
            .join("antigravity-cli")
            .join("brain")
            .join(conv)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("transcript.jsonl"),
            r#"{"step_index":125,"source":"MODEL","type":"GENERIC","status":"DONE","content":"Created At: 2026-11-24T15:45:15+08:00\nCompleted At: 2026-11-24T15:45:28+08:00\nMade 1 replacement in /Users/x/motor.c starting at line 906 (+25/-40 lines)."}"#,
        )
        .unwrap();
        let found = lookup_agy_transcript_edit_in(tmp.path(), conv, "125").expect("transcript fallback");
        assert_eq!(found.0, 25);
        assert_eq!(found.1, 40);
        assert!(found.2.contains("(+25/-40 lines)"));
    }

    #[test]
    fn tracks_codex_multifile_patch_claude_multiedit_cursor_nested_result_and_grok_call_id() {
        use std::sync::Mutex;

        struct VecSink(Mutex<Vec<Event>>);
        impl Sink for VecSink {
            fn log(&self, event: Event) -> Result<(), String> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
            fn stream(&self, _delta: StreamDelta) {}
        }

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(ws.join("src")).unwrap();

        // 1) Codex: single `item.completed` `file_change` (no `item.started`) with 3 files (`update` with unified diff, `add`, `delete`).
        let created_rs = ws.join("src/new_mod.rs");
        std::fs::write(&created_rs, "pub fn x() {}\npub fn y() {}\n").unwrap();
        let codex_params = TurnParams {
            kind: CliKind::Codex,
            workspace: &ws,
            snapshots: None,
            turn: 1,
            prompt: "patch".into(),
            conversation_id: None,
            model: "gpt-5.4".into(),
            effort: "high".into(),
            mode: Mode::Auto,
            sandboxed: false,
            read_only: false,
            extra_dirs: Vec::new(),
            mcp_servers: BTreeMap::new(),
        };
        let codex_sink = VecSink(Mutex::new(Vec::new()));
        let mut codex_rec = TurnRecorder::new(&codex_params, &codex_sink);
        let mut codex_parser = StreamParser::default();
        let codex_line = json!({
            "type": "item.completed",
            "item": {
                "id": "item_7",
                "type": "file_change",
                "status": "completed",
                "changes": [
                    {
                        "path": "src/lib.rs",
                        "kind": "update",
                        "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-fn old() {}\n+fn new1() {}\n+fn new2() {}\n"
                    },
                    { "path": created_rs.to_string_lossy(), "kind": "add" },
                    { "path": "src/obsolete.rs", "kind": "delete", "diff": "@@ -1,2 +0,0 @@\n-dead1\n-dead2\n" }
                ]
            }
        })
        .to_string();
        for ev in codex_parser.parse_line(CliKind::Codex, &codex_line) {
            codex_rec.handle_event(ev).unwrap();
        }
        let codex_results: Vec<(String, Value)> = codex_sink
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::ToolResult { tool, meta, .. } => Some((tool.clone(), meta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(codex_results.len(), 3);
        assert_eq!((codex_results[0].0.as_str(), codex_results[0].1["path"].as_str(), codex_results[0].1["added"].as_u64(), codex_results[0].1["removed"].as_u64()), ("edit_file", Some("src/lib.rs"), Some(2), Some(1)));
        assert!(codex_results[0].1["hunk"].is_object());
        assert_eq!((codex_results[1].0.as_str(), codex_results[1].1["path"].as_str(), codex_results[1].1["created"].as_bool(), codex_results[1].1["added"].as_u64()), ("write_file", Some("src/new_mod.rs"), Some(true), Some(2)));
        assert_eq!((codex_results[2].0.as_str(), codex_results[2].1["deleted"].as_bool(), codex_results[2].1["removed"].as_u64()), ("delete_file", Some(true), Some(2)));

        // 2) Claude Code `MultiEdit` fallback estimation on `edits` array.
        let (_, multi_in) = normalize_tool(
            "MultiEdit",
            &json!({
                "file_path": "src/app.ts",
                "edits": [
                    { "old_string": "a\n", "new_string": "a1\na2\n" },
                    { "old_string": "b1\nb2\n", "new_string": "b\n" }
                ]
            }),
        );
        let (_, multi_meta) = clean_tool_result("edit_file", &multi_in, "", false);
        assert_eq!((multi_meta["added"].as_u64(), multi_meta["removed"].as_u64()), (Some(3), Some(3)));

        // 3) Cursor `strReplaceToolCall` + nested `result.success`.
        let mut cursor_parser = StreamParser::default();
        let cur_start = cursor_parser.parse_line(
            CliKind::Cursor,
            r#"{"type":"tool_call","subtype":"started","call_id":"c9","tool_call":{"strReplaceToolCall":{"args":{"path":"src/main.rs"}}}}"#,
        );
        assert_eq!(cur_start[0], CliEvent::ToolStart {
            conversation_id: None,
            call_key: "c9".into(),
            name: "edit_file".into(),
            input: json!({ "path": "src/main.rs" }),
        });
        let cur_read_done = cursor_parser.parse_line(
            CliKind::Cursor,
            r#"{"type":"tool_call","subtype":"completed","call_id":"c10","tool_call":{"readToolCall":{"args":{"path":"src/main.rs"},"result":{"success":{"totalLines":88}}}}}"#,
        );
        if let CliEvent::ToolDone { output, is_error, .. } = &cur_read_done[0] {
            assert!(!is_error);
            assert_eq!(output, "88 lines");
        } else {
            panic!("expected ToolDone");
        }

        // 4) Grok `tool_use` + `tool_result` paired via `call_id`.
        let grok_file = ws.join("src/grok.rs");
        std::fs::write(&grok_file, "let x = 1;\n").unwrap();
        let grok_params = TurnParams {
            kind: CliKind::Grok,
            workspace: &ws,
            snapshots: None,
            turn: 1,
            prompt: "edit".into(),
            conversation_id: None,
            model: "grok-4".into(),
            effort: "high".into(),
            mode: Mode::Auto,
            sandboxed: false,
            read_only: false,
            extra_dirs: Vec::new(),
            mcp_servers: BTreeMap::new(),
        };
        let grok_sink = VecSink(Mutex::new(Vec::new()));
        let mut grok_rec = TurnRecorder::new(&grok_params, &grok_sink);
        let mut grok_parser = StreamParser::default();
        for ev in grok_parser.parse_line(
            CliKind::Grok,
            &json!({"type":"tool_use","tool":"str_replace_editor","call_id":"call_42","input":{"path":grok_file.to_string_lossy()}}).to_string(),
        ) {
            grok_rec.handle_event(ev).unwrap();
        }
        std::fs::write(&grok_file, "let x = 2;\nlet y = 3;\n").unwrap();
        for ev in grok_parser.parse_line(
            CliKind::Grok,
            r#"{"type":"tool_result","call_id":"call_42","content":"ok"}"#,
        ) {
            grok_rec.handle_event(ev).unwrap();
        }
        let grok_meta = grok_sink
            .0
            .lock()
            .unwrap()
            .iter()
            .find_map(|e| match e {
                Event::ToolResult { tool, meta, .. } if tool == "edit_file" => Some(meta.clone()),
                _ => None,
            })
            .expect("Grok edit_file paired via call_id");
        assert_eq!(grok_meta["path"], "src/grok.rs");
        assert_eq!((grok_meta["added"].as_u64(), grok_meta["removed"].as_u64()), (Some(2), Some(1)));
    }

    #[test]
    fn spills_oversized_prompts_to_stdin_or_temp_file() {
        let huge = "x".repeat(256);

        // Codex switches to `exec ... -` + stdin.
        let codex_prep = prepare_prompt_delivery(CliKind::Codex, &huge, 2, 64).unwrap();
        assert_eq!(codex_prep.stdin_bytes.as_deref(), Some(huge.as_bytes()));
        let mut codex_args = build_args(CliKind::Codex, &codex_prep.cli_prompt, None, None, "gpt-5.4", "high", Mode::Auto, false, false);
        apply_prompt_delivery_args(CliKind::Codex, &mut codex_args, &codex_prep);
        assert_eq!(codex_args.last().map(String::as_str), Some("-"));
        assert!(!codex_args.iter().any(|a| a == "--"));

        // Claude Code omits positional prompt and pipes via stdin.
        let claude_prep = prepare_prompt_delivery(CliKind::ClaudeCode, &huge, 2, 64).unwrap();
        assert_eq!(claude_prep.stdin_bytes.as_deref(), Some(huge.as_bytes()));
        let mut claude_args = build_args(CliKind::ClaudeCode, &claude_prep.cli_prompt, None, None, "opus", "high", Mode::Auto, false, false);
        apply_prompt_delivery_args(CliKind::ClaudeCode, &mut claude_args, &claude_prep);
        assert!(!claude_args.iter().any(|a| a == "--"));

        // Antigravity spills to a temporary UTF-8 file, adds `--add-dir`, and cleans up on drop.
        let agy_prep = prepare_prompt_delivery(CliKind::Antigravity, &huge, 2, 64).unwrap();
        assert!(agy_prep.stdin_bytes.is_none());
        let spilled_path = agy_prep._guard.0.clone().expect("temp prompt file");
        assert_eq!(std::fs::read_to_string(&spilled_path).unwrap(), huge);
        let mut agy_args = build_args(CliKind::Antigravity, &agy_prep.cli_prompt, None, None, "gemini-4-neon", "high", Mode::Auto, false, false);
        apply_prompt_delivery_args(CliKind::Antigravity, &mut agy_args, &agy_prep);
        assert!(agy_args.windows(2).any(|w| w[0] == "--add-dir" && w[1].contains("odg-prompts")));
        drop(agy_prep);
        assert!(!spilled_path.exists());
    }

    #[test]
    fn bridges_odg_skills_and_mcp_servers_to_cli_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("demo-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: 演示专项秘籍\n---\n\n第一步：先检查输入。\n第二步：输出结果。\n",
        )
        .unwrap();

        let roots = vec![tmp.path().to_path_buf()];
        let triggered = build_skills_brief(&roots, "按秘籍「demo-skill」（skill）办：处理这份数据", false).expect("triggered skill brief");
        assert!(triggered.contains("第一步：先检查输入"));
        assert!(!triggered.contains("[可用 odg 秘籍"));

        let first_turn = build_skills_brief(&roots, "帮我看看代码", true).expect("catalog brief");
        assert!(first_turn.contains("[可用 odg 秘籍 / Skills]"));
        assert!(first_turn.contains("demo-skill: 演示专项秘籍"));
        assert!(first_turn.contains("SKILL.md"));

        let mut servers = BTreeMap::new();
        servers.insert(
            "docs".to_string(),
            crate::mcp::ServerConfig {
                command: Some("npx".into()),
                args: vec!["-y".into(), "docs-mcp".into()],
                ..Default::default()
            },
        );
        servers.insert(
            "remote".to_string(),
            crate::mcp::ServerConfig {
                url: Some("https://mcp.example.com".into()),
                ..Default::default()
            },
        );

        let mut claude_args = build_args(CliKind::ClaudeCode, "hi", None, None, "opus", "high", Mode::Auto, false, false);
        let claude_guard = apply_mcp_bridge_args(CliKind::ClaudeCode, &mut claude_args, &servers, 1).expect("claude mcp temp file");
        let cfg_idx = claude_args.iter().position(|a| a == "--mcp-config").expect("--mcp-config flag");
        let sep_idx = claude_args.iter().position(|a| a == "--").expect("-- separator");
        assert!(cfg_idx < sep_idx);
        let cfg_path = PathBuf::from(&claude_args[cfg_idx + 1]);
        let cfg_json: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
        assert_eq!(cfg_json["mcpServers"]["docs"]["command"], "npx");
        assert_eq!(cfg_json["mcpServers"]["remote"]["type"], "http");
        drop(claude_guard);
        assert!(!cfg_path.exists());

        let mut codex_args = build_args(CliKind::Codex, "hi", None, None, "gpt-5.4", "high", Mode::Auto, false, false);
        assert!(apply_mcp_bridge_args(CliKind::Codex, &mut codex_args, &servers, 1).is_none());
        assert!(codex_args.iter().any(|a| a == "mcp_servers.docs.command=\"npx\""));
        assert!(codex_args.iter().any(|a| a == "mcp_servers.remote.url=\"https://mcp.example.com\""));

        let mut agy_args = build_args(CliKind::Antigravity, "hi", None, None, "gemini-4-neon", "high", Mode::Auto, false, false);
        assert!(apply_mcp_bridge_args(CliKind::Antigravity, &mut agy_args, &servers, 1).is_none());
        assert!(!agy_args.iter().any(|a| a == "--settings"));
        assert_eq!(
            build_agy_mcp_add_args("docs", &servers["docs"]).unwrap(),
            ["mcp", "add", "--type", "stdio", "docs", "--", "npx", "-y", "docs-mcp"]
        );
        assert_eq!(
            build_agy_mcp_add_args("remote", &servers["remote"]).unwrap(),
            ["mcp", "add", "--type", "http", "remote", "https://mcp.example.com"]
        );
    }

    #[test]
    fn cli_read_file_on_external_screenshot_sets_picture_meta_and_tracks_artifact() {
        let ws = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let shot = scratch.path().join("01-home.png");
        std::fs::write(&shot, b"\x89PNG\r\n\x1a\n").unwrap();

        struct Capture(std::sync::Mutex<Vec<Event>>);
        impl Sink for Capture {
            fn log(&self, event: Event) -> Result<(), String> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
            fn stream(&self, _delta: StreamDelta) {}
        }

        let params = TurnParams {
            kind: CliKind::Antigravity,
            workspace: ws.path(),
            snapshots: None,
            turn: 1,
            prompt: "test ui".into(),
            model: "gemini-4-neon".into(),
            effort: "high".into(),
            mode: Mode::Auto,
            sandboxed: false,
            read_only: false,
            conversation_id: None,
            extra_dirs: Vec::new(),
            mcp_servers: std::collections::BTreeMap::new(),
        };
        let sink = Capture(std::sync::Mutex::new(Vec::new()));
        let mut recorder = TurnRecorder::new(&params, &sink);
        recorder
            .handle_event(CliEvent::ToolStart {
                conversation_id: None,
                call_key: "c1".into(),
                name: "read_file".into(),
                input: json!({ "path": shot.display().to_string() }),
            })
            .unwrap();
        recorder
            .handle_event(CliEvent::ToolDone {
                conversation_id: None,
                call_key: "c1".into(),
                name: Some("read_file".into()),
                input: Value::Null,
                output: "Read image".into(),
                is_error: false,
            })
            .unwrap();

        let events = sink.0.lock().unwrap();
        let result = events.iter().find_map(|e| match e {
            Event::ToolResult { tool, meta, .. } if tool == "read_file" => Some(meta.clone()),
            _ => None,
        });
        let meta = result.expect("read_file ToolResult");
        assert_eq!(meta["picture"], "image/png");
        assert_eq!(meta["file"], shot.display().to_string());
        assert_eq!(recorder.external_artifacts, vec![shot]);
    }
}
