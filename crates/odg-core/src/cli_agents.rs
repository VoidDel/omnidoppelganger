//! Unified headless streaming runner and drivers for external coding-agent CLIs:
//! - Google Antigravity (`agy`) — [`CliKind::Antigravity`] (see [`crate::antigravity`])
//! - OpenAI Codex (`codex`) — [`CliKind::Codex`]
//! - Anthropic Claude Code (`claude`) — [`CliKind::ClaudeCode`]
//! - Cursor Agent (`cursor-agent`) — [`CliKind::Cursor`]
//! - xAI Grok CLI (`grok`) — [`CliKind::Grok`]
//! - OpenCode (`opencode`) — [`CliKind::OpenCode`]
//!
//! Each driver maps OmniDoppelganger's prompt, resumed session id, model, effort (`境界`),
//! permission [`Mode`], workspace trust, and sandbox flag onto the CLI's headless streaming command,
//! then parses its NDJSON stdout stream into normalized [`CliEvent`] items for the session log and UI.

mod args;
mod mcp_bridge;
mod models;
mod parser;
mod prompt;
mod recorder;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::events::Event;
use crate::llm::{StreamDelta, Tokens};
use crate::policy::Mode;
use crate::providers::Listed;

pub use args::{build_args, build_turn_args};
pub use models::resolve_cursor_model_for_effort;
pub use parser::{StreamParser, clean_tool_result, normalize_tool};
pub use prompt::{TempImagesGuard, build_handoff_brief, build_prompt, build_skills_brief, materialize_cli_images};

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
            return models::list_opencode_models(&bin).await;
        }
        if self == Self::Cursor {
            return models::list_cursor_models(&bin).await;
        }
        if self == Self::Grok {
            return models::list_grok_models(&bin).await;
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
        CliKind::Cursor | CliKind::Grok | CliKind::OpenCode => 500_000,
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
    let mut fallback_dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for rel in extra_home_rel {
            fallback_dirs.push(home.join(rel));
        }
        fallback_dirs.push(home.join(".local").join("bin"));
        fallback_dirs.push(home.join(".npm-global").join("bin"));
        fallback_dirs.push(home.join(".bun").join("bin"));
        fallback_dirs.push(home.join(".cargo").join("bin"));
        fallback_dirs.push(home.join("Library").join("pnpm"));
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            fallback_dirs.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            fallback_dirs.push(local.join("pnpm"));
            fallback_dirs.push(local.join("cursor-agent"));
            fallback_dirs.push(local.join("Programs").join("cursor-agent"));
        }
    }
    #[cfg(not(windows))]
    {
        fallback_dirs.push(PathBuf::from("/opt/homebrew/bin"));
        fallback_dirs.push(PathBuf::from("/usr/local/bin"));
    }
    for dir in fallback_dirs {
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
    None
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
    /// Whether `workspace` is trusted to execute repo-local instructions/hooks without prompting.
    pub workspace_trusted: bool,
    /// Extra readable directories (e.g. `~/.odg/skills`) granted to CLIs that support `--add-dir`.
    pub extra_dirs: Vec<PathBuf>,
    /// Materialized temporary paths of user-attached images for this turn.
    pub image_paths: Vec<PathBuf>,
    /// Enabled `odg` MCP servers (`~/.odg/mcp.json`) to bridge into CLIs supporting per-turn MCP config.
    pub mcp_servers: BTreeMap<String, crate::mcp::ServerConfig>,
}

/// Callbacks bridging external CLI stream events into the owning `Session`.
pub trait Sink {
    fn log(&self, event: Event) -> Result<(), String>;
    fn stream(&self, delta: StreamDelta);
}

/// Outcome of a single external CLI turn.
#[derive(Debug, Default)]
pub struct TurnOutcome {
    pub reason: &'static str,
    /// Newly created image/screenshot files outside the workspace observed during the turn
    /// (e.g. in an external CLI's scratch directory) so they still appear in 「产物」.
    pub extra_artifacts: Vec<PathBuf>,
}

/// Execute one headless streaming turn on any supported [`CliKind`].
pub async fn run_turn<S: Sink>(params: TurnParams<'_>, sink: &S, cancel: &CancellationToken) -> Result<TurnOutcome, String> {
    let bin = params
        .kind
        .find_binary()
        .ok_or_else(|| format!("未找到本机 {}，请先安装并完成登录。", params.kind.display_name()))?;
    if params.kind == CliKind::Cursor
        && !params.model.trim().is_empty()
        && params.model.trim() != "auto"
        && models::cursor_variant_cache().lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    {
        let _ = models::list_cursor_models(&bin).await;
    }
    let prepared = prompt::prepare_prompt_delivery(
        params.kind,
        &params.prompt,
        params.turn,
        prompt::MAX_CLI_PROMPT_BYTES,
        prompt::is_windows_batch_script(&bin),
    )?;
    let mut args = build_turn_args(
        params.kind,
        &prepared.cli_prompt,
        Some(params.workspace),
        params.conversation_id.as_deref(),
        &params.model,
        &params.effort,
        params.mode,
        params.sandboxed,
        params.read_only,
        params.workspace_trusted,
    );
    prompt::apply_prompt_delivery_args(params.kind, &mut args, &prepared);
    if !params.image_paths.is_empty() || !params.extra_dirs.is_empty() {
        let mut extra_flags = Vec::new();
        match params.kind {
            CliKind::Codex => {
                for path in &params.image_paths {
                    extra_flags.push("--image".to_string());
                    extra_flags.push(path.to_string_lossy().into_owned());
                }
            }
            CliKind::ClaudeCode => {
                let mut added_dirs: Vec<String> = Vec::new();
                let mut push_dir = |dir: &Path| {
                    if let Some(s) = dir.to_str()
                        && !added_dirs.iter().any(|existing| existing == s)
                    {
                        added_dirs.push(s.to_string());
                    }
                };
                if let Some(dir) = params.image_paths.first().and_then(|p| p.parent()) {
                    push_dir(dir);
                }
                for dir in &params.extra_dirs {
                    push_dir(dir);
                }
                for dir in added_dirs {
                    extra_flags.push("--add-dir".to_string());
                    extra_flags.push(dir);
                }
            }
            CliKind::OpenCode => {
                for path in &params.image_paths {
                    extra_flags.push("-f".to_string());
                    extra_flags.push(path.to_string_lossy().into_owned());
                }
            }
            _ => {}
        }
        prompt::insert_before_prompt_tail(&mut args, extra_flags);
    }
    if !params.read_only {
        if params.kind == CliKind::Antigravity {
            // Do NOT pass `~/.odg/skills` via `--add-dir`: `agy` sorts Active Workspaces alphabetically and
            // picks the first entry as Command Working Directory (`~/.odg/skills` sorts before `~/Projects/...`),
            // which would hijack the user's project workspace. `agy`'s `read_file` reads SKILL.md by absolute path directly.
            mcp_bridge::sync_antigravity_mcp(&bin, &params.mcp_servers).await;
        } else if params.kind == CliKind::Cursor && matches!(params.mode, Mode::Auto | Mode::Bypass) {
            mcp_bridge::sync_cursor_mcp(&params.mcp_servers);
        }
    }
    let _mcp_guard = if !params.read_only && matches!(params.mode, Mode::Auto | Mode::Bypass) {
        mcp_bridge::apply_mcp_bridge_args(params.kind, &mut args, &params.mcp_servers, params.turn)
    } else {
        None
    };
    let mut cmd = Command::new(&bin);
    cmd.args(&args)
        .current_dir(params.workspace)
        .env("PWD", params.workspace)
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
    let mut recorder = recorder::TurnRecorder::new(&params, sink);

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
