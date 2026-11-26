//! Tool name/input normalization, stateful NDJSON stream parsing, and tool output/diff cleanup.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::llm::Tokens;

use super::{CliEvent, CliKind};

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
        | "search_replace"
        | "strreplacetoolcall"
        | "str_replace_editor"
        | "str_replace_based_edit_tool"
        | "apply_patch"
        | "applypatch"
        | "applypatchtoolcall"
        | "file_change" => "edit_file",
        "delete_file" | "remove_file" | "delete" | "deletetoolcall" | "deletefiletoolcall" => "delete_file",
        "run_command" | "bash" | "shell" | "command_execution" | "shelltoolcall" | "bashtoolcall" | "runterminalcommand" | "run_terminal_command" => "run_command",
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

/// Stateful per-turn NDJSON parser for a CLI backend (needed where items stream cumulative text or partial blocks).
#[derive(Default)]
pub struct StreamParser {
    /// Tracks how many bytes of cumulative text were already emitted for an item id (used by Codex `item.updated`).
    seen_item_bytes: HashMap<String, usize>,
    /// Tracks whether Claude Code already streamed `stream_event` deltas for the current assistant message.
    saw_partial_delta: bool,
    /// Accumulates partial `assistant` deltas streamed by Cursor so the trailing full-message repeat is not emitted twice.
    cursor_streamed: String,
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
                    if self.saw_partial_delta || !delta.is_empty() || tokens.is_some() {
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
                } else if !self.cursor_streamed.is_empty() && text == self.cursor_streamed {
                    // `--stream-partial-output` emits incremental `assistant` chunks followed by one
                    // final `assistant` event repeating the entire message; flush without duplicating.
                    self.cursor_streamed.clear();
                    vec![CliEvent::AgentText { conversation_id: conv, delta: String::new(), done: true, usage: Value::Null, tokens: None }]
                } else {
                    self.cursor_streamed.push_str(&text);
                    vec![CliEvent::AgentText { conversation_id: conv, delta: text, done: false, usage: Value::Null, tokens: None }]
                }
            }
            "tool_call" => {
                self.cursor_streamed.clear();
                let call_key = v["call_id"]
                    .as_str()
                    .or_else(|| v["id"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| self.next_key("cursor"));
                let subtype = v["subtype"].as_str().unwrap_or("");
                // Cursor nests `{"readToolCall": {"args": {...}, "result": {"success": {...}}}}` inside `tool_call`
                // alongside metadata keys (`hookAdditionalContexts`, `toolCallId`, `startedAtMs`, `completedAtMs`).
                // Because `serde_json::Map` iterates alphabetically, pick the actual `*ToolCall` object entry
                // rather than `.next()` (which would otherwise pick `hookAdditionalContexts` before `readToolCall`).
                let nested = v["tool_call"].as_object().and_then(|map| {
                    map.iter()
                        .find(|(k, val)| val.is_object() && (k.ends_with("ToolCall") || val.get("args").is_some() || val.get("result").is_some()))
                        .or_else(|| {
                            map.iter()
                                .find(|(k, val)| val.is_object() && !matches!(k.as_str(), "hookAdditionalContexts" | "toolCallId" | "startedAtMs" | "completedAtMs"))
                        })
                });
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
                            .filter(|s| !s.is_empty())
                            .or_else(|| res["error"].as_str().filter(|s| !s.is_empty()))
                            .or_else(|| res["rejected"]["reason"].as_str().filter(|s| !s.is_empty()))
                            .or_else(|| payload["stderr"].as_str().filter(|s| !s.is_empty()))
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                if !res["rejected"].is_null() {
                                    "Command rejected (read-only mode)".to_string()
                                } else {
                                    res.to_string()
                                }
                            })
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
                self.cursor_streamed.clear();
                let is_error = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str() == Some("error");
                let response = v["result"].as_str().unwrap_or("").to_string();
                let error = is_error.then(|| if !response.is_empty() { response.clone() } else { "Cursor Agent turn failed".to_string() });
                let usage = v.get("usage").cloned().unwrap_or(Value::Null);
                let tokens = anthropic_like_tokens(&usage);
                vec![CliEvent::Result { conversation_id: conv, success: !is_error, response, error, usage, tokens }]
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
            .or_else(|| v["sessionId"].as_str())
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
                    .or_else(|| v["data"].as_str())
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
                let call_key = node["toolCallId"]
                    .as_str()
                    .or_else(|| node["call_id"].as_str())
                    .or_else(|| node["callID"].as_str())
                    .or_else(|| node["id"].as_str())
                    .or_else(|| node["tool_use_id"].as_str())
                    .or_else(|| v["toolCallId"].as_str())
                    .or_else(|| v["call_id"].as_str())
                    .or_else(|| v["callID"].as_str())
                    .or_else(|| v["id"].as_str())
                    .or_else(|| v["tool_use_id"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| self.next_key("grok"));
                let raw_name = node["toolName"]
                    .as_str()
                    .or_else(|| node["name"].as_str())
                    .or_else(|| node["tool"].as_str())
                    .or_else(|| node["title"].as_str())
                    .or_else(|| node["function"]["name"].as_str())
                    .unwrap_or("tool");
                let parsed_fn_args: Option<Value> = node["arguments"]
                    .as_str()
                    .or_else(|| node["function"]["arguments"].as_str())
                    .and_then(|s| serde_json::from_str::<Value>(s).ok());
                let args = if !node["rawInput"].is_null() {
                    &node["rawInput"]
                } else if !node["state"]["input"].is_null() {
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
            "tool_call_update" => {
                let status = v["status"].as_str().unwrap_or("");
                if !matches!(status, "completed" | "failed" | "error") {
                    return Vec::new();
                }
                let call_key = v["toolCallId"]
                    .as_str()
                    .or_else(|| v["call_id"].as_str())
                    .or_else(|| v["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let mut output = extract_content_text(&v["content"]);
                if output.is_empty() {
                    output = v["rawOutput"]["FileContent"]["raw_output"]
                        .as_str()
                        .or_else(|| v["rawOutput"]["FileContent"]["content"].as_str())
                        .or_else(|| v["rawOutput"]["output"].as_str())
                        .or_else(|| v["rawOutput"]["stdout"].as_str())
                        .or_else(|| v["rawOutput"].as_str())
                        .unwrap_or("")
                        .to_string();
                }
                let is_error = matches!(status, "failed" | "error") || v["is_error"].as_bool().unwrap_or(false);
                vec![CliEvent::ToolDone { conversation_id: conv, call_key, name: None, input: Value::Null, output, is_error }]
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
            "end" => {
                let usage = v["usage"].clone();
                let tokens = anthropic_like_tokens(&usage);
                vec![CliEvent::Result { conversation_id: conv, success: true, response: String::new(), error: None, usage, tokens }]
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
            .filter_map(|item| {
                item["text"]
                    .as_str()
                    .or_else(|| item["content"]["text"].as_str())
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if content.is_null() { String::new() } else { content.to_string() }
}

/// Parse an OpenAI Chat Completions-style usage object where `prompt_tokens` already includes
/// any `cached_tokens` (so `cached_tokens` must never be added on top of `prompt_tokens`).
pub(super) fn openai_like_tokens(usage: &Value) -> Option<Tokens> {
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let prompt_tokens = count("prompt_tokens");
    let cached = count("cached_tokens")
        .max(count("cached_input_tokens"))
        .max(usage["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0))
        .max(count("cache_read_tokens"))
        .max(count("cache_read_input_tokens"));
    let input = prompt_tokens.max(cached);
    let output = count("completion_tokens")
        .max(count("output_tokens"))
        .max(count("outputTokens") + count("reasoningTokens"))
        .max(count("output") + count("reasoning"));
    (input > 0 || output > 0).then_some(Tokens { input, cached: cached.min(input), output })
}

fn anthropic_like_tokens(usage: &Value) -> Option<Tokens> {
    if usage.get("prompt_tokens").and_then(Value::as_u64).is_some() {
        return openai_like_tokens(usage);
    }
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let cache_read = count("cache_read_input_tokens")
        .max(count("cache_read_tokens"))
        .max(count("cacheReadTokens"))
        .max(count("cached_tokens"))
        .max(count("cached_input_tokens"))
        .max(usage["cache"]["read"].as_u64().unwrap_or(0));
    let cache_write = count("cache_creation_input_tokens")
        .max(count("cache_write_tokens"))
        .max(count("cacheWriteTokens"))
        .max(usage["cache"]["write"].as_u64().unwrap_or(0));
    let base_input = count("input_tokens")
        .max(count("inputTokens"))
        .max(count("input"));
    let input = base_input + cache_read + cache_write;
    let output = count("output_tokens")
        .max(count("completion_tokens"))
        .max(count("outputTokens") + count("reasoningTokens"))
        .max(count("output") + count("reasoning"));
    (input > 0 || output > 0).then_some(Tokens { input, cached: cache_read, output })
}

pub(super) fn strip_agy_timestamp_envelope(raw_output: &str) -> String {
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
/// the [`crate::events::Event::ToolResult`] `meta` fields that `ToolCard`, `changedFiles`, and `todos` read.
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

pub(super) fn parse_plus_minus_lines(text: &str) -> Option<(usize, usize)> {
    // Matches `"(+23/-7 lines)"` in `agy` edit summaries.
    let open = text.find("(+")?;
    let rest = &text[open + 2..];
    let (plus, after_slash) = rest.split_once("/-")?;
    let (minus, _) = after_slash.split_once(" lines)")?;
    Some((plus.trim().parse().ok()?, minus.trim().parse().ok()?))
}

pub(super) fn estimate_edit_lines(tool_name: &str, input: &Value, path: &str) -> (usize, usize) {
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
pub(super) fn parse_unified_diff(diff_text: &str) -> Option<crate::tools::LineDiff> {
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
