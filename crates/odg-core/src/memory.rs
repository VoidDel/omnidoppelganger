//! Layered persistent Markdown memory (`识海`) for OmniDoppelganger.
//!
//! Stores user-readable, zero-dependency Markdown memory files under `<home>/memory/`:
//! - `<home>/memory/global.md` — global preferences, communication style, and environment notes.
//! - `<home>/memory/projects/<hash>.md` — per-workspace architectural decisions and repo conventions.
//! - `<home>/memory/projects/index.json` — maps `<hash>` back to workspace paths for management UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Maximum size of a single memory Markdown file (32 KiB).
pub const MAX_FILE_BYTES: usize = 32 * 1024;
/// Maximum characters injected into the system prompt per scope to keep prompt prefix compact & cacheable.
pub const MAX_PROMPT_CHARS_PER_SCOPE: usize = 2_000;

/// Gives the root memory directory (`<home>/memory`) while memory is enabled, or `None` when disabled.
pub type MemorySource = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryProjectSummary {
    pub hash: String,
    pub workspace: String,
    pub chars: usize,
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub enabled: bool,
    pub global: String,
    pub project: String,
    pub project_workspace: Option<String>,
    pub projects: Vec<MemoryProjectSummary>,
    pub memory_dir: String,
}

/// Deterministic 64-bit FNV-1a hex hash of a canonicalized/normalized workspace path.
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let norm = canonical
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in norm.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn memory_dir(home: &Path) -> PathBuf {
    home.join("memory")
}

pub fn global_path(memory_root: &Path) -> PathBuf {
    memory_root.join("global.md")
}

pub fn projects_dir(memory_root: &Path) -> PathBuf {
    memory_root.join("projects")
}

pub fn project_path(memory_root: &Path, workspace: &Path) -> PathBuf {
    projects_dir(memory_root).join(format!("{}.md", workspace_hash(workspace)))
}

fn index_path(memory_root: &Path) -> PathBuf {
    projects_dir(memory_root).join("index.json")
}

fn read_index(memory_root: &Path) -> BTreeMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(index_path(memory_root)) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_index(memory_root: &Path, index: &BTreeMap<String, String>) -> Result<(), String> {
    let dir = projects_dir(memory_root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建记忆目录失败：{e}"))?;
    let json = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    std::fs::write(index_path(memory_root), format!("{json}\n")).map_err(|e| format!("写入记忆索引失败：{e}"))
}

pub fn read_global(memory_root: &Path) -> String {
    std::fs::read_to_string(global_path(memory_root)).unwrap_or_default()
}

pub fn read_project(memory_root: &Path, workspace: &Path) -> String {
    std::fs::read_to_string(project_path(memory_root, workspace)).unwrap_or_default()
}

pub fn read_project_by_hash(memory_root: &Path, hash: &str) -> String {
    if !is_valid_hash(hash) {
        return String::new();
    }
    std::fs::read_to_string(projects_dir(memory_root).join(format!("{hash}.md"))).unwrap_or_default()
}

fn is_valid_hash(hash: &str) -> bool {
    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn write_scope(
    memory_root: &Path,
    scope: MemoryScope,
    workspace: Option<&Path>,
    content: &str,
) -> Result<(), String> {
    if content.len() > MAX_FILE_BYTES {
        return Err(format!(
            "记忆内容超过上限（最多 {} KB），请精简后再保存。",
            MAX_FILE_BYTES / 1024
        ));
    }
    let trimmed = content.trim();
    match scope {
        MemoryScope::Global => {
            let path = global_path(memory_root);
            if trimmed.is_empty() {
                if path.exists() {
                    let _ = std::fs::remove_file(path);
                }
                return Ok(());
            }
            std::fs::create_dir_all(memory_root).map_err(|e| format!("创建记忆目录失败：{e}"))?;
            std::fs::write(path, format!("{trimmed}\n")).map_err(|e| format!("写入全局记忆失败：{e}"))
        }
        MemoryScope::Project => {
            let ws = workspace.ok_or_else(|| "缺少当前洞府路径，无法保存项目记忆。".to_string())?;
            let hash = workspace_hash(ws);
            let path = projects_dir(memory_root).join(format!("{hash}.md"));
            let mut index = read_index(memory_root);
            if trimmed.is_empty() {
                if path.exists() {
                    let _ = std::fs::remove_file(&path);
                }
                index.remove(&hash);
                let _ = write_index(memory_root, &index);
                return Ok(());
            }
            std::fs::create_dir_all(projects_dir(memory_root)).map_err(|e| format!("创建项目记忆目录失败：{e}"))?;
            std::fs::write(&path, format!("{trimmed}\n")).map_err(|e| format!("写入项目记忆失败：{e}"))?;
            let canonical = ws.canonicalize().unwrap_or_else(|_| ws.to_path_buf());
            index.insert(hash, canonical.display().to_string());
            write_index(memory_root, &index)
        }
    }
}

/// Append a concise bullet entry to the given memory scope (deduplicating exact matches).
pub fn append_entry(
    memory_root: &Path,
    scope: MemoryScope,
    workspace: Option<&Path>,
    entry: &str,
) -> Result<String, String> {
    let clean = entry
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let clean = clean.trim_start_matches(['-', '*', '•', ' ']).trim();
    if clean.is_empty() {
        return Err("记忆内容不能为空。".to_string());
    }
    let bullet = format!("- {clean}");
    let existing = match scope {
        MemoryScope::Global => read_global(memory_root),
        MemoryScope::Project => workspace.map(|w| read_project(memory_root, w)).unwrap_or_default(),
    };
    let already_present = existing.lines().any(|line| {
        line.trim()
            .trim_start_matches(['-', '*', '•', ' '])
            .trim()
            .eq_ignore_ascii_case(clean)
    });
    let updated = if already_present {
        existing.trim().to_string()
    } else if existing.trim().is_empty() {
        bullet
    } else {
        format!("{}\n{}", existing.trim_end(), bullet)
    };
    write_scope(memory_root, scope, workspace, &updated)?;
    Ok(updated)
}

/// Remove any lines matching `query` (case-insensitive substring) from the given memory scope.
pub fn remove_matching(
    memory_root: &Path,
    scope: MemoryScope,
    workspace: Option<&Path>,
    query: &str,
) -> Result<(usize, String), String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("请提供要删除的记忆关键词。".to_string());
    }
    let existing = match scope {
        MemoryScope::Global => read_global(memory_root),
        MemoryScope::Project => workspace.map(|w| read_project(memory_root, w)).unwrap_or_default(),
    };
    let mut kept = Vec::new();
    let mut removed = 0usize;
    for line in existing.lines() {
        if line.to_lowercase().contains(&needle) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    let updated = kept.join("\n").trim().to_string();
    if removed > 0 {
        write_scope(memory_root, scope, workspace, &updated)?;
    }
    Ok((removed, updated))
}

pub fn list_projects(memory_root: &Path) -> Vec<MemoryProjectSummary> {
    let index = read_index(memory_root);
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_dir(memory_root)) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(hash) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
            continue;
        };
        if !is_valid_hash(&hash) {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let workspace = index.get(&hash).cloned().unwrap_or_else(|| hash.clone());
        items.push(MemoryProjectSummary {
            hash,
            workspace,
            chars: trimmed.chars().count(),
            lines: trimmed.lines().count(),
        });
    }
    items.sort_by(|a, b| a.workspace.cmp(&b.workspace));
    items
}

/// Ensure the Markdown file for a memory scope exists on disk so an external editor can open it directly.
pub fn ensure_file(
    memory_root: &Path,
    scope: MemoryScope,
    workspace: Option<&Path>,
    hash: Option<&str>,
) -> Result<PathBuf, String> {
    let path = match scope {
        MemoryScope::Global => {
            std::fs::create_dir_all(memory_root).map_err(|e| format!("创建识海目录失败：{e}"))?;
            global_path(memory_root)
        }
        MemoryScope::Project => {
            std::fs::create_dir_all(projects_dir(memory_root)).map_err(|e| format!("创建洞府铭碑目录失败：{e}"))?;
            if let Some(h) = hash.filter(|h| is_valid_hash(h)) {
                projects_dir(memory_root).join(format!("{h}.md"))
            } else if let Some(ws) = workspace {
                let h = workspace_hash(ws);
                let mut index = read_index(memory_root);
                let canonical = ws.canonicalize().unwrap_or_else(|_| ws.to_path_buf());
                index
                    .entry(h.clone())
                    .or_insert_with(|| canonical.display().to_string());
                let _ = write_index(memory_root, &index);
                projects_dir(memory_root).join(format!("{h}.md"))
            } else {
                return Err("缺少洞府标识，无法打开洞府铭碑文件。".to_string());
            }
        }
    };
    if !path.exists() {
        std::fs::write(&path, "").map_err(|e| format!("创建识海文件失败：{e}"))?;
    }
    Ok(path)
}

pub fn delete_project(memory_root: &Path, hash: &str) -> Result<(), String> {
    if !is_valid_hash(hash) {
        return Err("无效的项目记忆标识。".to_string());
    }
    let path = projects_dir(memory_root).join(format!("{hash}.md"));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("删除项目记忆失败：{e}"))?;
    }
    let mut index = read_index(memory_root);
    if index.remove(hash).is_some() {
        let _ = write_index(memory_root, &index);
    }
    Ok(())
}

pub fn snapshot(home: &Path, enabled: bool, workspace: Option<&Path>) -> MemorySnapshot {
    let root = memory_dir(home);
    let global = read_global(&root);
    let project = workspace.map(|w| read_project(&root, w)).unwrap_or_default();
    let project_workspace = workspace.map(|w| w.display().to_string());
    let projects = list_projects(&root);
    MemorySnapshot {
        enabled,
        global,
        project,
        project_workspace,
        projects,
        memory_dir: root.display().to_string(),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push_str("\n…(truncated)");
    out
}

/// Format global + current workspace memory for injection into the system prompt.
/// Returns `None` when both scopes are empty.
pub fn format_for_prompt(memory_root: &Path, workspace: &Path) -> Option<String> {
    let global = truncate_chars(&read_global(memory_root), MAX_PROMPT_CHARS_PER_SCOPE);
    let project = truncate_chars(&read_project(memory_root, workspace), MAX_PROMPT_CHARS_PER_SCOPE);
    if global.is_empty() && project.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("Persistent memories saved across sessions (prefer current workspace files and the user's latest words if anything conflicts):\n");
    if !global.is_empty() {
        out.push_str("\n<global-memory>\n");
        out.push_str(&global);
        out.push_str("\n</global-memory>");
    }
    if !project.is_empty() {
        out.push_str("\n<project-memory>\n");
        out.push_str(&project);
        out.push_str("\n</project-memory>");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_deduplicates_removes_and_formats_layered_memory() {
        let dir = tempfile::tempdir().unwrap();
        let root = memory_dir(dir.path());
        let ws = dir.path().join("repo-a");
        std::fs::create_dir_all(&ws).unwrap();

        assert!(format_for_prompt(&root, &ws).is_none());

        append_entry(&root, MemoryScope::Global, None, "Reply concisely in Chinese").unwrap();
        // Case-insensitive duplicate is ignored.
        append_entry(&root, MemoryScope::Global, None, "- reply concisely in chinese").unwrap();
        assert_eq!(read_global(&root).trim(), "- Reply concisely in Chinese");

        append_entry(&root, MemoryScope::Project, Some(&ws), "Run cargo fmt before pushing").unwrap();
        append_entry(
            &root,
            MemoryScope::Project,
            Some(&ws),
            "Node 24 lives under /opt/homebrew",
        )
        .unwrap();

        let prompt = format_for_prompt(&root, &ws).unwrap();
        assert!(prompt.contains("<global-memory>"));
        assert!(prompt.contains("<project-memory>"));
        assert!(prompt.contains("Run cargo fmt before pushing"));

        let projects = list_projects(&root);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].lines, 2);

        let (removed, remaining) = remove_matching(&root, MemoryScope::Project, Some(&ws), "homebrew").unwrap();
        assert_eq!(removed, 1);
        assert!(!remaining.contains("homebrew"));
        assert!(remaining.contains("cargo fmt"));

        delete_project(&root, &projects[0].hash).unwrap();
        assert!(list_projects(&root).is_empty());
    }
}
