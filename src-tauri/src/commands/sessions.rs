//! Session lifecycle commands: listing, single-item lookup, creation, chat, fork,
//! markdown export, worktree landing, deletion, search, side conversations, and turns.

use std::path::PathBuf;
use std::sync::Arc;

use odg_core::events::{Decision, FILES_PER_MESSAGE, Image, Reply, attach, check_images};
use odg_core::llm::Llm;
use odg_core::policy::Mode;
use odg_core::providers::vision;
use odg_core::worktree::{self, Landed};
use serde::Serialize;
use tauri::{Manager, State};

use crate::state::{App, AppCore, SessionItem, SessionView};

/// What an undo did, for the window to report.
#[derive(Serialize)]
pub struct Undone {
    pub restored: Vec<String>,
    pub skipped: Vec<String>,
}

pub fn sanitize_export_filename(title: &str) -> String {
    let trimmed = title.trim();
    let base = if trimmed.is_empty() { "session" } else { trimmed };
    let replaced: String = base
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = replaced
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .chars()
        .take(60)
        .collect::<String>();
    let stem = if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    };
    format!("{stem}.md")
}

pub fn ensure_markdown_extension(path: PathBuf) -> PathBuf {
    let Some(parent) = path.parent() else {
        return PathBuf::from("session.md");
    };
    let raw_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let trimmed = raw_name.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() {
        return parent.join("session.md");
    }
    let has_md_ext = std::path::Path::new(trimmed)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if has_md_ext {
        parent.join(trimmed)
    } else {
        parent.join(format!("{trimmed}.md"))
    }
}

pub fn export_session_markdown_with_title<L: Llm + 'static>(
    core: &AppCore<L>,
    id: &str,
) -> Result<(String, String), String> {
    let (events, fallback_title) = match core.all_live().get(id) {
        Some(live) => (live.snapshot(), live.summary().title),
        None => {
            let evs = core.store.read(id).map_err(|e| e.to_string())?;
            let t = odg_core::session::summarize(&evs).title;
            (evs, t)
        }
    };
    Ok((odg_core::export::to_markdown(&events, None), fallback_title))
}

pub async fn send_message_core<L: Llm + 'static>(
    core: &Arc<AppCore<L>>,
    id: &str,
    text: String,
    images: Vec<Image>,
    files: Vec<String>,
) -> Result<bool, String> {
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() && files.is_empty() {
        return Err("the message is empty".to_string());
    }
    check_images(&images)?;
    if files.len() > FILES_PER_MESSAGE {
        return Err(format!(
            "a message takes at most {FILES_PER_MESSAGE} files, not {}",
            files.len()
        ));
    }
    let files = tokio::task::spawn_blocking(move || {
        files
            .iter()
            .map(|path| {
                attach(std::path::Path::new(path))
                    .map_err(|why| format!("{path} cannot be attached: {}", why.replace('_', " ")))
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|error| error.to_string())??;
    let session = core.live(id).await?;
    let summary = session.summary();
    if !images.is_empty() && vision(&summary.provider, &summary.model) == Some(false) {
        return Err(format!(
            "{} takes text only: remove the images, or start a session on a model that takes them",
            summary.model
        ));
    }
    Ok(session.send_with(text, images, files))
}

#[tauri::command]
pub async fn list_sessions(app: State<'_, App>) -> Result<Vec<SessionItem>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.list_sessions_blocking())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_session_item(app: State<'_, App>, id: String) -> Result<SessionItem, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.get_session_item_blocking(&id))
        .await
        .map_err(|error| error.to_string())?
}

/// The side conversation (旁聊) beside session `id`, if it has one.
#[tauri::command]
pub async fn aside_session(app: State<'_, App>, id: String) -> Result<Option<SessionView>, String> {
    let core = app.0.clone();
    let id_lookup = id.clone();
    let core_lookup = core.clone();
    let found = tokio::task::spawn_blocking(move || core_lookup.find_aside(&id_lookup))
        .await
        .map_err(|error| error.to_string())?;
    match found {
        Some(aside) => {
            let session = core.live(&aside).await?;
            Ok(Some(core.view(&session)))
        }
        None => Ok(None),
    }
}

/// The side conversation beside session `id`, made now when it has none.
#[tauri::command]
pub async fn start_aside(app: State<'_, App>, id: String) -> Result<SessionView, String> {
    let core = app.0.clone();
    let id_lookup = id.clone();
    let core_lookup = core.clone();
    if let Some(aside) = tokio::task::spawn_blocking(move || core_lookup.find_aside(&id_lookup))
        .await
        .map_err(|error| error.to_string())?
    {
        let session = core.live(&aside).await?;
        return Ok(core.view(&session));
    }
    let main = core.live(&id).await?;
    let core_make = core.clone();
    tokio::task::spawn_blocking(move || {
        let session = core_make.start_aside_for(&main)?;
        Ok(core_make.view(&session))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Take away the side conversation beside session `id`, to start it over. Fails while it is answering.
#[tauri::command]
pub async fn reset_aside(app: State<'_, App>, id: String) -> Result<(), String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.reset_aside_blocking(&id))
        .await
        .map_err(|error| error.to_string())?
}

/// Start a session in `workspace`. With `isolated`, in a Git worktree made for it.
#[tauri::command]
pub async fn create_session(
    app: State<'_, App>,
    workspace: String,
    isolated: Option<bool>,
) -> Result<SessionView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let session = core.start(PathBuf::from(workspace), isolated == Some(true), None)?;
        core.set_active_session(Some(session.id()));
        Ok(core.view(&session))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Start a conversation (闲谈): a session in the folder kept for them, made the first time it is wanted.
#[tauri::command]
pub async fn start_chat(app: State<'_, App>) -> Result<SessionView, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || {
        let folder = core.chat_folder();
        std::fs::create_dir_all(&folder).map_err(|error| format!("Cannot make {}: {error}", folder.display()))?;
        let session = core.start(folder, false, None)?;
        core.set_active_session(Some(session.id()));
        Ok(core.view(&session))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Start a session that goes on from a point in another's conversation.
#[tauri::command]
pub async fn fork_session(app: State<'_, App>, id: String, before: Option<u64>) -> Result<SessionView, String> {
    let core = app.0.clone();
    let source = core.live(&id).await?;
    tokio::task::spawn_blocking(move || core.fork_from(&source, before))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn export_session_markdown(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: String,
    title: Option<String>,
    save: Option<bool>,
) -> Result<Option<String>, String> {
    let core = app.0.clone();
    let (markdown, fallback_title) =
        tokio::task::spawn_blocking(move || export_session_markdown_with_title(&core, &id))
            .await
            .map_err(|e| e.to_string())??;
    if save != Some(true) {
        return Ok(Some(markdown));
    }

    use tauri_plugin_dialog::DialogExt;
    let raw_title = title.filter(|t| !t.trim().is_empty()).unwrap_or(fallback_title);
    let default_name = sanitize_export_filename(&raw_title);
    let mut dialog = handle
        .dialog()
        .file()
        .set_title("导出会话为 Markdown")
        .set_file_name(default_name)
        .add_filter("Markdown", &["md"]);
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(window) = handle.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    dialog.save_file(move |picked| {
        let _ = tx.send(picked);
    });
    let Some(picked) = rx.await.map_err(|_| "保存对话框已关闭".to_string())? else {
        return Ok(None);
    };
    let raw_path = picked.into_path().map_err(|e| format!("无效的导出路径：{e}"))?;
    let dest = ensure_markdown_extension(raw_path);
    tokio::task::spawn_blocking(move || {
        odg_core::fsutil::write_atomically(&dest, markdown.as_bytes()).map_err(|e| format!("写入导出文件失败：{e}"))?;
        Ok(Some(dest.display().to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The folder a new session opens in without asking, when `ODG_E2E_WORKSPACE` names one.
#[tauri::command]
pub fn preset_workspace() -> Option<String> {
    std::env::var("ODG_E2E_WORKSPACE")
        .ok()
        .filter(|path| !path.trim().is_empty())
}

#[tauri::command]
pub async fn open_session(app: State<'_, App>, id: String) -> Result<SessionView, String> {
    let core = app.0.clone();
    core.set_active_session(Some(&id));
    let session = core.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        core.ensure_valid_model(&session);
        core.evict_idle_lru();
        core.view(&session)
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn send_message(
    app: State<'_, App>,
    id: String,
    text: String,
    images: Vec<Image>,
    files: Vec<String>,
) -> Result<bool, String> {
    send_message_core(&app.0, &id, text, images, files).await
}

#[tauri::command]
pub async fn resolve_approval(
    app: State<'_, App>,
    id: String,
    tool_use_id: String,
    decision: Decision,
) -> Result<bool, String> {
    Ok(app.0.live(&id).await?.resolve_approval(&tool_use_id, decision))
}

#[tauri::command]
pub async fn answer_questions(
    app: State<'_, App>,
    id: String,
    tool_use_id: String,
    answers: Option<Vec<Reply>>,
) -> Result<bool, String> {
    Ok(app.0.live(&id).await?.answer_questions(&tool_use_id, answers))
}

#[tauri::command]
pub async fn rename_session(app: State<'_, App>, id: String, title: String) -> Result<SessionItem, String> {
    let core = app.0.clone();
    let session = core.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        session.rename(&title)?;
        Ok(core.item(session.summary(), session.status()))
    })
    .await
    .map_err(|error| error.to_string())?
}

pub async fn land_session_core<L: Llm + 'static>(core: &Arc<AppCore<L>>, id: &str) -> Result<Landed, String> {
    let session = core.live(id).await?;
    let made = session
        .worktree()
        .cloned()
        .ok_or("this session works in the folder itself, so there is nothing to bring back")?;
    if !session.is_idle_tree() {
        return Err(crate::error::coded(
            crate::error::ERR_WORKTREE_BUSY,
            "the agent is working; stop it or wait before bringing its work back",
        ));
    }
    // Clone loaded session handles in a short critical section and immediately release the table lock
    // before inspecting sibling sessions sharing the same worktree checkout.
    let loaded = core.loaded_sessions();
    if loaded.iter().any(|other| {
        other.id() != session.id()
            && other.worktree().is_some_and(|wt| wt.checkout == made.checkout)
            && !other.is_idle_tree()
    }) {
        return Err(crate::error::coded(
            crate::error::ERR_WORKTREE_BUSY,
            "another session sharing this worktree is still working; stop it or wait before bringing its work back",
        ));
    }
    let summary = session.summary();
    let message = format!(
        "odg: {}",
        if summary.title.is_empty() {
            "work of a session"
        } else {
            summary.title.as_str()
        }
    );
    tokio::task::spawn_blocking(move || worktree::land(&made, &message))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn land_session(app: State<'_, App>, id: String) -> Result<Landed, String> {
    land_session_core(&app.0, &id).await
}

#[tauri::command]
pub async fn delete_session(app: State<'_, App>, id: String) -> Result<(), String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.delete_session_blocking(&id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn search_sessions(app: State<'_, App>, query: String) -> Result<Vec<String>, String> {
    let core = app.0.clone();
    tokio::task::spawn_blocking(move || core.search_sessions_blocking(&query))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_model(
    app: State<'_, App>,
    id: String,
    provider: String,
    model: String,
) -> Result<SessionItem, String> {
    let core = app.0.clone();
    let session = core.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        session.set_model(&provider, &model)?;
        Ok(core.item(session.summary(), session.status()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn set_effort(app: State<'_, App>, id: String, effort: String) -> Result<SessionItem, String> {
    let core = app.0.clone();
    let session = core.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        session.set_effort(&effort)?;
        Ok(core.item(session.summary(), session.status()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn set_mode(app: State<'_, App>, id: String, mode: Mode) -> Result<SessionItem, String> {
    let core = app.0.clone();
    let session = core.live(&id).await?;
    if mode == Mode::Bypass && session.summary().worktree.is_none() {
        return Err(
            "only a session in a worktree of its own can be left to ask about nothing; start one with the branch button beside the folder"
                .to_string(),
        );
    }
    session.set_mode(mode);
    tokio::task::spawn_blocking(move || core.item(session.summary(), session.status()))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn undo_turn(app: State<'_, App>, id: String, turn: u64) -> Result<Undone, String> {
    let session = app.0.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        let outcome = session.undo(turn)?;
        Ok(Undone {
            restored: outcome.restored,
            skipped: outcome.skipped,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn set_workspace_trust(app: State<'_, App>, id: String, trusted: bool) -> Result<SessionItem, String> {
    let core = app.0.clone();
    let session = core.live(&id).await?;
    tokio::task::spawn_blocking(move || {
        session.set_project_trust(trusted)?;
        Ok(core.item(session.summary(), session.status()))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn compact_session(app: State<'_, App>, id: String, focus: Option<String>) -> Result<u64, String> {
    let session = app.0.live(&id).await?;
    session.compact_now_with(focus).await
}

#[tauri::command]
pub async fn cancel_turn(app: State<'_, App>, id: String) -> Result<(), String> {
    app.0.live(&id).await?.cancel();
    Ok(())
}

#[tauri::command]
pub async fn deliver_queued(app: State<'_, App>, id: String) -> Result<bool, String> {
    Ok(app.0.live(&id).await?.deliver_queued())
}
