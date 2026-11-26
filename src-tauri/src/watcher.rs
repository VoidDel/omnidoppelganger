//! Debounced filesystem watcher for the active workspace so external edits and
//! interactive PTY shell commands (`git add`, `git checkout`, `touch`, etc.)
//! refresh the file tree, open viewer tabs, and Git panel automatically.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use odg_core::sync_ext::MutexExt;
use serde::Serialize;
use tokio::sync::mpsc;

pub const WORKSPACE_CHANGED_EVENT: &str = "workspace-changed";
const SOURCE_DEBOUNCE: Duration = Duration::from_millis(350);
const BUILD_DEBOUNCE: Duration = Duration::from_millis(800);
const MAX_BURST_WAIT: Duration = Duration::from_millis(1500);

#[cfg(any(target_os = "linux", test))]
const MAX_LINUX_WATCH_DIRS: usize = 4096;

/// Directories skipped when registering per-directory Linux `inotify` watches so large dependency
/// or build trees never exhaust `fs.inotify.max_user_watches`.
#[cfg(any(target_os = "linux", test))]
pub const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".turbo",
    ".cache",
    ".pnpm-store",
    "coverage",
    "vendor",
];

/// Directories whose contents are typically gitignored build outputs or package trees:
/// changes inside them should refresh the `FileTree` (if expanded) without spawning `git status`.
const BUILD_OR_DEP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", ".next", "coverage"];

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

fn is_max_files_watch_error(err: &notify::Error) -> bool {
    matches!(err.kind, notify::ErrorKind::MaxFilesWatch)
        || matches!(&err.kind, notify::ErrorKind::Io(io_err) if io_err.raw_os_error() == Some(28))
}

/// Collect source directories to watch on Linux without descending into `IGNORED_DIRS` or `.git/objects`.
#[cfg(any(target_os = "linux", test))]
pub fn collect_linux_watch_dirs(target: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, RecursiveMode)>) {
        if out.len() >= MAX_LINUX_WATCH_DIRS {
            return;
        }
        out.push((dir.to_path_buf(), RecursiveMode::NonRecursive));
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir() && !ft.is_symlink()))
            .map(|e| e.path())
            .collect();
        children.sort();
        for child in children {
            if out.len() >= MAX_LINUX_WATCH_DIRS {
                break;
            }
            let Some(name) = child.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.eq_ignore_ascii_case(".git") {
                out.push((child.clone(), RecursiveMode::NonRecursive));
                let refs = child.join("refs");
                if refs.is_dir() && out.len() < MAX_LINUX_WATCH_DIRS {
                    out.push((refs, RecursiveMode::Recursive));
                }
                continue;
            }
            if IGNORED_DIRS.iter().any(|ignored| name.eq_ignore_ascii_case(ignored)) {
                continue;
            }
            walk(&child, out);
        }
    }

    let mut out = Vec::new();
    walk(target, &mut out);
    out
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
    if leaf == ".DS_Store"
        || leaf.ends_with(".tmp")
        || leaf.ends_with(".swp")
        || leaf.ends_with(".lock")
        || leaf.ends_with('~')
    {
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

    if parts.len() > 1
        && parts[..parts.len() - 1]
            .iter()
            .any(|seg| EPHEMERAL_CACHE_DIRS.iter().any(|c| seg.eq_ignore_ascii_case(c)))
    {
        return PathImpact::None;
    }
    if parts
        .iter()
        .any(|seg| EPHEMERAL_CACHE_DIRS.iter().any(|c| seg.eq_ignore_ascii_case(c)))
    {
        return PathImpact::TreeOnly;
    }

    if parts
        .iter()
        .any(|seg| BUILD_OR_DEP_DIRS.iter().any(|n| seg.eq_ignore_ascii_case(n)))
    {
        return PathImpact::TreeOnly;
    }

    PathImpact::TreeAndGit
}

pub(crate) async fn run_debounce_loop_with_timers<F>(
    mut rx: mpsc::UnboundedReceiver<PathImpact>,
    watched_str: String,
    source_debounce: Duration,
    build_debounce: Duration,
    max_burst: Duration,
    callback: Arc<F>,
) where
    F: Fn(WorkspaceChanged) + Send + Sync + 'static,
{
    while let Some(first) = rx.recv().await {
        let burst_start = tokio::time::Instant::now();
        let mut impact = first;
        loop {
            let wait = if impact == PathImpact::TreeAndGit {
                source_debounce
            } else {
                build_debounce
            };
            let remaining = max_burst.saturating_sub(burst_start.elapsed());
            tokio::time::sleep(wait.min(remaining)).await;
            let mut more = false;
            while let Ok(next) = rx.try_recv() {
                more = true;
                if next > impact {
                    impact = next;
                }
            }
            if !more || burst_start.elapsed() >= max_burst {
                break;
            }
        }
        callback(WorkspaceChanged {
            workspace: watched_str.clone(),
            git: impact == PathImpact::TreeAndGit,
        });
    }
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
        Self {
            active: Mutex::new(None),
        }
    }

    pub fn watch<F>(
        &self,
        workspace: Option<PathBuf>,
        runtime: tokio::runtime::Handle,
        on_changed: F,
    ) -> Result<(), String>
    where
        F: Fn(WorkspaceChanged) + Send + Sync + 'static,
    {
        let mut guard = self.active.lock_unpoisoned();
        let Some(target) = workspace.and_then(|p| p.canonicalize().ok()) else {
            *guard = None;
            return Ok(());
        };

        if guard.as_ref().is_some_and(|cur| cur.workspace == target) {
            return Ok(());
        }

        let (tx, rx) = mpsc::unbounded_channel::<PathImpact>();
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

        #[cfg(target_os = "linux")]
        {
            for (dir, mode) in collect_linux_watch_dirs(&target) {
                match watcher.watch(&dir, mode) {
                    Ok(()) => {}
                    Err(err) if is_max_files_watch_error(&err) => break,
                    Err(_) => {}
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            match watcher.watch(&target, RecursiveMode::Recursive) {
                Ok(()) => {}
                Err(err) if is_max_files_watch_error(&err) => {
                    *guard = None;
                    return Ok(());
                }
                Err(error) => return Err(format!("无法监听目录 {}：{error}", target.display())),
            }
        }

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
        runtime.spawn(run_debounce_loop_with_timers(
            rx,
            watched_str,
            SOURCE_DEBOUNCE,
            BUILD_DEBOUNCE,
            MAX_BURST_WAIT,
            callback,
        ));

        *guard = Some(ActiveWatch {
            workspace: target,
            _watcher: watcher,
        });
        Ok(())
    }

    pub fn stop(&self) {
        *self.active.lock_unpoisoned() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn classifies_source_build_bundle_and_ephemeral_paths() {
        let root = Path::new("/tmp/demo");
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/src/main.rs")),
            PathImpact::TreeAndGit
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.gitignore")),
            PathImpact::TreeAndGit
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.git/HEAD")),
            PathImpact::TreeAndGit
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.git/index")),
            PathImpact::TreeAndGit
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.git/refs/heads/main")),
            PathImpact::TreeAndGit
        );

        assert_eq!(
            classify_path(
                root,
                Path::new("/tmp/demo/target/release/bundle/nsis/OmniDoppelganger_0.7.0_x64-setup.exe")
            ),
            PathImpact::TreeOnly
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/dist/index.html")),
            PathImpact::TreeOnly
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/node_modules/pkg/index.js")),
            PathImpact::TreeOnly
        );

        assert_eq!(strip_verbatim(Path::new(r"\\?\D:\proj")), Path::new(r"D:\proj"));
        #[cfg(windows)]
        assert_eq!(
            classify_path(
                Path::new(r"\\?\D:\proj"),
                Path::new(r"D:\proj\target\release\bundle\nsis\setup.exe")
            ),
            PathImpact::TreeOnly
        );

        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.git/index.lock")),
            PathImpact::None
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/.git/objects/12/3456")),
            PathImpact::None
        );
        assert_eq!(
            classify_path(root, Path::new("/tmp/demo/target/release/incremental/app/s-123.bin")),
            PathImpact::None
        );
        assert_eq!(
            classify_path(
                root,
                Path::new("/tmp/demo/target/debug/.fingerprint/app/invoked.timestamp")
            ),
            PathImpact::None
        );
        assert_eq!(classify_path(root, Path::new("/tmp/demo/.DS_Store")), PathImpact::None);
    }

    #[test]
    fn linux_watch_collector_skips_ignored_dirs_and_git_objects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/components")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/react")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::create_dir_all(root.join(".next/cache")).unwrap();
        std::fs::create_dir_all(root.join("dist/assets")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects/aa")).unwrap();
        std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();

        let watched = collect_linux_watch_dirs(root);
        let paths: Vec<PathBuf> = watched.iter().map(|(p, _)| p.clone()).collect();
        assert!(paths.contains(&root.to_path_buf()));
        assert!(paths.contains(&root.join("src")));
        assert!(paths.contains(&root.join("src/components")));
        assert!(paths.contains(&root.join(".git")));
        assert!(paths.contains(&root.join(".git/refs")));
        for forbidden in ["node_modules", "target", ".venv", ".next", "dist", ".git/objects"] {
            assert!(
                !paths.iter().any(|p| p.starts_with(root.join(forbidden))),
                "must not watch ignored directory {forbidden}: {paths:?}"
            );
        }
        assert!(is_max_files_watch_error(&notify::Error::new(
            notify::ErrorKind::MaxFilesWatch
        )));
    }

    #[tokio::test]
    async fn debounce_coalesces_50_rapid_source_events_into_single_callback() {
        let (tx, rx) = mpsc::unbounded_channel();
        let count = Arc::new(AtomicUsize::new(0));
        let git_flags = Arc::new(Mutex::new(Vec::new()));
        let count_sink = count.clone();
        let git_sink = git_flags.clone();

        let handle = tokio::spawn(run_debounce_loop_with_timers(
            rx,
            "/ws/proj".to_string(),
            SOURCE_DEBOUNCE,
            BUILD_DEBOUNCE,
            MAX_BURST_WAIT,
            Arc::new(move |ev: WorkspaceChanged| {
                count_sink.fetch_add(1, Ordering::SeqCst);
                git_sink.lock().unwrap().push(ev.git);
            }),
        ));

        // Burst of 50 source edits spaced across ~100ms (crossing no 350ms quiet window) must coalesce to 1 emit!
        for i in 0..50 {
            tx.send(if i % 5 == 0 {
                PathImpact::TreeOnly
            } else {
                PathImpact::TreeAndGit
            })
            .unwrap();
            if i % 10 == 9 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "50 burst source events must coalesce into 1 callback"
        );
        assert_eq!(*git_flags.lock().unwrap(), vec![true]);
    }

    #[tokio::test]
    async fn debounce_flushes_at_max_burst_cap_under_sustained_churn() {
        let (tx, rx) = mpsc::unbounded_channel();
        let count = Arc::new(AtomicUsize::new(0));
        let count_sink = count.clone();

        let handle = tokio::spawn(run_debounce_loop_with_timers(
            rx,
            "/ws/proj".to_string(),
            Duration::from_millis(60),
            Duration::from_millis(100),
            Duration::from_millis(180),
            Arc::new(move |_: WorkspaceChanged| {
                count_sink.fetch_add(1, Ordering::SeqCst);
            }),
        ));

        // Feed events every 30ms for 240ms (> 180ms max_burst): first emission must fire before stream ends!
        for _ in 0..8 {
            tx.send(PathImpact::TreeAndGit).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(
            count.load(Ordering::SeqCst) >= 1,
            "max_burst ceiling must flush even while churn continues"
        );
        drop(tx);
        handle.await.unwrap();
    }
}
