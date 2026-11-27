use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use odg_core::agent::{Status, UiEvent};
use odg_core::events::{Envelope, Event};
use odg_core::llm::{AssistantTurn, Llm, LlmError, LlmRequest, StreamDelta, Tokens};
use odg_core::policy::Mode;
use odg_core::schedule::{Schedule, When};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::commands::git::{git_checkout_core, session_git_workspace};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::commands::runtime_lsp::run_inline_shell_with_sandbox;
use crate::commands::runtime_lsp::{INLINE_COMMAND_TIMEOUT_SECS, strip_inline_ansi};
use crate::commands::sessions::{
    ensure_markdown_extension, export_session_markdown_with_title, land_session_core, sanitize_export_filename,
    send_message_core,
};
use crate::commands::workbench::{
    materialize_snapshot_in, open_attachment_blocking, picture_blocking, prune_stale_pastes_in, remove_pasted_file_in,
    safe_to_open_directly, save_pasted_file_in,
};
use crate::error::{
    ERR_SKILL_EXISTS, ERR_SKILL_SUBFOLDERS, ERR_WORKTREE_BUSY, IpcError, coded, coded_with_params, parse_coded,
};
use crate::state::{AppCore, MAX_IDLE_SESSIONS, safe_session_id};
use crate::{forward_session_events, should_auto_update_runtime};

enum ScriptedStep {
    Reply(AssistantTurn),
    HoldUntil(CancellationToken, AssistantTurn),
}

#[derive(Default)]
struct MockLlm {
    steps: Mutex<VecDeque<ScriptedStep>>,
}

impl MockLlm {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn push_text(&self, text: &str) {
        self.steps.lock().unwrap().push_back(ScriptedStep::Reply(AssistantTurn {
            content: vec![json!({ "type": "text", "text": text })],
            stop_reason: Some("end_turn".to_string()),
            model: "claude-sonnet-4-6".to_string(),
            usage: json!({ "input_tokens": 12, "output_tokens": 6 }),
            invalid_tool_inputs: HashMap::new(),
        }));
    }

    fn push_tool(&self, id: &str, name: &str, input: Value) {
        self.steps.lock().unwrap().push_back(ScriptedStep::Reply(AssistantTurn {
            content: vec![json!({ "type": "tool_use", "id": id, "name": name, "input": input })],
            stop_reason: Some("tool_use".to_string()),
            model: "claude-sonnet-4-6".to_string(),
            usage: json!({ "input_tokens": 15, "output_tokens": 8 }),
            invalid_tool_inputs: HashMap::new(),
        }));
    }

    fn push_held_text(&self, gate: CancellationToken, text: &str) {
        self.steps.lock().unwrap().push_back(ScriptedStep::HoldUntil(
            gate,
            AssistantTurn {
                content: vec![json!({ "type": "text", "text": text })],
                stop_reason: Some("end_turn".to_string()),
                model: "claude-sonnet-4-6".to_string(),
                usage: json!({ "input_tokens": 10, "output_tokens": 4 }),
                invalid_tool_inputs: HashMap::new(),
            },
        ));
    }
}

impl Llm for MockLlm {
    async fn stream(
        &self,
        _request: LlmRequest,
        deltas: UnboundedSender<StreamDelta>,
        cancel: CancellationToken,
    ) -> Result<AssistantTurn, LlmError> {
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            Some(ScriptedStep::Reply(turn)) => {
                if let Some(txt) = turn.content.first().and_then(|b| b["text"].as_str()) {
                    let _ = deltas.send(StreamDelta::TextDelta {
                        index: 0,
                        text: txt.to_string(),
                    });
                }
                Ok(turn)
            }
            Some(ScriptedStep::HoldUntil(gate, turn)) => {
                tokio::select! {
                    () = gate.cancelled() => Ok(turn),
                    () = cancel.cancelled() => Err(LlmError::Cancelled),
                }
            }
            None => Ok(AssistantTurn {
                content: vec![json!({ "type": "text", "text": "default mock reply" })],
                stop_reason: Some("end_turn".to_string()),
                model: "claude-sonnet-4-6".to_string(),
                usage: json!({ "input_tokens": 5, "output_tokens": 3 }),
                invalid_tool_inputs: HashMap::new(),
            }),
        }
    }
}

fn setup_core() -> (
    tempfile::TempDir,
    PathBuf,
    Arc<MockLlm>,
    Arc<AppCore<MockLlm>>,
    UnboundedReceiver<UiEvent>,
) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let ws = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let llm = MockLlm::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let core = AppCore::new_for_test(home, llm.clone(), tx, tokio::runtime::Handle::current());
    (temp, ws, llm, core, rx)
}

fn init_git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["init", "--quiet", "--initial-branch", "main"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(dir.join("README.md"), "# Demo\n").unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@localhost",
                "commit",
                "--quiet",
                "-m",
                "init"
            ])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn safe_to_open_directly_enforces_whitelist_trailing_dot_and_unix_exec_mode() {
    let dir = tempfile::tempdir().unwrap();
    let make = |name: &str, mode: u32| -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, b"sample").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path
    };

    for blocked in [
        "launch.terminal",
        "entry.desktop",
        "bundle.AppImage",
        "script.py",
        "page.html",
        "page.htm",
        "diagram.svg",
        "feed.xml",
        "site.url",
        "mac.webloc",
        "auto.scpt",
        "run.command",
        "setup.exe",
        "RUN.BAT",
        "build.cmd",
        "tool.ps1",
        "install.msi",
        "shortcut.lnk",
        "script.sh",
        "README",
        "Makefile",
        "archive.tar.gz",
        ".md",
        "evil.exe.",
        "sneaky.pdf.",
        "trailing.md ",
    ] {
        let p = make(blocked, 0o644);
        assert!(!safe_to_open_directly(&p), "{blocked} must be blocked");
    }

    for allowed in [
        "report.pdf",
        "spec.docx",
        "photo.png",
        "notes.md",
        "UPPER.PDF",
        "sheet.xlsx",
        "build.log",
        "data.json",
        "config.toml",
        ".exe-notes.txt",
    ] {
        let p = make(allowed, 0o644);
        assert!(safe_to_open_directly(&p), "{allowed} with 0o644 should be allowed");
    }

    #[cfg(unix)]
    for exec_file in ["exec.pdf", "exec.docx", "exec.png", "exec.md", "elf_bin"] {
        let p = make(exec_file, 0o755);
        assert!(!safe_to_open_directly(&p), "0o755 {exec_file} must be blocked on Unix");
    }

    let folder = dir.path().join("folder.pdf");
    std::fs::create_dir(&folder).unwrap();
    assert!(!safe_to_open_directly(&folder));
    assert!(!safe_to_open_directly(&dir.path().join("missing.pdf")));
}

#[test]
fn materialize_snapshot_uses_private_dir_and_rejects_unsafe_names() {
    let dir = tempfile::tempdir().unwrap();
    let versions_root = dir.path().join("versions");
    std::fs::create_dir_all(&versions_root).unwrap();

    for bad_snap in ["", ".", "..", "../secret", "a/b", "a\\b", "snap\0x"] {
        assert!(
            materialize_snapshot_in(
                &versions_root,
                "sess-1",
                Path::new("report.pdf"),
                bad_snap,
                Some(1),
                b"pdf"
            )
            .is_err(),
            "expected error for snapshot {bad_snap:?}"
        );
    }
    assert!(
        materialize_snapshot_in(
            &versions_root,
            "../..",
            Path::new("report.pdf"),
            "3-0123456789abcdef",
            None,
            b"pdf"
        )
        .is_err()
    );

    let preview = materialize_snapshot_in(
        &versions_root,
        "sess-1",
        Path::new("report.pdf"),
        "3-0123456789abcdef",
        Some(2),
        b"%PDF-1.7",
    )
    .unwrap();
    assert_eq!(preview.file_name().and_then(|n| n.to_str()), Some("report.v2.pdf"));
    assert_eq!(std::fs::read(&preview).unwrap(), b"%PDF-1.7");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::symlink_metadata(preview.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::symlink_metadata(&preview).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }
}

#[test]
fn export_markdown_helpers_enforce_md_extension() {
    assert_eq!(sanitize_export_filename("我的会话: a/b*c?"), "我的会话- a-b-c-.md");
    assert_eq!(sanitize_export_filename("   "), "session.md");
    assert_eq!(
        ensure_markdown_extension(PathBuf::from("/tmp/notes")),
        PathBuf::from("/tmp/notes.md")
    );
    assert_eq!(
        ensure_markdown_extension(PathBuf::from("/tmp/evil.sh")),
        PathBuf::from("/tmp/evil.sh.md")
    );
    assert_eq!(
        ensure_markdown_extension(PathBuf::from("/tmp/evil.exe.")),
        PathBuf::from("/tmp/evil.exe.md")
    );
    assert_eq!(
        ensure_markdown_extension(PathBuf::from("/tmp/keep.md")),
        PathBuf::from("/tmp/keep.md")
    );
    assert_eq!(
        ensure_markdown_extension(PathBuf::from("/tmp/KEEP.MD")),
        PathBuf::from("/tmp/KEEP.MD")
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn inline_shell_sandbox_confines_outside_writes_and_protects_git_hooks_on_macos() {
    if odg_core::sandbox::support().is_err() {
        return;
    }
    let scratch_parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/inline-sandbox-test");
    std::fs::create_dir_all(&scratch_parent).unwrap();
    let dir = tempfile::tempdir_in(&scratch_parent).unwrap();
    let ws = dir.path().join("ws");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(&ws)
            .args(["init", "--quiet", "--initial-branch", "main"])
            .status()
            .unwrap()
            .success()
    );

    let ok_res = run_inline_shell_with_sandbox("echo ok > inside.txt", &ws, Some(Vec::new()), 10, 8192)
        .await
        .unwrap();
    assert_eq!(ok_res.exit_code, Some(0), "{}", ok_res.output);
    assert!(ws.join("inside.txt").is_file());

    let out_target = outside.join("escaped.txt");
    let bad_out = run_inline_shell_with_sandbox(
        &format!("echo pwn > '{}'", out_target.display()),
        &ws,
        Some(Vec::new()),
        10,
        8192,
    )
    .await
    .unwrap();
    assert_ne!(bad_out.exit_code, Some(0));
    assert!(!out_target.exists());

    #[cfg(target_os = "macos")]
    for attempt in [
        "mkdir -p .git/hooks && echo pwn > .git/hooks/pre-commit",
        "echo '[core]' >> .git/config",
        "echo lock > .git/config.lock",
        "mkdir -p .odg && echo pwn > .odg/settings.json",
    ] {
        let denied = run_inline_shell_with_sandbox(attempt, &ws, Some(Vec::new()), 10, 8192)
            .await
            .unwrap();
        assert_ne!(denied.exit_code, Some(0), "`{attempt}` should be denied by Seatbelt");
    }
}

#[test]
fn strip_inline_ansi_removes_csi_and_osc_escape_sequences_cleanly() {
    let raw = b"\x1b[32mgreen\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x1b\\ plain\r\n";
    assert_eq!(strip_inline_ansi(raw), "green link plain\n");
}

#[test]
fn invoke_handler_registers_all_102_frontend_api_commands() {
    let api_ts = include_str!("../../../src/lib/api.ts");
    let mod_rs = include_str!("mod.rs");

    let mut frontend_cmds = HashSet::new();
    let mut rest = api_ts;
    while let Some(pos) = rest.find("invoke") {
        rest = &rest[pos + 6..];
        let trimmed = rest.trim_start();
        let after_generic = if trimmed.starts_with('<') {
            let mut depth = 0usize;
            let mut end_idx = None;
            for (i, ch) in trimmed.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end_idx = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end_idx {
                Some(idx) => trimmed[idx..].trim_start(),
                None => continue,
            }
        } else {
            trimmed
        };
        if let Some(inside) = after_generic.strip_prefix("(\"")
            && let Some(end_quote) = inside.find('"')
        {
            frontend_cmds.insert(inside[..end_quote].to_string());
        }
    }

    assert_eq!(
        frontend_cmds.len(),
        115,
        "expected 115 invoke commands in api.ts, got: {frontend_cmds:?}"
    );
    assert!(frontend_cmds.contains("daemon_status"));
    assert!(frontend_cmds.contains("install_daemon"));
    assert!(frontend_cmds.contains("uninstall_daemon"));
    assert!(frontend_cmds.contains("get_usage_ledger_summary"));
    assert!(frontend_cmds.contains("get_price_catalog"));
    assert!(frontend_cmds.contains("sync_price_catalog"));
    assert!(frontend_cmds.contains("set_custom_price"));
    assert!(frontend_cmds.contains("delete_custom_price"));
    assert!(frontend_cmds.contains("get_session_item"));
    assert!(frontend_cmds.contains("remove_pasted_file"));
    assert!(frontend_cmds.contains("install_app_update"));
    assert!(frontend_cmds.contains("install_cli_link"));
    assert!(frontend_cmds.contains("get_kimi_quota"));
    assert!(frontend_cmds.contains("kimi_oauth_start"));
    assert!(frontend_cmds.contains("kimi_oauth_poll"));
    assert!(frontend_cmds.contains("kimi_oauth_cancel"));
    assert!(frontend_cmds.contains("kimi_oauth_logout"));

    let mut backend_cmds = HashSet::new();
    for line in mod_rs.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        if let Some((_, fn_name)) = trimmed.rsplit_once("::")
            && fn_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            backend_cmds.insert(fn_name.to_string());
        }
    }

    for cmd in &frontend_cmds {
        assert!(
            backend_cmds.contains(cmd),
            "frontend command `{cmd}` missing from commands/mod.rs generate_handler!"
        );
    }
    assert_eq!(frontend_cmds, backend_cmds);
}

#[tokio::test]
async fn app_core_create_get_session_item_list_and_summary_cache_work_end_to_end() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();
    init_git_repo(&ws);
    std::fs::write(ws.join("AGENTS.md"), "# Untrusted Rules\n").unwrap();

    let s1 = core.start(ws.clone(), false, None).unwrap();
    let s2 = core.start(ws.clone(), false, None).unwrap();
    s2.rename("Second Session").unwrap();

    let item1 = core.get_session_item_blocking(s1.id()).unwrap();
    assert_eq!(item1.branch.as_deref(), Some("main"));
    assert!(!item1.project_trusted);
    assert!(item1.untrusted_resources.iter().any(|r| r.contains("AGENTS.md")));

    // Unload `s1` from memory so `list_sessions_blocking` exercises `SessionStore::peek_idle` cache.
    let s1_id = s1.id().to_string();
    drop(s1);
    core.set_active_session(Some(s2.id()));
    let listed1 = core.list_sessions_blocking();
    let listed2 = core.list_sessions_blocking();
    assert_eq!(listed1.len(), 2);
    assert_eq!(listed2.len(), 2);
    let cold_item = core.get_session_item_blocking(&s1_id).unwrap();
    assert_eq!(cold_item.status, Status::Idle);
    assert!(!cold_item.summary.turn_open);
}

#[tokio::test]
async fn app_core_start_chat_creates_trusted_conversation_folder_session() {
    let (_tmp, _ws, _llm, core, _rx) = setup_core();
    let chat_dir = core.chat_folder();
    std::fs::create_dir_all(&chat_dir).unwrap();
    let chat = core.start(chat_dir.clone(), false, None).unwrap();
    let view = core.view(&chat);
    assert!(
        view.item.project_trusted,
        "dedicated chat folder must always be trusted"
    );
    assert!(view.item.untrusted_resources.is_empty());
}

#[tokio::test]
async fn app_core_rename_mode_effort_and_model_switching_persist_to_log() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();
    let session = core.start(ws, false, None).unwrap();
    let id = session.id().to_string();

    session.rename("  水墨重构测试  ").unwrap();
    assert_eq!(session.summary().title, "水墨重构测试");

    session.set_mode(Mode::Plan);
    assert_eq!(session.mode(), Mode::Plan);

    session.set_effort("high").unwrap();
    assert_eq!(session.summary().effort.as_deref(), Some("high"));

    session.set_model("openai", "gpt-5.4").unwrap();
    let item = core.get_session_item_blocking(&id).unwrap();
    assert_eq!(item.summary.provider, "openai");
    assert_eq!(item.summary.model, "gpt-5.4");
}

#[tokio::test]
async fn app_core_worktree_session_allows_bypass_mode_and_delete_cleans_worktree() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();
    init_git_repo(&ws);

    let wt_session = core.start(ws.clone(), true, None).unwrap();
    let wt_id = wt_session.id().to_string();
    let checkout_path = PathBuf::from(&wt_session.summary().worktree.unwrap().checkout);
    assert!(checkout_path.is_dir());

    wt_session.set_mode(Mode::Bypass);
    assert_eq!(wt_session.mode(), Mode::Bypass);
    drop(wt_session);

    core.delete_session_blocking(&wt_id).unwrap();
    assert!(
        !checkout_path.exists(),
        "worktree checkout must be removed when its sole session is deleted"
    );
    assert!(core.store.peek_idle(&wt_id).is_err());
}

#[tokio::test]
async fn app_core_fork_session_copies_events_and_preserves_shared_worktree_until_last_deleted() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    init_git_repo(&ws);
    llm.push_text("Turn 1 answer");
    llm.push_text("Turn 2 answer");

    let origin = core.start(ws.clone(), true, None).unwrap();
    let origin_id = origin.id().to_string();
    let checkout_path = PathBuf::from(&origin.summary().worktree.unwrap().checkout);

    send_message_core(&core, &origin_id, "first prompt".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    origin.wait_idle().await;
    send_message_core(&core, &origin_id, "second prompt".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    origin.wait_idle().await;

    // Fork before turn 2 -> keeps turn 1, records abandoned_brief from turn 2.
    let forked = core.fork_from(&origin, Some(2)).unwrap();
    let fork_id = forked.item.summary.id.clone();
    assert_eq!(
        forked.item.summary.fork.as_ref().map(|f| f.session.as_str()),
        Some(origin_id.as_str())
    );
    assert!(forked.abandoned_brief.is_some());
    drop(origin);

    // Deleting `origin` while `fork` still shares the same worktree MUST keep `checkout_path` alive!
    core.delete_session_blocking(&origin_id).unwrap();
    assert!(
        checkout_path.is_dir(),
        "shared worktree must survive while forked session still exists"
    );

    // Deleting `fork` (now the last owner) removes `checkout_path`.
    core.delete_session_blocking(&fork_id).unwrap();
    assert!(!checkout_path.exists());
}

#[tokio::test]
async fn app_core_aside_lifecycle_excludes_aside_from_list_and_search_and_cascades_on_delete() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    llm.push_text("secret_aside_keyword");

    let main = core.start(ws, false, None).unwrap();
    let main_id = main.id().to_string();
    let aside = core.start_aside_for(&main).unwrap();
    let aside_id = aside.id().to_string();

    // Aside cannot itself spawn a nested aside.
    assert!(core.start_aside_for(&aside).is_err());
    assert_eq!(core.find_aside(&main_id).as_deref(), Some(aside_id.as_str()));

    send_message_core(&core, &aside_id, "ask aside".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    aside.wait_idle().await;

    // Aside must never appear in `list_sessions_blocking` or `search_sessions_blocking`.
    let listed = core.list_sessions_blocking();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].summary.id, main_id);
    assert!(core.search_sessions_blocking("secret_aside_keyword").is_empty());

    // Resetting aside removes it cleanly; creating a second aside and deleting main cascades to it.
    drop(aside);
    core.reset_aside_blocking(&main_id).unwrap();
    assert!(core.find_aside(&main_id).is_none());

    let aside2 = core.start_aside_for(&main).unwrap();
    let aside2_id = aside2.id().to_string();
    drop(aside2);
    drop(main);

    core.delete_session_blocking(&main_id).unwrap();
    assert!(
        core.store.peek_idle(&aside2_id).is_err(),
        "deleting main session must cascade-delete its aside"
    );
}

#[tokio::test]
async fn app_core_delete_session_cleans_materialized_version_previews() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();
    let session = core.start(ws, false, None).unwrap();
    let id = session.id().to_string();
    drop(session);

    let versions_root = core.versions_dir();
    std::fs::create_dir_all(&versions_root).unwrap();
    let preview = materialize_snapshot_in(
        &versions_root,
        &id,
        Path::new("draft.md"),
        "1-0123456789abcdef",
        Some(1),
        b"# Draft",
    )
    .unwrap();
    assert!(preview.is_file());

    let session_ver_dir = versions_root.join(safe_session_id(&id));
    assert!(session_ver_dir.is_dir());

    core.delete_session_blocking(&id).unwrap();
    assert!(
        !session_ver_dir.exists(),
        "delete_session must clean up <home>/tmp/versions/<safe_session_id>"
    );
}

#[tokio::test]
async fn app_core_refuses_delete_or_aside_reset_while_turn_is_running() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    let gate = CancellationToken::new();
    llm.push_held_text(gate.clone(), "finished");

    let session = core.start(ws, false, None).unwrap();
    let id = session.id().to_string();
    send_message_core(&core, &id, "hold turn".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();

    assert!(
        core.delete_session_blocking(&id).is_err(),
        "must refuse deleting running session"
    );

    gate.cancel();
    session.wait_idle().await;
    drop(session);
    core.delete_session_blocking(&id).unwrap();
}

#[tokio::test]
async fn app_core_evicts_least_recently_used_idle_sessions_above_24_while_keeping_active_and_busy() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    let gate = CancellationToken::new();
    llm.push_held_text(gate.clone(), "busy done");

    // s0 is busy (Running) -> must NEVER be evicted even as oldest!
    let s0_busy = core.start(ws.clone(), false, None).unwrap();
    let busy_id = s0_busy.id().to_string();
    send_message_core(&core, &busy_id, "stay running".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();

    let mut idle_ids = Vec::new();
    for _ in 0..28 {
        let s = core.start(ws.clone(), false, None).unwrap();
        core.set_active_session(Some(s.id()));
        idle_ids.push(s.id().to_string());
    }

    // Touch `idle_ids[0]` before creating one more session so `idle_ids[1]` is the oldest idle candidate.
    let touched_id = idle_ids[0].clone();
    let _ = core.live(&touched_id).await.unwrap();
    let newest = core.start(ws.clone(), false, None).unwrap();
    let newest_id = newest.id().to_string();
    core.set_active_session(Some(&newest_id));
    core.evict_idle_lru();

    let loaded = core.loaded_session_ids();
    // 24 idle sessions + 1 busy session = 25 loaded total.
    assert_eq!(
        loaded.len(),
        MAX_IDLE_SESSIONS + 1,
        "expected 24 idle + 1 busy loaded, got {}",
        loaded.len()
    );
    assert!(loaded.contains(&busy_id), "busy session must not be evicted");
    assert!(loaded.contains(&newest_id), "active session must not be evicted");
    assert!(
        loaded.contains(&touched_id),
        "recently accessed session must stay cached"
    );
    assert!(
        !loaded.contains(&idle_ids[1]),
        "oldest untouched idle session must be evicted"
    );

    // Reopening an evicted session from disk works seamlessly.
    let reopened = core.live(&idle_ids[1]).await.unwrap();
    assert_eq!(reopened.id(), idle_ids[1]);

    gate.cancel();
    s0_busy.wait_idle().await;
}

#[tokio::test]
async fn app_core_concurrent_once_cell_live_loads_repair_open_turn_without_duplicate_writers() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();

    // Create a cold session log on disk with an unclosed crashed turn + unanswered tool_use.
    let crashed_id = {
        let mut log = core.store.create(&ws, "anthropic", "claude-sonnet-4-6").unwrap();
        log.append(Event::TurnStart { turn: 1 }).unwrap();
        log.append(Event::AssistantMessage {
            content: vec![
                json!({ "type": "tool_use", "id": "tu_crash", "name": "read_file", "input": { "path": "a" } }),
            ],
            stop_reason: Some("tool_use".to_string()),
            model: "claude-sonnet-4-6".to_string(),
            usage: Value::Null,
            tokens: None,
        })
        .unwrap();
        log.id().to_string()
    };

    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = core.clone();
        let id = crashed_id.clone();
        handles.push(tokio::spawn(async move { c.live(&id).await.unwrap() }));
    }
    let mut opened = Vec::new();
    for h in handles {
        opened.push(h.await.unwrap());
    }
    for s in &opened {
        assert_eq!(s.status(), Status::Idle);
        let events = s.snapshot();
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(Event::TurnEnd { reason, .. }) if reason == "interrupted"
        ));
    }
}

#[tokio::test]
async fn app_core_search_sessions_matches_live_and_stored_conversations() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    llm.push_text("灵石缓存命中率优化完毕");

    let s1 = core.start(ws.clone(), false, None).unwrap();
    let s1_id = s1.id().to_string();
    send_message_core(&core, &s1_id, "检查性能瓶颈".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    s1.wait_idle().await;

    let s2 = core.start(ws, false, None).unwrap();
    s2.rename("无关会话").unwrap();

    let hits = core.search_sessions_blocking("灵石缓存");
    assert_eq!(hits, vec![s1_id]);
}

#[tokio::test]
async fn app_core_export_session_markdown_renders_turns_for_live_and_cold_sessions() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    llm.push_text("导出内容正文。");

    let s = core.start(ws, false, None).unwrap();
    let id = s.id().to_string();
    s.rename("架构评审纪要").unwrap();
    send_message_core(&core, &id, "请总结阶段二方案".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    s.wait_idle().await;

    let (md_live, title) = export_session_markdown_with_title(&core, &id).unwrap();
    assert_eq!(title, "架构评审纪要");
    assert!(md_live.contains("请总结阶段二方案") && md_live.contains("导出内容正文。"));
}

#[tokio::test]
async fn open_attachment_and_picture_enforce_zero_copy_session_file_ownership() {
    let (tmp, ws, llm, core, _rx) = setup_core();
    let unrelated = tmp.path().join("secret.md");
    std::fs::write(&unrelated, "top secret").unwrap();
    let unrelated_png = tmp.path().join("secret.png");
    std::fs::write(&unrelated_png, b"\x89PNG\r\n\x1a\n").unwrap();

    let attached_doc = ws.join("notes.md");
    std::fs::write(&attached_doc, "# Notes\n").unwrap();
    let script_file = ws.join("deploy.sh");

    llm.push_tool(
        "tu_w1",
        "write_file",
        json!({ "path": "deploy.sh", "content": "#!/bin/sh\necho hi\n" }),
    );
    llm.push_tool(
        "tu_w2",
        "write_file",
        json!({ "path": "chart.png", "content": "fake-png-bytes" }),
    );
    llm.push_text("Wrote files.");

    let session = core.start(ws.clone(), false, None).unwrap();
    session.set_mode(Mode::Edit);
    let id = session.id().to_string();
    send_message_core(
        &core,
        &id,
        "read notes and write outputs".to_string(),
        Vec::new(),
        vec![attached_doc.display().to_string()],
    )
    .await
    .unwrap();
    session.wait_idle().await;

    // Unrelated files outside session history MUST be rejected by `has_session_file`!
    assert!(
        open_attachment_blocking(
            &core,
            &session,
            &unrelated.display().to_string(),
            false,
            None,
            None,
            true
        )
        .is_err()
    );
    assert!(picture_blocking(&session, &unrelated_png.display().to_string(), None).is_err());

    // Attached safe `.md` opens directly; written `.sh` script is revealed instead of executed; written `.png` loads as base64.
    assert_eq!(
        open_attachment_blocking(
            &core,
            &session,
            &attached_doc.display().to_string(),
            false,
            None,
            None,
            true
        )
        .unwrap(),
        "opened"
    );
    assert_eq!(
        open_attachment_blocking(
            &core,
            &session,
            &script_file.display().to_string(),
            false,
            None,
            None,
            true
        )
        .unwrap(),
        "revealed"
    );
    let img = picture_blocking(&session, &ws.join("chart.png").display().to_string(), None).unwrap();
    assert_eq!(img.media_type, "image/png");
    assert!(!img.data.is_empty());
}

#[tokio::test]
async fn open_attachment_materializes_historical_snapshot_and_reveals_unsafe_extensions() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    llm.push_tool(
        "tu_snap",
        "write_file",
        json!({ "path": "report.md", "content": "# Version One\n" }),
    );
    llm.push_text("Done v1.");

    let session = core.start(ws.clone(), false, None).unwrap();
    session.set_mode(Mode::Edit);
    let id = session.id().to_string();
    send_message_core(&core, &id, "write report".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    session.wait_idle().await;

    let report_path = ws.join("report.md").display().to_string();
    let snap_name = session
        .snapshot()
        .into_iter()
        .find_map(|env| match env.event {
            Event::ToolResult { meta, .. } => meta["snapshot"].as_str().map(str::to_string),
            _ => None,
        })
        .expect("write_file snapshot recorded");

    // Overwrite live file on disk to v2, then open historical v1 snapshot!
    std::fs::write(ws.join("report.md"), "# Version Two\n").unwrap();
    let action =
        open_attachment_blocking(&core, &session, &report_path, false, Some(&snap_name), Some(1), true).unwrap();
    assert_eq!(action, "opened");

    let preview_file = core
        .versions_dir()
        .join(safe_session_id(&id))
        .join(&snap_name)
        .join("report.v1.md");
    assert_eq!(std::fs::read_to_string(&preview_file).unwrap(), "# Version One\n");
}

#[test]
fn save_pasted_file_sanitizes_names_and_enforces_10mb_limit() {
    let dir = tempfile::tempdir().unwrap();
    let pastes = dir.path().join("pastes");

    let picked = save_pasted_file_in(&pastes, "../weird name?.svg", "<svg></svg>").unwrap();
    let path = match picked {
        odg_core::events::Picked::File { path, name, .. } => {
            assert_eq!(name, "_weird_name_.svg");
            PathBuf::from(path)
        }
        other => panic!("expected Picked::File, got {other:?}"),
    };
    assert!(path.starts_with(&pastes));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "<svg></svg>");

    let too_big = "a".repeat(10 * 1024 * 1024 + 1);
    assert!(save_pasted_file_in(&pastes, "huge.txt", &too_big).is_err());
}

#[test]
fn remove_pasted_file_strictly_confines_to_pastes_dir_and_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let pastes = dir.path().join("pastes");
    let outside = dir.path().join("important.txt");
    std::fs::write(&outside, "keep me").unwrap();

    // Even before `pastes` directory exists, deleting outside file MUST fail!
    assert!(remove_pasted_file_in(&pastes, &outside.display().to_string()).is_err());
    assert!(outside.is_file());

    let picked = save_pasted_file_in(&pastes, "snippet.txt", "hello paste").unwrap();
    let paste_path = match picked {
        odg_core::events::Picked::File { path, .. } => PathBuf::from(path),
        other => panic!("unexpected {other:?}"),
    };
    let stamp_dir = paste_path.parent().unwrap().to_path_buf();

    // Reject `..` traversal, root directory itself, and outside files.
    let traversal = format!("{}/../../important.txt", stamp_dir.display());
    assert!(remove_pasted_file_in(&pastes, &traversal).is_err());
    assert!(remove_pasted_file_in(&pastes, &pastes.display().to_string()).is_err());
    assert!(remove_pasted_file_in(&pastes, &outside.display().to_string()).is_err());
    assert!(outside.is_file());

    // Valid paste file is deleted along with its now-empty `<stamp-seq>` parent folder; second call is idempotent.
    remove_pasted_file_in(&pastes, &paste_path.display().to_string()).unwrap();
    assert!(!paste_path.exists());
    assert!(!stamp_dir.exists());
    remove_pasted_file_in(&pastes, &paste_path.display().to_string()).unwrap();
}

#[test]
fn prune_stale_pastes_sweeps_entries_older_than_7_days_and_keeps_fresh_ones() {
    let dir = tempfile::tempdir().unwrap();
    let pastes = dir.path().join("pastes");

    let old_picked = save_pasted_file_in(&pastes, "old.txt", "stale").unwrap();
    let old_path = match old_picked {
        odg_core::events::Picked::File { path, .. } => PathBuf::from(path),
        _ => unreachable!(),
    };

    // With 7-day retention, newly created paste is kept intact.
    prune_stale_pastes_in(&pastes, Duration::from_secs(7 * 86_400));
    assert!(old_path.is_file());

    // Wait 30ms and sweep with a 10ms max_age -> stale paste directory is removed!
    std::thread::sleep(Duration::from_millis(30));
    prune_stale_pastes_in(&pastes, Duration::from_millis(10));
    assert!(!old_path.exists());
    assert!(!old_path.parent().unwrap().exists());
}

#[tokio::test]
async fn session_git_workspace_isolates_untrusted_hooks_and_blocks_root_worktree_checkout() {
    let (tmp, ws, _llm, core, _rx) = setup_core();
    init_git_repo(&ws);

    // Plant a malicious pre-commit hook in the workspace repo.
    let marker = tmp.path().join("hook-ran");
    let hooks_dir = ws.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook = hooks_dir.join("pre-commit");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho pwn > \"{}\"\nexit 1\n",
            marker.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let session = core.start(ws.clone(), false, None).unwrap();
    let id = session.id().to_string();
    let (_, repo_dir, trusted) = session_git_workspace(&core, &id, None).await.unwrap();
    assert!(!trusted, "new workspace must be untrusted by default");

    std::fs::write(ws.join("README.md"), "# Updated safely\n").unwrap();
    odg_core::git::commit_with_trust(&repo_dir, "safe commit", true, trusted).unwrap();
    assert!(
        !marker.exists(),
        "untrusted session git commit must never execute .git/hooks/pre-commit"
    );

    // Worktree session: root checkout is blocked, while child sub-repo checkout is allowed!
    let wt_session = core.start(ws.clone(), true, None).unwrap();
    let wt_id = wt_session.id().to_string();
    assert!(
        git_checkout_core(&core, &wt_id, "other-branch".to_string(), true, None)
            .await
            .is_err()
    );

    let wt_root = PathBuf::from(wt_session.summary().workspace);
    let sub_repo = wt_root.join("subpkg");
    init_git_repo(&sub_repo);
    let switched = git_checkout_core(
        &core,
        &wt_id,
        "sub-feature".to_string(),
        true,
        Some("subpkg".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(switched.summary.id, wt_id);
    assert_eq!(
        odg_core::git::status(&sub_repo).unwrap().branch.as_deref(),
        Some("sub-feature")
    );
}

#[tokio::test]
async fn settings_view_exposes_static_package_version_and_manages_providers_keys_and_grants() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();

    let initial = core.settings_view();
    assert_eq!(initial.version, env!("CARGO_PKG_VERSION"));
    assert!(!initial.version.is_empty());

    core.settings.save_api_key("openai", "sk-primary-1234567890").unwrap();
    let added_id = core.settings.add_provider("openai", "Internal Gateway").unwrap();
    core.settings.save_api_key(&added_id, "sk-test-1234567890").unwrap();
    core.grants.add(&ws, "command:cargo test").unwrap();

    let updated = core.settings_view();
    assert!(
        updated
            .providers
            .iter()
            .any(|p| p.id == added_id && p.name == "Internal Gateway")
    );
    assert_eq!(updated.keys.get(&added_id).map(|k| k.source.as_str()), Some("saved"));
    assert!(updated.grants.contains_key(ws.to_string_lossy().as_ref()));

    core.grants.remove(&ws.to_string_lossy(), "command:cargo test").unwrap();
    core.settings.delete_api_key(&added_id).unwrap();
    core.settings.remove_provider(&added_id).unwrap();
    assert!(!core.settings_view().providers.iter().any(|p| p.id == added_id));
}

#[tokio::test]
async fn prompt_templates_withhold_project_prompts_until_workspace_is_trusted() {
    let (_tmp, ws, _llm, core, _rx) = setup_core();
    let user_prompts = core.home.join("prompts");
    let proj_prompts = ws.join(".odg/prompts");
    std::fs::create_dir_all(&user_prompts).unwrap();
    std::fs::create_dir_all(&proj_prompts).unwrap();
    std::fs::write(
        user_prompts.join("global.md"),
        "---\ndescription: Global\n---\nGlobal: $ARGUMENTS",
    )
    .unwrap();
    std::fs::write(
        proj_prompts.join("local.md"),
        "---\ndescription: Local\n---\nLocal: $ARGUMENTS",
    )
    .unwrap();

    let session = core.start(ws, false, None).unwrap();
    let id = session.id().to_string();

    let (u1, p1) = core.prompt_roots_for_session(Some(&id));
    assert!(p1.is_none(), "untrusted workspace must withhold .odg/prompts");
    assert_eq!(odg_core::templates::listing(&u1, p1.as_deref()).len(), 1);

    session.set_project_trust(true).unwrap();
    let (u2, p2) = core.prompt_roots_for_session(Some(&id));
    assert!(p2.is_some());
    assert_eq!(odg_core::templates::listing(&u2, p2.as_deref()).len(), 2);
    assert_eq!(
        odg_core::templates::expand_named(&u2, p2.as_deref(), "local", "check").unwrap(),
        "Local: check"
    );
}

#[tokio::test]
async fn schedules_save_list_fire_and_skip_when_previous_run_is_still_busy() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    let gate = CancellationToken::new();
    llm.push_held_text(gate.clone(), "scheduled task done");

    let sched = Schedule {
        id: String::new(),
        name: "每日巡检".to_string(),
        workspace: ws.display().to_string(),
        prompt: "run daily check".to_string(),
        when: When::Every { minutes: 60 },
        mode: Mode::Plan,
        isolated: false,
        enabled: true,
        next: None,
        last: None,
    };
    core.schedules.save(sched, &odg_core::schedule::now()).unwrap();
    let saved = core.schedules.all().into_iter().next().unwrap();

    let first_run = core.fire(&saved);
    assert!(first_run.error.is_none());
    let started_sid = first_run.session.clone().unwrap();
    core.schedules.record(&saved.id, first_run).unwrap();

    // Firing again while `started_sid` is still running must pass over without starting a duplicate!
    let updated = core.schedules.all().into_iter().next().unwrap();
    let second_run = core.fire(&updated);
    assert!(second_run.error.as_deref().is_some_and(|e| e.contains("still working")));

    gate.cancel();
    core.live(&started_sid).await.unwrap().wait_idle().await;
    core.schedules.remove(&saved.id).unwrap();
    assert!(core.schedules.all().is_empty());
}

#[tokio::test]
async fn model_usage_store_flushes_pending_debounced_writes_on_sync_flush() {
    let (_tmp, _ws, _llm, core, _rx) = setup_core();
    core.model_usage.record_usage(
        "anthropic",
        "claude-sonnet-4-6",
        &Tokens {
            input: 1000,
            cached: 400,
            cache_write: 0,
            output: 250,
        },
    );
    core.model_usage.flush_sync();

    let usage_file = core.home.join("model-usage.json");
    assert!(
        usage_file.is_file(),
        "flush_sync must persist model-usage.json immediately"
    );
    let reopened = odg_core::model_usage::ModelUsageStore::at(&usage_file);
    let entries = reopened.list();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input_tokens, 1000);
    assert_eq!(entries[0].cached_tokens, 400);
    assert_eq!(entries[0].output_tokens, 250);
}

#[tokio::test]
async fn stream_event_forwarder_coalesces_12ms_text_and_thinking_deltas_by_session_and_index() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let emitted: Arc<Mutex<Vec<UiEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = emitted.clone();

    let task = tokio::spawn(forward_session_events(rx, move |ev| {
        sink.lock().unwrap().push(ev);
    }));

    for i in 0..80 {
        tx.send(UiEvent::Stream {
            session_id: "s1".to_string(),
            delta: StreamDelta::ThinkingDelta {
                index: 0,
                text: format!("t{i},"),
            },
        })
        .unwrap();
    }
    for i in 0..120 {
        tx.send(UiEvent::Stream {
            session_id: "s1".to_string(),
            delta: StreamDelta::TextDelta {
                index: 1,
                text: format!("w{i} "),
            },
        })
        .unwrap();
    }
    drop(tx);
    task.await.unwrap();

    let out = emitted.lock().unwrap();
    // 200 raw deltas coalesced down to a tiny handful of batched emissions (typically 2: 1 thinking + 1 text)!
    assert!(
        out.len() <= 6,
        "expected 200 deltas to coalesce within 12ms window, got {} emits",
        out.len()
    );

    let mut thinking = String::new();
    let mut text = String::new();
    for ev in out.iter() {
        match ev {
            UiEvent::Stream {
                delta: StreamDelta::ThinkingDelta { index: 0, text: s },
                ..
            } => thinking.push_str(s),
            UiEvent::Stream {
                delta: StreamDelta::TextDelta { index: 1, text: s },
                ..
            } => text.push_str(s),
            other => panic!("unexpected event {other:?}"),
        }
    }
    let expected_thinking: String = (0..80).map(|i| format!("t{i},")).collect();
    let expected_text: String = (0..120).map(|i| format!("w{i} ")).collect();
    assert_eq!(thinking, expected_thinking);
    assert_eq!(text, expected_text);
}

#[tokio::test]
async fn stream_event_forwarder_preserves_strict_order_across_reset_tool_start_log_and_status() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let emitted: Arc<Mutex<Vec<UiEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = emitted.clone();

    let task = tokio::spawn(forward_session_events(rx, move |ev| {
        sink.lock().unwrap().push(ev);
    }));

    tx.send(UiEvent::Status {
        session_id: "s1".to_string(),
        status: Status::Running,
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::TextDelta {
            index: 0,
            text: "hel".to_string(),
        },
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::TextDelta {
            index: 0,
            text: "lo".to_string(),
        },
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::Reset,
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::TextDelta {
            index: 0,
            text: "fresh ".to_string(),
        },
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::TextDelta {
            index: 0,
            text: "start".to_string(),
        },
    })
    .unwrap();
    tx.send(UiEvent::Stream {
        session_id: "s1".to_string(),
        delta: StreamDelta::ToolUseStart {
            index: 1,
            id: "call_1".to_string(),
            name: "read_file".to_string(),
        },
    })
    .unwrap();
    tx.send(UiEvent::Log {
        session_id: "s1".to_string(),
        envelope: Envelope {
            seq: 2,
            ts: "2026-11-26T00:00:00Z".to_string(),
            event: Event::TurnEnd {
                turn: 1,
                reason: "completed".to_string(),
                error: None,
            },
        },
    })
    .unwrap();
    tx.send(UiEvent::Status {
        session_id: "s1".to_string(),
        status: Status::Idle,
    })
    .unwrap();
    drop(tx);
    task.await.unwrap();

    let out = emitted.lock().unwrap();
    let tags: Vec<String> = out
        .iter()
        .map(|ev| match ev {
            UiEvent::Status { status, .. } => format!("status:{status:?}"),
            UiEvent::Stream {
                delta: StreamDelta::TextDelta { text, .. },
                ..
            } => format!("text:{text}"),
            UiEvent::Stream {
                delta: StreamDelta::Reset,
                ..
            } => "reset".to_string(),
            UiEvent::Stream {
                delta: StreamDelta::ToolUseStart { name, .. },
                ..
            } => format!("tool:{name}"),
            UiEvent::Log { .. } => "log:turn_end".to_string(),
            other => format!("other:{other:?}"),
        })
        .collect();

    assert_eq!(
        tags,
        vec![
            "status:Running",
            "text:hello",
            "reset",
            "text:fresh start",
            "tool:read_file",
            "log:turn_end",
            "status:Idle",
        ]
    );
}

#[test]
fn structured_ipc_error_codes_align_with_frontend_parser_and_skill_codes() {
    let plain = coded(ERR_SKILL_EXISTS, "skill already installed");
    assert_eq!(plain, "[ERR_SKILL_EXISTS] skill already installed");
    // Must not double-wrap already-prefixed `odg_core::skills` errors.
    assert_eq!(coded(ERR_SKILL_EXISTS, &plain), plain);

    let parsed_plain = parse_coded(&plain).expect("parse plain coded error");
    assert_eq!(parsed_plain, IpcError::new(ERR_SKILL_EXISTS, "skill already installed"));

    let parameterized = coded_with_params(
        ERR_SKILL_SUBFOLDERS,
        [("choices", "alpha, beta")],
        "choose a skill subfolder",
    );
    assert_eq!(
        parameterized,
        "[ERR_SKILL_SUBFOLDERS] choices=alpha, beta | choose a skill subfolder"
    );
    let parsed_params = parse_coded(&parameterized).expect("parse parameterized error");
    assert_eq!(parsed_params.code, ERR_SKILL_SUBFOLDERS);
    assert_eq!(
        parsed_params.params.get("choices").map(String::as_str),
        Some("alpha, beta")
    );
    assert_eq!(parsed_params.message, "choose a skill subfolder");
}

#[tokio::test]
async fn settings_persist_locale_and_auto_update_runtime_and_listed_models_expose_free_flag() {
    let (_tmp, _ws, _llm, core, _rx) = setup_core();

    // Default `auto_update_runtime` MUST be false so startup never silently downloads runtime updates.
    let initial = core.settings_view();
    assert_eq!(initial.settings.locale, "zh-CN");
    assert!(!initial.settings.auto_update_runtime);
    assert_eq!(INLINE_COMMAND_TIMEOUT_SECS, 30);

    let stale = odg_core::runtime::Status::Stale {
        python: PathBuf::from("/tmp/python"),
        version: "3.12".to_string(),
    };
    let ready = odg_core::runtime::Status::Ready {
        python: PathBuf::from("/tmp/python"),
        version: "3.12".to_string(),
    };
    assert!(!should_auto_update_runtime(&initial.settings, &stale));

    let mut next = initial.settings.clone();
    next.locale = "en".to_string();
    next.auto_update_runtime = true;
    core.settings.save(next).unwrap();

    let reloaded = core.settings_view();
    assert_eq!(reloaded.settings.locale, "en");
    assert!(reloaded.settings.auto_update_runtime);
    assert!(should_auto_update_runtime(&reloaded.settings, &stale));
    assert!(!should_auto_update_runtime(&reloaded.settings, &ready));

    // Verify `odg_core::providers::Listed` DTO returned by `list_models` serializes `free: bool` cleanly.
    let listed = odg_core::providers::Listed {
        id: "openrouter/free-coder:free".to_string(),
        name: "Free Coder".to_string(),
        context_window: Some(128_000),
        free: true,
    };
    let wire = serde_json::to_value(&listed).unwrap();
    assert_eq!(wire["id"], "openrouter/free-coder:free");
    assert_eq!(wire["name"], "Free Coder");
    assert_eq!(wire["context_window"], 128_000);
    assert_eq!(wire["free"], true);
}

#[tokio::test]
async fn land_session_refuses_when_forked_sibling_sharing_worktree_is_busy_and_succeeds_once_idle() {
    let (_tmp, ws, llm, core, _rx) = setup_core();
    init_git_repo(&ws);

    let origin = core.start(ws.clone(), true, None).unwrap();
    let origin_id = origin.id().to_string();
    origin.rename("feature work").unwrap();
    let checkout_path = PathBuf::from(&origin.worktree().unwrap().checkout);
    std::fs::write(checkout_path.join("feature.txt"), "shared worktree edit\n").unwrap();

    // Fork a sibling session that shares the exact same worktree checkout directory.
    let forked_view = core.fork_from(&origin, None).unwrap();
    let fork_id = forked_view.item.summary.id;
    let fork_session = core.live(&fork_id).await.unwrap();
    assert_eq!(
        fork_session.worktree().map(|wt| wt.checkout.as_str()),
        origin.worktree().map(|wt| wt.checkout.as_str())
    );

    // Hold a running turn on `fork_session` while `origin` itself is completely idle.
    let gate = CancellationToken::new();
    llm.push_held_text(gate.clone(), "sibling turn finished");
    send_message_core(&core, &fork_id, "keep sibling busy".to_string(), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert!(origin.is_idle_tree());
    assert!(!fork_session.is_idle_tree());

    // Landing `origin` while `fork_session` is running in the shared worktree MUST fail with `[ERR_WORKTREE_BUSY]`!
    let err = land_session_core(&core, &origin_id)
        .await
        .expect_err("must refuse landing while sibling sharing worktree is busy");
    let parsed = parse_coded(&err).expect("expected structured [ERR_WORKTREE_BUSY] error");
    assert_eq!(parsed.code, ERR_WORKTREE_BUSY);

    // Once the sibling finishes its turn and returns to Idle, landing `origin` succeeds cleanly.
    gate.cancel();
    fork_session.wait_idle().await;

    let landed = land_session_core(&core, &origin_id).await.unwrap();
    assert_eq!(landed.commits, 1);
    assert_eq!(landed.into, "main");
    assert_eq!(
        std::fs::read_to_string(ws.join("feature.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "shared worktree edit\n"
    );
}

#[cfg(unix)]
#[test]
fn link_cli_binary_into_creates_and_refreshes_symlink_without_clobbering_regular_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app_dir = tmp.path().join("app");
    let bin_dir = tmp.path().join(".local/bin");
    std::fs::create_dir_all(&app_dir).unwrap();
    let fake_cli = app_dir.join("odg");
    std::fs::write(&fake_cli, "#!/bin/sh\necho odg\n").unwrap();

    let linked = crate::commands::settings_ext::link_cli_binary_into(&fake_cli, &bin_dir).unwrap();
    assert_eq!(linked, bin_dir.join("odg"));
    assert_eq!(std::fs::read_link(&linked).unwrap(), fake_cli);

    // Refreshing an existing symlink points cleanly to an updated binary location.
    let updated_cli = app_dir.join("odg-v2");
    std::fs::write(&updated_cli, "#!/bin/sh\necho odg2\n").unwrap();
    let relinked = crate::commands::settings_ext::link_cli_binary_into(&updated_cli, &bin_dir).unwrap();
    assert_eq!(std::fs::read_link(&relinked).unwrap(), updated_cli);

    // Refuses to overwrite a regular standalone file at `~/.local/bin/odg`.
    std::fs::remove_file(&relinked).unwrap();
    std::fs::write(&relinked, "standalone binary").unwrap();
    assert!(crate::commands::settings_ext::link_cli_binary_into(&updated_cli, &bin_dir).is_err());
}
