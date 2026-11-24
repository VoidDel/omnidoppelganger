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

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::events::{Attached, Event};
use crate::llm::{StreamDelta, Tokens};
use crate::policy::Mode;
use crate::providers::Listed;

pub const BACKEND: &str = "antigravity";

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
    if let Ok(explicit) = std::env::var("ODG_AGY_BIN") {
        let path = PathBuf::from(explicit.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    let name = if cfg!(windows) { "agy.exe" } else { "agy" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let mut fallbacks = Vec::new();
    if let Some(home) = dirs::home_dir() {
        fallbacks.push(home.join(".local").join("bin").join(name));
        fallbacks.push(home.join(".cargo").join("bin").join(name));
    }
    #[cfg(not(windows))]
    {
        fallbacks.push(PathBuf::from("/opt/homebrew/bin/agy"));
        fallbacks.push(PathBuf::from("/usr/local/bin/agy"));
    }
    fallbacks.into_iter().find(|path| path.is_file())
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

/// Map `agy` tool names and parameter keys onto OmniDoppelganger's built-in tool shapes so
/// `ToolCard` displays native seals (`读`, `写`, `改`, `跑`, `搜`, `寻`, `查`) and readable targets.
fn normalize_tool(raw_name: &str, params: &Value) -> (String, Value) {
    let mapped_name = match raw_name {
        "read_file" | "view_file" => "read_file",
        "write_file" | "write_to_file" => "write_file",
        "edit_file" | "replace_file_content" | "multi_replace_file_content" | "sed_file" => "edit_file",
        "run_command" => "run_command",
        "grep_search" | "search" => "search",
        "find_by_name" | "list_dir" | "find_files" => "find_files",
        "search_web" | "web_search" => "web_search",
        "read_url_content" | "fetch_url" => "fetch_url",
        "generate_image" => "generate_image",
        other => other,
    };
    let mut normalized = params.as_object().cloned().unwrap_or_default();
    if !normalized.contains_key("path")
        && let Some(path) = ["Path", "TargetFile", "AbsolutePath", "DirectoryPath", "SearchDirectory", "SearchPath"]
            .iter()
            .find_map(|key| normalized.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    {
        normalized.insert("path".to_string(), Value::String(path));
    }
    if !normalized.contains_key("command")
        && let Some(cmd) = normalized.get("CommandLine").and_then(Value::as_str).map(str::to_string)
    {
        normalized.insert("command".to_string(), Value::String(cmd));
    }
    if !normalized.contains_key("pattern")
        && let Some(query) = ["Query", "Pattern"].iter().find_map(|key| normalized.get(*key).and_then(Value::as_str)).map(str::to_string)
    {
        normalized.insert("pattern".to_string(), Value::String(query));
    }
    if !normalized.contains_key("url")
        && let Some(url) = normalized.get("Url").and_then(Value::as_str).map(str::to_string)
    {
        normalized.insert("url".to_string(), Value::String(url));
    }
    (mapped_name.to_string(), Value::Object(normalized))
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
                    let (name, input) = normalize_tool(raw_name, &info["parameters"]);
                    if matches!(state, "DONE" | "ERROR" | "CANCELED") {
                        let output = info["output"]
                            .as_str()
                            .or_else(|| update["error"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_error = state != "DONE" || info["is_error"].as_bool().unwrap_or(false);
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

/// Build the prompt passed to `agy --print`, including any side-conversation briefing and attached file paths.
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

/// Build the CLI argument list for one headless `agy` turn.
pub fn build_args(
    prompt: &str,
    conversation_id: Option<&str>,
    model: &str,
    effort: &str,
    mode: Mode,
    sandboxed: bool,
    read_only: bool,
) -> Vec<String> {
    let mut args = vec!["--print".to_string(), prompt.to_string(), "--output-format".to_string(), "stream-json".to_string()];
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
            Mode::Auto => {
                args.push("--mode".to_string());
                args.push("auto".to_string());
            }
            Mode::Bypass => {
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

/// Parameters for executing a single headless `agy` turn.
pub struct TurnParams<'a> {
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

/// Callbacks bridging `agy` stream events into the owning `Session`.
pub trait Sink {
    fn log(&self, event: Event) -> Result<(), String>;
    fn stream(&self, delta: StreamDelta);
}

/// Execute one headless streaming turn via `agy --print --output-format stream-json`.
pub async fn run_turn<S: Sink>(params: TurnParams<'_>, sink: &S, cancel: &CancellationToken) -> Result<&'static str, String> {
    let bin = find_binary().ok_or_else(|| "未找到本机 agy CLI，请先安装并登录 Google Antigravity CLI。".to_string())?;
    let args = build_args(
        &params.prompt,
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
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|error| format!("无法启动 agy CLI：{error}"))?;
    let stdout = child.stdout.take().ok_or("agy stdout pipe unavailable")?;
    let mut stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr.take() {
            let _ = pipe.read_to_string(&mut buf).await;
        }
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut logged_conv = params.conversation_id.clone();
    let mut active_text = String::new();
    let mut emitted_any_text = false;
    let mut started_tools: HashMap<u64, (String, String)> = HashMap::new();
    let mut result_error: Option<String> = None;

    let mut record_conv = |conv: Option<String>| -> Result<(), String> {
        if let Some(id) = conv
            && !id.is_empty()
            && logged_conv.as_deref() != Some(id.as_str())
        {
            sink.log(Event::ExternalSession { backend: BACKEND.to_string(), session_id: id.clone() })?;
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
                let _ = child.kill().await;
                break true;
            }
            res = lines.next_line() => res,
        };
        let line = match next_line {
            Ok(Some(line)) => line,
            Ok(None) => break false,
            Err(error) => {
                let _ = child.kill().await;
                return Err(format!("读取 agy 输出失败：{error}"));
            }
        };
        let Some(ev) = parse_ndjson_line(&line) else { continue };
        match ev {
            AgyEvent::Init { conversation_id } => {
                record_conv(conversation_id)?;
            }
            AgyEvent::AgentText { conversation_id, delta, done, usage, tokens, .. } => {
                record_conv(conversation_id)?;
                if !delta.is_empty() {
                    active_text.push_str(&delta);
                    sink.stream(StreamDelta::TextDelta { index: 0, text: delta });
                }
                if done {
                    flush_text(&mut active_text, usage, tokens, &params.model, &mut emitted_any_text)?;
                }
            }
            AgyEvent::ToolStart { conversation_id, step_index, name, input } => {
                record_conv(conversation_id)?;
                flush_text(&mut active_text, Value::Null, None, &params.model, &mut emitted_any_text)?;
                let tool_use_id = format!("agy-{}-{step_index}", params.turn);
                if let std::collections::hash_map::Entry::Vacant(slot) = started_tools.entry(step_index) {
                    slot.insert((tool_use_id.clone(), name.clone()));
                    sink.log(Event::AssistantMessage {
                        content: vec![json!({ "type": "tool_use", "id": tool_use_id, "name": name, "input": input })],
                        stop_reason: Some("tool_use".to_string()),
                        model: params.model.clone(),
                        usage: Value::Null,
                        tokens: None,
                    })?;
                }
            }
            AgyEvent::ToolDone { conversation_id, step_index, name, input, output, is_error } => {
                record_conv(conversation_id)?;
                let (tool_use_id, tool_name) = match started_tools.remove(&step_index) {
                    Some(existing) => existing,
                    None => {
                        let id = format!("agy-{}-{step_index}", params.turn);
                        sink.log(Event::AssistantMessage {
                            content: vec![json!({ "type": "tool_use", "id": id, "name": name, "input": input })],
                            stop_reason: Some("tool_use".to_string()),
                            model: params.model.clone(),
                            usage: Value::Null,
                            tokens: None,
                        })?;
                        (id, name)
                    }
                };
                let meta = match tool_name.as_str() {
                    "run_command" => json!({ "command": input["command"].as_str().unwrap_or(""), "output": output }),
                    _ => Value::Null,
                };
                sink.log(Event::ToolResult { tool_use_id, tool: tool_name, content: output, is_error, meta, images: Vec::new() })?;
            }
            AgyEvent::Result { conversation_id, success, response, error, usage, tokens } => {
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
    };

    if cancelled {
        let _ = child.wait().await;
        return Ok("cancelled");
    }

    flush_text(&mut active_text, Value::Null, None, &params.model, &mut emitted_any_text)?;
    let status = child.wait().await.map_err(|error| format!("等待 agy 进程结束失败：{error}"))?;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if let Some(err) = result_error {
        return Err(err);
    }
    if !status.success() {
        let trimmed = stderr_text.trim();
        return Err(if trimmed.is_empty() { format!("agy 进程异常退出（{status}）。") } else { trimmed.to_string() });
    }
    Ok("completed")
}

#[cfg(test)]
mod tests {
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
        let args = build_args("hello", Some("c-1"), "gemini-4-neon", "high", Mode::Edit, true, false);
        assert_eq!(
            args,
            ["--print", "hello", "--output-format", "stream-json", "--conversation", "c-1", "--model", "gemini-4-neon-high", "--mode", "accept-edits", "--sandbox"]
        );
        let bypass = build_args("hi", None, "", "", Mode::Bypass, false, false);
        assert_eq!(bypass, ["--print", "hi", "--output-format", "stream-json", "--dangerously-skip-permissions"]);
        let ro = build_args("hi", None, "", "", Mode::Bypass, false, true);
        assert_eq!(ro, ["--print", "hi", "--output-format", "stream-json", "--mode", "plan"]);
    }
}
