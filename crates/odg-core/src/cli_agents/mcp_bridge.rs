//! MCP server configuration bridging from `~/.odg/mcp.json` into external CLIs.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::process::Command;

use super::CliKind;
use super::prompt::{TempPromptGuard, insert_before_prompt_tail};

/// Bridge enabled `odg` MCP servers (`~/.odg/mcp.json`) into CLIs that support per-turn MCP injection:
/// - Claude Code: `--mcp-config <temp.json>`
/// - Codex: `-c mcp_servers.<name>.command=...` / `.args=[...]` / `.url=...`
///
/// Note: Antigravity (`agy`) manages MCP servers via `agy mcp add` rather than `--settings`,
/// and rejects `"mcpServers"` in `--settings`, so we do not pass `--settings` to `agy`.
pub(super) fn apply_mcp_bridge_args(
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
pub(super) fn build_agy_mcp_add_args(name: &str, srv: &crate::mcp::ServerConfig) -> Option<Vec<String>> {
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
pub(super) async fn sync_antigravity_mcp(bin: &Path, servers: &BTreeMap<String, crate::mcp::ServerConfig>) {
    static LAST_SYNCED: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    let enabled: Vec<(&String, &crate::mcp::ServerConfig)> = servers.iter().filter(|(_, s)| !s.disabled).collect();
    if enabled.is_empty() {
        return;
    }
    let sig = serde_json::to_string(&enabled).unwrap_or_default();
    let cache = LAST_SYNCED.get_or_init(|| std::sync::Mutex::new(None));
    if cache.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(sig.as_str()) {
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
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(sig);
    }
}

/// Merge enabled `odg` MCP servers (`~/.odg/mcp.json`) into an existing Cursor `mcp.json` value,
/// preserving any non-`odg` entries the user configured directly in Cursor.
pub(super) fn merge_cursor_mcp_json(existing: Option<&str>, servers: &BTreeMap<String, crate::mcp::ServerConfig>) -> Option<String> {
    let mut root = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let mut map = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut any = false;
    for (name, srv) in servers {
        if srv.disabled {
            continue;
        }
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
            entry.insert("url".into(), json!(url));
            if !srv.headers.is_empty() {
                entry.insert("headers".into(), json!(srv.headers));
            }
        } else {
            continue;
        }
        map.insert(name.clone(), Value::Object(entry));
        any = true;
    }
    if !any {
        return None;
    }
    root.insert("mcpServers".into(), Value::Object(map));
    serde_json::to_string_pretty(&Value::Object(root)).ok()
}

/// Sync enabled `odg` MCP servers (`~/.odg/mcp.json`) into `~/.cursor/mcp.json` so `cursor-agent`
/// loads them natively when `--approve-mcps` is passed. Cached by config signature to avoid redundant writes.
pub(super) fn sync_cursor_mcp(servers: &BTreeMap<String, crate::mcp::ServerConfig>) {
    static LAST_SYNCED: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    let enabled: Vec<(&String, &crate::mcp::ServerConfig)> = servers.iter().filter(|(_, s)| !s.disabled).collect();
    if enabled.is_empty() {
        return;
    }
    let sig = serde_json::to_string(&enabled).unwrap_or_default();
    let cache = LAST_SYNCED.get_or_init(|| std::sync::Mutex::new(None));
    if cache.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(sig.as_str()) {
        return;
    }
    let Some(home) = dirs::home_dir() else { return };
    let cursor_dir = home.join(".cursor");
    let mcp_path = cursor_dir.join("mcp.json");
    let existing = std::fs::read_to_string(&mcp_path).ok();
    let Some(merged) = merge_cursor_mcp_json(existing.as_deref(), servers) else {
        return;
    };
    if std::fs::create_dir_all(&cursor_dir).is_ok() && std::fs::write(&mcp_path, merged).is_ok() {
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(sig);
    }
}
