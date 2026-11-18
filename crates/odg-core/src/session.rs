//! The append-only session log on disk and the projections derived from it.
//!
//! A session is one file, `<root>/<id>/session.v1.jsonl`, one [`Envelope`]
//! per line. Lines are only ever appended.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::events::{Envelope, Event};

pub const FORMAT_VERSION: u32 = 1;
const FILE_NAME: &str = "session.v1.jsonl";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session {0} does not exist")]
    NotFound(String),
    #[error("session log {path} is damaged at line {line}: {message}")]
    Damaged { path: String, line: usize, message: String },
    #[error("session log {0} has no session/created header")]
    MissingHeader(String),
    #[error("session format {0} is newer than this build reads")]
    FutureFormat(u32),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What a session list shows, folded from the log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
    /// A turn has started and not ended.
    pub turn_open: bool,
    /// An approval was requested and not resolved.
    pub awaiting_approval: bool,
}

pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create a session whose agent works in `workspace`.
    pub fn create(&self, workspace: &Path, model: &str) -> Result<SessionLog, SessionError> {
        let id = uuid::Uuid::now_v7().to_string();
        let directory = self.root.join(&id);
        fs::create_dir_all(&directory)?;
        let path = directory.join(FILE_NAME);
        let file = OpenOptions::new().create_new(true).append(true).open(&path)?;
        let mut log = SessionLog { id: id.clone(), path, events: Vec::new(), file };
        log.append(Event::SessionCreated {
            id,
            workspace: workspace.to_string_lossy().into_owned(),
            model: model.to_string(),
            format: FORMAT_VERSION,
        })?;
        Ok(log)
    }

    /// Open a stored session. A final line cut off by a crash is dropped.
    pub fn open(&self, id: &str) -> Result<SessionLog, SessionError> {
        let path = self.root.join(id).join(FILE_NAME);
        if !path.is_file() {
            return Err(SessionError::NotFound(id.to_string()));
        }
        let bytes = fs::read(&path)?;
        let mut events = Vec::new();
        let mut good_bytes = 0usize;
        let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
        for (index, line) in lines.iter().enumerate() {
            let complete = line.ends_with(b"\n");
            let parsed = std::str::from_utf8(line)
                .map_err(|error| error.to_string())
                .and_then(|text| serde_json::from_str::<Envelope>(text.trim_end()).map_err(|error| error.to_string()));
            match parsed {
                Ok(envelope) if complete => {
                    events.push(envelope);
                    good_bytes += line.len();
                }
                // Only the last line may be incomplete: the process died while writing it.
                _ if index + 1 == lines.len() => break,
                Ok(_) => unreachable!("only the last line can lack a newline"),
                Err(message) => {
                    return Err(SessionError::Damaged { path: path.display().to_string(), line: index + 1, message });
                }
            }
        }
        match events.first().map(|envelope| &envelope.event) {
            Some(Event::SessionCreated { format, .. }) if *format > FORMAT_VERSION => {
                return Err(SessionError::FutureFormat(*format));
            }
            Some(Event::SessionCreated { .. }) => {}
            _ => return Err(SessionError::MissingHeader(path.display().to_string())),
        }
        if good_bytes < bytes.len() {
            // Windows refuses to truncate through an append-only handle.
            OpenOptions::new().write(true).open(&path)?.set_len(good_bytes as u64)?;
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(SessionLog { id: id.to_string(), path, events, file })
    }

    /// Summaries of every readable session, most recently updated first.
    pub fn list(&self) -> Vec<SessionSummary> {
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut summaries: Vec<SessionSummary> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|id| self.open(&id).ok())
            .map(|log| summarize(log.events()))
            .collect();
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        summaries
    }
}

pub struct SessionLog {
    id: String,
    path: PathBuf,
    events: Vec<Envelope>,
    file: File,
}

impl SessionLog {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn events(&self) -> &[Envelope] {
        &self.events
    }

    /// Workspace and model recorded when the session was created.
    pub fn header(&self) -> (PathBuf, String) {
        match &self.events[0].event {
            Event::SessionCreated { workspace, model, .. } => (PathBuf::from(workspace), model.clone()),
            _ => unreachable!("create and open both guarantee the header"),
        }
    }

    /// Append one event and flush it to disk before returning it.
    pub fn append(&mut self, event: Event) -> io::Result<Envelope> {
        let envelope = Envelope {
            seq: self.events.len() as u64 + 1,
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            event,
        };
        let mut line = serde_json::to_vec(&envelope).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.events.push(envelope.clone());
        Ok(envelope)
    }

    /// Close a turn that a crash left open: answer every unanswered tool call
    /// and end the turn as `interrupted`. Returns the appended events.
    pub fn repair_tail(&mut self) -> io::Result<Vec<Envelope>> {
        let Some(turn) = open_turn(&self.events) else { return Ok(Vec::new()) };
        let mut appended = Vec::new();
        for (tool_use_id, tool) in unanswered_tool_uses(&self.events) {
            appended.push(self.append(Event::ToolResult {
                tool_use_id,
                tool,
                content: "The app closed before this tool ran.".to_string(),
                is_error: true,
                meta: Value::Null,
            })?);
        }
        appended.push(self.append(Event::TurnEnd { turn, reason: "interrupted".to_string(), error: None })?);
        Ok(appended)
    }
}

/// Number of the turn that has started and not ended, if any.
pub fn open_turn(events: &[Envelope]) -> Option<u64> {
    let mut open = None;
    for envelope in events {
        match &envelope.event {
            Event::TurnStart { turn } => open = Some(*turn),
            Event::TurnEnd { .. } => open = None,
            _ => {}
        }
    }
    open
}

/// `(tool_use_id, tool name)` of every tool call that has no result yet, in call order.
pub fn unanswered_tool_uses(events: &[Envelope]) -> Vec<(String, String)> {
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut answered: HashSet<&str> = HashSet::new();
    for envelope in events {
        match &envelope.event {
            Event::AssistantMessage { content, .. } => {
                for block in content.iter().filter(|block| block["type"] == "tool_use") {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        calls.push((id.to_string(), name.to_string()));
                    }
                }
            }
            Event::ToolResult { tool_use_id, .. } => {
                answered.insert(tool_use_id);
            }
            _ => {}
        }
    }
    calls.into_iter().filter(|(id, _)| !answered.contains(id.as_str())).collect()
}

/// The `messages` array of a model request, derived from the log alone.
///
/// Tool results and user text between two assistant messages form one user
/// message; the log orders results before any steering text, which is the
/// order the API requires.
pub fn derive_messages(events: &[Envelope]) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    for envelope in events {
        match &envelope.event {
            Event::UserMessage { text, .. } => pending.push(json!({ "type": "text", "text": text })),
            Event::ToolResult { tool_use_id, content, is_error, .. } => pending.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": if content.is_empty() { "(no output)" } else { content.as_str() },
                "is_error": is_error,
            })),
            Event::AssistantMessage { content, .. } if !content.is_empty() => {
                if !pending.is_empty() {
                    messages.push(json!({ "role": "user", "content": std::mem::take(&mut pending) }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        messages.push(json!({ "role": "user", "content": pending }));
    }
    messages
}

/// Fold a log into its list entry. The title is the first user message, cut to 40 characters.
pub fn summarize(events: &[Envelope]) -> SessionSummary {
    let (id, workspace, model) = match &events[0].event {
        Event::SessionCreated { id, workspace, model, .. } => (id.clone(), workspace.clone(), model.clone()),
        _ => unreachable!("create and open both guarantee the header"),
    };
    let title = events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::UserMessage { text, .. } => Some(text.split_whitespace().collect::<Vec<_>>().join(" ")),
            _ => None,
        })
        .map(|text| {
            let mut title: String = text.chars().take(40).collect();
            if text.chars().count() > 40 {
                title.push('…');
            }
            title
        })
        .unwrap_or_default();
    let mut requested: HashSet<&str> = HashSet::new();
    for envelope in events {
        match &envelope.event {
            Event::ApprovalRequested { tool_use_id, .. } => {
                requested.insert(tool_use_id);
            }
            Event::ApprovalResolved { tool_use_id, .. } | Event::ToolResult { tool_use_id, .. } => {
                requested.remove(tool_use_id.as_str());
            }
            _ => {}
        }
    }
    let turn_open = open_turn(events).is_some();
    SessionSummary {
        id,
        title,
        workspace,
        model,
        created_at: events[0].ts.clone(),
        updated_at: events[events.len() - 1].ts.clone(),
        turn_open,
        awaiting_approval: turn_open && !requested.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Decision;

    fn store() -> (tempfile::TempDir, SessionStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = SessionStore::new(directory.path());
        (directory, store)
    }

    fn assistant(content: Vec<Value>) -> Event {
        Event::AssistantMessage { content, stop_reason: Some("tool_use".into()), model: "m".into(), usage: json!({}) }
    }

    #[test]
    fn reopens_what_it_appended() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "claude-opus-5-5").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage { text: "hello".into(), steering: false }).unwrap();
        let id = log.id().to_string();
        let written = log.events().to_vec();
        drop(log);

        let reopened = store.open(&id).unwrap();
        assert_eq!(reopened.events(), written.as_slice());
        assert_eq!(reopened.events().iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(reopened.header(), (PathBuf::from("/work"), "claude-opus-5-5".to_string()));
    }

    #[test]
    fn drops_a_line_cut_off_by_a_crash_and_keeps_appending() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "m").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        let (id, path) = (log.id().to_string(), log.path().to_path_buf());
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":3,"ts":"2026-01-01T00:00:00Z","type":"user/mess"#).unwrap();
        drop(file);

        let mut reopened = store.open(&id).unwrap();
        assert_eq!(reopened.events().len(), 2);
        reopened.append(Event::UserMessage { text: "again".into(), steering: false }).unwrap();
        drop(reopened);
        assert_eq!(store.open(&id).unwrap().events().len(), 3);
    }

    #[test]
    fn reports_damage_in_the_middle_of_a_log() {
        let (_directory, store) = store();
        let log = store.create(Path::new("/work"), "m").unwrap();
        let (id, path) = (log.id().to_string(), log.path().to_path_buf());
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"not json\n{\"seq\":3}\n").unwrap();
        drop(file);
        assert!(matches!(store.open(&id), Err(SessionError::Damaged { line: 2, .. })));
    }

    #[test]
    fn derives_messages_with_results_before_steering_text() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "m").unwrap();
        let thinking = json!({ "type": "thinking", "thinking": "", "signature": "sig" });
        let call = json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "a" } });
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage { text: "read a".into(), steering: false }).unwrap();
        log.append(assistant(vec![thinking.clone(), call.clone()])).unwrap();
        log.append(Event::ToolResult { tool_use_id: "t1".into(), tool: "read_file".into(), content: String::new(), is_error: false, meta: Value::Null }).unwrap();
        log.append(Event::UserMessage { text: "also b".into(), steering: true }).unwrap();

        let messages = derive_messages(log.events());
        assert_eq!(messages, vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "read a" }] }),
            json!({ "role": "assistant", "content": [thinking, call] }),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "(no output)", "is_error": false },
                { "type": "text", "text": "also b" },
            ] }),
        ]);
    }

    #[test]
    fn repairs_a_turn_left_open_by_a_crash() {
        let (_directory, store) = store();
        let mut log = store.create(Path::new("/work"), "m").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::UserMessage { text: "go".into(), steering: false }).unwrap();
        log.append(assistant(vec![
            json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": {} }),
            json!({ "type": "tool_use", "id": "t2", "name": "run_command", "input": {} }),
        ])).unwrap();
        log.append(Event::ToolResult { tool_use_id: "t1".into(), tool: "read_file".into(), content: "x".into(), is_error: false, meta: Value::Null }).unwrap();
        log.append(Event::ApprovalRequested { tool_use_id: "t2".into(), tool: "run_command".into(), reason: "command".into(), detail: "ls".into() }).unwrap();
        assert!(summarize(log.events()).awaiting_approval);

        let appended = log.repair_tail().unwrap();
        assert_eq!(appended.len(), 2);
        assert!(matches!(&appended[0].event, Event::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "t2"));
        assert!(matches!(&appended[1].event, Event::TurnEnd { turn: 1, reason, .. } if reason == "interrupted"));
        assert!(unanswered_tool_uses(log.events()).is_empty());
        let summary = summarize(log.events());
        assert!(!summary.turn_open && !summary.awaiting_approval);
        assert!(log.repair_tail().unwrap().is_empty());
    }

    #[test]
    fn summarizes_title_and_lists_newest_first() {
        let (_directory, store) = store();
        let mut first = store.create(Path::new("/a"), "m").unwrap();
        first.append(Event::UserMessage { text: "  fix   the\nparser ".into(), steering: false }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut second = store.create(Path::new("/b"), "m").unwrap();
        second.append(Event::UserMessage { text: "x".repeat(50), steering: false }).unwrap();
        second.append(Event::ApprovalResolved { tool_use_id: "none".into(), decision: Decision::Deny }).unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].workspace, "/b");
        assert_eq!(list[0].title.chars().count(), 41);
        assert_eq!(list[1].title, "fix the parser");
    }
}
