//! Dynamic model discovery and effort-variant resolution for Cursor, Grok, and OpenCode CLIs.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use tokio::process::Command;

use crate::providers::Listed;

use super::{CliKind, context_window};

pub(super) type CursorVariantMap = HashMap<String, Vec<(u8, String)>>;

/// Cached mapping from collapsed Cursor base model id (e.g. `grok-4.7`, `claude-opus-5-5-fast`)
/// to `(effort_rank, exact_raw_model_id)` pairs reported by `cursor-agent --list-models`.
static CURSOR_VARIANT_CACHE: std::sync::OnceLock<std::sync::Mutex<CursorVariantMap>> = std::sync::OnceLock::new();

pub(super) fn cursor_variant_cache() -> &'static std::sync::Mutex<CursorVariantMap> {
    CURSOR_VARIANT_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

const CURSOR_EFFORT_SUFFIXES: [(&str, u8); 8] = [
    ("-extra-high", 4),
    ("-minimal", 0),
    ("-medium", 2),
    ("-xhigh", 4),
    ("-none", 0),
    ("-high", 3),
    ("-low", 1),
    ("-max", 5),
];

pub(super) fn cursor_effort_rank(effort: &str) -> u8 {
    match effort.trim().to_ascii_lowercase().as_str() {
        "none" | "minimal" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" | "extra-high" => 4,
        "max" => 5,
        _ => 3,
    }
}

/// Split a raw Cursor model id into `(collapsed_base_id, Option<effort_rank>)`.
/// Keeps `-fast` and `-thinking` as distinct model lines while folding effort suffixes
/// (`none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`extra-high`/`max`) into the base model.
fn split_cursor_effort_variant(id: &str) -> (String, Option<u8>) {
    let (head, tail) = if let Some(prefix) = id.strip_suffix("-fast") {
        (prefix, "-fast")
    } else if let Some(prefix) = id.strip_suffix("-thinking") {
        (prefix, "-thinking")
    } else {
        (id, "")
    };
    for &(suffix, rank) in &CURSOR_EFFORT_SUFFIXES {
        if let Some(base) = head.strip_suffix(suffix)
            && !base.is_empty()
        {
            return (format!("{base}{tail}"), Some(rank));
        }
    }
    (id.to_string(), None)
}

/// Strip standalone effort words (`Extra High`, `Minimal`, `Medium`, `None`, `High`, `Low`, `Max`)
/// from a Cursor display label and collapse redundant spaces.
fn clean_cursor_display_name(raw: &str) -> String {
    let without_extra = raw.replace("Extra High", " ");
    let mut words = Vec::new();
    for word in without_extra.split_whitespace() {
        if matches!(word, "None" | "Minimal" | "Low" | "Medium" | "High" | "XHigh" | "Max") {
            continue;
        }
        words.push(word);
    }
    if words.is_empty() {
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        words.join(" ")
    }
}

pub(super) fn pick_cursor_variant(requested_rank: u8, variants: &[(u8, String)]) -> Option<&str> {
    if variants.is_empty() {
        return None;
    }
    if let Some((_, id)) = variants.iter().find(|(r, _)| *r == requested_rank) {
        return Some(id.as_str());
    }
    if let Some((_, id)) = variants.iter().filter(|(r, _)| *r < requested_rank).max_by_key(|(r, _)| *r) {
        return Some(id.as_str());
    }
    variants.iter().min_by_key(|(r, _)| *r).map(|(_, id)| id.as_str())
}

/// Resolve a session's `(model, effort)` into the concrete Cursor `--model` id.
/// Accepts either a collapsed base id (`grok-4.7`, `claude-opus-5-5-fast`) or a concrete variant id.
pub fn resolve_cursor_model_for_effort(model: &str, effort: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed == "auto" {
        return trimmed.to_string();
    }
    let (base_id, original_rank) = split_cursor_effort_variant(trimmed);
    let target_rank = if effort.trim().is_empty() {
        original_rank.unwrap_or(3)
    } else {
        cursor_effort_rank(effort)
    };
    let cache = cursor_variant_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(variants) = cache.get(&base_id)
        && let Some(chosen) = pick_cursor_variant(target_rank, variants)
    {
        return chosen.to_string();
    }
    trimmed.to_string()
}

pub(super) async fn list_cursor_models(bin: &Path) -> Result<Vec<Listed>, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--list-models")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in super::scrubbed_env_keys(CliKind::Cursor, "") {
        cmd.env_remove(key);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(std::time::Duration::from_secs(12), cmd.output())
        .await
        .map_err(|_| "查询 cursor-agent --list-models 超时（12s）。".to_string())?
        .map_err(|e| format!("运行 cursor-agent --list-models 失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "cursor-agent --list-models 执行失败，请检查登录状态。".to_string()
        } else {
            format!("cursor-agent --list-models 失败：{err}")
        });
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let (parsed, variants) = parse_cursor_models_output(&raw);
    if parsed.is_empty() {
        return Err("cursor-agent --list-models 未返回任何可用模型，请先登录 Cursor CLI。".to_string());
    }
    *cursor_variant_cache().lock().unwrap_or_else(|e| e.into_inner()) = variants;
    Ok(parsed)
}

pub(super) async fn list_grok_models(bin: &Path) -> Result<Vec<Listed>, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("models")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in super::scrubbed_env_keys(CliKind::Grok, "") {
        cmd.env_remove(key);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(std::time::Duration::from_secs(12), cmd.output())
        .await
        .map_err(|_| "查询 grok models 超时（12s）。".to_string())?
        .map_err(|e| format!("运行 grok models 失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "grok models 执行失败，请检查登录状态。".to_string()
        } else {
            format!("grok models 失败：{err}")
        });
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let parsed = parse_grok_models_output(&raw);
    if parsed.is_empty() {
        return Err("grok models 未返回任何可用模型，请先运行 grok login。".to_string());
    }
    Ok(parsed)
}

pub(super) fn parse_grok_models_output(raw: &str) -> Vec<Listed> {
    let mut seen = std::collections::HashSet::new();
    let mut default_item: Option<Listed> = None;
    let mut models = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        let (is_default, rest) = if let Some(r) = trimmed.strip_prefix("* ") {
            (true, r.trim())
        } else if let Some(r) = trimmed.strip_prefix("- ") {
            (false, r.trim())
        } else {
            continue;
        };
        let id = rest.strip_suffix("(default)").map(str::trim).unwrap_or(rest);
        if id.is_empty() || id.contains(char::is_whitespace) || !seen.insert(id.to_string()) {
            continue;
        }
        let label = id.rsplit('/').next().unwrap_or(id);
        let name = if is_default {
            format!("{label}（默认）")
        } else {
            label.to_string()
        };
        let listed = Listed {
            id: id.to_string(),
            name,
            context_window: Some(context_window(CliKind::Grok)),
        };
        if is_default && default_item.is_none() {
            default_item = Some(listed);
        } else {
            models.push(listed);
        }
    }
    if let Some(def) = default_item {
        models.insert(0, def);
    }
    models
}

pub(super) fn parse_cursor_models_output(raw: &str) -> (Vec<Listed>, CursorVariantMap) {
    let mut models: Vec<Listed> = Vec::new();
    let mut index_by_base: HashMap<String, usize> = HashMap::new();
    let mut variants: HashMap<String, Vec<(u8, String)>> = HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{200b}');
        if trimmed.is_empty() || trimmed.starts_with("Available ") || trimmed.starts_with("Tip:") || trimmed.starts_with("Loading ") {
            continue;
        }
        let Some((id_raw, label_raw)) = trimmed.split_once(" - ") else {
            continue;
        };
        let id = id_raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{200b}');
        let name = label_raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{200b}');
        if id.is_empty() || id.contains(' ') {
            continue;
        }
        let (base_id, rank_opt) = split_cursor_effort_variant(id);
        // Unsuffixed models that have effort siblings (e.g. `gpt-5.3-codex` alongside `gpt-5.3-codex-low/-high`)
        // act as the default medium (rank 2) variant.
        let rank = rank_opt.unwrap_or(2);
        let entry = variants.entry(base_id.clone()).or_default();
        if !entry.iter().any(|(_, existing)| existing == id) {
            entry.push((rank, id.to_string()));
        }
        let display = if rank_opt.is_some() {
            clean_cursor_display_name(name)
        } else if name.is_empty() {
            base_id.clone()
        } else {
            name.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        if let Some(&idx) = index_by_base.get(&base_id) {
            // Prefer the unsuffixed official display name if it appears later (e.g. `gpt-5.3-codex - Codex 5.3`).
            if rank_opt.is_none() && !display.is_empty() {
                models[idx].name = display;
            }
        } else {
            index_by_base.insert(base_id.clone(), models.len());
            models.push(Listed {
                id: base_id,
                name: display,
                context_window: Some(context_window(CliKind::Cursor)),
            });
        }
    }
    (models, variants)
}

async fn run_opencode_subcommand(bin: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in super::scrubbed_env_keys(CliKind::OpenCode, "") {
        cmd.env_remove(key);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(std::time::Duration::from_secs(12), cmd.output())
        .await
        .map_err(|_| format!("查询 opencode {} 超时（12s）。", args.join(" ")))?
        .map_err(|e| format!("运行 opencode {} 失败：{e}", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("opencode {} 执行失败。", args.join(" "))
        } else {
            format!("opencode {} 失败：{err}", args.join(" "))
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) async fn list_opencode_models(bin: &Path) -> Result<Vec<Listed>, String> {
    // Prefer structured `opencode api model.list` (carries cost, providerID, friendly name, and context window).
    if let Ok(raw_models) = run_opencode_subcommand(bin, &["api", "model.list"]).await {
        let free_only = match run_opencode_subcommand(bin, &["api", "provider.list"]).await {
            Ok(raw_providers) => opencode_providers_is_anonymous(&raw_providers),
            Err(_) => false,
        };
        let parsed = parse_opencode_api_models(&raw_models, free_only);
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }
    // Fallback to plain `opencode models` for older OpenCode releases.
    let raw = run_opencode_subcommand(bin, &["models"]).await?;
    let parsed = parse_opencode_models_output(&raw);
    if parsed.is_empty() {
        return Err("opencode models 未返回任何可用模型，请先检查 OpenCode 配置。".to_string());
    }
    Ok(parsed)
}

/// Returns true when `opencode api provider.list` reports zero enabled/authenticated providers.
fn opencode_providers_is_anonymous(raw: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
        return false;
    };
    let Some(arr) = v["data"].as_array().or_else(|| v.as_array()) else {
        return false;
    };
    !arr.iter().any(|p| p["activation"].as_str() == Some("enabled"))
}

fn is_opencode_cost_free(cost: &Value) -> Option<bool> {
    let tier = if let Some(arr) = cost.as_array() { arr.first()? } else if cost.is_object() { cost } else { return None };
    let input = tier["input"].as_f64()?;
    let output = tier["output"].as_f64().unwrap_or(0.0);
    Some(input == 0.0 && output == 0.0)
}

fn opencode_tier_tag(provider_id: &str, model_id: &str, is_free: bool) -> (u8, Option<&'static str>) {
    if is_free {
        return (0, Some("免费"));
    }
    if provider_id.eq_ignore_ascii_case("opencode-go") || provider_id.to_lowercase().contains("opencode-go") {
        return (1, Some("Go"));
    }
    let lower_id = model_id.to_lowercase();
    if lower_id.ends_with("-free") || lower_id == "big-pickle" {
        return (0, Some("免费"));
    }
    (2, None)
}

pub(super) fn parse_opencode_api_models(raw: &str, free_only: bool) -> Vec<Listed> {
    let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
        return Vec::new();
    };
    let Some(arr) = v["data"].as_array().or_else(|| v.as_array()) else {
        return Vec::new();
    };
    let mut ranked: Vec<(u8, usize, Listed)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (idx, item) in arr.iter().enumerate() {
        let provider_id = item["providerID"].as_str().unwrap_or("").trim();
        let model_id = item["id"].as_str().or_else(|| item["modelID"].as_str()).unwrap_or("").trim();
        if provider_id.is_empty() || model_id.is_empty() {
            continue;
        }
        let full_id = format!("{provider_id}/{model_id}");
        if !seen.insert(full_id.clone()) {
            continue;
        }
        let is_free = is_opencode_cost_free(&item["cost"]).unwrap_or(false);
        let (tier, tag) = opencode_tier_tag(provider_id, model_id, is_free);
        if free_only && tier != 0 {
            continue;
        }
        let base_name = item["name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(model_id);
        let display = match tag {
            Some(t) => format!("{base_name} · {t}"),
            None => base_name.to_string(),
        };
        let ctx = item["limit"]["context"].as_u64().filter(|&n| n > 0).or(Some(context_window(CliKind::OpenCode)));
        ranked.push((tier, idx, Listed { id: full_id, name: display, context_window: ctx }));
    }
    ranked.sort_by_key(|(tier, idx, _)| (*tier, *idx));
    ranked.into_iter().map(|(_, _, listed)| listed).collect()
}

pub(super) fn parse_opencode_models_output(raw: &str) -> Vec<Listed> {
    let mut ranked: Vec<(u8, usize, Listed)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.contains(char::is_whitespace) || !trimmed.contains('/') {
            continue;
        }
        if !seen.insert(trimmed.to_string()) {
            continue;
        }
        let (provider_id, model_id) = trimmed.split_once('/').unwrap_or(("", trimmed));
        let (tier, tag) = opencode_tier_tag(provider_id, model_id, false);
        let name = match tag {
            Some(t) => format!("{model_id} · {t}"),
            None => trimmed.to_string(),
        };
        ranked.push((
            tier,
            idx,
            Listed {
                id: trimmed.to_string(),
                name,
                context_window: Some(context_window(CliKind::OpenCode)),
            },
        ));
    }
    ranked.sort_by_key(|(tier, idx, _)| (*tier, *idx));
    ranked.into_iter().map(|(_, _, listed)| listed).collect()
}
