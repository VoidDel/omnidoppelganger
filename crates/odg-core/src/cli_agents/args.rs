//! CLI argument builders mapping OmniDoppelganger's permission [`Mode`], OS sandbox toggle,
//! and workspace trust state onto each external coding-agent CLI's headless flags.
//!
//! Because headless runs (`--print`, `exec`, `run`) have no interactive confirmation channel back
//! to OmniDoppelganger's UI, each mode is mapped conservatively:
//! - [`Mode::Plan`] (and `read_only`): read-only planning mode (`plan` / `read-only`).
//! - [`Mode::Manual`] ("步步请示"): never auto-accepts file edits or shell commands (`manual`,
//!   `default`, `ask`, `read-only`, or `plan`).
//! - [`Mode::Edit`] ("府内自便"): allows workspace file edits without force-approving arbitrary
//!   shell commands (`accept-edits` / `acceptEdits`, or `workspace-write` on Codex).
//! - [`Mode::Auto`] ("放手施为"): uses each CLI's safety classifier / auto mode (`auto`,
//!   `--auto-review`, `--approve-for-me`, `--auto`, or `agy`'s headless auto-approval).
//! - [`Mode::Bypass`] ("全权托付"): full permission bypass (`--dangerously-skip-permissions`,
//!   `--force`, `--always-approve`, `--dangerously-bypass-approvals-and-sandbox`), while still
//!   honoring `sandboxed` when enabled.

use std::path::Path;

use crate::policy::Mode;

use super::CliKind;
use super::models::resolve_cursor_model_for_effort;

const UNTRUSTED_CLI_CONFIG_ENTRIES: &[&str] = &[
    ".claude",
    ".mcp.json",
    ".cursor",
    ".cursorrules",
    ".codex",
    ".gemini",
    ".agent",
    ".agents",
    ".opencode",
    "opencode.json",
    ".grok",
];

/// Scan `workspace` for repository-local external CLI configuration directories, rules,
/// MCP server definitions, or hook folders so untrusted repositories can prompt or isolate them.
pub fn detect_untrusted_cli_configs(workspace: &Path) -> Vec<String> {
    UNTRUSTED_CLI_CONFIG_ENTRIES
        .iter()
        .filter(|entry| std::fs::symlink_metadata(workspace.join(entry)).is_ok())
        .map(|entry| (*entry).to_string())
        .collect()
}

/// Block headless external CLIs that lack a CLI flag to ignore repository-local hooks/configs
/// (`Codex`, `Antigravity`, `Grok`, `OpenCode`) when `workspace` is untrusted and not in [`Mode::Bypass`].
pub fn enforce_untrusted_cli_isolation(
    kind: CliKind,
    workspace: &Path,
    mode: Mode,
    workspace_trusted: bool,
) -> Result<(), String> {
    if workspace_trusted || mode == Mode::Bypass {
        return Ok(());
    }
    let relevant: &[&str] = match kind {
        CliKind::Codex => &[".codex", ".mcp.json"],
        CliKind::Antigravity => &[".gemini", ".agent", ".agents", ".mcp.json"],
        CliKind::Grok => &[".grok", ".mcp.json"],
        CliKind::OpenCode => &[".opencode", "opencode.json", ".mcp.json"],
        // `ClaudeCode` isolates via `--setting-sources user`; `Cursor` omits `--trust`.
        CliKind::ClaudeCode | CliKind::Cursor => return Ok(()),
    };
    let found: Vec<&str> = relevant
        .iter()
        .copied()
        .filter(|entry| std::fs::symlink_metadata(workspace.join(entry)).is_ok())
        .collect();
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "当前洞府尚未授信，且检测到本地 CLI 配置或钩子（{}）。请先在顶栏点击「信任此洞府」或审查本地配置后再运行 {}。",
        found.join("、"),
        kind.display_name()
    ))
}

/// Build the CLI argument vector for one headless turn of `kind` (assuming a trusted workspace).
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
    build_turn_args(
        kind,
        prompt,
        workspace,
        conversation_id,
        model,
        effort,
        mode,
        sandboxed,
        read_only,
        true,
    )
}

/// Build the CLI argument vector for one headless turn of `kind`, honoring workspace trust.
#[allow(clippy::too_many_arguments)]
pub fn build_turn_args(
    kind: CliKind,
    prompt: &str,
    workspace: Option<&Path>,
    conversation_id: Option<&str>,
    model: &str,
    effort: &str,
    mode: Mode,
    sandboxed: bool,
    read_only: bool,
    workspace_trusted: bool,
) -> Vec<String> {
    let resume = conversation_id.map(str::trim).filter(|id| !id.is_empty());
    let model = model.trim();
    let effort = effort.trim();
    match kind {
        CliKind::Antigravity => {
            crate::antigravity::build_args(prompt, workspace, resume, model, effort, mode, sandboxed, read_only)
        }
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
            // `codex exec resume` inherits sandbox flags on newer builds, while fresh `exec` takes `--sandbox`.
            if resume.is_none() {
                if read_only || matches!(mode, Mode::Plan | Mode::Manual) {
                    args.push("--sandbox".to_string());
                    args.push("read-only".to_string());
                } else if mode == Mode::Bypass && !sandboxed {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                } else {
                    args.push("--sandbox".to_string());
                    args.push("workspace-write".to_string());
                    if mode == Mode::Auto {
                        args.push("--approve-for-me".to_string());
                    }
                }
            } else if !read_only {
                if mode == Mode::Bypass && !sandboxed {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                } else if mode == Mode::Auto {
                    args.push("--approve-for-me".to_string());
                }
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
                    Mode::Manual => "manual",
                    Mode::Edit => "acceptEdits",
                    Mode::Auto => "auto",
                    Mode::Bypass => "bypassPermissions",
                }
            };
            args.push("--permission-mode".to_string());
            args.push(perm.to_string());
            if !read_only && mode == Mode::Bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            if !workspace_trusted {
                args.push("--setting-sources".to_string());
                args.push("user".to_string());
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
            if workspace_trusted || mode == Mode::Bypass {
                args.push("--trust".to_string());
            }
            if !read_only && matches!(mode, Mode::Auto | Mode::Bypass) {
                args.push("--approve-mcps".to_string());
            }
            if let Some(ws) = workspace {
                args.push("--workspace".to_string());
                args.push(ws.to_string_lossy().into_owned());
            }
            if let Some(chat_id) = resume {
                args.push("--resume".to_string());
                args.push(chat_id.to_string());
            }
            if !model.is_empty() {
                let resolved = resolve_cursor_model_for_effort(model, effort);
                args.push("--model".to_string());
                args.push(resolved);
            }
            if sandboxed {
                args.push("--sandbox".to_string());
                args.push("enabled".to_string());
            }
            if read_only || mode == Mode::Plan {
                args.push("--mode".to_string());
                args.push("plan".to_string());
            } else if mode == Mode::Manual {
                args.push("--mode".to_string());
                args.push("ask".to_string());
            } else if mode == Mode::Auto {
                args.push("--auto-review".to_string());
            } else if mode == Mode::Bypass {
                args.push("--force".to_string());
            }
            args.push("--".to_string());
            args.push(prompt.to_string());
            args
        }
        CliKind::Grok => {
            let mut args = vec!["--output-format".to_string(), "streaming-json".to_string()];
            if let Some(ws) = workspace {
                args.push("--cwd".to_string());
                args.push(ws.to_string_lossy().into_owned());
            }
            if let Some(session_id) = resume {
                args.push("--resume".to_string());
                args.push(session_id.to_string());
            }
            if !model.is_empty() {
                args.push("--model".to_string());
                args.push(model.to_string());
            }
            if !effort.is_empty() {
                let mapped = match effort {
                    "low" | "high" | "xhigh" | "max" => effort,
                    _ => "high",
                };
                args.push("--reasoning-effort".to_string());
                args.push(mapped.to_string());
            }
            let perm = if read_only {
                "plan"
            } else {
                match mode {
                    Mode::Plan => "plan",
                    Mode::Manual => "default",
                    Mode::Edit => "acceptEdits",
                    Mode::Auto => "auto",
                    Mode::Bypass => "bypassPermissions",
                }
            };
            args.push("--permission-mode".to_string());
            args.push(perm.to_string());
            if sandboxed && !read_only && mode != Mode::Plan {
                args.push("--sandbox".to_string());
                args.push("workspace-write".to_string());
            }
            if !read_only && mode == Mode::Bypass {
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
            if read_only || matches!(mode, Mode::Plan | Mode::Manual) {
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
