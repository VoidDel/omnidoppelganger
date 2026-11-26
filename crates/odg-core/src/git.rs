//! Git repository status, structured multi-hunk diffs, staging, committing,
//! branch management, commit history, and remote synchronization for the
//! desktop workbench.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::explorer::{self, MAX_TEXT_VIEW_BYTES};

const SAFE_GIT_CONFIG: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "protocol.file.allow=never"];
const FALLBACK_IDENTITY: [&str; 4] = ["-c", "user.name=OmniDoppelganger", "-c", "user.email=odg@localhost"];
const UNTRACKED_CAP: usize = 500;
const SYNC_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitFile {
    /// Workspace-relative slash-separated path.
    pub path: String,
    pub orig_path: Option<String>,
    /// Short status code: `"M"`, `"A"`, `"D"`, `"R"`, `"C"`, `"U"`, or `"?"`.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepoInfo {
    /// Workspace-relative slash-separated path to this sub-repository (e.g. `"backend"`).
    pub path: String,
    /// Display name (the relative path or folder name).
    pub name: String,
    pub branch: Option<String>,
    /// Total count of conflicted, staged, unstaged, and untracked files.
    pub dirty: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStatus {
    pub is_repo: bool,
    /// When the workspace itself is a container folder with multiple sub-repositories,
    /// the workspace-relative path of the sub-repository this status belongs to.
    pub active_repo: Option<String>,
    /// Discovered sub-repositories in a multi-repo workspace (empty for a normal single-repo workspace).
    pub repos: Vec<GitRepoInfo>,
    pub branch: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub conflicts: Vec<GitFile>,
    pub staged: Vec<GitFile>,
    pub unstaged: Vec<GitFile>,
    pub untracked: Vec<GitFile>,
    pub untracked_truncated: bool,
}

impl GitStatus {
    pub fn not_a_repo() -> Self {
        Self {
            is_repo: false,
            active_repo: None,
            repos: Vec::new(),
            branch: None,
            detached: false,
            upstream: None,
            ahead: 0,
            behind: 0,
            conflicts: Vec::new(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            untracked_truncated: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStage {
    Unstaged,
    Staged,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RichDiffLine {
    pub kind: &'static str,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RichDiffHunk {
    pub header: String,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<RichDiffLine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitDiffView {
    pub path: String,
    pub name: String,
    pub language: Option<String>,
    pub stage: DiffStage,
    pub commit: Option<String>,
    pub binary: bool,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<RichDiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
    pub remote: bool,
    pub upstream: Option<String>,
    /// Checked out in another Git worktree, so it cannot be checked out here simultaneously.
    pub locked_elsewhere: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommitItem {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub timestamp: i64,
    pub refs: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommitResult {
    pub hash: String,
    pub branch: Option<String>,
}

fn untrusted_hooks_config() -> Result<[String; 2], String> {
    #[cfg(unix)]
    {
        Ok(["-c".to_string(), "core.hooksPath=/dev/null".to_string()])
    }
    #[cfg(not(unix))]
    {
        let dir = crate::fsutil::private_temp_dir("empty-hooks")
            .map_err(|error| format!("无法准备安全空钩子目录：{error}"))?;
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let _ = fs::remove_dir_all(&p);
                } else {
                    let _ = fs::remove_file(&p);
                }
            }
        }
        Ok(["-c".to_string(), format!("core.hooksPath={}", dir.display())])
    }
}

fn git_bytes(directory: &Path, config: &[&str], args: &[&str]) -> Result<Vec<u8>, String> {
    git_bytes_with_trust(directory, true, config, args)
}

fn git_bytes_with_trust(
    directory: &Path,
    trusted: bool,
    config: &[&str],
    args: &[&str],
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(directory).args(SAFE_GIT_CONFIG);
    if !trusted {
        let hooks = untrusted_hooks_config()?;
        cmd.args(&hooks);
    }
    cmd.args(config)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|error| format!("无法运行 git：{error}"))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let msg: Vec<String> = [stderr, stdout].into_iter().filter(|s| !s.is_empty()).collect();
    Err(if msg.is_empty() {
        format!("git {} 执行失败", args.first().copied().unwrap_or_default())
    } else {
        msg.join("\n")
    })
}

fn git_text(directory: &Path, config: &[&str], args: &[&str]) -> Result<String, String> {
    let bytes = git_bytes(directory, config, args)?;
    Ok(String::from_utf8_lossy(&bytes).trim_end_matches(['\r', '\n']).to_string())
}

/// Resolve minimal writable Git subdirectories and `(git_dir, common_git_dir)` for `workspace`
/// when confining commands so `<common_git_dir>/{hooks,config,info}` is never exposed as a writable root.
pub fn sandbox_git_paths(workspace: &Path) -> (Vec<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let git_dir = git_text(workspace, &[], &["rev-parse", "--path-format=absolute", "--git-dir"])
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| PathBuf::from(s.trim()).canonicalize().ok());
    let common_dir = git_text(workspace, &[], &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| PathBuf::from(s.trim()).canonicalize().ok());
    let (Some(git_dir), Some(common_dir)) = (git_dir, common_dir) else {
        return (Vec::new(), None, None);
    };
    let ws_canon = workspace.canonicalize().ok();
    let mut writable = Vec::new();
    if git_dir != common_dir {
        writable.push(git_dir.clone());
    }
    if git_dir != common_dir || ws_canon.as_ref().is_none_or(|ws| !common_dir.starts_with(ws)) {
        for sub in ["objects", "refs", "logs"] {
            let p = common_dir.join(sub);
            if p.exists() {
                writable.push(p);
            }
        }
        let packed = common_dir.join("packed-refs");
        if packed.exists() {
            writable.push(packed);
        }
        if git_dir == common_dir {
            let index = common_dir.join("index");
            if index.exists() {
                writable.push(index);
            }
        }
    }
    (writable, Some(git_dir), Some(common_dir))
}

fn identity(directory: &Path) -> &'static [&'static str] {
    if git_text(directory, &[], &["config", "user.email"]).is_ok_and(|s| !s.trim().is_empty()) {
        &[]
    } else {
        &FALLBACK_IDENTITY
    }
}

fn has_head(directory: &Path) -> bool {
    git_bytes(directory, &[], &["rev-parse", "--verify", "HEAD"]).is_ok()
}

/// Returns `(repo_top, workspace_prefix_inside_repo)` when `workspace` is inside a Git repository.
fn repo_context(workspace: &Path) -> Option<(PathBuf, String)> {
    let raw_top = git_text(workspace, &[], &["rev-parse", "--show-toplevel"]).ok()?;
    if raw_top.trim().is_empty() {
        return None;
    }
    let top = PathBuf::from(raw_top.trim()).canonicalize().ok()?;
    let ws = workspace.canonicalize().ok()?;
    let prefix = ws
        .strip_prefix(&top)
        .ok()
        .map(|rel| {
            rel.components()
                .filter_map(|c| match c {
                    Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    Some((top, prefix))
}

/// Convert a repo-root-relative slash path into a workspace-relative path, or `None` if outside `workspace`.
fn to_workspace_rel(repo_rel: &str, ws_prefix: &str) -> Option<String> {
    let clean = repo_rel.replace('\\', "/");
    if ws_prefix.is_empty() {
        return (!clean.is_empty()).then_some(clean);
    }
    let stripped = clean.strip_prefix(ws_prefix)?.strip_prefix('/')?;
    (!stripped.is_empty()).then(|| stripped.to_string())
}

/// Convert a validated workspace-relative path into a repo-root-relative path.
fn to_repo_rel(workspace: &Path, ws_prefix: &str, rel_path: &str) -> Result<String, String> {
    let (_, clean) = explorer::resolve_confined(workspace, rel_path, false, false)?;
    if ws_prefix.is_empty() {
        Ok(clean)
    } else {
        Ok(format!("{ws_prefix}/{clean}"))
    }
}

const SKIP_SCAN_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", ".venv", "venv", "__pycache__", "vendor"];
const MAX_REPO_SCAN_DEPTH: usize = 2;
const MAX_SUB_REPOS: usize = 32;

/// Discover Git repositories located in subdirectories of `workspace` (up to depth 2).
/// Does not descend inside a discovered repository.
pub fn discover_repos(workspace: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        if depth > MAX_REPO_SCAN_DEPTH || out.len() >= MAX_SUB_REPOS {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else { return };
        let mut subdirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        for sub in subdirs {
            if out.len() >= MAX_SUB_REPOS {
                break;
            }
            let Some(name) = sub.file_name().and_then(|n| n.to_str()) else { continue };
            if name.starts_with('.') || SKIP_SCAN_DIRS.contains(&name) {
                continue;
            }
            if sub.join(".git").exists() {
                if let Ok(rel) = sub.strip_prefix(base) {
                    let slash = rel
                        .components()
                        .filter_map(|c| match c {
                            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    if !slash.is_empty() {
                        out.push(slash);
                    }
                }
            } else if depth < MAX_REPO_SCAN_DEPTH {
                walk(base, &sub, depth + 1, out);
            }
        }
    }
    let mut found = Vec::new();
    walk(workspace, workspace, 1, &mut found);
    found
}

/// Resolve an optional workspace-relative sub-repository path safely inside `workspace`.
pub fn resolve_repo_dir(workspace: &Path, repo: Option<&str>) -> Result<PathBuf, String> {
    let Some(rel) = repo.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(workspace.to_path_buf());
    };
    let (disk, _) = explorer::resolve_confined(workspace, rel, false, false)?;
    if !disk.is_dir() {
        return Err(format!("子仓库目录不存在：{rel}"));
    }
    Ok(disk)
}

/// Inspect repository branch, ahead/behind counts, conflicts, staged, unstaged, and untracked files.
/// Automatically discovers subdirectory Git repositories when `workspace` itself is not a Git repository.
pub fn status(workspace: &Path) -> Result<GitStatus, String> {
    status_for(workspace, None)
}

/// Inspect Git status for `workspace` or a specific sub-repository `repo` within `workspace`.
pub fn status_for(workspace: &Path, repo: Option<&str>) -> Result<GitStatus, String> {
    status_for_with_trust(workspace, repo, false)
}

/// Inspect Git status with explicit workspace trust (`!trusted` disables repository hooks).
pub fn status_for_with_trust(workspace: &Path, repo: Option<&str>, trusted: bool) -> Result<GitStatus, String> {
    let explicit = repo.map(str::trim).filter(|s| !s.is_empty());
    if explicit.is_none() && repo_context(workspace).is_some() {
        return status_single(workspace, trusted);
    }

    let discovered = discover_repos(workspace);
    if discovered.is_empty() {
        if let Some(rel) = explicit {
            let dir = resolve_repo_dir(workspace, Some(rel))?;
            let mut st = status_single(&dir, trusted)?;
            st.active_repo = Some(rel.to_string());
            return Ok(st);
        }
        return Ok(GitStatus::not_a_repo());
    }

    let mut summaries = Vec::with_capacity(discovered.len());
    let mut statuses: Vec<(String, GitStatus)> = Vec::with_capacity(discovered.len());
    for rel in &discovered {
        let dir = workspace.join(rel);
        if let Ok(st) = status_single(&dir, trusted)
            && st.is_repo
        {
            let dirty = st.conflicts.len() + st.staged.len() + st.unstaged.len() + st.untracked.len();
            summaries.push(GitRepoInfo {
                path: rel.clone(),
                name: rel.clone(),
                branch: st.branch.clone(),
                dirty,
            });
            statuses.push((rel.clone(), st));
        }
    }
    if statuses.is_empty() {
        return Ok(GitStatus::not_a_repo());
    }

    let chosen_idx = explicit
        .and_then(|want| statuses.iter().position(|(rel, _)| rel == want))
        .or_else(|| summaries.iter().position(|r| r.dirty > 0))
        .unwrap_or(0);
    let (chosen_path, mut chosen_status) = statuses.swap_remove(chosen_idx);
    chosen_status.active_repo = Some(chosen_path);
    chosen_status.repos = summaries;
    Ok(chosen_status)
}

fn status_single(workspace: &Path, trusted: bool) -> Result<GitStatus, String> {
    let Some((top, ws_prefix)) = repo_context(workspace) else {
        return Ok(GitStatus::not_a_repo());
    };
    let raw = git_bytes_with_trust(
        &top,
        trusted,
        &["-c", "core.quotepath=false"],
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
    )?;

    let mut out = GitStatus {
        is_repo: true,
        active_repo: None,
        repos: Vec::new(),
        branch: None,
        detached: false,
        upstream: None,
        ahead: 0,
        behind: 0,
        conflicts: Vec::new(),
        staged: Vec::new(),
        unstaged: Vec::new(),
        untracked: Vec::new(),
        untracked_truncated: false,
    };

    let tokens: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut idx = 0usize;
    while idx < tokens.len() {
        let token = String::from_utf8_lossy(tokens[idx]);
        idx += 1;
        if token.is_empty() {
            continue;
        }
        if let Some(head) = token.strip_prefix("# branch.head ") {
            let head = head.trim();
            if head == "(detached)" {
                out.detached = true;
                out.branch = git_text(&top, &[], &["rev-parse", "--short", "HEAD"]).ok();
            } else {
                out.branch = Some(head.to_string());
            }
            continue;
        }
        if let Some(up) = token.strip_prefix("# branch.upstream ") {
            out.upstream = Some(up.trim().to_string());
            continue;
        }
        if let Some(ab) = token.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(a) = part.strip_prefix('+') {
                    out.ahead = a.parse().unwrap_or(0);
                } else if let Some(b) = part.strip_prefix('-') {
                    out.behind = b.parse().unwrap_or(0);
                }
            }
            continue;
        }
        if token.starts_with('#') {
            continue;
        }

        if let Some(rest) = token.strip_prefix("? ") {
            if let Some(path) = to_workspace_rel(rest, &ws_prefix) {
                if out.untracked.len() < UNTRACKED_CAP {
                    out.untracked.push(GitFile { path, orig_path: None, status: "?".to_string() });
                } else {
                    out.untracked_truncated = true;
                }
            }
            continue;
        }

        if token.starts_with("u ") {
            // Unmerged entry: `u <xy> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
            let parts: Vec<&str> = token.splitn(11, ' ').collect();
            if parts.len() == 11
                && let Some(path) = to_workspace_rel(parts[10], &ws_prefix)
            {
                out.conflicts.push(GitFile { path, orig_path: None, status: "U".to_string() });
            }
            continue;
        }

        if token.starts_with("1 ") {
            // Ordinary entry: `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
            let parts: Vec<&str> = token.splitn(9, ' ').collect();
            if parts.len() == 9 {
                let xy: Vec<char> = parts[1].chars().collect();
                let x = xy.first().copied().unwrap_or('.');
                let y = xy.get(1).copied().unwrap_or('.');
                if let Some(path) = to_workspace_rel(parts[8], &ws_prefix) {
                    if x != '.' {
                        out.staged.push(GitFile { path: path.clone(), orig_path: None, status: x.to_string() });
                    }
                    if y != '.' {
                        out.unstaged.push(GitFile { path, orig_path: None, status: y.to_string() });
                    }
                }
            }
            continue;
        }

        if token.starts_with("2 ") {
            // Renamed/copied entry: `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>` followed by `<origPath>\0`
            let orig_raw = if idx < tokens.len() {
                let s = String::from_utf8_lossy(tokens[idx]).into_owned();
                idx += 1;
                Some(s)
            } else {
                None
            };
            let parts: Vec<&str> = token.splitn(10, ' ').collect();
            if parts.len() == 10 {
                let xy: Vec<char> = parts[1].chars().collect();
                let x = xy.first().copied().unwrap_or('.');
                let y = xy.get(1).copied().unwrap_or('.');
                let orig_path = orig_raw.as_deref().and_then(|o| to_workspace_rel(o, &ws_prefix));
                if let Some(path) = to_workspace_rel(parts[9], &ws_prefix) {
                    if x != '.' {
                        out.staged.push(GitFile { path: path.clone(), orig_path: orig_path.clone(), status: x.to_string() });
                    }
                    if y != '.' {
                        out.unstaged.push(GitFile { path, orig_path, status: y.to_string() });
                    }
                }
            }
        }
    }

    Ok(out)
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    // Format: `@@ -old_start[,old_count] +new_start[,new_count] @@ ...`
    let after = line.strip_prefix("@@ -")?;
    let end = after.find(" @@")?;
    let spec = &after[..end];
    let (old_part, new_part) = spec.split_once(" +")?;
    let parse_range = |s: &str| -> Option<(usize, usize)> {
        if let Some((start, count)) = s.split_once(',') {
            Some((start.parse().ok()?, count.parse().ok()?))
        } else {
            Some((s.parse().ok()?, 1))
        }
    };
    let (old_start, old_lines) = parse_range(old_part)?;
    let (new_start, new_lines) = parse_range(new_part)?;
    Some((old_start, old_lines, new_start, new_lines))
}

fn parse_unified_diff(raw: &str) -> (bool, usize, usize, Vec<RichDiffHunk>) {
    let mut binary = false;
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut hunks: Vec<RichDiffHunk> = Vec::new();
    let mut old_no = 0usize;
    let mut new_no = 0usize;

    for raw_line in raw.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            binary = true;
            continue;
        }
        if let Some((old_start, old_lines, new_start, new_lines)) = parse_hunk_header(line) {
            old_no = old_start;
            new_no = new_start;
            hunks.push(RichDiffHunk {
                header: line.to_string(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(current) = hunks.last_mut() else { continue };
        if let Some(rest) = line.strip_prefix('+') {
            added += 1;
            current.lines.push(RichDiffLine {
                kind: "+",
                old_line: None,
                new_line: Some(new_no),
                text: rest.to_string(),
            });
            new_no += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            removed += 1;
            current.lines.push(RichDiffLine {
                kind: "-",
                old_line: Some(old_no),
                new_line: None,
                text: rest.to_string(),
            });
            old_no += 1;
        } else if let Some(rest) = line.strip_prefix(' ') {
            current.lines.push(RichDiffLine {
                kind: " ",
                old_line: Some(old_no),
                new_line: Some(new_no),
                text: rest.to_string(),
            });
            old_no += 1;
            new_no += 1;
        }
    }
    (binary, added, removed, hunks)
}

fn validate_ref(reference: &str) -> Result<&str, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') || trimmed.contains('\0') {
        return Err("无效的 Git 引用或分支名称。".to_string());
    }
    Ok(trimmed)
}

/// Compute structured diff hunks for an unstaged file, staged file, or a file in a historical commit.
pub fn diff(workspace: &Path, rel_path: &str, stage: DiffStage, commit: Option<&str>) -> Result<GitDiffView, String> {
    diff_with_trust(workspace, rel_path, stage, commit, false)
}

/// Compute structured diff hunks with explicit workspace trust (`!trusted` disables textconv filters & hooks).
pub fn diff_with_trust(
    workspace: &Path,
    rel_path: &str,
    stage: DiffStage,
    commit: Option<&str>,
    trusted: bool,
) -> Result<GitDiffView, String> {
    let Some((top, ws_prefix)) = repo_context(workspace) else {
        let clean = rel_path.replace('\\', "/");
        for sub in discover_repos(workspace) {
            if let Some(inner) = clean.strip_prefix(&format!("{sub}/")) {
                let mut view = diff_with_trust(&workspace.join(&sub), inner, stage, commit, trusted)?;
                view.path = clean;
                return Ok(view);
            }
        }
        return Err("当前目录不在 Git 仓库中。".to_string());
    };
    let (disk_file, clean_rel) = explorer::resolve_confined(workspace, rel_path, false, false)?;
    let repo_rel = to_repo_rel(workspace, &ws_prefix, &clean_rel)?;
    let name = Path::new(&clean_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| clean_rel.clone());
    let language = explorer::detect_language(Path::new(&clean_rel)).map(str::to_string);

    match stage {
        DiffStage::Unstaged => {
            // Check whether the file is untracked; if so, render its full contents as an added hunk.
            let tracked = git_bytes_with_trust(&top, trusted, &[], &["ls-files", "--error-unmatch", "--", &repo_rel]).is_ok();
            if !tracked && disk_file.is_file() {
                let meta = fs::metadata(&disk_file).map_err(|e| format!("无法读取文件：{e}"))?;
                if meta.len() > MAX_TEXT_VIEW_BYTES {
                    return Ok(GitDiffView {
                        path: clean_rel,
                        name,
                        language,
                        stage,
                        commit: None,
                        binary: true,
                        added: 0,
                        removed: 0,
                        hunks: Vec::new(),
                    });
                }
                let bytes = fs::read(&disk_file).map_err(|e| format!("无法读取文件：{e}"))?;
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    return Ok(GitDiffView {
                        path: clean_rel,
                        name,
                        language,
                        stage,
                        commit: None,
                        binary: true,
                        added: 0,
                        removed: 0,
                        hunks: Vec::new(),
                    });
                }
                let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
                let lines: Vec<RichDiffLine> = text
                    .lines()
                    .enumerate()
                    .map(|(i, l)| RichDiffLine { kind: "+", old_line: None, new_line: Some(i + 1), text: l.to_string() })
                    .collect();
                let count = lines.len();
                let hunks = if count == 0 {
                    Vec::new()
                } else {
                    vec![RichDiffHunk {
                        header: format!("@@ -0,0 +1,{count} @@ (新文件)"),
                        old_start: 0,
                        old_lines: 0,
                        new_start: 1,
                        new_lines: count,
                        lines,
                    }]
                };
                return Ok(GitDiffView {
                    path: clean_rel,
                    name,
                    language,
                    stage,
                    commit: None,
                    binary: false,
                    added: count,
                    removed: 0,
                    hunks,
                });
            }
            let mut args = vec!["diff", "-U4", "--no-color", "--no-ext-diff"];
            if !trusted {
                args.push("--no-textconv");
            }
            args.extend(["--", &repo_rel]);
            let raw_bytes = git_bytes_with_trust(&top, trusted, &["-c", "core.quotepath=false"], &args)?;
            let (binary, added, removed, hunks) = parse_unified_diff(&String::from_utf8_lossy(&raw_bytes));
            Ok(GitDiffView { path: clean_rel, name, language, stage, commit: None, binary, added, removed, hunks })
        }
        DiffStage::Staged => {
            let mut args = vec!["diff", "--cached", "-U4", "--no-color", "--no-ext-diff"];
            if !trusted {
                args.push("--no-textconv");
            }
            args.extend(["--", &repo_rel]);
            let raw_bytes = git_bytes_with_trust(&top, trusted, &["-c", "core.quotepath=false"], &args)?;
            let (binary, added, removed, hunks) = parse_unified_diff(&String::from_utf8_lossy(&raw_bytes));
            Ok(GitDiffView { path: clean_rel, name, language, stage, commit: None, binary, added, removed, hunks })
        }
        DiffStage::Commit => {
            let rev = validate_ref(commit.ok_or("缺少提交哈希。")?)?;
            let mut args = vec!["show", "--format=", "-U4", "--no-color", "--no-ext-diff"];
            if !trusted {
                args.push("--no-textconv");
            }
            args.extend([rev, "--", &repo_rel]);
            let raw_bytes = git_bytes_with_trust(&top, trusted, &["-c", "core.quotepath=false"], &args)?;
            let (binary, added, removed, hunks) = parse_unified_diff(&String::from_utf8_lossy(&raw_bytes));
            Ok(GitDiffView {
                path: clean_rel,
                name,
                language,
                stage,
                commit: Some(rev.to_string()),
                binary,
                added,
                removed,
                hunks,
            })
        }
    }
}

/// Stage specific workspace-relative paths, or everything in `workspace` when `paths` is empty.
pub fn stage(workspace: &Path, paths: &[String]) -> Result<(), String> {
    stage_with_trust(workspace, paths, false)
}

/// Stage paths with explicit workspace trust (`!trusted` disables repository hooks).
pub fn stage_with_trust(workspace: &Path, paths: &[String], trusted: bool) -> Result<(), String> {
    let (top, ws_prefix) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    if paths.is_empty() {
        let scope = if ws_prefix.is_empty() { "." } else { ws_prefix.as_str() };
        git_bytes_with_trust(&top, trusted, &[], &["add", "-A", "--", scope])?;
        return Ok(());
    }
    let repo_paths: Vec<String> = paths.iter().map(|p| to_repo_rel(workspace, &ws_prefix, p)).collect::<Result<_, _>>()?;
    let mut args: Vec<&str> = vec!["add", "-A", "--"];
    args.extend(repo_paths.iter().map(String::as_str));
    git_bytes_with_trust(&top, trusted, &[], &args)?;
    Ok(())
}

/// Unstage specific workspace-relative paths, or everything in `workspace` when `paths` is empty.
pub fn unstage(workspace: &Path, paths: &[String]) -> Result<(), String> {
    unstage_with_trust(workspace, paths, false)
}

/// Unstage paths with explicit workspace trust (`!trusted` disables repository hooks).
pub fn unstage_with_trust(workspace: &Path, paths: &[String], trusted: bool) -> Result<(), String> {
    let (top, ws_prefix) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let head = has_head(&top);
    if paths.is_empty() {
        let scope = if ws_prefix.is_empty() { "." } else { ws_prefix.as_str() };
        if head {
            git_bytes_with_trust(&top, trusted, &[], &["reset", "-q", "HEAD", "--", scope])?;
        } else {
            git_bytes_with_trust(&top, trusted, &[], &["rm", "-q", "--cached", "-r", "--", scope])?;
        }
        return Ok(());
    }
    let repo_paths: Vec<String> = paths.iter().map(|p| to_repo_rel(workspace, &ws_prefix, p)).collect::<Result<_, _>>()?;
    let mut args: Vec<&str> = if head { vec!["reset", "-q", "HEAD", "--"] } else { vec!["rm", "-q", "--cached", "-r", "--"] };
    args.extend(repo_paths.iter().map(String::as_str));
    git_bytes_with_trust(&top, trusted, &[], &args)?;
    Ok(())
}

/// Discard unstaged working-tree modifications or remove untracked files.
pub fn discard(workspace: &Path, paths: &[String]) -> Result<(), String> {
    discard_with_trust(workspace, paths, false)
}

/// Discard unstaged changes with explicit workspace trust (`!trusted` disables repository hooks).
pub fn discard_with_trust(workspace: &Path, paths: &[String], trusted: bool) -> Result<(), String> {
    let (top, ws_prefix) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    if paths.is_empty() {
        return Ok(());
    }
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    for rel in paths {
        let repo_rel = to_repo_rel(workspace, &ws_prefix, rel)?;
        if git_bytes_with_trust(&top, trusted, &[], &["ls-files", "--error-unmatch", "--", &repo_rel]).is_ok() {
            tracked.push(repo_rel);
        } else {
            untracked.push(rel.clone());
        }
    }
    if !tracked.is_empty() {
        let mut args: Vec<&str> = vec!["checkout", "--"];
        args.extend(tracked.iter().map(String::as_str));
        git_bytes_with_trust(&top, trusted, &[], &args)?;
    }
    for rel in &untracked {
        explorer::delete_entry(workspace, rel)?;
    }
    Ok(())
}

/// Discard and revert ALL uncommitted changes inside `workspace`:
/// unstages staged edits/additions, restores tracked files to HEAD, and removes untracked files/directories.
pub fn discard_all(workspace: &Path) -> Result<(), String> {
    discard_all_with_trust(workspace, false)
}

/// Discard all uncommitted changes with explicit workspace trust (`!trusted` disables repository hooks).
pub fn discard_all_with_trust(workspace: &Path, trusted: bool) -> Result<(), String> {
    let (top, ws_prefix) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let scope = if ws_prefix.is_empty() { "." } else { ws_prefix.as_str() };
    let has_head = git_bytes_with_trust(&top, trusted, &[], &["rev-parse", "--verify", "HEAD"]).is_ok();
    if has_head {
        git_bytes_with_trust(&top, trusted, &[], &["reset", "HEAD", "--", scope])?;
        // Only run checkout if any tracked files exist under `scope`.
        if !git_bytes_with_trust(&top, trusted, &[], &["ls-files", "--", scope]).unwrap_or_default().is_empty() {
            git_bytes_with_trust(&top, trusted, &[], &["checkout", "--", scope])?;
        }
    } else {
        let _ = git_bytes_with_trust(&top, trusted, &[], &["rm", "-rf", "--cached", "--ignore-unmatch", "--", scope]);
    }
    git_bytes_with_trust(&top, trusted, &[], &["clean", "-fd", "--", scope])?;
    Ok(())
}

/// Commit staged changes (optionally staging all tracked/untracked changes first when `stage_all` is true).
pub fn commit(workspace: &Path, message: &str, stage_all: bool) -> Result<GitCommitResult, String> {
    commit_with_trust(workspace, message, stage_all, false)
}

/// Commit staged changes with explicit workspace trust (`!trusted` disables `pre-commit` / `commit-msg` / `post-commit` hooks).
pub fn commit_with_trust(
    workspace: &Path,
    message: &str,
    stage_all: bool,
    trusted: bool,
) -> Result<GitCommitResult, String> {
    let (top, _) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let msg = message.trim();
    if msg.is_empty() {
        return Err("请填写提交说明。".to_string());
    }
    if stage_all {
        stage_with_trust(workspace, &[], trusted)?;
    }
    if git_bytes_with_trust(&top, trusted, &[], &["diff", "--cached", "--quiet"]).is_ok() {
        return Err("暂存区还是空的，请先勾选暂存要提交的改动。".to_string());
    }
    git_bytes_with_trust(&top, trusted, identity(&top), &["commit", "-m", msg])?;
    let hash = git_text(&top, &[], &["rev-parse", "--short", "HEAD"])?;
    let branch = git_text(&top, &[], &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| b != "HEAD");
    Ok(GitCommitResult { hash, branch })
}

/// List local and remote-only branches in the repository.
pub fn branches(workspace: &Path) -> Result<Vec<GitBranch>, String> {
    let Some((top, _)) = repo_context(workspace) else {
        return Ok(Vec::new());
    };
    let text = git_text(
        &top,
        &[],
        &[
            "for-each-ref",
            "--format=%(refname)%09%(refname:short)%09%(HEAD)%09%(upstream:short)%09%(worktreepath)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let top_canon = top.canonicalize().ok();
    let mut locals: Vec<GitBranch> = Vec::new();
    let mut local_names = std::collections::HashSet::new();
    let mut remotes: Vec<GitBranch> = Vec::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        let full = cols[0];
        let short = cols[1];
        let current = cols[2] == "*";
        let upstream = (!cols[3].is_empty()).then(|| cols[3].to_string());
        let wt = cols[4].trim();
        let locked_elsewhere = !current
            && !wt.is_empty()
            && Path::new(wt).canonicalize().ok() != top_canon;

        if full.starts_with("refs/heads/") {
            local_names.insert(short.to_string());
            locals.push(GitBranch { name: short.to_string(), current, remote: false, upstream, locked_elsewhere });
        } else if full.starts_with("refs/remotes/") && !short.ends_with("/HEAD") && short.contains('/') {
            remotes.push(GitBranch { name: short.to_string(), current: false, remote: true, upstream: None, locked_elsewhere: false });
        }
    }

    // Include remote branches that do not already have a local branch of the same name.
    for rem in remotes {
        let bare = rem.name.split_once('/').map(|(_, b)| b).unwrap_or(&rem.name);
        if !local_names.contains(bare) {
            locals.push(rem);
        }
    }
    locals.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.remote.cmp(&b.remote)).then_with(|| a.name.cmp(&b.name)));
    Ok(locals)
}

/// Switch to an existing branch (or remote tracking branch) or create a new local branch.
pub fn checkout(workspace: &Path, branch: &str, create: bool) -> Result<String, String> {
    checkout_with_trust(workspace, branch, create, false)
}

/// Switch or create a branch with explicit workspace trust (`!trusted` disables `post-checkout` hooks).
pub fn checkout_with_trust(workspace: &Path, branch: &str, create: bool, trusted: bool) -> Result<String, String> {
    let (top, _) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let name = validate_ref(branch)?;
    if create {
        git_bytes_with_trust(&top, trusted, &[], &["checkout", "-b", name])?;
        return Ok(name.to_string());
    }
    if git_bytes_with_trust(&top, trusted, &[], &["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok() {
        git_bytes_with_trust(&top, trusted, &[], &["checkout", name])?;
        return Ok(name.to_string());
    }
    // Remote branch like `origin/feature` -> create local tracking branch `feature`.
    if git_bytes_with_trust(&top, trusted, &[], &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{name}")]).is_ok() {
        git_bytes_with_trust(&top, trusted, &[], &["checkout", "--track", name])?;
        return git_text(&top, &[], &["rev-parse", "--abbrev-ref", "HEAD"]);
    }
    git_bytes_with_trust(&top, trusted, &[], &["checkout", name])?;
    Ok(name.to_string())
}

/// Recent commits on the current HEAD. Returns empty list when the repository has no commits yet.
pub fn log(workspace: &Path, limit: usize) -> Result<Vec<GitCommitItem>, String> {
    log_with_options(workspace, limit, false)
}

/// Read commit history in topological order, optionally across all local & remote refs for a branch graph.
pub fn log_with_options(workspace: &Path, limit: usize, all_branches: bool) -> Result<Vec<GitCommitItem>, String> {
    let Some((top, _)) = repo_context(workspace) else {
        return Ok(Vec::new());
    };
    if !has_head(&top) {
        return Ok(Vec::new());
    }
    let cap = limit.clamp(1, 200).to_string();
    let mut args = vec!["log", "--topo-order", "-n", &cap, "--format=%H%x09%h%x09%P%x09%an%x09%at%x09%D%x09%s"];
    if all_branches {
        args.insert(1, "--all");
    }
    let text = git_text(&top, &[], &args)?;
    let mut items = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.splitn(7, '\t').collect();
        if cols.len() < 7 {
            continue;
        }
        let parents = cols[2].split_whitespace().map(str::to_string).collect();
        items.push(GitCommitItem {
            hash: cols[0].to_string(),
            short_hash: cols[1].to_string(),
            parents,
            author: cols[3].to_string(),
            timestamp: cols[4].parse().unwrap_or(0),
            refs: cols[5].to_string(),
            subject: cols[6].to_string(),
        });
    }
    Ok(items)
}

/// Files touched by a specific commit, relative to `workspace`.
pub fn commit_files(workspace: &Path, commit: &str) -> Result<Vec<GitFile>, String> {
    let (top, ws_prefix) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let rev = validate_ref(commit)?;
    let bytes = git_bytes(
        &top,
        &["-c", "core.quotepath=false"],
        &["diff-tree", "--root", "--no-commit-id", "--name-status", "-r", "-z", rev],
    )?;
    let tokens: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    let mut files = Vec::new();
    let mut idx = 0usize;
    while idx + 1 < tokens.len() {
        let status_raw = String::from_utf8_lossy(tokens[idx]);
        idx += 1;
        if status_raw.is_empty() {
            continue;
        }
        let code = status_raw.chars().next().unwrap_or('M');
        if code == 'R' || code == 'C' {
            if idx + 1 >= tokens.len() {
                break;
            }
            let old_p = String::from_utf8_lossy(tokens[idx]).into_owned();
            let new_p = String::from_utf8_lossy(tokens[idx + 1]).into_owned();
            idx += 2;
            if let Some(path) = to_workspace_rel(&new_p, &ws_prefix) {
                files.push(GitFile {
                    path,
                    orig_path: to_workspace_rel(&old_p, &ws_prefix),
                    status: code.to_string(),
                });
            }
        } else {
            let p = String::from_utf8_lossy(tokens[idx]).into_owned();
            idx += 1;
            if let Some(path) = to_workspace_rel(&p, &ws_prefix) {
                files.push(GitFile { path, orig_path: None, status: code.to_string() });
            }
        }
    }
    Ok(files)
}

/// Run `fetch`, `pull`, or `push` asynchronously with interactive prompts disabled and a safety timeout.
pub async fn sync(workspace: &Path, action: &str) -> Result<String, String> {
    sync_with_trust(workspace, action, false).await
}

/// Run `fetch`, `pull`, or `push` with explicit workspace trust (`!trusted` disables `pre-push` / `post-merge` hooks).
pub async fn sync_with_trust(workspace: &Path, action: &str, trusted: bool) -> Result<String, String> {
    let (top, _) = repo_context(workspace).ok_or("当前目录不在 Git 仓库中。")?;
    let args: Vec<&str> = match action {
        "fetch" => vec!["fetch", "--prune"],
        "pull" => vec!["pull", "--ff-only"],
        "push" => {
            // If current branch has no upstream yet, push with `-u origin HEAD`.
            let has_up = git_text(&top, &[], &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
            if has_up {
                vec!["push"]
            } else {
                vec!["push", "-u", "origin", "HEAD"]
            }
        }
        _ => return Err(format!("不支持的同步操作：{action}")),
    };

    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(&top).args(SAFE_GIT_CONFIG);
    if !trusted {
        let hooks = untrusted_hooks_config()?;
        cmd.args(&hooks);
    }
    cmd.args(&args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }

    let out = tokio::time::timeout(SYNC_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {action} 超时（超过 {} 秒），请检查网络或远端凭据。", SYNC_TIMEOUT.as_secs()))?
        .map_err(|error| format!("无法执行 git {action}：{error}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        let note = [stdout, stderr].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        Ok(note)
    } else {
        let msg = [stderr, stdout].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        Err(if msg.is_empty() { format!("git {action} 失败") } else { msg })
    }
}

/// Auto-populated Pull Request draft defaults derived from the current branch and its commits.
#[derive(Debug, Clone, Serialize)]
pub struct PrDefaults {
    pub branch: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub has_gh: bool,
}

/// Outcome of creating a Pull Request via `gh pr create`.
#[derive(Debug, Clone, Serialize)]
pub struct PullRequestResult {
    pub url: String,
    pub branch: String,
}

/// Inspect the current branch and commits ahead of the default base branch to prefill a PR form.
pub fn pr_defaults(directory: &Path) -> Result<PrDefaults, String> {
    let (top, _) = repo_context(directory).ok_or_else(|| "当前洞府不是 Git 仓库".to_string())?;
    let branch = git_text(&top, &[], &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("当前处于游离 HEAD 状态，请先切到具体分支再创建 PR。".to_string());
    }
    let base = git_text(&top, &[], &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|s| s.trim().strip_prefix("origin/").unwrap_or(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if git_text(&top, &[], &["show-ref", "--verify", "--quiet", "refs/heads/main"]).is_ok() {
                "main".to_string()
            } else if git_text(&top, &[], &["show-ref", "--verify", "--quiet", "refs/heads/master"]).is_ok() {
                "master".to_string()
            } else {
                "main".to_string()
            }
        });
    let range = format!("{base}..HEAD");
    let subjects: Vec<String> = git_text(&top, &[], &["log", &range, "--max-count=20", "--format=%s"])
        .ok()
        .map(|out| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .filter(|list: &Vec<String>| !list.is_empty())
        .unwrap_or_else(|| {
            git_text(&top, &[], &["log", "-n", "1", "--format=%s"])
                .ok()
                .map(|out| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
                .unwrap_or_default()
        });
    let title = subjects.first().cloned().unwrap_or_else(|| branch.clone());
    let body = if subjects.is_empty() {
        String::new()
    } else {
        let mut lines = vec!["## 改动摘要".to_string(), String::new()];
        for subj in &subjects {
            lines.push(format!("- {subj}"));
        }
        lines.join("\n")
    };
    let has_gh = crate::cli_agents::find_named_binary("ODG_GH_BIN", "gh", &[]).is_some();
    Ok(PrDefaults { branch, base, title, body, has_gh })
}

/// Push the current branch if needed and create a GitHub Pull Request via `gh pr create`.
pub async fn create_pr(directory: &Path, title: &str, body: &str, base: Option<&str>, draft: bool) -> Result<PullRequestResult, String> {
    create_pr_with_trust(directory, title, body, base, draft, false).await
}

/// Push the current branch with explicit workspace trust and create a GitHub Pull Request via `gh pr create`.
pub async fn create_pr_with_trust(
    directory: &Path,
    title: &str,
    body: &str,
    base: Option<&str>,
    draft: bool,
    trusted: bool,
) -> Result<PullRequestResult, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("PR 标题不能为空。".to_string());
    }
    let gh = crate::cli_agents::find_named_binary("ODG_GH_BIN", "gh", &[])
        .ok_or_else(|| "未找到 GitHub CLI (gh)，请先安装并运行 `gh auth login` 完成登录。".to_string())?;
    let (top, _) = repo_context(directory).ok_or_else(|| "当前洞府不是 Git 仓库".to_string())?;
    let branch = git_text(&top, &[], &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("当前处于游离 HEAD 状态，请先切到具体分支再创建 PR。".to_string());
    }
    // Ensure the branch is pushed to origin before creating the PR.
    sync_with_trust(&top, "push", trusted).await?;

    let mut args = vec!["pr".to_string(), "create".to_string(), "--title".to_string(), title.to_string(), "--body".to_string(), body.to_string()];
    if let Some(base) = base.map(str::trim).filter(|b| !b.is_empty() && *b != branch) {
        args.push("--base".to_string());
        args.push(base.to_string());
    }
    if draft {
        args.push("--draft".to_string());
    }
    let mut cmd = tokio::process::Command::new(&gh);
    cmd.args(&args)
        .current_dir(&top)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);

    let out = tokio::time::timeout(SYNC_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("创建 PR 超时（超过 {} 秒）。", SYNC_TIMEOUT.as_secs()))?
        .map_err(|error| format!("无法执行 gh pr create：{error}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        let msg = [stderr, stdout].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        return Err(if msg.is_empty() { "gh pr create 执行失败".to_string() } else { msg });
    }
    let url = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .unwrap_or(stdout.as_str())
        .to_string();
    Ok(PullRequestResult { url, branch })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        git_bytes(&root, &[], &["init", "--quiet", "--initial-branch", "main"]).unwrap();
        fs::write(root.join("src/hello.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        git_bytes(&root, &[], &["add", "-A"]).unwrap();
        git_bytes(&root, &FALLBACK_IDENTITY, &["commit", "--quiet", "-m", "initial commit"]).unwrap();
        (dir, root)
    }

    #[test]
    fn non_repo_returns_clean_not_a_repo_status() {
        let dir = tempfile::tempdir().unwrap();
        if repo_context(dir.path()).is_none() {
            let st = status(dir.path()).unwrap();
            assert!(!st.is_repo);
        }
    }

    #[test]
    fn tracks_unicode_paths_staging_diffs_commits_branches_and_discard() {
        let (_dir, root) = init_repo();

        // Modify tracked file and add Chinese-named untracked file.
        fs::write(root.join("src/hello.rs"), "fn main() {\n    println!(\"hello world\");\n}\n").unwrap();
        fs::write(root.join("说明文档.md"), "# 标题\n内容第一行\n").unwrap();

        let st = status(&root).unwrap();
        assert!(st.is_repo);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.unstaged.len(), 1);
        assert_eq!(st.unstaged[0].path, "src/hello.rs");
        assert_eq!(st.untracked.len(), 1);
        assert_eq!(st.untracked[0].path, "说明文档.md");

        // Unstaged diff on tracked file has old & new line numbers.
        let mod_diff = diff(&root, "src/hello.rs", DiffStage::Unstaged, None).unwrap();
        assert_eq!(mod_diff.added, 1);
        assert_eq!(mod_diff.removed, 1);
        assert_eq!(mod_diff.hunks.len(), 1);

        // Unstaged diff on untracked file synthesizes added lines.
        let new_diff = diff(&root, "说明文档.md", DiffStage::Unstaged, None).unwrap();
        assert_eq!(new_diff.added, 2);
        assert_eq!(new_diff.removed, 0);

        // Stage only the Chinese markdown file, then unstage, then stage both and commit.
        stage(&root, &["说明文档.md".to_string()]).unwrap();
        let st2 = status(&root).unwrap();
        assert_eq!(st2.staged.len(), 1);
        assert_eq!(st2.staged[0].path, "说明文档.md");
        assert!(st2.untracked.is_empty());

        unstage(&root, &["说明文档.md".to_string()]).unwrap();
        assert!(status(&root).unwrap().staged.is_empty());

        stage(&root, &[]).unwrap();
        let committed = commit(&root, "Update greeting and add docs", false).unwrap();
        assert_eq!(committed.branch.as_deref(), Some("main"));
        assert!(status(&root).unwrap().staged.is_empty());

        // History & commit diff inspection.
        let history = log(&root, 10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].subject, "Update greeting and add docs");
        let touched = commit_files(&root, &history[0].hash).unwrap();
        assert_eq!(touched.len(), 2);
        let hist_diff = diff(&root, "src/hello.rs", DiffStage::Commit, Some(&history[0].hash)).unwrap();
        assert_eq!(hist_diff.added, 1);

        // Create & switch branch, then discard a dirty edit.
        assert_eq!(checkout(&root, "feat/workbench", true).unwrap(), "feat/workbench");
        assert_eq!(branches(&root).unwrap().iter().find(|b| b.current).unwrap().name, "feat/workbench");

        fs::write(root.join("src/hello.rs"), "dirty\n").unwrap();
        fs::write(root.join("temp.txt"), "throwaway\n").unwrap();
        discard(&root, &["src/hello.rs".to_string(), "temp.txt".to_string()]).unwrap();
        let clean = status(&root).unwrap();
        assert!(clean.unstaged.is_empty() && clean.untracked.is_empty());

        // Discard ALL uncommitted changes at once (staged edit, staged new file, unstaged edit, untracked dir).
        fs::write(root.join("src/hello.rs"), "staged-change\n").unwrap();
        fs::write(root.join("staged-new.txt"), "new\n").unwrap();
        stage(&root, &["src/hello.rs".to_string(), "staged-new.txt".to_string()]).unwrap();
        fs::write(root.join("说明文档.md"), "unstaged-change\n").unwrap();
        fs::create_dir_all(root.join("doc/screenshots")).unwrap();
        fs::write(root.join("doc/screenshots/01.png"), "png").unwrap();
        discard_all(&root).unwrap();
        let wiped = status(&root).unwrap();
        assert!(wiped.staged.is_empty() && wiped.unstaged.is_empty() && wiped.untracked.is_empty());
        assert!(!root.join("staged-new.txt").exists());
        assert!(!root.join("doc/screenshots").exists());
    }

    #[test]
    fn discovers_and_operates_on_multiple_sub_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();
        let repo_a = workspace.join("app-a");
        let repo_b = workspace.join("services/api-b");
        fs::create_dir_all(&repo_a).unwrap();
        fs::create_dir_all(&repo_b).unwrap();

        for (dir, file) in [(&repo_a, "a.txt"), (&repo_b, "b.txt")] {
            assert!(Command::new("git").current_dir(dir).args(["init", "-b", "main"]).status().unwrap().success());
            fs::write(dir.join(file), "v1\n").unwrap();
            stage(dir, &[file.to_string()]).unwrap();
            commit(dir, "init", false).unwrap();
        }

        // Dirty only services/api-b so auto-selection prefers the dirty sub-repo.
        fs::write(repo_b.join("b.txt"), "v1\nv2\n").unwrap();

        let st = status(workspace).unwrap();
        assert!(st.is_repo);
        assert_eq!(st.active_repo.as_deref(), Some("services/api-b"));
        assert_eq!(st.repos.len(), 2);
        assert_eq!(st.repos[0].path, "app-a");
        assert_eq!(st.repos[0].dirty, 0);
        assert_eq!(st.repos[1].path, "services/api-b");
        assert_eq!(st.repos[1].dirty, 1);
        assert_eq!(st.unstaged.len(), 1);
        assert_eq!(st.unstaged[0].path, "b.txt");

        // Workspace-relative diff resolves into the owning sub-repo automatically.
        let d = diff(workspace, "services/api-b/b.txt", DiffStage::Unstaged, None).unwrap();
        assert_eq!(d.path, "services/api-b/b.txt");
        assert_eq!(d.added, 1);

        // Explicit switch to clean sub-repo app-a.
        let st_a = status_for(workspace, Some("app-a")).unwrap();
        assert_eq!(st_a.active_repo.as_deref(), Some("app-a"));
        assert!(st_a.unstaged.is_empty());
    }

    #[test]
    fn git_status_ignores_malicious_core_fsmonitor_in_repo_config() {
        let (dir, root) = init_repo();
        let marker = dir.path().join("fsmonitor-pwned");
        let payload = dir.path().join("fsmonitor-hook.sh");
        let marker_slash = marker.to_string_lossy().replace('\\', "/");
        fs::write(&payload, format!("#!/bin/sh\necho pwn > \"{marker_slash}\"\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&payload, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let payload_slash = payload.to_string_lossy().replace('\\', "/");
        assert!(
            Command::new("git")
                .current_dir(&root)
                .args(["config", "core.fsmonitor", &payload_slash])
                .status()
                .unwrap()
                .success()
        );
        // Dirty working tree & untracked file so `git status` refreshes the index/worktree.
        fs::write(root.join("src/hello.rs"), "fn main() { println!(\"changed\"); }\n").unwrap();
        fs::write(root.join("untracked.txt"), "new\n").unwrap();

        let st_untrusted = status_for(&root, None).unwrap();
        assert!(st_untrusted.is_repo);
        assert_eq!(st_untrusted.unstaged.len(), 1);
        assert_eq!(st_untrusted.untracked.len(), 1);
        assert!(!marker.exists(), "core.fsmonitor must never execute during status_for");

        let st_trusted = status_for_with_trust(&root, None, true).unwrap();
        assert!(st_trusted.is_repo);
        assert!(!marker.exists(), "core.fsmonitor must remain disabled even in trusted workspaces");
    }

    #[test]
    fn untrusted_git_commit_ignores_malicious_pre_commit_hook() {
        let (dir, root) = init_repo();
        let pre_commit_marker = dir.path().join("pre-commit-pwned");
        let post_checkout_marker = dir.path().join("post-checkout-pwned");
        let hooks_dir = root.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();

        let pre_commit = hooks_dir.join("pre-commit");
        let pre_marker_slash = pre_commit_marker.to_string_lossy().replace('\\', "/");
        fs::write(&pre_commit, format!("#!/bin/sh\necho pwn > \"{pre_marker_slash}\"\nexit 1\n")).unwrap();

        let post_checkout = hooks_dir.join("post-checkout");
        let post_marker_slash = post_checkout_marker.to_string_lossy().replace('\\', "/");
        fs::write(&post_checkout, format!("#!/bin/sh\necho pwn > \"{post_marker_slash}\"\nexit 1\n")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pre_commit, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&post_checkout, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Untrusted commit (`trusted = false`) must succeed without running `.git/hooks/pre-commit`.
        fs::write(root.join("src/hello.rs"), "fn main() { println!(\"safe\"); }\n").unwrap();
        let res = commit_with_trust(&root, "safe commit in untrusted repo", true, false).unwrap();
        assert!(!res.hash.is_empty());
        assert!(!pre_commit_marker.exists(), "untrusted git commit must not execute pre-commit hook");

        // Untrusted checkout (`trusted = false`) must succeed without running `.git/hooks/post-checkout`.
        let branch = checkout_with_trust(&root, "feat/isolated", true, false).unwrap();
        assert_eq!(branch, "feat/isolated");
        assert!(!post_checkout_marker.exists(), "untrusted git checkout must not execute post-checkout hook");

        // Sanity check on Unix: when `trusted = true`, the armed `pre-commit` hook IS invoked and blocks the commit.
        #[cfg(unix)]
        {
            fs::write(root.join("src/hello.rs"), "fn main() { println!(\"trusted\"); }\n").unwrap();
            assert!(commit_with_trust(&root, "trusted commit runs hook", true, true).is_err());
            assert!(pre_commit_marker.exists(), "trusted git commit should honour repository hooks");
        }
    }
}
