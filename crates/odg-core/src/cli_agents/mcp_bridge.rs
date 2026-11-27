//! MCP server configuration bridging from `~/.odg/mcp.json` into external CLIs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

use super::CliKind;
use super::prompt::{TempPromptGuard, insert_before_prompt_tail};

pub(super) fn apply_mcp_bridge_args_in(
    base_home: &Path,
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
            let payload = serde_json::to_vec_pretty(&json!({ "mcpServers": map })).ok()?;
            let path =
                crate::fsutil::write_private_new_in(base_home, "mcp", &format!("mcp-{turn}-"), ".json", &payload)
                    .ok()?;
            insert_before_prompt_tail(
                args,
                vec!["--mcp-config".to_string(), path.to_string_lossy().into_owned()],
            );
            Some(TempPromptGuard::file_only(path))
        }
        CliKind::Codex => {
            let mut flags = Vec::new();
            for (name, srv) in servers {
                if srv.disabled {
                    continue;
                }
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
                } else if srv.headers.is_empty()
                    && let Some(url) = srv.url.as_deref().filter(|u| !u.trim().is_empty())
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

/// Bridge enabled `odg` MCP servers (`~/.odg/mcp.json`) into CLIs that support per-turn MCP injection:
/// - Claude Code: `--mcp-config <private-temp.json>` (`0600` under `<ODG_HOME>/tmp/mcp`)
/// - Codex: `-c mcp_servers.<name>.command=...` / `.args=[...]` / `.url=...`
pub(super) fn apply_mcp_bridge_args(
    kind: CliKind,
    args: &mut Vec<String>,
    servers: &BTreeMap<String, crate::mcp::ServerConfig>,
    turn: u64,
) -> Option<TempPromptGuard> {
    apply_mcp_bridge_args_in(&crate::settings::home(), kind, args, servers, turn)
}

/// Build `agy mcp add ...` arguments for one enabled `odg` MCP server.
/// Never passes `--env KEY=SECRET` or `--header "Authorization: ..."` on the command line (which would
/// expose credentials in the process table); servers carrying `env` or `headers` return `None` here
/// and are merged into Antigravity's private `0600` local config file instead.
pub(super) fn build_agy_mcp_add_args(name: &str, srv: &crate::mcp::ServerConfig) -> Option<Vec<String>> {
    if srv.disabled || !srv.env.is_empty() || !srv.headers.is_empty() {
        return None;
    }
    let mut args = vec!["mcp".to_string(), "add".to_string()];
    if let Some(cmd) = srv.command.as_deref().filter(|c| !c.trim().is_empty()) {
        args.push("--type".to_string());
        args.push("stdio".to_string());
        args.push(name.to_string());
        args.push("--".to_string());
        args.push(cmd.to_string());
        args.extend(srv.args.iter().cloned());
        Some(args)
    } else if let Some(url) = srv.url.as_deref().filter(|u| !u.trim().is_empty()) {
        args.push("--type".to_string());
        args.push("http".to_string());
        args.push(name.to_string());
        args.push(url.to_string());
        Some(args)
    } else {
        None
    }
}

/// Locate Antigravity's local `mcp_config.json` path under `~/.gemini/antigravity{-cli}` if the
/// file or its parent Antigravity config directory exists on disk.
pub(super) fn resolve_agy_mcp_config_path(user_home: &Path) -> Option<PathBuf> {
    let candidates = [
        user_home
            .join(".gemini")
            .join("antigravity-cli")
            .join("mcp_config.json"),
        user_home.join(".gemini").join("antigravity").join("mcp_config.json"),
    ];
    for cfg in &candidates {
        if cfg.is_file() {
            return Some(cfg.clone());
        }
    }
    for cfg in &candidates {
        if let Some(parent) = cfg.parent()
            && parent.is_dir()
        {
            return Some(cfg.clone());
        }
    }
    None
}

/// Merge enabled `odg` MCP servers (`~/.odg/mcp.json`) into Antigravity's `mcp_config.json`,
/// preserving any non-`odg` entries configured directly in Antigravity.
pub(super) fn merge_agy_mcp_json(
    existing: Option<&str>,
    servers: &BTreeMap<String, crate::mcp::ServerConfig>,
) -> Option<String> {
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
            entry.insert("serverUrl".into(), json!(url));
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

pub(super) async fn sync_antigravity_mcp_in(
    user_home: Option<&Path>,
    bin: &Path,
    servers: &BTreeMap<String, crate::mcp::ServerConfig>,
) -> bool {
    let enabled: Vec<(&String, &crate::mcp::ServerConfig)> = servers.iter().filter(|(_, s)| !s.disabled).collect();
    if enabled.is_empty() {
        return true;
    }
    // Prefer merging into Antigravity's local `mcp_config.json` with `0600` permissions so secret
    // env vars and auth headers never appear in subprocess command-line arguments.
    if let Some(cfg_path) = user_home.and_then(resolve_agy_mcp_config_path) {
        let existing = std::fs::read_to_string(&cfg_path).ok();
        if let Some(merged) = merge_agy_mcp_json(existing.as_deref(), servers)
            && crate::fsutil::write_private_atomically(&cfg_path, merged.as_bytes()).is_ok()
        {
            return true;
        }
    }
    let mut all_ok = true;
    for (name, srv) in enabled {
        let Some(add_args) = build_agy_mcp_add_args(name, srv) else {
            continue;
        };
        let mut cmd = super::cli_command(bin);
        cmd.args(&add_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for key in super::scrubbed_env_keys(CliKind::Antigravity, "") {
            cmd.env_remove(key);
        }
        let ran = tokio::time::timeout(std::time::Duration::from_secs(8), cmd.status()).await;
        if !matches!(ran, Ok(Ok(status)) if status.success()) {
            all_ok = false;
        }
    }
    all_ok
}

/// Sync enabled `odg` MCP servers (`~/.odg/mcp.json`) into Antigravity CLI, preferring a private `0600`
/// merge into `~/.gemini/antigravity{-cli}/mcp_config.json` and never exposing secrets on argv.
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
    let home = dirs::home_dir();
    if sync_antigravity_mcp_in(home.as_deref(), bin, servers).await {
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(sig);
    }
}

/// Merge enabled `odg` MCP servers (`~/.odg/mcp.json`) into an existing Cursor `mcp.json` value,
/// preserving any non-`odg` entries the user configured directly in Cursor.
pub(super) fn merge_cursor_mcp_json(
    existing: Option<&str>,
    servers: &BTreeMap<String, crate::mcp::ServerConfig>,
) -> Option<String> {
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

pub(super) fn sync_cursor_mcp_in(user_home: &Path, servers: &BTreeMap<String, crate::mcp::ServerConfig>) -> bool {
    let cursor_dir = user_home.join(".cursor");
    if !cursor_dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&cursor_dir)
                .is_err()
            {
                return false;
            }
        }
        #[cfg(not(unix))]
        if std::fs::create_dir_all(&cursor_dir).is_err() {
            return false;
        }
    }
    let mcp_path = cursor_dir.join("mcp.json");
    let existing = std::fs::read_to_string(&mcp_path).ok();
    let Some(merged) = merge_cursor_mcp_json(existing.as_deref(), servers) else {
        return false;
    };
    crate::fsutil::write_private_atomically(&mcp_path, merged.as_bytes()).is_ok()
}

/// Sync enabled `odg` MCP servers (`~/.odg/mcp.json`) into `~/.cursor/mcp.json` (`0600`) so `cursor-agent`
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
    if sync_cursor_mcp_in(&home, servers) {
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(sig);
    }
}
