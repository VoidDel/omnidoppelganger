//! Git workbench IPC commands with untrusted-workspace hooks isolation (`P2-11` & `P2-3`).

use std::path::PathBuf;
use std::sync::Arc;

use odg_core::llm::Llm;
use tauri::State;

use crate::state::{App, AppCore, SessionItem};

pub async fn session_git_workspace<L: Llm + 'static>(
    core: &Arc<AppCore<L>>,
    id: &str,
    repo: Option<&str>,
) -> Result<(PathBuf, PathBuf, bool), String> {
    let session = core.live(id).await?;
    let trusted = session.is_project_trusted();
    let workspace = PathBuf::from(session.summary().workspace);
    let repo_dir = odg_core::git::resolve_repo_dir(&workspace, repo)?;
    Ok((workspace, repo_dir, trusted))
}

pub async fn git_checkout_core<L: Llm + 'static>(
    core: &Arc<AppCore<L>>,
    id: &str,
    branch: String,
    create: bool,
    repo: Option<String>,
) -> Result<SessionItem, String> {
    let session = core.live(id).await?;
    if session.summary().worktree.is_some() && repo.as_deref().is_none_or(|r| r.trim().is_empty()) {
        return Err("独立工作树分身绑定在专属分支上，不能直接切换分支；请在主洞府的分身里切换分支。".to_string());
    }
    let trusted = session.is_project_trusted();
    let workspace = PathBuf::from(session.summary().workspace);
    let repo_dir = odg_core::git::resolve_repo_dir(&workspace, repo.as_deref())?;
    let core_item = core.clone();
    tokio::task::spawn_blocking(move || {
        odg_core::git::checkout_with_trust(&repo_dir, &branch, create, trusted)?;
        Ok(core_item.item(session.summary(), session.status()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_status(
    app: State<'_, App>,
    id: String,
    repo: Option<String>,
) -> Result<odg_core::git::GitStatus, String> {
    let session = app.0.live(&id).await?;
    let trusted = session.is_project_trusted();
    let workspace = PathBuf::from(session.summary().workspace);
    tokio::task::spawn_blocking(move || odg_core::git::status_for_with_trust(&workspace, repo.as_deref(), trusted))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_diff(
    app: State<'_, App>,
    id: String,
    path: String,
    stage: odg_core::git::DiffStage,
    commit: Option<String>,
    repo: Option<String>,
) -> Result<odg_core::git::GitDiffView, String> {
    let (_, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || {
        odg_core::git::diff_with_trust(&repo_dir, &path, stage, commit.as_deref(), trusted)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_stage(
    app: State<'_, App>,
    id: String,
    paths: Vec<String>,
    repo: Option<String>,
) -> Result<odg_core::git::GitStatus, String> {
    let (workspace, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || {
        odg_core::git::stage_with_trust(&repo_dir, &paths, trusted)?;
        odg_core::git::status_for_with_trust(&workspace, repo.as_deref(), trusted)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_unstage(
    app: State<'_, App>,
    id: String,
    paths: Vec<String>,
    repo: Option<String>,
) -> Result<odg_core::git::GitStatus, String> {
    let (workspace, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || {
        odg_core::git::unstage_with_trust(&repo_dir, &paths, trusted)?;
        odg_core::git::status_for_with_trust(&workspace, repo.as_deref(), trusted)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_discard(
    app: State<'_, App>,
    id: String,
    paths: Vec<String>,
    repo: Option<String>,
) -> Result<odg_core::git::GitStatus, String> {
    let (workspace, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || {
        odg_core::git::discard_with_trust(&repo_dir, &paths, trusted)?;
        odg_core::git::status_for_with_trust(&workspace, repo.as_deref(), trusted)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_discard_all(
    app: State<'_, App>,
    id: String,
    repo: Option<String>,
) -> Result<odg_core::git::GitStatus, String> {
    let (workspace, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || {
        odg_core::git::discard_all_with_trust(&repo_dir, trusted)?;
        odg_core::git::status_for_with_trust(&workspace, repo.as_deref(), trusted)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_commit(
    app: State<'_, App>,
    id: String,
    message: String,
    stage_all: bool,
    repo: Option<String>,
) -> Result<odg_core::git::GitCommitResult, String> {
    let (_, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || odg_core::git::commit_with_trust(&repo_dir, &message, stage_all, trusted))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_branches(
    app: State<'_, App>,
    id: String,
    repo: Option<String>,
) -> Result<Vec<odg_core::git::GitBranch>, String> {
    let (_, repo_dir, _) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || odg_core::git::branches(&repo_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_checkout(
    app: State<'_, App>,
    id: String,
    branch: String,
    create: bool,
    repo: Option<String>,
) -> Result<SessionItem, String> {
    git_checkout_core(&app.0, &id, branch, create, repo).await
}

#[tauri::command]
pub async fn git_log(
    app: State<'_, App>,
    id: String,
    limit: Option<usize>,
    repo: Option<String>,
    all: Option<bool>,
) -> Result<Vec<odg_core::git::GitCommitItem>, String> {
    let (_, repo_dir, _) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    let cap = limit.unwrap_or(30);
    let all_branches = all.unwrap_or(false);
    tokio::task::spawn_blocking(move || odg_core::git::log_with_options(&repo_dir, cap, all_branches))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_commit_files(
    app: State<'_, App>,
    id: String,
    commit: String,
    repo: Option<String>,
) -> Result<Vec<odg_core::git::GitFile>, String> {
    let (_, repo_dir, _) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || odg_core::git::commit_files(&repo_dir, &commit))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_sync(app: State<'_, App>, id: String, action: String, repo: Option<String>) -> Result<String, String> {
    let (_, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    odg_core::git::sync_with_trust(&repo_dir, &action, trusted).await
}

#[tauri::command]
pub async fn git_pr_defaults(
    app: State<'_, App>,
    id: String,
    repo: Option<String>,
) -> Result<odg_core::git::PrDefaults, String> {
    let (_, repo_dir, _) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    tokio::task::spawn_blocking(move || odg_core::git::pr_defaults(&repo_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn git_create_pr(
    app: State<'_, App>,
    id: String,
    title: String,
    body: String,
    base: Option<String>,
    draft: bool,
    repo: Option<String>,
) -> Result<odg_core::git::PullRequestResult, String> {
    let (_, repo_dir, trusted) = session_git_workspace(&app.0, &id, repo.as_deref()).await?;
    odg_core::git::create_pr_with_trust(&repo_dir, &title, &body, base.as_deref(), draft, trusted).await
}
