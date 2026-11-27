//! Export a session log to a self-contained Markdown document, and summarize
//! abandoned turns when branching/forking from an earlier point in a session.

use crate::events::{Envelope, Event};
use crate::session::summarize;

fn clip_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}\n…（已省略 {} 字）", count - max)
    }
}

/// Render a session's event log into a clean GitHub-Flavored Markdown transcript.
pub fn to_markdown(events: &[Envelope], title_override: Option<&str>) -> String {
    let summary = summarize(events);
    let title = title_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(if summary.title.trim().is_empty() {
            "未命名会话"
        } else {
            summary.title.as_str()
        });

    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- **洞府**：`{}`\n", summary.workspace));
    out.push_str(&format!(
        "- **宗门 / 心法**：`{}` / `{}`\n",
        summary.provider, summary.model
    ));
    if !summary.created_at.is_empty() {
        out.push_str(&format!("- **创建时间**：{}\n", summary.created_at));
    }

    let mut total_in = 0u64;
    let mut total_out = 0u64;
    for env in events {
        if let Event::AssistantMessage { tokens: Some(t), .. } | Event::ContextCompacted { tokens: Some(t), .. } =
            &env.event
        {
            total_in += t.input;
            total_out += t.output;
        }
    }
    if total_in > 0 || total_out > 0 {
        out.push_str(&format!(
            "- **累计消耗**：输入 {total_in} tokens · 输出 {total_out} tokens\n"
        ));
    }
    out.push_str("\n---\n");

    let mut pending_tools: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();

    for env in events {
        match &env.event {
            Event::TurnStart { turn } => {
                out.push_str(&format!("\n## 第 {turn} 轮\n\n"));
            }
            Event::UserMessage {
                text,
                steering,
                images,
                files,
                ..
            } => {
                let heading = if *steering {
                    "### 本尊（中途传音）"
                } else {
                    "### 本尊"
                };
                out.push_str(&format!("{heading}\n\n"));
                if !images.is_empty() {
                    out.push_str(&format!("> 附带图片：{} 张\n\n", images.len()));
                }
                if !files.is_empty() {
                    let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                    out.push_str(&format!("> 附带文件：`{}`\n\n", names.join("`, `")));
                }
                if !text.trim().is_empty() {
                    out.push_str(text.trim());
                    out.push_str("\n\n");
                }
            }
            Event::AssistantMessage { content, .. } => {
                for block in content {
                    match block["type"].as_str() {
                        Some("thinking") => {
                            let thought = block["thinking"].as_str().unwrap_or("").trim();
                            if !thought.is_empty() {
                                out.push_str("<details>\n<summary>分身推演</summary>\n\n");
                                out.push_str(&clip_chars(thought, 3000));
                                out.push_str("\n\n</details>\n\n");
                            }
                        }
                        Some("text") => {
                            let reply = block["text"].as_str().unwrap_or("").trim();
                            if !reply.is_empty() {
                                out.push_str("### 分身\n\n");
                                out.push_str(reply);
                                out.push_str("\n\n");
                            }
                        }
                        Some("tool_use") => {
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("tool").to_string();
                            let input_str = serde_json::to_string_pretty(&block["input"]).unwrap_or_default();
                            pending_tools.insert(id, (name, input_str));
                        }
                        _ => {}
                    }
                }
            }
            Event::ToolResult {
                tool_use_id,
                tool,
                content,
                is_error,
                ..
            } => {
                let (name, input_str) = pending_tools
                    .remove(tool_use_id)
                    .unwrap_or_else(|| (tool.clone(), String::new()));
                let status = if *is_error { "失败" } else { "完成" };
                out.push_str(&format!(
                    "<details>\n<summary>工具调用：<code>{name}</code>（{status}）</summary>\n\n"
                ));
                if !input_str.is_empty() && input_str != "null" && input_str != "{}" {
                    out.push_str("```json\n");
                    out.push_str(&clip_chars(&input_str, 1500));
                    out.push_str("\n```\n\n");
                }
                if !content.trim().is_empty() {
                    out.push_str("```text\n");
                    out.push_str(&clip_chars(content.trim(), 3000));
                    out.push_str("\n```\n\n");
                }
                out.push_str("</details>\n\n");
            }
            Event::ContextCompacted {
                summary,
                tokens_before,
                focus,
                kept_from_turn,
                ..
            } => {
                out.push_str(&format!(
                    "> **前情凝练**（原约 {tokens_before} tokens{}{}）：\n>\n",
                    focus.as_deref().map(|f| format!("，重点保留：{f}")).unwrap_or_default(),
                    kept_from_turn
                        .map(|t| format!("，保留第 {t} 轮起上下文"))
                        .unwrap_or_default(),
                ));
                for line in summary.lines() {
                    out.push_str(&format!("> {line}\n"));
                }
                out.push('\n');
            }
            Event::FilesMade { files, .. } if !files.is_empty() => {
                out.push_str(&format!("> **本轮产出文件**：`{}`\n\n", files.join("`, `")));
            }
            Event::FilesRestored { turn, restored, .. } => {
                out.push_str(&format!(
                    "> **撤回改动**（自第 {turn} 轮起）：`{}`\n\n",
                    restored.join("`, `")
                ));
            }
            Event::TurnEnd { reason, error, .. } if reason != "completed" => {
                let detail = error.as_deref().unwrap_or(reason.as_str());
                out.push_str(&format!("> **本轮中止（{reason}）**：{detail}\n\n"));
            }
            _ => {}
        }
    }

    out
}

/// Build a concise summary of abandoned turns (`>= before_turn`) when forking from an earlier
/// turn in a session, so the user can optionally carry lessons/errors from the old branch into
/// the new branch (inspired by Pi's branch summary).
pub fn abandoned_branch_brief(events: &[Envelope], before_turn: u64) -> Option<String> {
    let start_idx = events
        .iter()
        .position(|env| matches!(env.event, Event::TurnStart { turn } if turn >= before_turn))?;
    let slice = &events[start_idx..];

    let mut prompts: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut edited: Vec<String> = Vec::new();
    let mut conclusions: Vec<String> = Vec::new();
    let mut has_assistant_activity = false;

    for env in slice {
        match &env.event {
            Event::UserMessage { text, .. } if !text.trim().is_empty() => {
                prompts.push(clip_chars(text.trim(), 140));
            }
            Event::ToolResult {
                tool,
                content,
                is_error,
                meta,
                ..
            } => {
                has_assistant_activity = true;
                if *is_error {
                    let first_line = content.lines().next().unwrap_or("执行失败").trim();
                    failures.push(format!("`{tool}`: {}", clip_chars(first_line, 140)));
                } else if let Some(path) = meta["path"].as_str().filter(|p| !p.is_empty())
                    && crate::tools::edits_files(tool)
                    && !edited.iter().any(|existing| existing == path)
                {
                    edited.push(path.to_string());
                }
            }
            Event::AssistantMessage { content, .. } => {
                has_assistant_activity = true;
                let text: String = content
                    .iter()
                    .filter(|b| b["type"].as_str() == Some("text"))
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    conclusions.push(clip_chars(trimmed, 220));
                }
            }
            Event::TurnEnd {
                reason,
                error: Some(err),
                ..
            } if reason != "completed" => {
                has_assistant_activity = true;
                failures.push(format!("回合中断: {}", clip_chars(err.trim(), 140)));
            }
            _ => {}
        }
    }

    if !has_assistant_activity {
        return None;
    }

    let mut lines = vec![format!(
        "[分叉前另一条分支（第 {before_turn} 轮起）的试错摘要，供参考避坑：]"
    )];
    if !prompts.is_empty() {
        lines.push(format!(
            "- 原分支尝试过的指令：{}",
            prompts.into_iter().take(3).collect::<Vec<_>>().join(" / ")
        ));
    }
    if !edited.is_empty() {
        lines.push(format!("- 原分支改动过的文件：{}", edited.join(", ")));
    }
    if !failures.is_empty() {
        lines.push(format!(
            "- 原分支遇到的报错：{}",
            failures.into_iter().take(3).collect::<Vec<_>>().join("；")
        ));
    }
    if let Some(last_reply) = conclusions.last() {
        lines.push(format!("- 原分支最后结论：{last_reply}"));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Tokens;
    use crate::policy::Mode;
    use serde_json::{Value, json};

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq,
            ts: "2026-11-26T00:00:00Z".into(),
            event,
        }
    }

    #[test]
    fn exports_markdown_and_summarizes_abandoned_branch() {
        let events = vec![
            env(
                1,
                Event::SessionCreated {
                    id: "s1".into(),
                    workspace: "/repo/demo".into(),
                    provider: Some("anthropic".into()),
                    model: "claude-sonnet-5-5".into(),
                    format: 21,
                    parent: None,
                    worktree: None,
                    schedule: None,
                    fork: None,
                    aside: None,
                },
            ),
            env(2, Event::TurnStart { turn: 1 }),
            env(
                3,
                Event::UserMessage {
                    text: "先分析架构".into(),
                    steering: false,
                    mode: Mode::Auto,
                    images: vec![],
                    files: vec![],
                },
            ),
            env(
                4,
                Event::AssistantMessage {
                    content: vec![json!({ "type": "text", "text": "架构分为三层。" })],
                    stop_reason: Some("end_turn".into()),
                    model: "claude-sonnet-5-5".into(),
                    usage: Value::Null,
                    tokens: Some(Tokens {
                        input: 120,
                        cached: 0,
                        cache_write: 0,
                        output: 30,
                    }),
                },
            ),
            env(
                5,
                Event::TurnEnd {
                    turn: 1,
                    reason: "completed".into(),
                    error: None,
                },
            ),
            env(6, Event::TurnStart { turn: 2 }),
            env(
                7,
                Event::UserMessage {
                    text: "用旧方案改 auth.rs".into(),
                    steering: false,
                    mode: Mode::Auto,
                    images: vec![],
                    files: vec![],
                },
            ),
            env(
                8,
                Event::AssistantMessage {
                    content: vec![
                        json!({ "type": "tool_use", "id": "t1", "name": "run_command", "input": { "command": "cargo test" } }),
                        json!({ "type": "text", "text": "旧方案导致生命周期冲突。" }),
                    ],
                    stop_reason: Some("end_turn".into()),
                    model: "claude-sonnet-5-5".into(),
                    usage: Value::Null,
                    tokens: Some(Tokens {
                        input: 200,
                        cached: 100,
                        cache_write: 0,
                        output: 50,
                    }),
                },
            ),
            env(
                9,
                Event::ToolResult {
                    tool_use_id: "t1".into(),
                    tool: "run_command".into(),
                    content: "error[E0597]: borrowed value does not live long enough".into(),
                    is_error: true,
                    meta: Value::Null,
                    images: vec![],
                },
            ),
            env(
                10,
                Event::TurnEnd {
                    turn: 2,
                    reason: "completed".into(),
                    error: None,
                },
            ),
        ];

        let md = to_markdown(&events, None);
        assert!(md.contains("# 先分析架构"));
        assert!(md.contains("## 第 1 轮"));
        assert!(md.contains("架构分为三层。"));
        assert!(md.contains("error[E0597]"));
        assert!(md.contains("输入 320 tokens · 输出 80 tokens"));

        let brief = abandoned_branch_brief(&events, 2).expect("abandoned brief");
        assert!(brief.contains("用旧方案改 auth.rs"));
        assert!(brief.contains("error[E0597]"));
        assert!(brief.contains("旧方案导致生命周期冲突。"));
        assert!(abandoned_branch_brief(&events, 3).is_none());
    }
}
