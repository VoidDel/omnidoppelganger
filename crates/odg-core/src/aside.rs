//! Side conversations (旁聊): a read-only session the user talks to beside another session while that one's
//! agent works, without interrupting it. Before each of the user's messages the side conversation is told how
//! the main session is getting on, by a briefing written here from the main session's log. The briefing is
//! logged in the side conversation as an `aside/briefed` event, so what the model was shown stays on record.

use std::collections::HashMap;

use serde_json::Value;

use crate::agent::Status;
use crate::events::{Envelope, Event};

/// The most steps of the main agent's current turn a briefing lists: the latest ones.
const STEPS: usize = 12;
/// The most characters kept of a message or a paragraph the briefing quotes.
const QUOTE_CHARS: usize = 1200;

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

/// One line for a tool call: what it did to what, in a few words.
fn step(name: &str, input: &Value) -> String {
    let field = |key: &str| input[key].as_str().map(|value| clip(value, 160)).unwrap_or_default();
    match name {
        "read_file" => format!("read {}", field("path")),
        "edit_file" => format!("edited {}", field("path")),
        "write_file" => format!("wrote {}", field("path")),
        "delete_file" => format!("deleted {}", field("path")),
        "move_file" => format!("moved {} to {}", field("from"), field("to")),
        "run_command" => format!("ran `{}`", field("command")),
        "search" => format!("searched for `{}`", field("pattern")),
        "find_files" => format!(
            "listed files {}",
            [field("path"), field("glob")]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "batch_inspect" => format!(
            "batch-inspected {} queries",
            input["queries"].as_array().map_or(0, Vec::len)
        ),
        "code_nav" => format!("code_nav {} {}", field("operation"), field("path")),
        "fetch_url" => format!("fetched {}", field("url")),
        "web_search" => format!("searched the web for `{}`", field("query")),
        "delegate" => format!("set off {} subagent(s)", input["tasks"].as_array().map_or(0, Vec::len)),
        other => other.to_string(),
    }
}

/// What the briefing says of the main session's status.
fn standing(status: Status) -> &'static str {
    match status {
        Status::Running => "working on a turn right now",
        Status::WaitingApproval => "stopped, waiting for the user to approve or answer something",
        Status::Idle => "idle, waiting for the user's next message",
    }
}

/// A briefing on the main session for its side conversation: what it was asked, its plan, its latest steps and
/// words, and how it stands. Written for the model, in English like the rest of what the app tells it; the
/// model still answers in the user's language.
pub fn brief(title: &str, status: Status, events: &[Envelope]) -> String {
    let mut out = String::from(
        "[Briefing on the main agent, written by the app: these are not the user's words. The user is talking to you in a side conversation while the main agent works in the same workspace.]\n",
    );
    let title = if title.trim().is_empty() {
        "untitled"
    } else {
        title.trim()
    };
    out.push_str(&format!(
        "Main session \"{}\": {}.\n",
        clip(title, 120),
        standing(status)
    ));

    // The plan as the main agent last wrote it, from anywhere in its log.
    let plan = events.iter().rev().find_map(|envelope| match &envelope.event {
        Event::ToolResult {
            tool,
            is_error: false,
            meta,
            ..
        } if tool == "update_todos" => meta["todos"].as_array().cloned(),
        _ => None,
    });

    let start = events
        .iter()
        .rposition(|envelope| matches!(envelope.event, Event::TurnStart { .. }));
    let Some(start) = start else {
        out.push_str("It has not been given a task yet.\n[End of briefing]");
        return out;
    };
    let turn = match events[start].event {
        Event::TurnStart { turn } => turn,
        _ => 0,
    };
    let current = &events[start..];

    let mut task = None;
    let mut steering = Vec::new();
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut outcome: HashMap<String, bool> = HashMap::new();
    let mut waiting: HashMap<String, String> = HashMap::new();
    let mut words = None;
    let mut ended = None;
    for envelope in current {
        match &envelope.event {
            Event::UserMessage {
                text, steering: false, ..
            } if task.is_none() => task = Some(text.clone()),
            Event::UserMessage { text, .. } => steering.push(text.clone()),
            Event::AssistantMessage { content, .. } => {
                for block in content {
                    match block["type"].as_str() {
                        Some("text") => {
                            if let Some(said) = block["text"].as_str().filter(|said| !said.trim().is_empty()) {
                                words = Some(said.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let name = block["name"].as_str().unwrap_or_default();
                            if name != "update_todos" {
                                calls.push((
                                    block["id"].as_str().unwrap_or_default().to_string(),
                                    step(name, &block["input"]),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::ApprovalRequested {
                tool_use_id,
                tool,
                detail,
                ..
            } => {
                waiting.insert(tool_use_id.clone(), format!("{tool}: {}", clip(detail, 200)));
            }
            Event::ApprovalResolved { tool_use_id, .. } => {
                waiting.remove(tool_use_id);
            }
            Event::ToolResult {
                tool_use_id, is_error, ..
            } => {
                outcome.insert(tool_use_id.clone(), *is_error);
                waiting.remove(tool_use_id);
            }
            Event::TurnEnd { reason, error, .. } => ended = Some((reason.clone(), error.clone())),
            _ => {}
        }
    }

    match &task {
        Some(task) => out.push_str(&format!(
            "What the user asked of it in turn {turn}:\n{}\n",
            clip(task, QUOTE_CHARS)
        )),
        None => out.push_str(&format!("Turn {turn} has no message from the user.\n")),
    }
    if !steering.is_empty() {
        out.push_str("What the user said to it since, during the turn:\n");
        for said in steering.iter().rev().take(3).rev() {
            out.push_str(&format!("- {}\n", clip(said, 300)));
        }
    }
    if let Some(plan) = plan.filter(|plan| !plan.is_empty()) {
        out.push_str("Its plan:\n");
        for item in &plan {
            let mark = match item["status"].as_str() {
                Some("done") => "[x]",
                Some("in_progress") => "[>]",
                _ => "[ ]",
            };
            out.push_str(&format!(
                "{mark} {}\n",
                clip(item["text"].as_str().unwrap_or_default(), 200)
            ));
        }
    }
    if !calls.is_empty() {
        let skipped = calls.len().saturating_sub(STEPS);
        out.push_str(&format!(
            "Its steps in this turn{}:\n",
            if skipped > 0 {
                format!(", the last {STEPS} of {}", calls.len())
            } else {
                String::new()
            }
        ));
        for (id, line) in calls.iter().skip(skipped) {
            let note = match (outcome.get(id), waiting.contains_key(id)) {
                (Some(true), _) => " (failed)",
                (Some(false), _) => "",
                (None, true) => " (waiting for approval)",
                (None, false) if ended.is_none() => " (running)",
                (None, false) => " (did not run)",
            };
            out.push_str(&format!("- {line}{note}\n"));
        }
    }
    for asked in waiting.values() {
        out.push_str(&format!("Waiting for the user's approval of {asked}\n"));
    }
    if let Some(said) = words {
        out.push_str(&format!("The last thing it said:\n{}\n", clip(&said, QUOTE_CHARS)));
    }
    if let Some((reason, error)) = ended {
        match error {
            Some(error) => out.push_str(&format!("The turn ended ({reason}): {}\n", clip(&error, 300))),
            None => out.push_str(&format!("The turn ended ({reason}).\n")),
        }
    }
    out.push_str("[End of briefing]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Mode;
    use serde_json::json;

    fn at(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq,
            ts: "2026-11-23T00:00:00Z".to_string(),
            event,
        }
    }

    fn header() -> Event {
        Event::SessionCreated {
            id: "main".into(),
            workspace: "/w".into(),
            provider: None,
            model: "m".into(),
            format: crate::session::FORMAT_VERSION,
            parent: None,
            worktree: None,
            schedule: None,
            fork: None,
            aside: None,
        }
    }

    fn said(text: &str, steering: bool) -> Event {
        Event::UserMessage {
            text: text.into(),
            steering,
            mode: Mode::Edit,
            images: vec![],
            files: vec![],
        }
    }

    #[test]
    fn a_main_session_with_no_turn_is_said_to_have_no_task() {
        let told = brief("", Status::Idle, &[at(1, header())]);
        assert!(
            told.contains("untitled") && told.contains("idle") && told.contains("not been given a task"),
            "{told}"
        );
    }

    #[test]
    fn briefs_the_task_plan_steps_approvals_and_last_words_of_the_current_turn() {
        let events = vec![
            at(1, header()),
            at(2, Event::TurnStart { turn: 1 }),
            at(3, said("an earlier task", false)),
            at(
                4,
                Event::TurnEnd {
                    turn: 1,
                    reason: "end_turn".into(),
                    error: None,
                },
            ),
            at(5, Event::TurnStart { turn: 2 }),
            at(6, said("fix the tokenizer bug", false)),
            at(
                7,
                Event::AssistantMessage {
                    content: vec![
                        json!({ "type": "text", "text": "I will look at the lexer first." }),
                        json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "src/lexer.rs" } }),
                        json!({ "type": "tool_use", "id": "t2", "name": "update_todos", "input": {} }),
                    ],
                    stop_reason: Some("tool_use".into()),
                    model: "m".into(),
                    usage: Value::Null,
                    tokens: None,
                },
            ),
            at(
                8,
                Event::ToolResult {
                    tool_use_id: "t1".into(),
                    tool: "read_file".into(),
                    content: "...".into(),
                    is_error: false,
                    meta: Value::Null,
                    images: vec![],
                },
            ),
            at(
                9,
                Event::ToolResult {
                    tool_use_id: "t2".into(),
                    tool: "update_todos".into(),
                    content: "ok".into(),
                    is_error: false,
                    meta: json!({ "todos": [{ "text": "Find the bug", "status": "done" }, { "text": "Fix it", "status": "in_progress" }, { "text": "Test", "status": "pending" }] }),
                    images: vec![],
                },
            ),
            at(10, said("also keep the old API", true)),
            at(
                11,
                Event::AssistantMessage {
                    content: vec![
                        json!({ "type": "tool_use", "id": "t3", "name": "run_command", "input": { "command": "cargo test" } }),
                        json!({ "type": "tool_use", "id": "t4", "name": "edit_file", "input": { "path": "src/lexer.rs" } }),
                    ],
                    stop_reason: Some("tool_use".into()),
                    model: "m".into(),
                    usage: Value::Null,
                    tokens: None,
                },
            ),
            at(
                12,
                Event::ToolResult {
                    tool_use_id: "t3".into(),
                    tool: "run_command".into(),
                    content: "1 failed".into(),
                    is_error: true,
                    meta: Value::Null,
                    images: vec![],
                },
            ),
            at(
                13,
                Event::ApprovalRequested {
                    tool_use_id: "t4".into(),
                    tool: "edit_file".into(),
                    reason: "edit".into(),
                    detail: "src/lexer.rs".into(),
                    grant: None,
                    expires_at: None,
                },
            ),
        ];
        let told = brief("Tokenizer", Status::WaitingApproval, &events);
        assert!(
            told.contains("\"Tokenizer\"") && told.contains("waiting for the user"),
            "{told}"
        );
        assert!(
            told.contains("turn 2:\nfix the tokenizer bug") && !told.contains("an earlier task"),
            "{told}"
        );
        assert!(told.contains("- also keep the old API"), "{told}");
        assert!(told.contains("[x] Find the bug\n[>] Fix it\n[ ] Test"), "{told}");
        assert!(
            told.contains(
                "- read src/lexer.rs\n- ran `cargo test` (failed)\n- edited src/lexer.rs (waiting for approval)"
            ),
            "{told}"
        );
        assert!(!told.contains("update_todos"), "{told}");
        assert!(
            told.contains("Waiting for the user's approval of edit_file: src/lexer.rs"),
            "{told}"
        );
        assert!(
            told.contains("The last thing it said:\nI will look at the lexer first."),
            "{told}"
        );
        assert!(told.ends_with("[End of briefing]"));
    }

    #[test]
    fn lists_only_the_latest_steps_of_a_long_turn_and_says_how_it_ended() {
        let mut events = vec![
            at(1, header()),
            at(2, Event::TurnStart { turn: 1 }),
            at(3, said("go", false)),
        ];
        let blocks: Vec<Value> = (0..20).map(|n| json!({ "type": "tool_use", "id": format!("r{n}"), "name": "read_file", "input": { "path": format!("f{n}.rs") } })).collect();
        events.push(at(
            4,
            Event::AssistantMessage {
                content: blocks,
                stop_reason: Some("tool_use".into()),
                model: "m".into(),
                usage: Value::Null,
                tokens: None,
            },
        ));
        events.push(at(
            5,
            Event::TurnEnd {
                turn: 1,
                reason: "cancelled".into(),
                error: None,
            },
        ));
        let told = brief("t", Status::Idle, &events);
        assert!(
            told.contains("the last 12 of 20") && told.contains("read f19.rs") && !told.contains("read f7.rs\n"),
            "{told}"
        );
        assert!(
            told.contains("(did not run)") && told.contains("The turn ended (cancelled)."),
            "{told}"
        );
    }
}
