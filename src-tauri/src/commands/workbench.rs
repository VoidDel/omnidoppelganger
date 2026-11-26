//! Workspace file tree, safe attachment preview & snapshot materialization,
//! image rendering, paste lifecycle management, and workspace watching.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use odg_core::events::{Image, Picked, pick};
use odg_core::llm::Llm;
use tauri::{Emitter, State};

use crate::state::{App, AppCore, safe_session_id};
use crate::watcher;

/// Explicit allowlist of static document, image, audio/video, and plain-text config extensions
/// that may be opened directly with the OS default handler (provided the file carries no Unix `+x` bits).
const SAFE_OPEN_EXTENSIONS: &[&str] = &[
    "md", "txt", "log", "pdf", "docx", "doc", "xlsx", "xls", "pptx", "ppt", "csv", "tsv",
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "mp3", "wav", "mp4", "webm",
    "json", "yaml", "yml", "toml",
];

/// Kinds of picture the conversation shows from what the 分身 wrote, and the media type each is shown as.
const PICTURES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
];

/// The largest picture shown in the conversation.
const PICTURE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_PASTE_BYTES: usize = 10 * 1024 * 1024;

fn versioned_filename(file: &Path, version: Option<u32>) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if matches!(c, '/' | '\\' | '\0') || c.is_control() { '_' } else { c })
            .collect()
    };
    let stem = file
        .file_stem()
        .map(|s| clean(&s.to_string_lossy()))
        .filter(|s| !s.is_empty() && s != "." && s != "..")
        .unwrap_or_else(|| "file".to_string());
    let ext = file
        .extension()
        .map(|e| clean(&e.to_string_lossy()))
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    match version {
        Some(n) => format!("{stem}.v{n}{ext}"),
        None => format!("{stem}{ext}"),
    }
}

pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!("无法准备预览目录：{} 不是受控私有目录", dir.display()));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(dir)
            };
            #[cfg(not(unix))]
            let created = std::fs::create_dir(dir);
            match created {
                Ok(()) => {}
                Err(race) if race.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(race) => return Err(format!("无法准备预览目录：{race}")),
            }
        }
        Err(err) => return Err(format!("无法准备预览目录：{err}")),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("无法准备预览目录：{e}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("无法准备预览目录：{} 不是受控私有目录", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("无法设置预览目录权限：{e}"))?;
    }
    Ok(())
}

pub fn materialize_snapshot_in(
    versions_root: &Path,
    session_id: &str,
    file: &Path,
    snapshot: &str,
    version: Option<u32>,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let safe_session = safe_session_id(session_id);
    if safe_session.is_empty() {
        return Err("无效的会话标识。".to_string());
    }
    if !odg_core::undo::valid_snapshot_name(snapshot) {
        return Err("无效的历史版本标识。".to_string());
    }
    let session_dir = versions_root.join(safe_session);
    ensure_private_dir(&session_dir)?;
    let dir = session_dir.join(snapshot);
    ensure_private_dir(&dir)?;
    let preview = dir.join(versioned_filename(file, version));
    if std::fs::read(&preview).ok().as_deref() != Some(bytes) {
        odg_core::fsutil::write_private_atomically(&preview, bytes)
            .map_err(|error| format!("无法准备历史版本预览文件：{error}"))?;
    }
    Ok(preview)
}

fn prepare_versions_root_for<L: Llm + 'static>(core: &AppCore<L>) -> Result<PathBuf, String> {
    if core.home == odg_core::settings::home() {
        odg_core::fsutil::private_temp_dir("versions").map_err(|error| format!("无法准备预览目录：{error}"))
    } else {
        let tmp_dir = core.home.join("tmp");
        ensure_private_dir(&tmp_dir)?;
        let versions = tmp_dir.join("versions");
        ensure_private_dir(&versions)?;
        Ok(versions)
    }
}

fn allowed_open_filename(name: &str) -> bool {
    let trimmed = name.trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() || trimmed.len() != name.len() {
        return false;
    }
    let Some(ext) = Path::new(trimmed).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    SAFE_OPEN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Whether `file` is safe to open directly via the OS default file association rather than revealing in its folder.
pub fn safe_to_open_directly(file: &Path) -> bool {
    let Some(raw_name) = file.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !allowed_open_filename(raw_name) {
        return false;
    }
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    if meta.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return false;
        }
    }
    if let Ok(canon) = file.canonicalize()
        && let Some(canon_name) = canon.file_name().and_then(|n| n.to_str())
        && canon_name != raw_name
        && !allowed_open_filename(canon_name)
    {
        return false;
    }
    true
}

pub fn open_attachment_blocking<L: Llm + 'static>(
    core: &AppCore<L>,
    session: &odg_core::agent::Session<L>,
    path: &str,
    reveal: bool,
    snapshot: Option<&str>,
    version: Option<u32>,
    dry: bool,
) -> Result<String, String> {
    let id = session.id();
    if let Some(snap) = snapshot.filter(|s| !s.is_empty()) {
        if !session.has_artifact_snapshot(path, snap) {
            return Err("这个历史版本不属于当前会话，不能从这里打开。".to_string());
        }
        let file = PathBuf::from(path);
        if reveal {
            let target = if file.exists() {
                file
            } else {
                let bytes = session.snapshot_bytes(snap)?;
                let versions_root = prepare_versions_root_for(core)?;
                materialize_snapshot_in(&versions_root, id, &file, snap, version, &bytes)?
            };
            if !dry {
                tauri_plugin_opener::reveal_item_in_dir(&target)
                    .map_err(|error| format!("没能在文件夹里显示它：{error}"))?;
            }
            return Ok("revealed".to_string());
        }
        let bytes = session.snapshot_bytes(snap)?;
        let versions_root = prepare_versions_root_for(core)?;
        let preview = materialize_snapshot_in(&versions_root, id, &file, snap, version, &bytes)?;
        if !safe_to_open_directly(&preview) || (file.exists() && !safe_to_open_directly(&file)) {
            if !dry {
                tauri_plugin_opener::reveal_item_in_dir(&preview)
                    .map_err(|error| format!("没能在文件夹里显示它：{error}"))?;
            }
            return Ok("revealed".to_string());
        }
        if !dry {
            tauri_plugin_opener::open_path(preview.to_string_lossy().as_ref(), None::<&str>)
                .map_err(|error| format!("没能打开它：{error}"))?;
        }
        return Ok("opened".to_string());
    }

    if !session.has_session_file(path) {
        return Err("这个文件不是这段对话里附过或写过的，不能从这里打开。".to_string());
    }
    let file = PathBuf::from(path);
    if !file.exists() {
        return Err(format!("找不到这个文件了，可能已经移走或删掉：{path}"));
    }
    if reveal || !safe_to_open_directly(&file) || file.is_dir() {
        if !dry {
            tauri_plugin_opener::reveal_item_in_dir(&file)
                .map_err(|error| format!("没能在文件夹里显示它：{error}"))?;
        }
        return Ok("revealed".to_string());
    }
    if !dry {
        tauri_plugin_opener::open_path(path, None::<&str>).map_err(|error| format!("没能打开它：{error}"))?;
    }
    Ok("opened".to_string())
}

pub fn picture_blocking<L: Llm + 'static>(
    session: &odg_core::agent::Session<L>,
    path: &str,
    snapshot: Option<&str>,
) -> Result<Image, String> {
    let file = PathBuf::from(path);
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let media_type = PICTURES
        .iter()
        .find(|(kind, _)| *kind == extension)
        .map(|(_, media)| (*media).to_string())
        .ok_or("这不是能显示的图片。")?;
    if let Some(snap) = snapshot.filter(|s| !s.is_empty()) {
        if !session.has_artifact_snapshot(path, snap) {
            return Err("这个图片版本不属于当前会话。".to_string());
        }
        if let Ok(bytes) = session.snapshot_bytes(snap) {
            if bytes.len() as u64 > PICTURE_BYTES {
                return Err(format!(
                    "这张图有 {} MB，太大，不在这里显示。",
                    bytes.len() / 1024 / 1024
                ));
            }
            return Ok(Image {
                media_type,
                data: odg_core::events::base64(&bytes),
            });
        }
    }
    if !session.has_session_file(path) {
        return Err("这个文件不是这段对话里写过的。".to_string());
    }
    let size = std::fs::metadata(&file)
        .map_err(|_| format!("找不到这个文件了，可能已经移走或删掉：{path}"))?
        .len();
    if size > PICTURE_BYTES {
        return Err(format!("这张图有 {} MB，太大，不在这里显示。", size / 1024 / 1024));
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("读不了这张图：{error}"))?;
    Ok(Image {
        media_type,
        data: odg_core::events::base64(&bytes),
    })
}

pub fn save_pasted_file_in(pastes_root: &Path, name: &str, content: &str) -> Result<Picked, String> {
    static PASTE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if content.len() > MAX_PASTE_BYTES {
        return Err("粘贴内容超过 10 MB，请先保存为文件后再附给分身。".to_string());
    }
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    let safe_name = cleaned.trim_matches('.').to_string();
    let safe_name = if safe_name.is_empty() || safe_name.len() > 80 {
        "pasted.txt".to_string()
    } else {
        safe_name
    };
    let millis = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let seq = PASTE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = pastes_root.join(format!("{millis}-{seq}"));
    std::fs::create_dir_all(&dir).map_err(|error| format!("无法创建粘贴缓存目录：{error}"))?;
    let path = dir.join(&safe_name);
    std::fs::write(&path, content.as_bytes()).map_err(|error| format!("无法写入粘贴文件：{error}"))?;
    Ok(pick(&path))
}

/// Remove a pasted file strictly confined inside `pastes_root`, rejecting any path outside it.
pub fn remove_pasted_file_in(pastes_root: &Path, raw_path: &str) -> Result<(), String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err("粘贴文件路径不能为空。".to_string());
    }
    let candidate = Path::new(trimmed);
    if candidate.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("拒绝删除粘贴缓存目录之外的文件。".to_string());
    }
    if !pastes_root.exists() {
        return if candidate.starts_with(pastes_root) && candidate != pastes_root {
            Ok(())
        } else {
            Err("拒绝删除粘贴缓存目录之外的文件。".to_string())
        };
    }
    let canon_root = pastes_root
        .canonicalize()
        .map_err(|e| format!("无法校验粘贴缓存目录：{e}"))?;
    let meta = match std::fs::symlink_metadata(candidate) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let lexical_inside = candidate.starts_with(pastes_root) || candidate.starts_with(&canon_root);
            let ancestor_inside = candidate
                .ancestors()
                .skip(1)
                .find_map(|anc| anc.canonicalize().ok())
                .is_some_and(|canon_anc| canon_anc.starts_with(&canon_root));
            return if lexical_inside || ancestor_inside {
                Ok(())
            } else {
                Err("拒绝删除粘贴缓存目录之外的文件。".to_string())
            };
        }
        Err(e) => return Err(format!("无法访问粘贴文件：{e}")),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("目标不是受控的粘贴缓存普通文件。".to_string());
    }
    let canon_file = candidate
        .canonicalize()
        .map_err(|e| format!("无法解析粘贴文件路径：{e}"))?;
    let Ok(rel) = canon_file.strip_prefix(&canon_root) else {
        return Err("拒绝删除粘贴缓存目录之外的文件。".to_string());
    };
    if rel.as_os_str().is_empty() {
        return Err("拒绝删除粘贴缓存根目录。".to_string());
    }
    std::fs::remove_file(&canon_file).map_err(|e| format!("删除粘贴缓存文件失败：{e}"))?;
    if let Some(parent) = canon_file.parent()
        && parent != canon_root
        && parent.starts_with(&canon_root)
    {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// Sweep stale paste directories under `pastes_root` whose latest modification time exceeds `max_age`.
pub fn prune_stale_pastes_in(pastes_root: &Path, max_age: Duration) {
    let Ok(root_meta) = std::fs::symlink_metadata(pastes_root) else {
        return;
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return;
    }
    let now = SystemTime::now();
    let is_stale = |mtime: SystemTime| now.duration_since(mtime).is_ok_and(|age| age > max_age);

    let Ok(entries) = std::fs::read_dir(pastes_root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_file() {
            if entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(is_stale)
            {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if !ft.is_dir() {
            continue;
        }
        let mut newest = entry.metadata().ok().and_then(|m| m.modified().ok());
        let mut skip = false;
        if let Ok(inner) = std::fs::read_dir(&path) {
            for child in inner.filter_map(Result::ok) {
                if child.file_type().is_ok_and(|t| t.is_symlink()) {
                    skip = true;
                    break;
                }
                if let Some(child_mtime) = child.metadata().ok().and_then(|m| m.modified().ok()) {
                    newest = Some(match newest {
                        Some(cur) if cur > child_mtime => cur,
                        _ => child_mtime,
                    });
                }
            }
        }
        if !skip && newest.is_some_and(is_stale) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[tauri::command]
pub async fn attach_files(paths: Vec<String>) -> Result<Vec<Picked>, String> {
    tokio::task::spawn_blocking(move || paths.iter().map(|path| pick(Path::new(path))).collect())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_pasted_file(app: State<'_, App>, name: String, content: String) -> Result<Picked, String> {
    let pastes_root = app.pastes_dir();
    tokio::task::spawn_blocking(move || save_pasted_file_in(&pastes_root, &name, &content))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn remove_pasted_file(app: State<'_, App>, path: String) -> Result<(), String> {
    let pastes_root = app.pastes_dir();
    tokio::task::spawn_blocking(move || remove_pasted_file_in(&pastes_root, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn open_attachment(
    app: State<'_, App>,
    id: String,
    path: String,
    reveal: bool,
    snapshot: Option<String>,
    version: Option<u32>,
) -> Result<String, String> {
    let core: Arc<AppCore> = app.0.clone();
    let session = core.live(&id).await?;
    let dry = std::env::var_os("ODG_E2E_DRY_OPEN").is_some();
    tokio::task::spawn_blocking(move || {
        open_attachment_blocking(&core, &session, &path, reveal, snapshot.as_deref(), version, dry)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn restore_artifact(
    app: State<'_, App>,
    id: String,
    path: String,
    snapshot: String,
    version: u32,
) -> Result<odg_core::agent::RestoredArtifact, String> {
    let session = app.0.live(&id).await?;
    tokio::task::spawn_blocking(move || session.restore_artifact(&path, &snapshot, version))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn save_artifact_copy(
    app: State<'_, App>,
    id: String,
    path: String,
    snapshot: String,
    version: u32,
) -> Result<String, String> {
    let session = app.0.live(&id).await?;
    tokio::task::spawn_blocking(move || session.save_artifact_copy(&path, &snapshot, version))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn picture(
    app: State<'_, App>,
    id: String,
    path: String,
    snapshot: Option<String>,
) -> Result<Image, String> {
    let session = app.0.live(&id).await?;
    tokio::task::spawn_blocking(move || picture_blocking(&session, &path, snapshot.as_deref()))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn list_workspace_dir(
    app: State<'_, App>,
    id: String,
    path: String,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<Vec<odg_core::explorer::TreeEntry>, String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || odg_core::explorer::list_directory(&workspace, &path, show_hidden, show_ignored))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn search_workspace_files(
    app: State<'_, App>,
    id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<odg_core::explorer::FoundFile>, String> {
    let workspace = app.0.session_workspace(&id).await?;
    let cap = limit.unwrap_or(odg_core::explorer::DEFAULT_SEARCH_LIMIT);
    tokio::task::spawn_blocking(move || odg_core::explorer::search_files(&workspace, &query, cap))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn read_workspace_file(
    app: State<'_, App>,
    id: String,
    path: String,
) -> Result<odg_core::explorer::FileView, String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || odg_core::explorer::read_file_view(&workspace, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn create_workspace_entry(
    app: State<'_, App>,
    id: String,
    path: String,
    is_dir: bool,
) -> Result<String, String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || odg_core::explorer::create_entry(&workspace, &path, is_dir))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn rename_workspace_entry(
    app: State<'_, App>,
    id: String,
    from: String,
    to: String,
) -> Result<String, String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || odg_core::explorer::rename_entry(&workspace, &from, &to))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn delete_workspace_entry(app: State<'_, App>, id: String, path: String) -> Result<(), String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || odg_core::explorer::delete_entry(&workspace, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn open_workspace_entry(
    app: State<'_, App>,
    id: String,
    path: String,
    reveal: bool,
) -> Result<String, String> {
    let workspace = app.0.session_workspace(&id).await?;
    tokio::task::spawn_blocking(move || {
        let (file, _) = odg_core::explorer::resolve_confined(&workspace, &path, true, true)?;
        if !file.exists() {
            return Err(format!("找不到这个文件了，可能已经移走或删掉：{path}"));
        }
        let dry = std::env::var_os("ODG_E2E_DRY_OPEN").is_some();
        if reveal || !safe_to_open_directly(&file) || file.is_dir() {
            if !dry {
                tauri_plugin_opener::reveal_item_in_dir(&file)
                    .map_err(|error| format!("没能在文件夹里显示它：{error}"))?;
            }
            return Ok("revealed".to_string());
        }
        if !dry {
            tauri_plugin_opener::open_path(file.to_string_lossy().as_ref(), None::<&str>)
                .map_err(|error| format!("没能打开它：{error}"))?;
        }
        Ok("opened".to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn watch_workspace(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: Option<String>,
) -> Result<(), String> {
    let target = match id {
        Some(session_id) if !session_id.is_empty() => Some(app.0.session_workspace(&session_id).await?),
        _ => None,
    };
    let runtime = tokio::runtime::Handle::current();
    app.watcher.watch(target, runtime, move |workspace| {
        let _ = handle.emit(watcher::WORKSPACE_CHANGED_EVENT, workspace);
    })
}
