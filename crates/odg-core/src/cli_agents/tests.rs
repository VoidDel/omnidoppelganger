use serde_json::{Value, json};

use crate::events::Event;
use crate::llm::{StreamDelta, Tokens};
use crate::policy::Mode;

use super::args::{build_turn_args, detect_untrusted_cli_configs};
use super::mcp_bridge::{
    apply_mcp_bridge_args_in, build_agy_mcp_add_args, sync_antigravity_mcp_in, sync_cursor_mcp_in,
};
use super::models::{
    cursor_effort_rank, parse_cursor_models_output, parse_opencode_api_models, parse_opencode_models_output,
    pick_cursor_variant,
};
use super::parser::openai_like_tokens;
use super::prompt::{
    apply_prompt_delivery_args, is_windows_batch_script, materialize_cli_images_in, prepare_prompt_delivery_in,
};
use super::recorder::{TurnRecorder, is_allowed_external_artifact_with_roots, lookup_agy_transcript_edit_in};
use super::*;

#[test]
fn builds_codex_claude_cursor_grok_and_opencode_cli_arguments() {
    // Codex: Manual & Plan are read-only; Edit uses workspace-write; Auto adds --approve-for-me; Bypass without sandbox uses full bypass.
    let codex_manual = build_args(
        CliKind::Codex,
        "inspect",
        None,
        None,
        "gpt-5.4",
        "high",
        Mode::Manual,
        false,
        false,
    );
    assert_eq!(
        codex_manual,
        [
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--model",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--sandbox",
            "read-only",
            "--",
            "inspect"
        ]
    );
    let codex_edit = build_args(
        CliKind::Codex,
        "fix bug",
        None,
        None,
        "gpt-5.4",
        "max",
        Mode::Edit,
        false,
        false,
    );
    assert_eq!(
        codex_edit,
        [
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--model",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "--sandbox",
            "workspace-write",
            "--",
            "fix bug"
        ]
    );
    let codex_auto = build_args(
        CliKind::Codex,
        "test",
        None,
        None,
        "gpt-5.4",
        "high",
        Mode::Auto,
        false,
        false,
    );
    assert_eq!(
        codex_auto,
        [
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--model",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--sandbox",
            "workspace-write",
            "--approve-for-me",
            "--",
            "test"
        ]
    );
    let codex_resume = build_args(
        CliKind::Codex,
        "next",
        None,
        Some("thr-1"),
        "gpt-5.4",
        "high",
        Mode::Edit,
        false,
        false,
    );
    assert_eq!(
        codex_resume,
        [
            "exec",
            "resume",
            "thr-1",
            "--json",
            "--skip-git-repo-check",
            "--model",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--",
            "next"
        ]
    );

    // Claude Code: Manual -> manual (never acceptEdits), Edit -> acceptEdits, Auto -> auto, Bypass -> bypassPermissions + --dangerously-skip-permissions.
    let claude_manual = build_args(
        CliKind::ClaudeCode,
        "check",
        None,
        None,
        "sonnet",
        "medium",
        Mode::Manual,
        false,
        false,
    );
    assert_eq!(
        claude_manual,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--model",
            "sonnet",
            "--effort",
            "medium",
            "--permission-mode",
            "manual",
            "--",
            "check"
        ]
    );
    let claude_edit = build_args(
        CliKind::ClaudeCode,
        "patch",
        None,
        None,
        "sonnet",
        "high",
        Mode::Edit,
        false,
        false,
    );
    assert!(
        claude_edit
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"])
    );
    assert!(!claude_edit.iter().any(|a| a == "--dangerously-skip-permissions"));
    let claude_bypass = build_args(
        CliKind::ClaudeCode,
        "review",
        None,
        Some("ses-9"),
        "opus",
        "xhigh",
        Mode::Bypass,
        false,
        false,
    );
    assert_eq!(
        claude_bypass,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--resume",
            "ses-9",
            "--model",
            "opus",
            "--effort",
            "xhigh",
            "--permission-mode",
            "bypassPermissions",
            "--dangerously-skip-permissions",
            "--",
            "review"
        ]
    );

    // Cursor: Plan -> --mode plan (no --approve-mcps, no --force); Manual -> --mode ask; Edit -> no --force; Auto -> --auto-review + --approve-mcps; Bypass -> --force; sandboxed -> --sandbox enabled; untrusted -> no --trust.
    let cursor_plan = build_args(
        CliKind::Cursor,
        "explain",
        Some(Path::new("/tmp/proj")),
        Some("chat-2"),
        "auto",
        "high",
        Mode::Plan,
        false,
        false,
    );
    assert_eq!(
        cursor_plan,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--trust",
            "--workspace",
            "/tmp/proj",
            "--resume",
            "chat-2",
            "--model",
            "auto",
            "--mode",
            "plan",
            "--",
            "explain"
        ]
    );
    let cursor_untrusted_manual = build_turn_args(
        CliKind::Cursor,
        "read",
        Some(Path::new("/tmp/proj")),
        None,
        "auto",
        "high",
        Mode::Manual,
        true,
        false,
        false,
    );
    assert_eq!(
        cursor_untrusted_manual,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--workspace",
            "/tmp/proj",
            "--model",
            "auto",
            "--sandbox",
            "enabled",
            "--mode",
            "ask",
            "--",
            "read"
        ]
    );
    let cursor_edit = build_args(
        CliKind::Cursor,
        "edit only",
        None,
        None,
        "grok-4.7-medium",
        "high",
        Mode::Edit,
        false,
        false,
    );
    assert_eq!(
        cursor_edit,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--trust",
            "--model",
            "grok-4.7-medium",
            "--",
            "edit only"
        ]
    );
    let cursor_auto = build_args(
        CliKind::Cursor,
        "fix",
        None,
        None,
        "grok-4.7-medium",
        "high",
        Mode::Auto,
        false,
        false,
    );
    assert_eq!(
        cursor_auto,
        [
            "--print",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--trust",
            "--approve-mcps",
            "--model",
            "grok-4.7-medium",
            "--auto-review",
            "--",
            "fix"
        ]
    );
    let cursor_bypass = build_args(
        CliKind::Cursor,
        "ship",
        None,
        None,
        "auto",
        "high",
        Mode::Bypass,
        false,
        false,
    );
    assert!(cursor_bypass.iter().any(|a| a == "--force"));

    // Grok: Manual -> default, Edit -> acceptEdits (no --always-approve), Auto -> auto (no --always-approve), Bypass -> bypassPermissions + --always-approve.
    let grok_edit = build_args(
        CliKind::Grok,
        "edit",
        Some(Path::new("/tmp/proj")),
        None,
        "grok-4.7",
        "high",
        Mode::Edit,
        false,
        false,
    );
    assert_eq!(
        grok_edit,
        [
            "--output-format",
            "streaming-json",
            "--cwd",
            "/tmp/proj",
            "--model",
            "grok-4.7",
            "--reasoning-effort",
            "high",
            "--permission-mode",
            "acceptEdits",
            "-p",
            "edit"
        ]
    );
    let grok_auto = build_args(
        CliKind::Grok,
        "hello",
        Some(Path::new("/tmp/proj")),
        Some("g-1"),
        "grok-4.7",
        "high",
        Mode::Auto,
        true,
        false,
    );
    assert_eq!(
        grok_auto,
        [
            "--output-format",
            "streaming-json",
            "--cwd",
            "/tmp/proj",
            "--resume",
            "g-1",
            "--model",
            "grok-4.7",
            "--reasoning-effort",
            "high",
            "--permission-mode",
            "auto",
            "--sandbox",
            "workspace-write",
            "-p",
            "hello"
        ]
    );
    let grok_bypass = build_args(
        CliKind::Grok,
        "yolo",
        None,
        None,
        "grok-4.7",
        "high",
        Mode::Bypass,
        false,
        false,
    );
    assert!(
        grok_bypass
            .windows(2)
            .any(|w| w == ["--permission-mode", "bypassPermissions"])
    );
    assert!(grok_bypass.iter().any(|a| a == "--always-approve"));

    // OpenCode: Plan & Manual -> --agent plan; Edit -> default; Auto & Bypass -> --auto.
    let opencode_manual = build_args(
        CliKind::OpenCode,
        "inspect",
        None,
        None,
        "anthropic/claude-sonnet-4-6",
        "high",
        Mode::Manual,
        false,
        false,
    );
    assert_eq!(
        opencode_manual,
        [
            "run",
            "--format",
            "json",
            "--model",
            "anthropic/claude-sonnet-4-6",
            "--agent",
            "plan",
            "--",
            "inspect"
        ]
    );
    let opencode_edit = build_args(
        CliKind::OpenCode,
        "refactor",
        None,
        Some("ses_7"),
        "anthropic/claude-sonnet-4-6",
        "high",
        Mode::Edit,
        false,
        false,
    );
    assert_eq!(
        opencode_edit,
        [
            "run",
            "--format",
            "json",
            "--session",
            "ses_7",
            "--model",
            "anthropic/claude-sonnet-4-6",
            "--",
            "refactor"
        ]
    );
    let opencode_auto = build_args(
        CliKind::OpenCode,
        "fix",
        None,
        None,
        "opencode/big-pickle",
        "high",
        Mode::Auto,
        false,
        false,
    );
    assert_eq!(
        opencode_auto,
        [
            "run",
            "--format",
            "json",
            "--model",
            "opencode/big-pickle",
            "--auto",
            "--",
            "fix"
        ]
    );
    assert_eq!(serde_json::to_value(CliKind::OpenCode).unwrap(), "opencode");
}

#[test]
fn parses_opencode_v2_stream_events_and_models() {
    let mut parser = StreamParser::default();
    let ev1 = parser.parse_line(
        CliKind::OpenCode,
        r#"{"type":"step_start","sessionID":"ses_1","part":{"type":"step-start"}}"#,
    );
    assert!(matches!(ev1.as_slice(), [CliEvent::Init { conversation_id: Some(id) }] if id == "ses_1"));

    let ev3 = parser.parse_line(
        CliKind::OpenCode,
        r#"{"type":"tool_use","sessionID":"ses_1","part":{"type":"tool","id":"call_1","tool":"read","state":{"status":"completed","input":{"path":"package.json"},"output":"ok"}}}"#,
    );
    assert_eq!(ev3.len(), 2);
    assert!(matches!(&ev3[0], CliEvent::ToolStart { name, .. } if name == "read_file"));
    assert!(matches!(&ev3[1], CliEvent::ToolDone { output, is_error: false, .. } if output == "ok"));

    let ev4 = parser.parse_line(
        CliKind::OpenCode,
        r#"{"type":"step_finish","sessionID":"ses_1","part":{"type":"step-finish","reason":"stop","tokens":{"input":788,"output":10,"reasoning":1,"cache":{"read":17256,"write":0}}}}"#,
    );
    assert!(matches!(
        ev4.as_slice(),
        [CliEvent::AgentText {
            done: true,
            tokens: Some(Tokens {
                input: 18044,
                cached: 17256,
                cache_write: 0,
                output: 11
            }),
            ..
        }]
    ));

    let listed = parse_opencode_models_output(
        "opencode/big-pickle\nopencode/claude-sonnet-5-5\nopencode/big-pickle\nnot a model\n",
    );
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "opencode/big-pickle");
    assert_eq!(listed[0].name, "big-pickle · 免费");
    assert!(listed[0].free);
    assert_eq!(listed[1].id, "opencode/claude-sonnet-5-5");
    assert!(!listed[1].free);

    let api_json = r#"{"data":[
        {"id":"gemini-4-neon","providerID":"opencode","name":"Gemini 4 Neon","cost":[{"input":3,"output":15}],"limit":{"context":1048576}},
        {"id":"glm-5.3","providerID":"opencode-go","name":"GLM-5.3","cost":[{"input":0.5,"output":2}],"limit":{"context":200000}},
        {"id":"big-pickle","providerID":"opencode","name":"Big Pickle","cost":[{"input":0,"output":0}],"limit":{"context":200000}}
    ]}"#;
    let all = parse_opencode_api_models(api_json, false);
    assert_eq!(
        all.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["opencode/big-pickle", "opencode-go/glm-5.3", "opencode/gemini-4-neon"]
    );
    assert_eq!(all[0].name, "Big Pickle · 免费");
    assert!(all[0].free);
    assert_eq!(all[1].name, "GLM-5.3 · Go");
    assert!(!all[1].free);
    assert_eq!(all[2].name, "Gemini 4 Neon");
    assert!(!all[2].free);
    assert_eq!(all[2].context_window, Some(1_048_576));

    let free_only = parse_opencode_api_models(api_json, true);
    assert_eq!(free_only.len(), 1);
    assert_eq!(free_only[0].id, "opencode/big-pickle");
}

#[test]
fn builds_cross_engine_handoff_brief_from_prior_turns() {
    let events = vec![
        crate::events::Envelope {
            seq: 1,
            ts: "2026-11-24T00:00:00Z".into(),
            event: Event::TurnStart { turn: 1 },
        },
        crate::events::Envelope {
            seq: 2,
            ts: "2026-11-24T00:00:01Z".into(),
            event: Event::UserMessage {
                text: "修一下 motor.c 的重锁逻辑".into(),
                steering: false,
                mode: Mode::Auto,
                images: vec![],
                files: vec![],
            },
        },
        crate::events::Envelope {
            seq: 3,
            ts: "2026-11-24T00:00:02Z".into(),
            event: Event::ToolResult {
                tool_use_id: "t1".into(),
                tool: "edit_file".into(),
                content: "ok".into(),
                is_error: false,
                meta: json!({ "path": "peripheral/src/motor.c", "added": 5, "removed": 1 }),
                images: vec![],
            },
        },
        crate::events::Envelope {
            seq: 4,
            ts: "2026-11-24T00:00:03Z".into(),
            event: Event::AssistantMessage {
                content: vec![json!({ "type": "text", "text": "已修复 motor.c 停稳判定。" })],
                stop_reason: Some("end_turn".into()),
                model: "claude-opus-4-7".into(),
                usage: Value::Null,
                tokens: None,
            },
        },
        crate::events::Envelope {
            seq: 5,
            ts: "2026-11-24T00:00:04Z".into(),
            event: Event::TurnStart { turn: 2 },
        },
    ];
    assert!(build_handoff_brief(&events, 1).is_none());
    let brief = build_handoff_brief(&events, 2).expect("handoff brief");
    assert!(brief.contains("peripheral/src/motor.c"));
    assert!(brief.contains("修一下 motor.c"));
    assert!(brief.contains("已修复 motor.c 停稳判定"));
}

#[test]
fn normalizes_update_steps_and_synthesizes_tool_result_meta() {
    let (name, input) = normalize_tool(
        "update_steps",
        &json!({
            "steps": [
                { "title": "构建引擎", "status": "done" },
                { "title": "渲染质检", "status": "in_progress" },
                { "title": "交付文件", "status": "pending" }
            ]
        }),
    );
    assert_eq!(name, "update_todos");
    let (_, todo_meta) = clean_tool_result(
        &name,
        &input,
        "Created At: 2026-11-24T14:00:00+08:00\nCompleted At: 2026-11-24T14:00:01+08:00\nOK",
        false,
    );
    assert_eq!(
        todo_meta["todos"],
        json!([
            { "text": "构建引擎", "status": "done" },
            { "text": "渲染质检", "status": "in_progress" },
            { "text": "交付文件", "status": "pending" }
        ])
    );

    let (cmd_out, cmd_meta) = clean_tool_result(
        "run_command",
        &json!({ "command": "echo hi" }),
        "Created At: 2026-11-24T14:00:00+08:00\nCompleted At: 2026-11-24T14:00:01+08:00\nCommand exited with code 0.\n\nCommand output:\nhi\n",
        false,
    );
    assert_eq!(cmd_out, "hi\n");
    assert_eq!(cmd_meta["exit_code"], 0);

    let (_, read_meta) = clean_tool_result(
        "read_file",
        &json!({ "path": "a.py" }),
        "1413 lines, 79900 bytes",
        false,
    );
    assert_eq!(read_meta["lines"], 1413);

    let (_, edit_meta) = clean_tool_result(
        "edit_file",
        &json!({ "path": "a.py" }),
        "Made 1 replacement in a.py starting at line 10 (+23/-7 lines).",
        false,
    );
    assert_eq!(
        (edit_meta["added"].as_u64(), edit_meta["removed"].as_u64()),
        (Some(23), Some(7))
    );
}

#[test]
fn parses_codex_ndjson_stream_with_incremental_agent_message() {
    let mut p = StreamParser::default();
    assert_eq!(
        p.parse_line(CliKind::Codex, r#"{"type":"thread.started","thread_id":"t-42"}"#),
        vec![CliEvent::Init {
            conversation_id: Some("t-42".into())
        }]
    );
    assert_eq!(
        p.parse_line(
            CliKind::Codex,
            r#"{"type":"item.updated","item":{"id":"m1","type":"agent_message","text":"Hel"}}"#
        ),
        vec![CliEvent::AgentText {
            conversation_id: None,
            delta: "Hel".into(),
            done: false,
            usage: Value::Null,
            tokens: None
        }]
    );
    assert_eq!(
        p.parse_line(
            CliKind::Codex,
            r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Hello!"}}"#
        ),
        vec![CliEvent::AgentText {
            conversation_id: None,
            delta: "lo!".into(),
            done: true,
            usage: Value::Null,
            tokens: None
        }]
    );
    let done = p.parse_line(
        CliKind::Codex,
        r#"{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":50,"output_tokens":12}}"#,
    );
    assert_eq!(
        done[0],
        CliEvent::Result {
            conversation_id: None,
            success: true,
            response: String::new(),
            error: None,
            usage: json!({"input_tokens":200,"cached_input_tokens":50,"output_tokens":12}),
            tokens: Some(Tokens {
                input: 200,
                cached: 50,
                cache_write: 0,
                output: 12
            }),
        }
    );
}

#[test]
fn parses_claude_cursor_and_grok_ndjson_events() {
    let mut claude = StreamParser::default();
    assert_eq!(
        claude.parse_line(CliKind::ClaudeCode, r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#),
        vec![CliEvent::AgentText { conversation_id: Some("s1".into()), delta: "Hi".into(), done: false, usage: Value::Null, tokens: None }]
    );
    // Subsequent full `assistant` message does not duplicate the already-streamed `"Hi"` text!
    let msg = claude.parse_line(CliKind::ClaudeCode, r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":10,"output_tokens":4}}}"#);
    assert_eq!(msg.len(), 2);
    assert_eq!(
        msg[0],
        CliEvent::AgentText {
            conversation_id: Some("s1".into()),
            delta: String::new(),
            done: true,
            usage: json!({"input_tokens":10,"output_tokens":4}),
            tokens: Some(Tokens {
                input: 10,
                cached: 0,
                cache_write: 0,
                output: 4
            }),
        }
    );
    assert_eq!(
        msg[1],
        CliEvent::ToolStart {
            conversation_id: Some("s1".into()),
            call_key: "tu1".into(),
            name: "run_command".into(),
            input: json!({"command": "ls"}),
        }
    );

    let mut cursor = StreamParser::default();
    let c1 = cursor.parse_line(CliKind::Cursor, r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"0."}]},"session_id":"ch1","timestamp_ms":1}"#);
    let c2 = cursor.parse_line(CliKind::Cursor, r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"7.16"}]},"session_id":"ch1","timestamp_ms":2}"#);
    let c_dup = cursor.parse_line(CliKind::Cursor, r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"0.7.16"}]},"session_id":"ch1"}"#);
    assert!(matches!(&c1[0], CliEvent::AgentText { delta, done: false, .. } if delta == "0."));
    assert!(matches!(&c2[0], CliEvent::AgentText { delta, done: false, .. } if delta == "7.16"));
    assert!(matches!(&c_dup[0], CliEvent::AgentText { delta, done: true, .. } if delta.is_empty()));
    let c_res = cursor.parse_line(
        CliKind::Cursor,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"0.7.16","session_id":"ch1","usage":{"inputTokens":885,"outputTokens":33,"cacheReadTokens":13568,"cacheWriteTokens":0,"reasoningTokens":28}}"#,
    );
    assert!(matches!(
        &c_res[0],
        CliEvent::Result {
            success: true,
            tokens: Some(Tokens {
                input: 14453,
                cached: 13568,
                cache_write: 0,
                output: 61
            }),
            ..
        }
    ));
    let tc = cursor.parse_line(
        CliKind::Cursor,
        r#"{"type":"tool_call","subtype":"started","call_id":"c1","session_id":"ch1","tool_call":{"hookAdditionalContexts":[],"readToolCall":{"args":{"path":"README.md"}},"startedAtMs":"1","toolCallId":"c1"}}"#,
    );
    let (parsed_models, variants) = parse_cursor_models_output(
        "Available models\n\n\
         auto - Auto (default)\n\
         grok-4.7-low - Grok 4.7  Low\n\
         grok-4.7-high - Grok 4.7  High\n\
         grok-4.7-medium-fast - Grok 4.7  Medium Fast\u{200b}\u{200b}\n\
         grok-4.7-xhigh-fast - Grok 4.7  Extra High Fast\u{200b}\u{200b}\n\
         gpt-5.3-codex-low - Codex 5.3 Low\n\
         gpt-5.3-codex - Codex 5.3\n\
         gpt-5.3-codex-high - Codex 5.3 High\n\
         claude-4.6-opus-high-thinking - Claude Opus 4.6 1M Thinking\n\
         claude-4.6-opus-max-thinking - Claude Opus 4.6 1M Max Thinking\n\n\
         Tip: use --model <id>",
    );
    assert_eq!(parsed_models.len(), 5);
    assert_eq!(parsed_models[0].id, "auto");
    assert_eq!(parsed_models[1].id, "grok-4.7");
    assert_eq!(parsed_models[1].name, "Grok 4.7");
    assert_eq!(parsed_models[2].id, "grok-4.7-fast");
    assert_eq!(parsed_models[2].name, "Grok 4.7 Fast");
    assert_eq!(parsed_models[3].id, "gpt-5.3-codex");
    assert_eq!(parsed_models[3].name, "Codex 5.3");
    assert_eq!(parsed_models[4].id, "claude-4.6-opus-thinking");
    assert_eq!(
        pick_cursor_variant(cursor_effort_rank("medium"), &variants["gpt-5.3-codex"]),
        Some("gpt-5.3-codex")
    );
    assert_eq!(
        pick_cursor_variant(cursor_effort_rank("high"), &variants["grok-4.7"]),
        Some("grok-4.7-high")
    );
    assert_eq!(
        pick_cursor_variant(cursor_effort_rank("max"), &variants["grok-4.7-fast"]),
        Some("grok-4.7-xhigh-fast")
    );
    assert_eq!(
        pick_cursor_variant(cursor_effort_rank("max"), &variants["claude-4.6-opus-thinking"]),
        Some("claude-4.6-opus-max-thinking")
    );
    assert_eq!(
        tc,
        vec![CliEvent::ToolStart {
            conversation_id: Some("ch1".into()),
            call_key: "c1".into(),
            name: "read_file".into(),
            input: json!({"path": "README.md"}),
        }]
    );

    let mut grok = StreamParser::default();
    let gt = grok.parse_line(CliKind::Grok, r#"{"type":"text","session_id":"g9","text":"Done"}"#);
    assert_eq!(
        gt,
        vec![CliEvent::AgentText {
            conversation_id: Some("g9".into()),
            delta: "Done".into(),
            done: false,
            usage: Value::Null,
            tokens: None
        }]
    );
}

#[test]
fn turn_recorder_computes_agy_edit_diffs_relativizes_paths_and_records_undo_snapshots() {
    use std::sync::Mutex;

    struct TestSink(Mutex<Vec<crate::events::Envelope>>);
    impl Sink for TestSink {
        fn log(&self, event: Event) -> Result<(), String> {
            let mut guard = self.0.lock().unwrap();
            let seq = guard.len() as u64 + 1;
            guard.push(crate::events::Envelope {
                seq,
                ts: "2026-11-24T08:00:00Z".to_string(),
                event,
            });
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("Firmware");
    let snap_dir = tmp.path().join("snapshots");
    let motor_dir = workspace.join("peripheral").join("src");
    std::fs::create_dir_all(&motor_dir).unwrap();
    let motor_c = motor_dir.join("motor.c");
    std::fs::write(&motor_c, "int a = 1;\nint b = 2;\nint c = 3;\n").unwrap();

    let params = TurnParams {
        kind: CliKind::Antigravity,
        workspace: &workspace,
        snapshots: Some(&snap_dir),
        turn: 1,
        prompt: "fix motor".into(),
        conversation_id: Some("35f75987-1c92-4e43-907b-a54a7f66ac2f".into()),
        model: "gemini-4-neon".into(),
        effort: "xhigh".into(),
        mode: Mode::Auto,
        sandboxed: false,
        read_only: false,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };
    let sink = TestSink(Mutex::new(Vec::new()));
    sink.log(Event::TurnStart { turn: 1 }).unwrap();
    let mut recorder = TurnRecorder::new(&params, &sink);

    // 1) `agy` edit_file omits Target/Replacement and sends empty output "".
    let abs_motor = motor_c.to_string_lossy().into_owned();
    let (_, edit_input) = normalize_tool("edit_file", &json!({ "Path": abs_motor }));
    recorder
        .handle_event(CliEvent::ToolStart {
            conversation_id: None,
            call_key: "121".into(),
            name: "edit_file".into(),
            input: edit_input.clone(),
        })
        .unwrap();
    std::fs::write(&motor_c, "int a = 1;\nint b = 20;\nint b2 = 21;\nint c = 3;\n").unwrap();
    recorder
        .handle_event(CliEvent::ToolDone {
            conversation_id: None,
            call_key: "121".into(),
            name: Some("edit_file".into()),
            input: edit_input,
            output: String::new(),
            is_error: false,
        })
        .unwrap();

    // 2) `agy` write_file creates a brand-new test script in workspace.
    let new_test = workspace.join("scripts").join("test_io_park_host.py");
    let (_, write_input) = normalize_tool("write_file", &json!({ "Path": new_test.to_string_lossy() }));
    recorder
        .handle_event(CliEvent::ToolStart {
            conversation_id: None,
            call_key: "137".into(),
            name: "write_file".into(),
            input: write_input.clone(),
        })
        .unwrap();
    std::fs::create_dir_all(new_test.parent().unwrap()).unwrap();
    std::fs::write(&new_test, "def test_park():\n    assert True\n").unwrap();
    recorder
        .handle_event(CliEvent::ToolDone {
            conversation_id: None,
            call_key: "137".into(),
            name: Some("write_file".into()),
            input: write_input,
            output: String::new(),
            is_error: false,
        })
        .unwrap();

    let events = sink.0.lock().unwrap().clone();
    let edit_res = events
        .iter()
        .find_map(|env| match &env.event {
            Event::ToolResult { tool, meta, .. } if tool == "edit_file" => Some(meta.clone()),
            _ => None,
        })
        .expect("edit_file ToolResult logged");
    assert_eq!(edit_res["path"], "peripheral/src/motor.c");
    assert_eq!(edit_res["file"], motor_c.to_string_lossy().as_ref());
    assert_eq!(edit_res["created"], false);
    assert_eq!(edit_res["added"], 2);
    assert_eq!(edit_res["removed"], 1);
    assert!(edit_res["hunk"].is_object(), "expected structured diff hunk");

    let write_res = events
        .iter()
        .find_map(|env| match &env.event {
            Event::ToolResult { tool, meta, .. } if tool == "write_file" => Some(meta.clone()),
            _ => None,
        })
        .expect("write_file ToolResult logged");
    assert_eq!(write_res["path"], "scripts/test_io_park_host.py");
    assert_eq!(write_res["created"], true);
    assert_eq!(write_res["added"], 2);
    assert_eq!(write_res["removed"], 0);

    // Verify undo restores `motor.c` to its original content and deletes the created script!
    let plan = crate::undo::plan(&events, 1);
    assert_eq!(plan.len(), 2);
    let outcome = crate::undo::apply(&plan, &snap_dir, &workspace);
    assert_eq!(
        outcome.restored,
        vec!["peripheral/src/motor.c", "scripts/test_io_park_host.py"]
    );
    assert_eq!(
        std::fs::read_to_string(&motor_c).unwrap(),
        "int a = 1;\nint b = 2;\nint c = 3;\n"
    );
    assert!(!new_test.exists());
}

#[test]
fn extracts_agy_transcript_fallback_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let conv = "35f75987-1c92-4e43-907b-a54a7f66ac2f";
    let log_dir = tmp
        .path()
        .join(".gemini")
        .join("antigravity-cli")
        .join("brain")
        .join(conv)
        .join(".system_generated")
        .join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(
        log_dir.join("transcript.jsonl"),
        r#"{"step_index":125,"source":"MODEL","type":"GENERIC","status":"DONE","content":"Created At: 2026-11-24T15:45:15+08:00\nCompleted At: 2026-11-24T15:45:28+08:00\nMade 1 replacement in /Users/x/motor.c starting at line 906 (+25/-40 lines)."}"#,
    )
    .unwrap();
    let found = lookup_agy_transcript_edit_in(tmp.path(), conv, "125").expect("transcript fallback");
    assert_eq!(found.0, 25);
    assert_eq!(found.1, 40);
    assert!(found.2.contains("(+25/-40 lines)"));
}

#[test]
fn tracks_codex_multifile_patch_claude_multiedit_cursor_nested_result_and_grok_call_id() {
    use std::sync::Mutex;

    struct VecSink(Mutex<Vec<Event>>);
    impl Sink for VecSink {
        fn log(&self, event: Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("repo");
    std::fs::create_dir_all(ws.join("src")).unwrap();

    // 1) Codex: single `item.completed` `file_change` (no `item.started`) with 3 files (`update` with unified diff, `add`, `delete`).
    let created_rs = ws.join("src/new_mod.rs");
    std::fs::write(&created_rs, "pub fn x() {}\npub fn y() {}\n").unwrap();
    let codex_params = TurnParams {
        kind: CliKind::Codex,
        workspace: &ws,
        snapshots: None,
        turn: 1,
        prompt: "patch".into(),
        conversation_id: None,
        model: "gpt-5.4".into(),
        effort: "high".into(),
        mode: Mode::Auto,
        sandboxed: false,
        read_only: false,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };
    let codex_sink = VecSink(Mutex::new(Vec::new()));
    let mut codex_rec = TurnRecorder::new(&codex_params, &codex_sink);
    let mut codex_parser = StreamParser::default();
    let codex_line = json!({
        "type": "item.completed",
        "item": {
            "id": "item_7",
            "type": "file_change",
            "status": "completed",
            "changes": [
                {
                    "path": "src/lib.rs",
                    "kind": "update",
                    "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-fn old() {}\n+fn new1() {}\n+fn new2() {}\n"
                },
                { "path": created_rs.to_string_lossy(), "kind": "add" },
                { "path": "src/obsolete.rs", "kind": "delete", "diff": "@@ -1,2 +0,0 @@\n-dead1\n-dead2\n" }
            ]
        }
    })
    .to_string();
    for ev in codex_parser.parse_line(CliKind::Codex, &codex_line) {
        codex_rec.handle_event(ev).unwrap();
    }
    let codex_results: Vec<(String, Value)> = codex_sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::ToolResult { tool, meta, .. } => Some((tool.clone(), meta.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(codex_results.len(), 3);
    assert_eq!(
        (
            codex_results[0].0.as_str(),
            codex_results[0].1["path"].as_str(),
            codex_results[0].1["added"].as_u64(),
            codex_results[0].1["removed"].as_u64()
        ),
        ("edit_file", Some("src/lib.rs"), Some(2), Some(1))
    );
    assert!(codex_results[0].1["hunk"].is_object());
    assert_eq!(
        (
            codex_results[1].0.as_str(),
            codex_results[1].1["path"].as_str(),
            codex_results[1].1["created"].as_bool(),
            codex_results[1].1["added"].as_u64()
        ),
        ("write_file", Some("src/new_mod.rs"), Some(true), Some(2))
    );
    assert_eq!(
        (
            codex_results[2].0.as_str(),
            codex_results[2].1["deleted"].as_bool(),
            codex_results[2].1["removed"].as_u64()
        ),
        ("delete_file", Some(true), Some(2))
    );

    // 2) Claude Code `MultiEdit` fallback estimation on `edits` array.
    let (_, multi_in) = normalize_tool(
        "MultiEdit",
        &json!({
            "file_path": "src/app.ts",
            "edits": [
                { "old_string": "a\n", "new_string": "a1\na2\n" },
                { "old_string": "b1\nb2\n", "new_string": "b\n" }
            ]
        }),
    );
    let (_, multi_meta) = clean_tool_result("edit_file", &multi_in, "", false);
    assert_eq!(
        (multi_meta["added"].as_u64(), multi_meta["removed"].as_u64()),
        (Some(3), Some(3))
    );

    // 3) Cursor `strReplaceToolCall` + nested `result.success`.
    let mut cursor_parser = StreamParser::default();
    let cur_start = cursor_parser.parse_line(
        CliKind::Cursor,
        r#"{"type":"tool_call","subtype":"started","call_id":"c9","tool_call":{"strReplaceToolCall":{"args":{"path":"src/main.rs"}}}}"#,
    );
    assert_eq!(
        cur_start[0],
        CliEvent::ToolStart {
            conversation_id: None,
            call_key: "c9".into(),
            name: "edit_file".into(),
            input: json!({ "path": "src/main.rs" }),
        }
    );
    let cur_read_done = cursor_parser.parse_line(
        CliKind::Cursor,
        r#"{"type":"tool_call","subtype":"completed","call_id":"c10","tool_call":{"readToolCall":{"args":{"path":"src/main.rs"},"result":{"success":{"totalLines":88}}}}}"#,
    );
    if let CliEvent::ToolDone { output, is_error, .. } = &cur_read_done[0] {
        assert!(!is_error);
        assert_eq!(output, "88 lines");
    } else {
        panic!("expected ToolDone");
    }

    // 4) Grok `tool_use` + `tool_result` paired via `call_id`.
    let grok_file = ws.join("src/grok.rs");
    std::fs::write(&grok_file, "let x = 1;\n").unwrap();
    let grok_params = TurnParams {
        kind: CliKind::Grok,
        workspace: &ws,
        snapshots: None,
        turn: 1,
        prompt: "edit".into(),
        conversation_id: None,
        model: "grok-4".into(),
        effort: "high".into(),
        mode: Mode::Auto,
        sandboxed: false,
        read_only: false,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };
    let grok_sink = VecSink(Mutex::new(Vec::new()));
    let mut grok_rec = TurnRecorder::new(&grok_params, &grok_sink);
    let mut grok_parser = StreamParser::default();
    for ev in grok_parser.parse_line(
        CliKind::Grok,
        &json!({"type":"tool_use","tool":"str_replace_editor","call_id":"call_42","input":{"path":grok_file.to_string_lossy()}}).to_string(),
    ) {
        grok_rec.handle_event(ev).unwrap();
    }
    std::fs::write(&grok_file, "let x = 2;\nlet y = 3;\n").unwrap();
    for ev in grok_parser.parse_line(
        CliKind::Grok,
        r#"{"type":"tool_result","call_id":"call_42","content":"ok"}"#,
    ) {
        grok_rec.handle_event(ev).unwrap();
    }
    let grok_meta = grok_sink
        .0
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::ToolResult { tool, meta, .. } if tool == "edit_file" => Some(meta.clone()),
            _ => None,
        })
        .expect("Grok edit_file paired via call_id");
    assert_eq!(grok_meta["path"], "src/grok.rs");
    assert_eq!(
        (grok_meta["added"].as_u64(), grok_meta["removed"].as_u64()),
        (Some(2), Some(1))
    );
}

#[test]
fn spills_oversized_prompts_to_stdin_or_temp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".odg");
    let huge = "x".repeat(256);

    // Codex switches to `exec ... -` + stdin.
    let codex_prep = prepare_prompt_delivery_in(&home, CliKind::Codex, &huge, 2, 64, false).unwrap();
    assert_eq!(codex_prep.stdin_bytes.as_deref(), Some(huge.as_bytes()));
    let mut codex_args = build_args(
        CliKind::Codex,
        &codex_prep.cli_prompt,
        None,
        None,
        "gpt-5.4",
        "high",
        Mode::Auto,
        false,
        false,
    );
    apply_prompt_delivery_args(CliKind::Codex, &mut codex_args, &codex_prep);
    assert_eq!(codex_args.last().map(String::as_str), Some("-"));
    assert!(!codex_args.iter().any(|a| a == "--"));

    // Claude Code omits positional prompt and pipes via stdin.
    let claude_prep = prepare_prompt_delivery_in(&home, CliKind::ClaudeCode, &huge, 2, 64, false).unwrap();
    assert_eq!(claude_prep.stdin_bytes.as_deref(), Some(huge.as_bytes()));
    let mut claude_args = build_args(
        CliKind::ClaudeCode,
        &claude_prep.cli_prompt,
        None,
        None,
        "opus",
        "high",
        Mode::Auto,
        false,
        false,
    );
    apply_prompt_delivery_args(CliKind::ClaudeCode, &mut claude_args, &claude_prep);
    assert!(!claude_args.iter().any(|a| a == "--"));

    // Cursor omits positional prompt and pipes via stdin (both for huge prompts and Windows .cmd/.bat shims).
    let cursor_prep = prepare_prompt_delivery_in(&home, CliKind::Cursor, &huge, 2, 64, false).unwrap();
    assert_eq!(cursor_prep.stdin_bytes.as_deref(), Some(huge.as_bytes()));
    let mut cursor_args = build_args(
        CliKind::Cursor,
        &cursor_prep.cli_prompt,
        None,
        None,
        "auto",
        "high",
        Mode::Auto,
        false,
        false,
    );
    apply_prompt_delivery_args(CliKind::Cursor, &mut cursor_args, &cursor_prep);
    assert!(!cursor_args.iter().any(|a| a == "--"));

    let cursor_batch = prepare_prompt_delivery_in(&home, CliKind::Cursor, "第一行\n你好", 1, 6_000, true).unwrap();
    assert_eq!(cursor_batch.stdin_bytes.as_deref(), Some("第一行\n你好".as_bytes()));
    let mut cursor_batch_args = build_args(
        CliKind::Cursor,
        &cursor_batch.cli_prompt,
        None,
        None,
        "auto",
        "high",
        Mode::Auto,
        false,
        false,
    );
    apply_prompt_delivery_args(CliKind::Cursor, &mut cursor_batch_args, &cursor_batch);
    assert!(
        !cursor_batch_args
            .iter()
            .any(|a| a == "--" || a.contains('\n') || a.contains('\r'))
    );

    // Antigravity spills to an isolated private per-turn directory `<ODG_HOME>/tmp/prompts/<uuid>/prompt.txt`,
    // adds `--add-dir` pointing only to that per-turn subdir, and cleans up both file and subdir on drop.
    let agy_prep = prepare_prompt_delivery_in(&home, CliKind::Antigravity, &huge, 2, 64, false).unwrap();
    assert!(agy_prep.stdin_bytes.is_none());
    assert!(!agy_prep.cli_prompt.contains('\n') && !agy_prep.cli_prompt.contains('\r'));
    let spilled_path = agy_prep._guard.file.clone().expect("temp prompt file");
    let spilled_dir = agy_prep._guard.dir.clone().expect("temp prompt subdir");
    assert_eq!(spilled_path, spilled_dir.join("prompt.txt"));
    assert_eq!(spilled_dir.parent(), Some(home.join("tmp").join("prompts").as_path()));
    assert_eq!(std::fs::read_to_string(&spilled_path).unwrap(), huge);
    let mut agy_args = build_args(
        CliKind::Antigravity,
        &agy_prep.cli_prompt,
        None,
        None,
        "gemini-4-neon",
        "high",
        Mode::Auto,
        false,
        false,
    );
    apply_prompt_delivery_args(CliKind::Antigravity, &mut agy_args, &agy_prep);
    assert!(
        agy_args
            .windows(2)
            .any(|w| w[0] == "--add-dir" && Path::new(&w[1]) == spilled_dir)
    );
    drop(agy_prep);
    assert!(!spilled_path.exists());
    assert!(!spilled_dir.exists());

    // Non-stdin CLIs on Windows .cmd/.bat with newlines also spill to a single-line temp file prompt.
    let grok_batch = prepare_prompt_delivery_in(&home, CliKind::Grok, "line1\nline2", 1, 6_000, true).unwrap();
    assert!(grok_batch.stdin_bytes.is_none());
    assert!(!grok_batch.cli_prompt.contains('\n') && !grok_batch.cli_prompt.contains('\r'));
    let grok_path = grok_batch._guard.file.clone().expect("temp prompt file");
    assert_eq!(std::fs::read_to_string(&grok_path).unwrap(), "line1\nline2");
    drop(grok_batch);
    assert!(!grok_path.exists());

    assert!(is_windows_batch_script(Path::new("C:\\Users\\a\\cursor-agent.cmd")));
    assert!(is_windows_batch_script(Path::new("C:\\Users\\a\\AGENT.BAT")));
    assert!(!is_windows_batch_script(Path::new("C:\\Users\\a\\cursor-agent.exe")));
    assert!(!is_windows_batch_script(Path::new("/usr/local/bin/cursor-agent")));
}

#[test]
fn bridges_odg_skills_and_mcp_servers_to_cli_turns() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".odg");
    let skill_dir = tmp.path().join("demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: 演示专项秘籍\n---\n\n第一步：先检查输入。\n第二步：输出结果。\n",
    )
    .unwrap();

    let roots = vec![tmp.path().to_path_buf()];
    let triggered = build_skills_brief(&roots, "按秘籍「demo-skill」（skill）办：处理这份数据", false)
        .expect("triggered skill brief");
    assert!(triggered.contains("第一步：先检查输入"));
    assert!(!triggered.contains("[可用 odg 秘籍"));

    let first_turn = build_skills_brief(&roots, "帮我看看代码", true).expect("catalog brief");
    assert!(first_turn.contains("[可用 odg 秘籍 / Skills]"));
    assert!(first_turn.contains("demo-skill: 演示专项秘籍"));
    assert!(first_turn.contains("SKILL.md"));

    let mut servers = BTreeMap::new();
    servers.insert(
        "docs".to_string(),
        crate::mcp::ServerConfig {
            command: Some("npx".into()),
            args: vec!["-y".into(), "docs-mcp".into()],
            ..Default::default()
        },
    );
    servers.insert(
        "remote".to_string(),
        crate::mcp::ServerConfig {
            url: Some("https://mcp.example.com".into()),
            ..Default::default()
        },
    );

    let mut claude_args = build_args(
        CliKind::ClaudeCode,
        "hi",
        None,
        None,
        "opus",
        "high",
        Mode::Auto,
        false,
        false,
    );
    let claude_guard = apply_mcp_bridge_args_in(&home, CliKind::ClaudeCode, &mut claude_args, &servers, 1)
        .expect("claude mcp temp file");
    let cfg_idx = claude_args
        .iter()
        .position(|a| a == "--mcp-config")
        .expect("--mcp-config flag");
    let sep_idx = claude_args.iter().position(|a| a == "--").expect("-- separator");
    assert!(cfg_idx < sep_idx);
    let cfg_path = PathBuf::from(&claude_args[cfg_idx + 1]);
    let cfg_json: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
    assert_eq!(cfg_json["mcpServers"]["docs"]["command"], "npx");
    assert_eq!(cfg_json["mcpServers"]["remote"]["type"], "http");
    drop(claude_guard);
    assert!(!cfg_path.exists());

    let mut codex_args = build_args(
        CliKind::Codex,
        "hi",
        None,
        None,
        "gpt-5.4",
        "high",
        Mode::Auto,
        false,
        false,
    );
    assert!(apply_mcp_bridge_args_in(&home, CliKind::Codex, &mut codex_args, &servers, 1).is_none());
    assert!(codex_args.iter().any(|a| a == "mcp_servers.docs.command=\"npx\""));
    assert!(
        codex_args
            .iter()
            .any(|a| a == "mcp_servers.remote.url=\"https://mcp.example.com\"")
    );

    let mut agy_args = build_args(
        CliKind::Antigravity,
        "hi",
        None,
        None,
        "gemini-4-neon",
        "high",
        Mode::Auto,
        false,
        false,
    );
    assert!(apply_mcp_bridge_args_in(&home, CliKind::Antigravity, &mut agy_args, &servers, 1).is_none());
    assert!(!agy_args.iter().any(|a| a == "--settings"));
    assert_eq!(
        build_agy_mcp_add_args("docs", &servers["docs"]).unwrap(),
        ["mcp", "add", "--type", "stdio", "docs", "--", "npx", "-y", "docs-mcp"]
    );
    assert_eq!(
        build_agy_mcp_add_args("remote", &servers["remote"]).unwrap(),
        ["mcp", "add", "--type", "http", "remote", "https://mcp.example.com"]
    );
}

#[test]
fn cli_read_file_on_external_screenshot_sets_picture_meta_and_tracks_artifact() {
    let ws = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let shot = scratch.path().join("01-home.png");
    std::fs::write(&shot, b"\x89PNG\r\n\x1a\n").unwrap();

    struct Capture(std::sync::Mutex<Vec<Event>>);
    impl Sink for Capture {
        fn log(&self, event: Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let params = TurnParams {
        kind: CliKind::Antigravity,
        workspace: ws.path(),
        snapshots: None,
        turn: 1,
        prompt: "test ui".into(),
        model: "gemini-4-neon".into(),
        effort: "high".into(),
        mode: Mode::Auto,
        sandboxed: false,
        read_only: false,
        workspace_trusted: true,
        conversation_id: None,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: std::collections::BTreeMap::new(),
    };
    let sink = Capture(std::sync::Mutex::new(Vec::new()));
    let mut recorder = TurnRecorder::new(&params, &sink);
    recorder
        .handle_event(CliEvent::ToolStart {
            conversation_id: None,
            call_key: "c1".into(),
            name: "read_file".into(),
            input: json!({ "path": shot.display().to_string() }),
        })
        .unwrap();
    recorder
        .handle_event(CliEvent::ToolDone {
            conversation_id: None,
            call_key: "c1".into(),
            name: Some("read_file".into()),
            input: Value::Null,
            output: "Read image".into(),
            is_error: false,
        })
        .unwrap();

    let events = sink.0.lock().unwrap().clone();
    let result = events.iter().find_map(|e| match e {
        Event::ToolResult { tool, meta, .. } if tool == "read_file" => Some(meta.clone()),
        _ => None,
    });
    let meta = result.expect("read_file ToolResult");
    assert_eq!(meta["picture"], "image/png");
    assert_eq!(meta["file"], shot.display().to_string());
    assert_eq!(recorder.external_artifacts, vec![shot]);
}

#[test]
fn cursor_mcp_merge_preserves_existing_servers_and_skips_disabled() {
    let existing = r#"{"mcpServers":{"user-custom":{"command":"node","args":["s.js"]}},"otherKey":true}"#;
    let mut servers = BTreeMap::new();
    servers.insert(
        "odg-docs".to_string(),
        crate::mcp::ServerConfig {
            url: Some("https://docs.example.com/mcp".into()),
            ..Default::default()
        },
    );
    servers.insert(
        "off".to_string(),
        crate::mcp::ServerConfig {
            command: Some("npx".into()),
            disabled: true,
            ..Default::default()
        },
    );
    let merged = mcp_bridge::merge_cursor_mcp_json(Some(existing), &servers).expect("merged json");
    let v: Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(v["otherKey"], true);
    assert_eq!(v["mcpServers"]["user-custom"]["command"], "node");
    assert_eq!(v["mcpServers"]["odg-docs"]["url"], "https://docs.example.com/mcp");
    assert!(v["mcpServers"].get("off").is_none());
}

#[tokio::test]
async fn temp_files_live_under_private_odg_home_with_0600_mode() {
    let root = tempfile::tempdir().unwrap();
    let odg_home = root.path().join(".odg");
    let fake_user_home = root.path().join("user_home");
    std::fs::create_dir_all(fake_user_home.join(".gemini").join("antigravity")).unwrap();

    // 1) MCP bridge temp file for Claude Code + secret stripping for Antigravity argv.
    let mut servers = BTreeMap::new();
    let mut env_map = BTreeMap::new();
    env_map.insert("API_TOKEN".to_string(), "super-secret-env".to_string());
    servers.insert(
        "secret-stdio".to_string(),
        crate::mcp::ServerConfig {
            command: Some("node".into()),
            args: vec!["srv.js".into()],
            env: env_map,
            ..Default::default()
        },
    );
    let mut hdr_map = BTreeMap::new();
    hdr_map.insert("Authorization".to_string(), "Bearer super-secret-bearer".to_string());
    servers.insert(
        "secret-http".to_string(),
        crate::mcp::ServerConfig {
            url: Some("https://mcp.internal/sse".into()),
            headers: hdr_map,
            ..Default::default()
        },
    );
    // Secret servers must NEVER be serialized onto `agy mcp add` command-line arguments.
    assert!(build_agy_mcp_add_args("secret-stdio", &servers["secret-stdio"]).is_none());
    assert!(build_agy_mcp_add_args("secret-http", &servers["secret-http"]).is_none());

    let mut claude_args = build_args(
        CliKind::ClaudeCode,
        "hi",
        None,
        None,
        "opus",
        "high",
        Mode::Auto,
        false,
        false,
    );
    let mcp_guard =
        apply_mcp_bridge_args_in(&odg_home, CliKind::ClaudeCode, &mut claude_args, &servers, 3).expect("mcp guard");
    let mcp_path = mcp_guard.file.clone().expect("mcp path");
    assert!(mcp_path.starts_with(odg_home.join("tmp").join("mcp")));
    assert!(!mcp_path.to_string_lossy().contains("odg-mcp"));

    // 2) Oversized prompt overflow creates distinct per-turn `<uuid>/prompt.txt` subdirs for `agy`.
    let prep1 = prepare_prompt_delivery_in(&odg_home, CliKind::Antigravity, &"a".repeat(128), 1, 32, false).unwrap();
    let prep2 = prepare_prompt_delivery_in(&odg_home, CliKind::Antigravity, &"b".repeat(128), 2, 32, false).unwrap();
    let p1_file = prep1._guard.file.as_ref().unwrap();
    let p2_file = prep2._guard.file.as_ref().unwrap();
    assert_ne!(
        p1_file.parent(),
        p2_file.parent(),
        "each turn gets an isolated --add-dir subdirectory"
    );
    assert!(p1_file.starts_with(odg_home.join("tmp").join("prompts")));

    // 3) Materialized CLI images live under `<ODG_HOME>/tmp/images/<uuid>/`.
    let img = crate::events::Image {
        media_type: "image/png".to_string(),
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            .to_string(),
    };
    let (img_paths, img_guard) = materialize_cli_images_in(&odg_home, &[img], 1).unwrap();
    assert_eq!(img_paths.len(), 1);
    assert!(img_paths[0].starts_with(odg_home.join("tmp").join("images")));

    // 4) Cursor & Antigravity local MCP config merges write `0600` files atomically.
    assert!(sync_cursor_mcp_in(&fake_user_home, &servers));
    let cursor_mcp = fake_user_home.join(".cursor").join("mcp.json");
    assert!(cursor_mcp.is_file());

    assert!(sync_antigravity_mcp_in(Some(&fake_user_home), Path::new("/nonexistent/agy"), &servers).await);
    let agy_mcp = fake_user_home
        .join(".gemini")
        .join("antigravity")
        .join("mcp_config.json");
    let agy_json: Value = serde_json::from_str(&std::fs::read_to_string(&agy_mcp).unwrap()).unwrap();
    assert_eq!(
        agy_json["mcpServers"]["secret-stdio"]["env"]["API_TOKEN"],
        "super-secret-env"
    );
    assert_eq!(
        agy_json["mcpServers"]["secret-http"]["headers"]["Authorization"],
        "Bearer super-secret-bearer"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [
            odg_home.join("tmp"),
            odg_home.join("tmp").join("mcp"),
            odg_home.join("tmp").join("prompts"),
            odg_home.join("tmp").join("images"),
            p1_file.parent().unwrap().to_path_buf(),
            img_paths[0].parent().unwrap().to_path_buf(),
        ] {
            let mode = std::fs::symlink_metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "expected 0700 on {}", dir.display());
        }
        for file in [&mcp_path, p1_file, p2_file, &img_paths[0], &cursor_mcp, &agy_mcp] {
            let mode = std::fs::symlink_metadata(file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected 0600 on {}", file.display());
        }
    }

    let img_dir = img_paths[0].parent().unwrap().to_path_buf();
    drop(img_guard);
    assert!(!img_paths[0].exists() && !img_dir.exists());
    drop(mcp_guard);
    assert!(!mcp_path.exists());
}

#[tokio::test]
async fn untrusted_workspace_detects_claude_mcp_codex_cursor_configs_and_isolates_claude_hooks() {
    struct NoopSink;
    impl Sink for NoopSink {
        fn log(&self, _event: Event) -> Result<(), String> {
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join(".claude")).unwrap();
    std::fs::write(ws.path().join(".claude").join("settings.json"), r#"{"hooks":{}}"#).unwrap();
    std::fs::write(ws.path().join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    std::fs::create_dir_all(ws.path().join(".codex")).unwrap();
    std::fs::create_dir_all(ws.path().join(".gemini")).unwrap();
    std::fs::create_dir_all(ws.path().join(".opencode")).unwrap();
    std::fs::create_dir_all(ws.path().join(".grok")).unwrap();

    let detected = detect_untrusted_cli_configs(ws.path());
    for expected in [".claude", ".mcp.json", ".codex", ".gemini", ".opencode", ".grok"] {
        assert!(
            detected.iter().any(|item| item == expected),
            "missing {expected} in {detected:?}"
        );
    }

    // Claude Code isolates untrusted repos via `--setting-sources user`.
    let claude_untrusted = build_turn_args(
        CliKind::ClaudeCode,
        "audit",
        Some(ws.path()),
        None,
        "sonnet",
        "high",
        Mode::Auto,
        true,
        false,
        false,
    );
    assert!(claude_untrusted.windows(2).any(|w| w == ["--setting-sources", "user"]));
    let claude_trusted = build_turn_args(
        CliKind::ClaudeCode,
        "audit",
        Some(ws.path()),
        None,
        "sonnet",
        "high",
        Mode::Auto,
        true,
        false,
        true,
    );
    assert!(!claude_trusted.iter().any(|a| a == "--setting-sources"));

    // Codex, Antigravity, Grok, and OpenCode block before launching when untrusted and local configs exist.
    let cancel = CancellationToken::new();
    for kind in [CliKind::Codex, CliKind::Antigravity, CliKind::Grok, CliKind::OpenCode] {
        let params = TurnParams {
            kind,
            workspace: ws.path(),
            snapshots: None,
            turn: 1,
            prompt: "hello".into(),
            conversation_id: None,
            model: "default".into(),
            effort: "high".into(),
            mode: Mode::Auto,
            sandboxed: true,
            read_only: false,
            workspace_trusted: false,
            extra_dirs: Vec::new(),
            image_paths: Vec::new(),
            mcp_servers: BTreeMap::new(),
        };
        let err = run_turn(params, &NoopSink, &cancel).await.unwrap_err();
        assert!(
            err.contains("尚未授信") && err.contains("信任此洞府"),
            "unexpected error for {kind:?}: {err}"
        );
    }

    // Env var minimization keeps only the target CLI's own auth keys.
    let codex_scrubbed = scrubbed_env_keys(CliKind::Codex, "gpt-5.4");
    assert!(codex_scrubbed.contains(&"ANTHROPIC_API_KEY"));
    assert!(codex_scrubbed.contains(&"GEMINI_API_KEY"));
    assert!(!codex_scrubbed.contains(&"OPENAI_API_KEY"));
    assert!(!codex_scrubbed.contains(&"CODEX_API_KEY"));

    let opencode_anth = scrubbed_env_keys(CliKind::OpenCode, "anthropic/claude-sonnet-4-6");
    assert!(!opencode_anth.contains(&"ANTHROPIC_API_KEY"));
    assert!(!opencode_anth.contains(&"OPENCODE_API_KEY"));
    assert!(opencode_anth.contains(&"OPENAI_API_KEY"));
    assert!(opencode_anth.contains(&"DEEPSEEK_API_KEY"));
}

#[tokio::test]
async fn antigravity_mcp_sync_is_skipped_in_plan_manual_and_edit_modes() {
    assert!(!should_bridge_mcp(Mode::Plan, false));
    assert!(!should_bridge_mcp(Mode::Manual, false));
    assert!(!should_bridge_mcp(Mode::Edit, false));
    assert!(!should_bridge_mcp(Mode::Auto, true));
    assert!(!should_bridge_mcp(Mode::Bypass, true));
    assert!(should_bridge_mcp(Mode::Auto, false));
    assert!(should_bridge_mcp(Mode::Bypass, false));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        struct NoopSink;
        impl Sink for NoopSink {
            fn log(&self, _event: Event) -> Result<(), String> {
                Ok(())
            }
            fn stream(&self, _delta: StreamDelta) {}
        }

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let log_file = tmp.path().join("agy-calls.log");
        let fake_agy = tmp.path().join("fake-agy.sh");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> \"{}\"\nif [ \"$1\" = \"mcp\" ]; then\n  exit 0\nfi\necho '{{\"event\":\"init\",\"conversation_id\":\"c1\"}}'\necho '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n",
            log_file.display()
        );
        std::fs::write(&fake_agy, script).unwrap();
        std::fs::set_permissions(&fake_agy, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut servers = BTreeMap::new();
        servers.insert(
            "test-gated-mcp".to_string(),
            crate::mcp::ServerConfig {
                command: Some("echo".into()),
                args: vec!["mcp".into()],
                ..Default::default()
            },
        );
        let cancel = CancellationToken::new();

        for (mode, read_only) in [
            (Mode::Plan, false),
            (Mode::Manual, false),
            (Mode::Edit, false),
            (Mode::Auto, true),
        ] {
            let params = TurnParams {
                kind: CliKind::Antigravity,
                workspace: &ws,
                snapshots: None,
                turn: 1,
                prompt: "plan only".into(),
                conversation_id: None,
                model: "gemini-4-neon".into(),
                effort: "high".into(),
                mode,
                sandboxed: false,
                read_only,
                workspace_trusted: true,
                extra_dirs: Vec::new(),
                image_paths: Vec::new(),
                mcp_servers: servers.clone(),
            };
            run_turn_with_limits(
                params,
                Some(&fake_agy),
                CLI_IDLE_TIMEOUT,
                MAX_CLI_LINE_BYTES,
                &NoopSink,
                &cancel,
            )
            .await
            .unwrap();
        }
        let logged = std::fs::read_to_string(&log_file).unwrap_or_default();
        assert!(
            !logged.contains("mcp add"),
            "MCP sync must be skipped in Plan/Manual/Edit/read_only: {logged}"
        );
    }
}

#[test]
fn codex_cursor_and_claude_turns_preserve_end_of_turn_token_usage_after_flushed_text() {
    use std::sync::Mutex;

    struct VecSink(Mutex<Vec<Event>>);
    impl Sink for VecSink {
        fn log(&self, event: Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let sum_tokens = |events: &[Event]| -> (Tokens, Vec<String>) {
        let mut sum = Tokens::default();
        let mut texts = Vec::new();
        for ev in events {
            if let Event::AssistantMessage { content, tokens, .. } = ev {
                if let Some(t) = tokens {
                    sum.input += t.input;
                    sum.cached += t.cached;
                    sum.output += t.output;
                }
                for block in content {
                    if block["type"].as_str() == Some("text")
                        && let Some(s) = block["text"].as_str()
                    {
                        texts.push(s.to_string());
                    }
                }
            }
        }
        (sum, texts)
    };

    let ws = tempfile::tempdir().unwrap();
    let make_params = |kind: CliKind| TurnParams {
        kind,
        workspace: ws.path(),
        snapshots: None,
        turn: 1,
        prompt: "hi".into(),
        conversation_id: None,
        model: "m".into(),
        effort: "high".into(),
        mode: Mode::Auto,
        sandboxed: false,
        read_only: false,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };

    // 1) Codex: `item.completed` flushes text without tokens, then `turn.completed` settles full usage without duplicating text.
    let codex_params = make_params(CliKind::Codex);
    let codex_sink = VecSink(Mutex::new(Vec::new()));
    let mut codex_rec = TurnRecorder::new(&codex_params, &codex_sink);
    let mut codex_parser = StreamParser::default();
    for line in [
        r#"{"type":"thread.started","thread_id":"t-1"}"#,
        r#"{"type":"item.updated","item":{"id":"m1","type":"agent_message","text":"Hel"}}"#,
        r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"Hello!"}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":50,"output_tokens":12}}"#,
    ] {
        for ev in codex_parser.parse_line(CliKind::Codex, line) {
            codex_rec.handle_event(ev).unwrap();
        }
    }
    codex_rec.finish().unwrap();
    let (codex_total, codex_texts) = sum_tokens(&codex_sink.0.lock().unwrap());
    assert_eq!(codex_texts, vec!["Hello!"]);
    assert_eq!(
        codex_total,
        Tokens {
            input: 200,
            cached: 50,
            cache_write: 0,
            output: 12
        }
    );

    // 2) Cursor: `--stream-partial-output` flushes text on the final `assistant` event, then `result` settles usage without repeating `result`.
    let cursor_params = make_params(CliKind::Cursor);
    let cursor_sink = VecSink(Mutex::new(Vec::new()));
    let mut cursor_rec = TurnRecorder::new(&cursor_params, &cursor_sink);
    let mut cursor_parser = StreamParser::default();
    for line in [
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"0."}]},"session_id":"ch1"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"7.16"}]},"session_id":"ch1"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"0.7.16"}]},"session_id":"ch1"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"0.7.16","session_id":"ch1","usage":{"inputTokens":885,"outputTokens":33,"cacheReadTokens":13568,"cacheWriteTokens":0,"reasoningTokens":28}}"#,
    ] {
        for ev in cursor_parser.parse_line(CliKind::Cursor, line) {
            cursor_rec.handle_event(ev).unwrap();
        }
    }
    cursor_rec.finish().unwrap();
    let (cursor_total, cursor_texts) = sum_tokens(&cursor_sink.0.lock().unwrap());
    assert_eq!(cursor_texts, vec!["0.7.16"]);
    assert_eq!(
        cursor_total,
        Tokens {
            input: 14453,
            cached: 13568,
            cache_write: 0,
            output: 61
        }
    );

    // 3) Claude Code: step 1 pure tool_use `assistant` with cache_creation + cache_read, step 2 streamed text + `result(cumulative)`.
    let claude_params = make_params(CliKind::ClaudeCode);
    let claude_sink = VecSink(Mutex::new(Vec::new()));
    let mut claude_rec = TurnRecorder::new(&claude_params, &claude_sink);
    let mut claude_parser = StreamParser::default();
    for line in [
        r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":100,"cache_read_input_tokens":400,"cache_creation_input_tokens":50,"output_tokens":20}}}"#,
        r#"{"type":"user","session_id":"s1","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"file.rs"}]}}"#,
        r#"{"type":"stream_event","session_id":"s1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"All set."}}}"#,
        r#"{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"All set."}],"usage":{"input_tokens":120,"cache_read_input_tokens":450,"cache_creation_input_tokens":0,"output_tokens":15}}}"#,
        r#"{"type":"result","session_id":"s1","is_error":false,"result":"All set.","usage":{"input_tokens":230,"cache_read_input_tokens":850,"cache_creation_input_tokens":50,"output_tokens":40}}"#,
    ] {
        for ev in claude_parser.parse_line(CliKind::ClaudeCode, line) {
            claude_rec.handle_event(ev).unwrap();
        }
    }
    claude_rec.finish().unwrap();
    let (claude_total, claude_texts) = sum_tokens(&claude_sink.0.lock().unwrap());
    assert_eq!(claude_texts, vec!["All set."]);
    // Step 1 input = 550 (cached 400), Step 2 input = 570 (cached 450), cumulative result input = 230 + 850 + 50 = 1130 (cached 850), output = 40.
    assert_eq!(
        claude_total,
        Tokens {
            input: 1130,
            cached: 850,
            cache_write: 0,
            output: 40
        }
    );

    // 4) OpenAI-style `prompt_tokens` already includes `cached_tokens` (never doubled!).
    assert_eq!(
        openai_like_tokens(&json!({
            "prompt_tokens": 1000,
            "prompt_tokens_details": { "cached_tokens": 800 },
            "completion_tokens": 45
        })),
        Some(Tokens {
            input: 1000,
            cached: 800,
            cache_write: 0,
            output: 45
        })
    );
    let mut grok_parser = StreamParser::default();
    let grok_res = grok_parser.parse_line(
        CliKind::Grok,
        r#"{"type":"result","result":"ok","usage":{"prompt_tokens":1000,"cached_tokens":800,"completion_tokens":45}}"#,
    );
    assert!(matches!(
        grok_res.as_slice(),
        [CliEvent::Result {
            tokens: Some(Tokens {
                input: 1000,
                cached: 800,
                cache_write: 0,
                output: 45
            }),
            ..
        }]
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_stdout_and_idle_timeout_are_handled_gracefully() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    struct VecSink(Mutex<Vec<Event>>);
    impl Sink for VecSink {
        fn log(&self, event: Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        fn stream(&self, _delta: StreamDelta) {}
    }

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // 1) Non-UTF-8 bytes on stdout, oversized stdout line (> 512 bytes), and > 512 KB stderr do not crash or deadlock.
    let noisy_bin = tmp.path().join("noisy-codex.sh");
    let noisy_script = "#!/bin/sh\n\
        printf '\\377\\376non-utf8-banner\\n'\n\
        dd if=/dev/zero bs=1024 count=600 2>/dev/null | tr '\\0' 'e' >&2\n\
        dd if=/dev/zero bs=1024 count=2 2>/dev/null | tr '\\0' 'x'\n\
        printf '\\n'\n\
        printf '{\"type\":\"thread.started\",\"thread_id\":\"t-bin\"}\\n'\n\
        printf '{\"type\":\"item.completed\",\"item\":{\"id\":\"m1\",\"type\":\"agent_message\",\"text\":\"survived\"}}\\n'\n\
        printf '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":2}}\\n'\n";
    std::fs::write(&noisy_bin, noisy_script).unwrap();
    std::fs::set_permissions(&noisy_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sink = VecSink(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();
    let params = TurnParams {
        kind: CliKind::Codex,
        workspace: &ws,
        snapshots: None,
        turn: 1,
        prompt: "hi".into(),
        conversation_id: None,
        model: "gpt-5.4".into(),
        effort: "high".into(),
        mode: Mode::Plan,
        sandboxed: false,
        read_only: true,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };
    let outcome = run_turn_with_limits(params, Some(&noisy_bin), Duration::from_secs(5), 512, &sink, &cancel)
        .await
        .expect("non-utf8 and oversized line handled gracefully");
    assert_eq!(outcome.reason, "completed");
    let events = sink.0.lock().unwrap().clone();
    assert!(events.iter().any(|ev| matches!(
        ev,
        Event::AssistantMessage { content, .. } if content.iter().any(|b| b["text"].as_str() == Some("survived"))
    )));

    // 2) Hung process with zero output triggers idle timeout and kills its entire process group.
    let pid_file = tmp.path().join("grandchild.pid");
    let hang_bin = tmp.path().join("hang-codex.sh");
    let hang_script = format!("#!/bin/sh\nsleep 60 &\necho $! > \"{}\"\nwait\n", pid_file.display());
    std::fs::write(&hang_bin, hang_script).unwrap();
    std::fs::set_permissions(&hang_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let hang_params = TurnParams {
        kind: CliKind::Codex,
        workspace: &ws,
        snapshots: None,
        turn: 1,
        prompt: "hang".into(),
        conversation_id: None,
        model: "gpt-5.4".into(),
        effort: "high".into(),
        mode: Mode::Plan,
        sandboxed: false,
        read_only: true,
        workspace_trusted: true,
        extra_dirs: Vec::new(),
        image_paths: Vec::new(),
        mcp_servers: BTreeMap::new(),
    };
    let err = run_turn_with_limits(
        hang_params,
        Some(&hang_bin),
        Duration::from_millis(200),
        MAX_CLI_LINE_BYTES,
        &VecSink(Mutex::new(Vec::new())),
        &cancel,
    )
    .await
    .unwrap_err();
    assert!(err.contains("输出超时"), "unexpected error: {err}");

    if let Ok(raw_pid) = std::fs::read_to_string(&pid_file)
        && let Ok(gpid) = raw_pid.trim().parse::<u32>()
    {
        for _ in 0..20 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &gpid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let still_alive = std::process::Command::new("kill")
            .args(["-0", &gpid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            !still_alive,
            "grandchild sleep process {gpid} should have been killed on idle timeout"
        );
    }
}

#[test]
fn external_artifacts_only_admits_verified_images_under_allowed_temp_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let allowed_tmp = tmp.path().join("allowed_tmp");
    let allowed_brain = tmp.path().join("brain");
    let user_docs = tmp.path().join("Documents");
    std::fs::create_dir_all(&allowed_tmp).unwrap();
    std::fs::create_dir_all(&allowed_brain).unwrap();
    std::fs::create_dir_all(&user_docs).unwrap();

    let roots = vec![allowed_tmp.clone(), allowed_brain.clone()];

    let valid_png = allowed_tmp.join("shot.png");
    std::fs::write(&valid_png, b"\x89PNG\r\n\x1a\npayload").unwrap();
    assert!(is_allowed_external_artifact_with_roots(&valid_png, &roots));

    let brain_jpg = allowed_brain.join("ui.jpg");
    std::fs::write(&brain_jpg, b"\xFF\xD8\xFF\xE0jpeg").unwrap();
    assert!(is_allowed_external_artifact_with_roots(&brain_jpg, &roots));

    // Sensitive path outside allowed temp/brain roots is rejected even if it is a real PNG.
    let home_png = user_docs.join("private.png");
    std::fs::write(&home_png, b"\x89PNG\r\n\x1a\npayload").unwrap();
    assert!(!is_allowed_external_artifact_with_roots(&home_png, &roots));

    // Non-image text disguised with `.png` extension inside allowed temp root is rejected.
    let fake_png = allowed_tmp.join("id_rsa.png");
    std::fs::write(&fake_png, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
    assert!(!is_allowed_external_artifact_with_roots(&fake_png, &roots));

    #[cfg(unix)]
    {
        let link_png = allowed_tmp.join("linked.png");
        std::os::unix::fs::symlink(&home_png, &link_png).unwrap();
        assert!(!is_allowed_external_artifact_with_roots(&link_png, &roots));
    }
}

#[test]
fn unwraps_windows_npm_and_exe_cmd_shims_without_cmd_exe() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // 1. Standard npm `.cmd` shim wrapping a JS entrypoint.
    let js_path = root
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("cli.js");
    std::fs::create_dir_all(js_path.parent().unwrap()).unwrap();
    std::fs::write(&js_path, "console.log('ok');").unwrap();
    let npm_shim = root.join("claude.cmd");
    std::fs::write(
        &npm_shim,
        "@ECHO off\r\n\"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
    )
    .unwrap();
    let fake_node = root.join("node.exe");
    std::fs::write(&fake_node, b"MZ").unwrap();

    let (prog, args) = super::unwrap_windows_cmd_shim_with(&npm_shim, || None).expect("unwrap npm shim");
    assert_eq!(prog, fake_node);
    assert_eq!(args, vec![js_path.into_os_string()]);

    // 2. Direct `.exe` wrapper `.cmd` shim (e.g. Antigravity / IDE / Scoop launcher).
    let bin_dir = root.join("app/bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let real_exe = bin_dir.join("agy.exe");
    std::fs::write(&real_exe, b"MZ").unwrap();
    let ide_shim = bin_dir.join("agy.cmd");
    std::fs::write(&ide_shim, "@echo off\r\n\"%~dp0\\agy.exe\" %*\r\n").unwrap();

    let (prog2, args2) = super::unwrap_windows_cmd_shim_with(&ide_shim, || None).expect("unwrap exe shim");
    assert_eq!(prog2, real_exe);
    assert!(args2.is_empty());

    // 3. Native `.exe` is returned as None (no unwrapping needed).
    assert!(super::unwrap_windows_cmd_shim_with(&real_exe, || None).is_none());
}
