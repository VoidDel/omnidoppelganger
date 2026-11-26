//! Per-turn event recorder translating [`CliEvent`] items into session log [`Event`]s,
//! capturing pre/post file contents across `ToolStart` -> `ToolDone` so external CLI edits get
//! accurate `+added / -removed` counts, unified diff hunks, workspace-relative display paths, and undo snapshots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::events::Event;
use crate::llm::{StreamDelta, Tokens};

use super::parser::{clean_tool_result, estimate_edit_lines, parse_plus_minus_lines, parse_unified_diff, strip_agy_timestamp_envelope};
use super::{CliEvent, CliKind, Sink, TurnParams};

struct StartedTool {
    id: String,
    name: String,
    input: Value,
    target: Option<(PathBuf, String)>,
    before_bytes: Option<Vec<u8>>,
    file_existed_at_start: bool,
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

pub(super) struct TurnRecorder<'a, S: Sink> {
    params: &'a TurnParams<'a>,
    sink: &'a S,
    backend_key: &'static str,
    logged_conv: Option<String>,
    active_text: String,
    emitted_any_text: bool,
    started_tools: HashMap<String, StartedTool>,
    known_files: HashMap<PathBuf, Vec<u8>>,
    turn_started: std::time::SystemTime,
    pub(super) external_artifacts: Vec<PathBuf>,
    pub(super) result_error: Option<String>,
}

impl<'a, S: Sink> TurnRecorder<'a, S> {
    pub(super) fn new(params: &'a TurnParams<'a>, sink: &'a S) -> Self {
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

    pub(super) fn flush_text(&mut self, usage: Value, tokens: Option<Tokens>) -> Result<(), String> {
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

    pub(super) fn handle_event(&mut self, ev: CliEvent) -> Result<(), String> {
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
                    let is_user_input_image = self.params.image_paths.iter().any(|p| p == path);
                    let created_this_turn = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .is_some_and(|mtime| mtime + std::time::Duration::from_secs(2) >= self.turn_started);
                    if !in_workspace && !is_user_input_image && created_this_turn && !self.external_artifacts.contains(path) {
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
pub(super) fn lookup_agy_transcript_edit_in(home: &Path, conv_id: &str, call_key: &str) -> Option<(usize, usize, String)> {
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
