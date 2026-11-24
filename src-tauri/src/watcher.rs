//! Debounced filesystem watcher for the active workspace so external edits and
//! interactive PTY shell commands (`git add`, `git checkout`, `touch`, etc.)
//! refresh the file tree, open viewer tabs, and Git panel automatically.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::mpsc;

pub const WORKSPACE_CHANGED_EVENT: &str = "workspace-changed";
const SOURCE_DEBOUNCE: Duration = Duration::from_millis(350);
const BUILD_DEBOUNCE: Duration = Duration::from_millis(800);

/// Directories whose contents are typically gitignored build outputs or package trees:
/// changes inside them should refresh the `FileTree` (if expanded) without spawning `git status`.
const BUILD_OR_DEP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    "coverage",
];

/// Internal compiler / tool scratch directories that churn thousands of files mid-build
/// and never hold user-facing deliverables.
const EPHEMERAL_CACHE_DIRS: &[&str] = &[
    "incremental",
    ".fingerprint",
    ".pnpm-store",
    "__pycache__",
    ".pytest_cache",
    ".turbo",
    ".cache",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathImpact {
    /// Ephemeral lock/temp/object file: ignore completely.
    None,
    /// Build output or dependency directory: refresh `FileTree` & file viewers only (skip `git status`).
    TreeOnly,
    /// Source file or Git ref/index: refresh both `FileTree` and `GitPanel`.
    TreeAndGit,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceChanged {
    pub workspace: String,
    pub git: bool,
}

fn strip_verbatim(path: &Path) -> &Path {
    path.to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .map(Path::new)
        .unwrap_or(path)
}

/// Classify how a filesystem change at `path` inside `workspace` should refresh the UI.
pub fn classify_path(workspace: &Path, path: &Path) -> PathImpact {
    let ws = strip_verbatim(workspace);
    let p = strip_verbatim(path);
    let rel = p.strip_prefix(ws).unwrap_or(p);
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(seg) => Some(seg.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return PathImpact::None;
    }

    let leaf = parts.last().map(String::as_str).unwrap_or("");
    if leaf == ".DS_Store" || leaf.ends_with(".tmp") || leaf.ends_with(".swp") || leaf.ends_with(".lock") || leaf.ends_with('~') {
        return PathImpact::None;
    }

    if let Some(git_idx) = parts.iter().position(|seg| seg.eq_ignore_ascii_case(".git")) {
        let after = &parts[git_idx + 1..];
        return if matches!(
            after.first().map(String::as_str),
            Some("HEAD" | "index" | "packed-refs" | "MERGE_HEAD" | "REBASE_HEAD" | "refs")
        ) {
            PathImpact::TreeAndGit
        } else {
            PathImpact::None
        };
    }

    // Ignore deep churn inside internal compiler/cache scratch folders (while still noticing if the folder itself is created/removed).
    if parts.len() > 1 && parts[..parts.len() - 1].iter().any(|seg| EPHEMERAL_CACHE_DIRS.iter().any(|c| seg.eq_ignore_ascii_case(c))) {
        return PathImpact::None;
    }
    if parts.iter().any(|seg| EPHEMERAL_CACHE_DIRS.iter().any(|c| seg.eq_ignore_ascii_case(c))) {
        return PathImpact::TreeOnly;
    }

    if parts.iter().any(|seg| BUILD_OR_DEP_DIRS.iter().any(|n| seg.eq_ignore_ascii_case(n))) {
        return PathImpact::TreeOnly;
    }

    PathImpact::TreeAndGit
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
        F: Fn(WorkspaceChanged) + Send + Sync + 'static,
    {
        let mut guard = self.active.lock().unwrap();
        let Some(target) = workspace.and_then(|p| p.canonicalize().ok()) else {
            *guard = None;
            return Ok(());
        };

        if guard.as_ref().is_some_and(|cur| cur.workspace == target) {
            return Ok(());
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<PathImpact>();
        let filter_root = target.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res
                    && !matches!(event.kind, notify::EventKind::Access(_))
                {
                    let impact = event
                        .paths
                        .iter()
                        .map(|p| classify_path(&filter_root, p))
                        .max()
                        .unwrap_or(PathImpact::None);
                    if impact != PathImpact::None {
                        let _ = tx.send(impact);
                    }
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

        let watched_str = strip_verbatim(&target).to_string_lossy().into_owned();
        let callback = Arc::new(on_changed);
        runtime.spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut impact = first;
                loop {
                    let wait = if impact == PathImpact::TreeAndGit { SOURCE_DEBOUNCE } else { BUILD_DEBOUNCE };
                    tokio::time::sleep(wait).await;
                    let mut more = false;
                    while let Ok(next) = rx.try_recv() {
                        more = true;
                        if next > impact {
                            impact = next;
                        }
                    }
                    // For build-output bursts (`target/`, `dist/`, `node_modules/`), wait until writes settle;
                    // for normal source edits, emit promptly after the first debounce window.
                    if !more || impact == PathImpact::TreeAndGit {
                        break;
                    }
                }
                callback(WorkspaceChanged {
                    workspace: watched_str.clone(),
                    git: impact == PathImpact::TreeAndGit,
                });
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
    fn classifies_source_build_bundle_and_ephemeral_paths() {
        let root = Path::new("/tmp/demo");
        assert_eq!(classify_path(root, Path::new("/tmp/demo/src/main.rs")), PathImpact::TreeAndGit);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.gitignore")), PathImpact::TreeAndGit);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.git/HEAD")), PathImpact::TreeAndGit);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.git/index")), PathImpact::TreeAndGit);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.git/refs/heads/main")), PathImpact::TreeAndGit);

        // Build outputs (`target/release/bundle/nsis/*.exe`, `dist/*`) refresh FileTree without triggering Git!
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/target/release/bundle/nsis/OmniDoppelganger_0.7.0_x64-setup.exe")),
            PathImpact::TreeOnly
        );
        assert_eq!(classify_path(root, Path::new("/tmp/demo/dist/index.html")), PathImpact::TreeOnly);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/node_modules/pkg/index.js")), PathImpact::TreeOnly);

        // Windows `\\?\` verbatim prefixes are stripped symmetrically.
        assert_eq!(strip_verbatim(Path::new(r"\\?\D:\proj")), Path::new(r"D:\proj"));
        #[cfg(windows)]
        assert_eq!(
            classify_path(Path::new(r"\\?\D:\proj"), Path::new(r"D:\proj\target\release\bundle\nsis\setup.exe")),
            PathImpact::TreeOnly
        );

        // Lock files, git objects, and Cargo incremental/fingerprint scratch dirs are ignored completely.
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.git/index.lock")), PathImpact::None);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.git/objects/12/3456")), PathImpact::None);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/target/release/incremental/app/s-123.bin")), PathImpact::None);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/target/debug/.fingerprint/app/invoked.timestamp")), PathImpact::None);
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.DS_Store")), PathImpact::None);
    }
}
