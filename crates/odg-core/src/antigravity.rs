//! Headless streaming adapter for the Google Antigravity CLI (`agy`).
//!
//! Inspired by provider-adapter architectures (such as t3code), each turn runs
//! `agy --print <prompt> --output-format stream-json` in the session's workspace,
//! resuming the same `agy` conversation via `--conversation <id>` across turns
//! and across app restarts. Native `agy` NDJSON events (`init`, `step_update`,
//! `result`) are translated into OmniDoppelganger's own `StreamDelta`,
//! `AssistantMessage`, and `ToolResult` log events so the transcript, tool cards,
//! side panel, and read-only aside all work seamlessly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use crate::llm::Tokens;
use crate::policy::Mode;
use crate::providers::Listed;

/// Effort suffixes that `agy models` bakes into variant model ids, ordered lowest to highest.
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Cached mapping from base model id (e.g. `gemini-4-neon`) to the ordered effort suffixes
/// that the local `agy models` reported for it (empty when the model has no effort variants).
static VARIANT_CACHE: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

fn variant_cache() -> &'static Mutex<HashMap<String, Vec<String>>> {
    VARIANT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Split a raw `agy` model id & display name into `(base_id, base_name, Option<effort_level>)`.
/// E.g. `("gemini-4-neon-xhigh", "Gemini 4 Neon (XHigh)")` -> `("gemini-4-neon", "Gemini 4 Neon", Some("xhigh"))`.
pub fn split_effort_variant<'a>(id: &'a str, display: &'a str) -> (&'a str, &'a str, Option<&'static str>) {
    for &level in &EFFORT_LEVELS {
        let dash = format!("-{level}");
        if let Some(base_id) = id.strip_suffix(&dash)
            && !base_id.is_empty()
        {
            let base_name = strip_effort_paren(display, level);
            return (base_id, base_name, Some(level));
        }
    }
    (id, display, None)
}

fn strip_effort_paren<'a>(display: &'a str, level: &str) -> &'a str {
    let trimmed = display.trim();
    if let Some(open) = trimmed.rfind('(')
        && trimmed.ends_with(')')
    {
        let inside = &trimmed[open + 1..trimmed.len() - 1];
        if inside.eq_ignore_ascii_case(level) {
            return trimmed[..open].trim_end();
        }
    }
    trimmed
}

/// Fallback supported effort levels for known `agy` model families when `agy models` has not yet been queried.
fn known_efforts(base_id: &str) -> &'static [&'static str] {
    match base_id {
        "gemini-4-neon" | "gemini-4-hydrogen" => &["low", "medium", "high", "xhigh"],
        "gemini-4-carbon" | "gemini-4-argon" | "gemini-3.8-flash" | "claude-opus-5-5" | "claude-sonnet-5-5" => &["low", "medium", "high"],
        "gpt-oss-120b" => &["medium"],
        _ => &[],
    }
}

/// Choose the closest supported effort level in `available` for `requested`:
/// exact match first, otherwise the highest level <= requested, otherwise the lowest available level.
fn pick_closest_effort<'a>(requested: &str, available: &'a [String]) -> Option<&'a str> {
    if available.is_empty() {
        return None;
    }
    let rank = |level: &str| EFFORT_LEVELS.iter().position(|known| *known == level).unwrap_or(2);
    let target = rank(requested);
    if let Some(exact) = available.iter().find(|level| rank(level) == target) {
        return Some(exact.as_str());
    }
    // Highest available level not exceeding requested (e.g. `max` -> `xhigh` or `high`).
    if let Some(lower) = available.iter().filter(|level| rank(level) < target).max_by_key(|level| rank(level)) {
        return Some(lower.as_str());
    }
    // Otherwise clamp up to the lowest available level (e.g. `low` -> `medium` for gpt-oss-120b).
    available.iter().min_by_key(|level| rank(level)).map(String::as_str)
}

/// Resolve a session's `(model, effort)` into the concrete `(--model, Option<--effort>)` pair to pass to `agy`.
/// Accepts either a base model id (`gemini-4-neon`) or a legacy suffixed id (`gemini-4-neon-high`).
/// Because `agy` rejects `--effort` when `--model` already encodes an effort suffix (e.g. `gemini-4-neon-low`
/// with `--effort=xhigh`), `--effort` is omitted whenever a concrete `--model` is supplied.
pub fn resolve_model_for_effort(model: &str, effort: &str) -> (String, Option<String>) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        let eff = effort.trim();
        return (String::new(), (!eff.is_empty()).then(|| eff.to_string()));
    }
    let (base_id, _, original_suffix) = split_effort_variant(trimmed, trimmed);
    let requested = if effort.trim().is_empty() { original_suffix.unwrap_or("high") } else { effort.trim() };

    let available: Vec<String> = {
        let cache = variant_cache().lock().unwrap();
        if let Some(levels) = cache.get(base_id) {
            levels.clone()
        } else {
            known_efforts(base_id).iter().map(|s| (*s).to_string()).collect()
        }
    };

    if let Some(chosen) = pick_closest_effort(requested, &available) {
        (format!("{base_id}-{chosen}"), None)
    } else {
        (trimmed.to_string(), None)
    }
}

/// Locate the `agy` CLI binary on this machine.
/// Checks `ODG_AGY_BIN`, then `PATH`, then common per-user / Homebrew install locations
/// so apps launched from Finder/Dock still find `~/.local/bin/agy`.
pub fn find_binary() -> Option<PathBuf> {
    crate::cli_agents::find_named_binary("ODG_AGY_BIN", "agy", &[])
}

/// Parse the text output of `agy models` into deduplicated base [`Listed`] model entries,
/// grouping effort-suffixed variants (`-low`, `-medium`, `-high`, `-xhigh`, `-max`) under one base model
/// and recording each family's supported effort levels so `境界` selects the matching variant.
pub fn parse_models_output(raw: &str) -> (Vec<Listed>, HashMap<String, Vec<String>>) {
    let mut models: Vec<Listed> = Vec::new();
    let mut variants: HashMap<String, Vec<String>> = HashMap::new();
    for line in raw.lines() {
        // Spinner frames overwrite via `\r`; keep only the final segment on each line.
        let segment = line.rsplit('\r').next().unwrap_or("").trim();
        if segment.is_empty() || segment.starts_with("Fetching") || segment.starts_with("warning:") {
            continue;
        }
        let mut parts = segment.splitn(2, char::is_whitespace);
        let Some(id) = parts.next().filter(|id| !id.is_empty()) else { continue };
        // Real model ids are ASCII slug-like tokens (e.g. `gemini-4-neon-high`, `claude-opus-5-5-high`).
        let valid_id = id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
            && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && (id.contains('-') || id.bytes().any(|b| b.is_ascii_digit()));
        if !valid_id {
            continue;
        }
        let display = parts.next().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(id);
        let (base_id, base_name, level) = split_effort_variant(id, display);
        let entry = variants.entry(base_id.to_string()).or_default();
        if let Some(level) = level
            && !entry.iter().any(|existing| existing == level)
        {
            entry.push(level.to_string());
        }
        if !models.iter().any(|existing| existing.id == base_id) {
            models.push(Listed { id: base_id.to_string(), name: base_name.to_string(), context_window: Some(1_000_000) });
        }
    }
    (models, variants)
}

/// Ask the local `agy` CLI which models the signed-in account can use.
pub async fn list_models() -> Result<Vec<Listed>, String> {
    let bin = find_binary().ok_or_else(|| "未找到本机 agy CLI，请先安装并登录 Google Antigravity CLI。".to_string())?;
    let mut cmd = Command::new(&bin);
    cmd.arg("models").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "查询 agy models 超时（30 秒）。".to_string())?
        .map_err(|error| format!("无法启动 agy CLI：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("agy models 退出异常（{}）。请先在终端运行 `agy` 完成登录。", output.status)
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (models, variants) = parse_models_output(&stdout);
    if models.is_empty() {
        Err("agy models 未返回可用模型，请先在终端运行 `agy` 确认已登录。".to_string())
    } else {
        *variant_cache().lock().unwrap() = variants;
        Ok(models)
    }
}

/// Convert `agy`'s usage object into OmniDoppelganger's [`Tokens`].
pub fn parse_tokens(usage: &Value) -> Option<Tokens> {
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let cached = count("cache_read_tokens");
    let input = count("input_tokens") + cached;
    let output = count("output_tokens");
    (input > 0 || output > 0).then_some(Tokens { input, cached, output })
}

/// Normalized NDJSON event parsed from one line of `agy --output-format stream-json`.
#[derive(Debug, Clone, PartialEq)]
pub enum AgyEvent {
    Init {
        conversation_id: Option<String>,
    },
    AgentText {
        conversation_id: Option<String>,
        step_index: u64,
        delta: String,
        done: bool,
        usage: Value,
        tokens: Option<Tokens>,
    },
    ToolStart {
        conversation_id: Option<String>,
        step_index: u64,
        name: String,
        input: Value,
    },
    ToolDone {
        conversation_id: Option<String>,
        step_index: u64,
        name: String,
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

/// Convert a parsed [`AgyEvent`] into the unified [`crate::cli_agents::CliEvent`].
pub fn into_cli_event(ev: AgyEvent) -> crate::cli_agents::CliEvent {
    use crate::cli_agents::CliEvent;
    match ev {
        AgyEvent::Init { conversation_id } => CliEvent::Init { conversation_id },
        AgyEvent::AgentText { conversation_id, delta, done, usage, tokens, .. } => {
            CliEvent::AgentText { conversation_id, delta, done, usage, tokens }
        }
        AgyEvent::ToolStart { conversation_id, step_index, name, input } => {
            CliEvent::ToolStart { conversation_id, call_key: step_index.to_string(), name, input }
        }
        AgyEvent::ToolDone { conversation_id, step_index, name, input, output, is_error } => {
            CliEvent::ToolDone { conversation_id, call_key: step_index.to_string(), name: Some(name), input, output, is_error }
        }
        AgyEvent::Result { conversation_id, success, response, error, usage, tokens } => {
            CliEvent::Result { conversation_id, success, response, error, usage, tokens }
        }
    }
}

/// Parse one NDJSON line emitted by `agy --output-format stream-json`.
pub fn parse_ndjson_line(line: &str) -> Option<AgyEvent> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    let event = value["event"].as_str()?;
    let top_conv = value["conversation_id"].as_str().map(str::to_string);
    match event {
        "init" => Some(AgyEvent::Init { conversation_id: top_conv }),
        "step_update" => {
            let update = &value["step_update"];
            let conv = update["conversation_id"].as_str().map(str::to_string).or(top_conv);
            let step_index = update["step_index"].as_u64().unwrap_or(0);
            let state = update["state"].as_str().unwrap_or("");
            let step_type = update["step_type"].as_str().unwrap_or("");
            match step_type {
                "agent_response" => {
                    let delta = update["text_delta"].as_str().unwrap_or("").to_string();
                    let done = matches!(state, "DONE" | "ERROR" | "CANCELED");
                    let usage = update["usage"].clone();
                    let tokens = parse_tokens(&usage);
                    if delta.is_empty() && !done {
                        return None;
                    }
                    Some(AgyEvent::AgentText { conversation_id: conv, step_index, delta, done, usage, tokens })
                }
                "tool" => {
                    let info = &update["tool_info"];
                    let raw_name = update["tool_name"].as_str().or_else(|| info["name"].as_str()).unwrap_or("tool");
                    let (name, input) = crate::cli_agents::normalize_tool(raw_name, &info["parameters"]);
                    if matches!(state, "DONE" | "ERROR" | "CANCELED") {
                        let is_error = state != "DONE" || info["is_error"].as_bool().unwrap_or(false);
                        let raw_output = [
                            info["output"].as_str(),
                            info["result"].as_str(),
                            info["error"].as_str(),
                            info["error_message"].as_str(),
                            info["errorMessage"].as_str(),
                            update["error"].as_str(),
                            update["error_message"].as_str(),
                            update["errorMessage"].as_str(),
                            update["status_detail"].as_str(),
                        ]
                        .into_iter()
                        .flatten()
                        .find(|s| !s.trim().is_empty())
                        .unwrap_or("");
                        let output = if raw_output.is_empty() && is_error {
                            "外部 CLI 执行此步骤未通过权限确认或返回错误（若需自动放行命令，可将禁制切换为「放手施为」）。".to_string()
                        } else {
                            raw_output.to_string()
                        };
                        Some(AgyEvent::ToolDone { conversation_id: conv, step_index, name, input, output, is_error })
                    } else {
                        Some(AgyEvent::ToolStart { conversation_id: conv, step_index, name, input })
                    }
                }
                _ => None,
            }
        }
        "result" => {
            let res = &value["result"];
            let conv = res["conversation_id"].as_str().map(str::to_string).or(top_conv);
            let status = res["status"].as_str().unwrap_or("SUCCESS");
            let success = status.eq_ignore_ascii_case("SUCCESS");
            let response = res["response"].as_str().unwrap_or("").to_string();
            let error = res["error"].as_str().map(str::to_string).or_else(|| (!success).then(|| format!("Antigravity returned status {status}")));
            let usage = res["usage"].clone();
            let tokens = parse_tokens(&usage);
            Some(AgyEvent::Result { conversation_id: conv, success, response, error, usage, tokens })
        }
        _ => None,
    }
}

/// Build the CLI argument list for one headless `agy` turn.
#[allow(clippy::too_many_arguments)]
pub fn build_args(
    prompt: &str,
    workspace: Option<&Path>,
    conversation_id: Option<&str>,
    model: &str,
    effort: &str,
    mode: Mode,
    sandboxed: bool,
    read_only: bool,
) -> Vec<String> {
    let mut args = vec!["--print".to_string(), prompt.to_string(), "--output-format".to_string(), "stream-json".to_string()];
    if let Some(dir) = workspace.and_then(Path::to_str).filter(|s| !s.trim().is_empty()) {
        args.push("--add-dir".to_string());
        args.push(dir.to_string());
    }
    if let Some(id) = conversation_id.filter(|id| !id.trim().is_empty()) {
        args.push("--conversation".to_string());
        args.push(id.to_string());
    }
    let (resolved_model, resolved_effort) = resolve_model_for_effort(model, effort);
    if !resolved_model.is_empty() {
        args.push("--model".to_string());
        args.push(resolved_model);
    }
    if let Some(eff) = resolved_effort {
        args.push("--effort".to_string());
        args.push(eff);
    }
    if read_only {
        args.push("--mode".to_string());
        args.push("plan".to_string());
    } else {
        match mode {
            Mode::Plan => {
                args.push("--mode".to_string());
                args.push("plan".to_string());
            }
            Mode::Edit => {
                args.push("--mode".to_string());
                args.push("accept-edits".to_string());
            }
            // In `agy --print` mode, `--mode auto` is gated behind an inactive rollout flag and falls back
            // to `request-review`, which soft-denies any command confirmation and aborts the turn.
            // Auto-approve tool permissions in `Mode::Auto` and `Mode::Bypass` (confined by `--sandbox` when enabled).
            Mode::Auto | Mode::Bypass => {
                args.push("--dangerously-skip-permissions".to_string());
            }
            Mode::Manual => {}
        }
    }
    if sandboxed {
        args.push("--sandbox".to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_agy_models_output_grouping_effort_variants() {
        let raw = "\r⠋ Fetching available models...\r⠙ Fetching available models...\rgemini-4-neon-high             Gemini 4 Neon (High)\ngemini-4-neon-xhigh            Gemini 4 Neon (XHigh)\ngemini-4-neon-low              Gemini 4 Neon (Low)\nclaude-opus-5-5-high           Claude Opus 5.5 (High)\nclaude-opus-5-5-medium         Claude Opus 5.5 (Medium)\ngemini-pro-orchestrator-neon   Orchestrator (Neon)\nwarning: something\n";
        let (listed, variants) = parse_models_output(raw);
        assert_eq!(
            listed,
            vec![
                Listed { id: "gemini-4-neon".into(), name: "Gemini 4 Neon".into(), context_window: Some(1_000_000) },
                Listed { id: "claude-opus-5-5".into(), name: "Claude Opus 5.5".into(), context_window: Some(1_000_000) },
                Listed { id: "gemini-pro-orchestrator-neon".into(), name: "Orchestrator (Neon)".into(), context_window: Some(1_000_000) },
            ]
        );
        assert_eq!(variants["gemini-4-neon"], vec!["high", "xhigh", "low"]);
        assert_eq!(variants["claude-opus-5-5"], vec!["high", "medium"]);
        assert!(variants["gemini-pro-orchestrator-neon"].is_empty());
    }

    #[test]
    fn maps_effort_to_closest_supported_agy_variant() {
        assert_eq!(resolve_model_for_effort("gemini-4-neon", "xhigh"), ("gemini-4-neon-xhigh".into(), None));
        // `max` clamps down to `xhigh` for gemini-4-neon, and to `high` for claude-opus-5-5.
        assert_eq!(resolve_model_for_effort("gemini-4-neon", "max"), ("gemini-4-neon-xhigh".into(), None));
        assert_eq!(resolve_model_for_effort("claude-opus-5-5", "max"), ("claude-opus-5-5-high".into(), None));
        assert_eq!(resolve_model_for_effort("claude-opus-5-5", "xhigh"), ("claude-opus-5-5-high".into(), None));
        // Legacy suffixed id still respects the current effort setting without passing a conflicting --effort flag.
        assert_eq!(resolve_model_for_effort("gemini-4-neon-low", "xhigh"), ("gemini-4-neon-xhigh".into(), None));
        // Single-variant model clamps up or down to its only supported level.
        assert_eq!(resolve_model_for_effort("gpt-oss-120b", "low"), ("gpt-oss-120b-medium".into(), None));
        assert_eq!(resolve_model_for_effort("gpt-oss-120b", "max"), ("gpt-oss-120b-medium".into(), None));
        // Model without effort variants keeps its id unchanged.
        assert_eq!(resolve_model_for_effort("gemini-pro-orchestrator-neon", "high"), ("gemini-pro-orchestrator-neon".into(), None));
        // Empty model forwards --effort directly to agy's default model.
        assert_eq!(resolve_model_for_effort("", "xhigh"), (String::new(), Some("xhigh".into())));
    }

    #[test]
    fn parses_real_agy_stream_json_events() {
        let init = r#"{"event":"init","conversation_id":"conv-1","init":{"cwd":"/tmp"}}"#;
        assert_eq!(parse_ndjson_line(init), Some(AgyEvent::Init { conversation_id: Some("conv-1".into()) }));

        let tool_start = r#"{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{"Path":"/tmp/a.rs"}}}}"#;
        assert_eq!(
            parse_ndjson_line(tool_start),
            Some(AgyEvent::ToolStart {
                conversation_id: Some("conv-1".into()),
                step_index: 2,
                name: "read_file".into(),
                input: json!({ "Path": "/tmp/a.rs", "path": "/tmp/a.rs" }),
            })
        );

        let tool_done = r#"{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":2,"state":"DONE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{"Path":"/tmp/a.rs"},"output":"40 lines"}}}"#;
        assert_eq!(
            parse_ndjson_line(tool_done),
            Some(AgyEvent::ToolDone {
                conversation_id: Some("conv-1".into()),
                step_index: 2,
                name: "read_file".into(),
                input: json!({ "Path": "/tmp/a.rs", "path": "/tmp/a.rs" }),
                output: "40 lines".into(),
                is_error: false,
            })
        );

        let delta = r#"{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"OK\n","usage":{"input_tokens":100,"output_tokens":5,"cache_read_tokens":50}}}"#;
        assert_eq!(
            parse_ndjson_line(delta),
            Some(AgyEvent::AgentText {
                conversation_id: Some("conv-1".into()),
                step_index: 3,
                delta: "OK\n".into(),
                done: true,
                usage: json!({"input_tokens":100,"output_tokens":5,"cache_read_tokens":50}),
                tokens: Some(Tokens { input: 150, cached: 50, output: 5 }),
            })
        );
    }

    #[test]
    fn builds_agy_args_with_resume_mode_and_sandbox() {
        let args = build_args("hello", Some(Path::new("/tmp/ws")), Some("c-1"), "gemini-4-neon", "high", Mode::Edit, true, false);
        assert_eq!(
            args,
            [
                "--print",
                "hello",
                "--output-format",
                "stream-json",
                "--add-dir",
                "/tmp/ws",
                "--conversation",
                "c-1",
                "--model",
                "gemini-4-neon-high",
                "--mode",
                "accept-edits",
                "--sandbox",
            ]
        );
        let auto = build_args("hi", None, None, "", "", Mode::Auto, false, false);
        assert_eq!(auto, ["--print", "hi", "--output-format", "stream-json", "--dangerously-skip-permissions"]);
        let ro = build_args("hi", None, None, "", "", Mode::Bypass, false, true);
        assert_eq!(ro, ["--print", "hi", "--output-format", "stream-json", "--mode", "plan"]);
    }
}
