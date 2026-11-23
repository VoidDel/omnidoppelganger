//! Debounced filesystem watcher for the active workspace so external edits and
//! interactive PTY shell commands (`git add`, `git checkout`, `touch`, etc.)
//! refresh the file tree, open viewer tabs, and Git panel automatically.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

pub const WORKSPACE_CHANGED_EVENT: &str = "workspace-changed";
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(350);

const NOISY_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    ".pnpm-store",
    "__pycache__",
    ".pytest_cache",
    ".next",
    ".turbo",
    ".cache",
];

/// Decide whether a filesystem change at `path` inside `workspace` is meaningful to the UI.
pub fn is_relevant_path(workspace: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(workspace).unwrap_or(path);
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return false;
    }

    let leaf = parts.last().map(String::as_str).unwrap_or("");
    if leaf == ".DS_Store" || leaf.ends_with(".tmp") || leaf.ends_with(".swp") || leaf.ends_with(".lock") || leaf.ends_with('~') {
        return false;
    }

    if parts.iter().any(|seg| NOISY_DIRS.iter().any(|n| seg.eq_ignore_ascii_case(n))) {
        return false;
    }

    if let Some(git_idx) = parts.iter().position(|seg| seg.eq_ignore_ascii_case(".git")) {
        let after = &parts[git_idx + 1..];
        return matches!(
            after.first().map(String::as_str),
            Some("HEAD" | "index" | "packed-refs" | "MERGE_HEAD" | "REBASE_HEAD" | "refs")
        );
    }

    true
}

struct ActiveWatch {
    workspace: PathBuf,
    _watcher: RecommendedWatcher,
}

pub struct WorkspaceWatcher {
    active: Mutex<Option<ActiveWatch>>,
}

impl Default for WorkspaceWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceWatcher {
    pub fn new() -> Self {
        Self { active: Mutex::new(None) }
    }

    pub fn watch<F>(&self, workspace: Option<PathBuf>, runtime: tokio::runtime::Handle, on_changed: F) -> Result<(), String>
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        let mut guard = self.active.lock().unwrap();
        let Some(target) = workspace.and_then(|p| p.canonicalize().ok()) else {
            *guard = None;
            return Ok(());
        };

        if guard.as_ref().is_some_and(|cur| cur.workspace == target) {
            return Ok(());
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let filter_root = target.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res
                    && !matches!(event.kind, notify::EventKind::Access(_))
                    && event.paths.iter().any(|p| is_relevant_path(&filter_root, p))
                {
                    let _ = tx.send(());
                }
            },
            Config::default(),
        )
        .map_err(|error| format!("无法建立目录监听：{error}"))?;

        watcher
            .watch(&target, RecursiveMode::Recursive)
            .map_err(|error| format!("无法监听目录 {}：{error}", target.display()))?;

        // If this workspace is a Git worktree whose `.git` is a file pointing to a common gitdir,
        // watch its worktree gitdir (non-recursively) so `HEAD`/`index` updates fire too.
        let dot_git = target.join(".git");
        if dot_git.is_file()
            && let Ok(pointer) = std::fs::read_to_string(&dot_git)
            && let Some(raw_gitdir) = pointer.trim().strip_prefix("gitdir:").map(str::trim)
        {
            let gitdir = if Path::new(raw_gitdir).is_absolute() {
                PathBuf::from(raw_gitdir)
            } else {
                target.join(raw_gitdir)
            };
            if gitdir.is_dir() {
                let _ = watcher.watch(&gitdir, RecursiveMode::NonRecursive);
            }
        }

        let watched_str = target.to_string_lossy().into_owned();
        let callback = Arc::new(on_changed);
        runtime.spawn(async move {
            while rx.recv().await.is_some() {
                tokio::time::sleep(DEBOUNCE_WINDOW).await;
                while rx.try_recv().is_ok() {}
                callback(watched_str.clone());
            }
        });

        *guard = Some(ActiveWatch { workspace: target, _watcher: watcher });
        Ok(())
    }

    pub fn stop(&self) {
        *self.active.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_noisy_build_and_git_object_paths_while_keeping_source_and_git_head() {
        let root = Path::new("/tmp/demo");
        assert!(is_relevant_path(root, Path::new("/tmp/demo/src/main.rs")));
        assert!(is_relevant_path(root, Path::new("/tmp/demo/.gitignore")));
        assert!(is_relevant_path(root, Path::new("/tmp/demo/.git/HEAD")));
        assert!(is_relevant_path(root, Path::new("/tmp/demo/.git/index")));
        assert!(is_relevant_path(root, Path::new("/tmp/demo/.git/refs/heads/main")));

        assert!(!is_relevant_path(root, Path::new("/tmp/demo/.git/index.lock")));
        assert!(!is_relevant_path(root, Path::new("/tmp/demo/.git/objects/12/3456")));
        assert!(!is_relevant_path(root, Path::new("/tmp/demo/target/debug/app")));
        assert!(!is_relevant_path(root, Path::new("/tmp/demo/node_modules/pkg/index.js")));
        assert!(!is_relevant_path(root, Path::new("/tmp/demo/.DS_Store")));
    }
}
