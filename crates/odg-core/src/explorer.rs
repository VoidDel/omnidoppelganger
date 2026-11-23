//! Safe workspace file tree exploration, file operations, and read-only file
//! inspection for the desktop workbench.
//!
//! Every path is resolved against the session's workspace root, confined inside
//! it (following symlinks in existing ancestors), and barred from touching
//! `.git` internals.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;
use serde::Serialize;

use crate::documents;
use crate::events::base64;
use crate::tools::{in_git_directory, inside_workspace, resolve};

/// Largest text file loaded whole into the code viewer (2 MB).
pub const MAX_TEXT_VIEW_BYTES: u64 = 2 * 1024 * 1024;
/// Largest image file loaded as a data preview (15 MB).
pub const MAX_IMAGE_VIEW_BYTES: u64 = 15 * 1024 * 1024;
/// Default cap for fuzzy file search results.
pub const DEFAULT_SEARCH_LIMIT: usize = 100;

const PICTURES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// One row in a workspace directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeEntry {
    pub name: String,
    /// Slash-separated path relative to the workspace root.
    pub path: String,
    pub kind: EntryKind,
    /// True when `kind` is `Dir`, or a `Symlink` pointing to a directory inside the workspace.
    pub expandable: bool,
    pub size: Option<u64>,
    pub modified_ms: Option<u64>,
    pub ignored: bool,
}

/// One matching file returned by fuzzy workspace file search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundFile {
    pub name: String,
    pub path: String,
    pub dir: String,
}

/// Read-only view of a workspace file for the center workbench viewer.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileView {
    Text {
        path: String,
        name: String,
        language: Option<String>,
        content: String,
        lines: usize,
        size: u64,
        modified_ms: Option<u64>,
        eol: &'static str,
        /// SVG files carry text XML and can also be previewed as an image.
        svg_data: Option<String>,
    },
    Image {
        path: String,
        name: String,
        media_type: String,
        data: String,
        size: u64,
        modified_ms: Option<u64>,
    },
    Document {
        path: String,
        name: String,
        doc_kind: documents::Kind,
        parts: Option<usize>,
        content: String,
        size: u64,
        modified_ms: Option<u64>,
    },
    Binary {
        path: String,
        name: String,
        size: u64,
        modified_ms: Option<u64>,
        reason: &'static str,
    },
}

fn modified_millis(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

fn rel_slash(workspace: &Path, target: &Path) -> String {
    target
        .strip_prefix(workspace)
        .unwrap_or(target)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolve a user-supplied relative path strictly inside `workspace` without following
/// a leaf symlink outside when `follow_leaf` is false (used by rename/delete).
pub fn resolve_confined(workspace: &Path, raw: &str, allow_root: bool, follow_leaf: bool) -> Result<(PathBuf, String), String> {
    let canonical_root = workspace
        .canonicalize()
        .map_err(|error| format!("无法访问洞府目录 {}：{error}", workspace.display()))?;
    let trimmed = raw.trim().trim_matches(|c| c == '/' || c == '\\');
    if trimmed.is_empty() {
        if allow_root {
            return Ok((canonical_root, String::new()));
        }
        return Err("路径不能为空。".to_string());
    }
    if Path::new(trimmed).is_absolute() {
        return Err("只允许使用洞府内的相对路径。".to_string());
    }
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                if part.to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")) {
                    return Err("不能直接访问或修改 .git 内部目录。".to_string());
                }
            }
            Component::CurDir => {}
            _ => return Err("路径不能超出当前洞府范围。".to_string()),
        }
    }
    let resolved = resolve(&canonical_root, trimmed);
    if !resolved.starts_with(&canonical_root) || (!allow_root && resolved == canonical_root) {
        return Err("路径不能超出当前洞府范围。".to_string());
    }
    if follow_leaf {
        if !inside_workspace(&canonical_root, &resolved) {
            return Err("目标链接指向洞府之外，无法访问。".to_string());
        }
        if in_git_directory(&canonical_root, &resolved) {
            return Err("不能直接访问或修改 .git 内部目录。".to_string());
        }
    } else {
        let parent = resolved.parent().ok_or("无效的目标路径。")?;
        if !inside_workspace(&canonical_root, parent) {
            return Err("父目录位于洞府之外，无法操作。".to_string());
        }
        if in_git_directory(&canonical_root, parent) {
            return Err("不能直接修改 .git 内部目录。".to_string());
        }
    }
    let rel = rel_slash(&canonical_root, &resolved);
    Ok((resolved, rel))
}

/// List one directory inside `workspace`, sorted with folders first then case-insensitive name.
pub fn list_directory(workspace: &Path, rel_path: &str, show_hidden: bool, show_ignored: bool) -> Result<Vec<TreeEntry>, String> {
    let canonical_root = workspace
        .canonicalize()
        .map_err(|error| format!("无法访问洞府目录 {}：{error}", workspace.display()))?;
    let (dir, _) = resolve_confined(&canonical_root, rel_path, true, true)?;
    let meta = fs::metadata(&dir).map_err(|error| format!("无法读取目录：{error}"))?;
    if !meta.is_dir() {
        return Err("指定路径不是文件夹。".to_string());
    }

    // Collect non-ignored entries in this directory via `ignore::WalkBuilder` at depth 1.
    let mut unignored: HashSet<std::ffi::OsString> = HashSet::new();
    let mut walker = WalkBuilder::new(&dir);
    walker
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true);
    for entry in walker.build().flatten() {
        if entry.depth() == 1 {
            unignored.insert(entry.file_name().to_os_string());
        }
    }

    let mut entries = Vec::new();
    let read = fs::read_dir(&dir).map_err(|error| format!("无法列出目录内容：{error}"))?;
    for item in read {
        let Ok(item) = item else { continue };
        let os_name = item.file_name();
        let name = os_name.to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case(".git") || name == ".DS_Store" {
            continue;
        }
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let ignored = !unignored.contains(&os_name);
        if ignored && !show_ignored {
            continue;
        }
        let full = item.path();
        let Ok(sym_meta) = fs::symlink_metadata(&full) else { continue };
        let file_type = sym_meta.file_type();
        let (kind, expandable, size, modified_ms) = if file_type.is_symlink() {
            let target_dir = inside_workspace(&canonical_root, &full)
                && !in_git_directory(&canonical_root, &full)
                && fs::metadata(&full).is_ok_and(|m| m.is_dir());
            let size = fs::metadata(&full).ok().filter(|m| m.is_file()).map(|m| m.len());
            (EntryKind::Symlink, target_dir, size, modified_millis(&sym_meta))
        } else if file_type.is_dir() {
            (EntryKind::Dir, true, None, modified_millis(&sym_meta))
        } else {
            (EntryKind::File, false, Some(sym_meta.len()), modified_millis(&sym_meta))
        };
        let path = rel_slash(&canonical_root, &full);
        entries.push(TreeEntry { name, path, kind, expandable, size, modified_ms, ignored });
    }

    entries.sort_by(|a, b| {
        b.expandable
            .cmp(&a.expandable)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

/// Fuzzy-match score of `query` against `path` and `name`. Higher is better; `None` means no match.
fn match_score(query_lower: &str, name_lower: &str, path_lower: &str) -> Option<i32> {
    if query_lower.is_empty() {
        return Some(0);
    }
    if name_lower == query_lower {
        return Some(1000 - path_lower.len() as i32);
    }
    if name_lower.starts_with(query_lower) {
        return Some(800 - name_lower.len() as i32);
    }
    if let Some(pos) = name_lower.find(query_lower) {
        return Some(600 - (pos as i32 * 4) - name_lower.len() as i32);
    }
    if let Some(pos) = path_lower.find(query_lower) {
        return Some(400 - pos as i32 - path_lower.len() as i32);
    }
    // Ordered character subsequence across the relative path (e.g. `compsx` -> `src/components/Composer.tsx`).
    let mut chars = query_lower.chars().peekable();
    let mut gaps = 0i32;
    let mut matched_any = false;
    for ch in path_lower.chars() {
        if chars.peek() == Some(&ch) {
            chars.next();
            matched_any = true;
            if chars.peek().is_none() {
                break;
            }
        } else if matched_any {
            gaps += 1;
        }
    }
    if chars.peek().is_none() {
        Some(200 - gaps - path_lower.len() as i32)
    } else {
        None
    }
}

/// Fast fuzzy search over non-ignored files in `workspace`.
pub fn search_files(workspace: &Path, query: &str, limit: usize) -> Result<Vec<FoundFile>, String> {
    let canonical_root = workspace
        .canonicalize()
        .map_err(|error| format!("无法访问洞府目录 {}：{error}", workspace.display()))?;
    let cap = limit.clamp(1, 500);
    let query_lower = query.trim().replace('\\', "/").to_lowercase();

    let mut walker = WalkBuilder::new(&canonical_root);
    walker.hidden(false).git_ignore(true).git_global(true).git_exclude(true).parents(true);

    let mut scored: Vec<(i32, FoundFile)> = Vec::new();
    for entry in walker.build().flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        if path.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s.eq_ignore_ascii_case(".git") || s == ".DS_Store")) {
            continue;
        }
        let Some(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            continue;
        }
        if ft.is_symlink() && !inside_workspace(&canonical_root, path) {
            continue;
        }
        let rel = rel_slash(&canonical_root, path);
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(score) = match_score(&query_lower, &name.to_lowercase(), &rel.to_lowercase()) {
            let cut = rel.rfind('/').unwrap_or(0);
            let dir = if rel.contains('/') { rel[..cut].to_string() } else { String::new() };
            scored.push((score, FoundFile { name, path: rel, dir }));
            // When query is empty, stop walking once we have enough initial candidates.
            if query_lower.is_empty() && scored.len() >= cap {
                break;
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    scored.truncate(cap);
    Ok(scored.into_iter().map(|(_, found)| found).collect())
}

/// Create a new empty file or directory at `rel_path` inside `workspace`.
pub fn create_entry(workspace: &Path, rel_path: &str, is_dir: bool) -> Result<String, String> {
    let (target, rel) = resolve_confined(workspace, rel_path, false, true)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!("「{rel}」已经存在了。"));
    }
    if is_dir {
        fs::create_dir_all(&target).map_err(|error| format!("无法创建文件夹「{rel}」：{error}"))?;
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("无法准备父文件夹：{error}"))?;
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("无法创建文件「{rel}」：{error}"))?;
    }
    Ok(rel)
}

/// Rename or move an entry inside `workspace` without overwriting an existing target.
pub fn rename_entry(workspace: &Path, from_rel: &str, to_rel: &str) -> Result<String, String> {
    let (from, from_clean) = resolve_confined(workspace, from_rel, false, false)?;
    let (to, to_clean) = resolve_confined(workspace, to_rel, false, false)?;
    if from_clean == to_clean {
        return Ok(to_clean);
    }
    if fs::symlink_metadata(&from).is_err() {
        return Err(format!("找不到要改名的「{from_clean}」。"));
    }
    // Allow case-only rename on case-insensitive filesystems (e.g. `readme.md` -> `README.md`),
    // while blocking overwriting a distinct existing file.
    let same_inode = match (from.canonicalize(), to.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same_inode && fs::symlink_metadata(&to).is_ok() {
        return Err(format!("目标位置「{to_clean}」已经有同名文件或文件夹了。"));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建目标父目录：{error}"))?;
    }
    fs::rename(&from, &to).map_err(|error| format!("无法重命名为「{to_clean}」：{error}"))?;
    Ok(to_clean)
}

/// Delete a file, symlink, or directory inside `workspace`. Never follows a symlink to delete outside targets.
pub fn delete_entry(workspace: &Path, rel_path: &str) -> Result<(), String> {
    let (target, rel) = resolve_confined(workspace, rel_path, false, false)?;
    let meta = fs::symlink_metadata(&target).map_err(|_| format!("找不到「{rel}」，可能已经被删掉了。"))?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        // On Windows a directory symlink is removed with `remove_dir`, falling back to `remove_file`.
        #[cfg(windows)]
        if fs::remove_file(&target).is_err() {
            fs::remove_dir(&target).map_err(|error| format!("无法删除链接「{rel}」：{error}"))?;
            return Ok(());
        }
        #[cfg(not(windows))]
        fs::remove_file(&target).map_err(|error| format!("无法删除链接「{rel}」：{error}"))?;
        Ok(())
    } else if file_type.is_dir() {
        fs::remove_dir_all(&target).map_err(|error| format!("无法删除文件夹「{rel}」：{error}"))
    } else {
        fs::remove_file(&target).map_err(|error| format!("无法删除文件「{rel}」：{error}"))
    }
}

/// Map a file's extension or special name to a `highlight.js` language identifier.
pub fn detect_language(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    let lower_name = name.to_ascii_lowercase();
    match lower_name.as_str() {
        "dockerfile" | "containerfile" => return Some("dockerfile"),
        "makefile" | "gnumakefile" => return Some("makefile"),
        "cmakelists.txt" => return Some("cmake"),
        ".gitignore" | ".dockerignore" | ".ignore" => return Some("ini"),
        ".bashrc" | ".zshrc" | ".zprofile" | ".profile" => return Some("bash"),
        _ => {}
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "json" | "jsonc" | "jsonl" => "json",
        "toml" | "lock" if lower_name == "cargo.lock" || ext == "toml" => "ini",
        "yaml" | "yml" => "yaml",
        "md" | "mdx" | "markdown" => "markdown",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "html" | "htm" | "xml" | "svg" | "plist" | "xhtml" | "vue" | "svelte" => "xml",
        "py" | "pyi" | "pyw" => "python",
        "go" | "mod" if ext == "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "r" => "r",
        "sql" => "sql",
        "sh" | "bash" | "zsh" | "command" => "bash",
        "ps1" | "psm1" | "psd1" => "powershell",
        "bat" | "cmd" => "dos",
        "ini" | "cfg" | "conf" | "env" | "properties" => "ini",
        "diff" | "patch" => "diff",
        "graphql" | "gql" => "graphql",
        "proto" => "protobuf",
        _ => return None,
    };
    Some(lang)
}

/// Read a workspace file for read-only viewing in the workbench.
pub fn read_file_view(workspace: &Path, rel_path: &str) -> Result<FileView, String> {
    let (file, rel) = resolve_confined(workspace, rel_path, false, true)?;
    let meta = fs::metadata(&file).map_err(|error| format!("无法读取「{rel}」：{error}"))?;
    if meta.is_dir() {
        return Err(format!("「{rel}」是文件夹，请在左侧展开查看。"));
    }
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| rel.clone());
    let size = meta.len();
    let modified_ms = modified_millis(&meta);
    let ext = file.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();

    // 1. Raster images: return safe base64 image view.
    if let Some((_, media_type)) = PICTURES.iter().find(|(kind, _)| *kind == ext) {
        if size > MAX_IMAGE_VIEW_BYTES {
            return Ok(FileView::Binary { path: rel, name, size, modified_ms, reason: "too_large" });
        }
        let bytes = fs::read(&file).map_err(|error| format!("无法读取图片「{rel}」：{error}"))?;
        return Ok(FileView::Image {
            path: rel,
            name,
            media_type: (*media_type).to_string(),
            data: base64(&bytes),
            size,
            modified_ms,
        });
    }

    // 2. Office / PDF documents: extract readable text via `odg_core::documents`.
    if let Some(found) = documents::kind_of_file(&file) {
        if let Ok(doc_kind) = found
            && size <= documents::DOCUMENT_BYTES
            && let Ok(Ok(read)) = documents::file_text(&file, doc_kind)
        {
            return Ok(FileView::Document {
                path: rel,
                name,
                doc_kind: read.kind,
                parts: read.parts,
                content: read.text.clone(),
                size,
                modified_ms,
            });
        }
        return Ok(FileView::Binary {
            path: rel,
            name,
            size,
            modified_ms,
            reason: if size > documents::DOCUMENT_BYTES { "too_large" } else { "binary" },
        });
    }

    // 3. Plain text & source code (including SVG, with optional image preview data).
    if size > MAX_TEXT_VIEW_BYTES {
        return Ok(FileView::Binary { path: rel, name, size, modified_ms, reason: "too_large" });
    }
    let bytes = fs::read(&file).map_err(|error| format!("无法读取文件「{rel}」：{error}"))?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Ok(FileView::Binary { path: rel, name, size, modified_ms, reason: "binary" });
    }
    let raw_text = String::from_utf8_lossy(&bytes);
    let eol = if raw_text.contains("\r\n") { "CRLF" } else { "LF" };
    let content = raw_text.replace("\r\n", "\n");
    let lines = if content.is_empty() { 0 } else { content.lines().count() };
    let svg_data = (ext == "svg").then(|| base64(&bytes));
    let language = detect_language(&file).map(str::to_string);

    Ok(FileView::Text {
        path: rel,
        name,
        language,
        content,
        lines,
        size,
        modified_ms,
        eol,
        svg_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_marks_gitignored_files_and_hides_dot_git() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::write(root.join(".gitignore"), "dist/\n*.log\n").unwrap();
        fs::write(root.join("README.md"), "# Hello\n").unwrap();
        fs::write(root.join("debug.log"), "noise\n").unwrap();

        let shown = list_directory(&root, "", true, true).unwrap();
        let names: Vec<&str> = shown.iter().map(|e| e.name.as_str()).collect();
        // Folders (`dist`, `src`) come before files (`.gitignore`, `debug.log`, `README.md`), and `.git` is never listed.
        assert_eq!(names, vec!["dist", "src", ".gitignore", "debug.log", "README.md"]);
        assert!(shown.iter().find(|e| e.name == "dist").unwrap().ignored);
        assert!(shown.iter().find(|e| e.name == "debug.log").unwrap().ignored);
        assert!(!shown.iter().find(|e| e.name == "src").unwrap().ignored);

        let filtered = list_directory(&root, "", false, false).unwrap();
        let filtered_names: Vec<&str> = filtered.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(filtered_names, vec!["src", "README.md"]);
    }

    #[test]
    fn blocks_path_traversal_and_dot_git_access_in_every_operation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("secret.txt");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
        fs::write(&outside, "outside").unwrap();

        for bad in ["../secret.txt", "..", ".git", ".git/config", "sub/../../secret.txt"] {
            assert!(list_directory(&root, bad, true, true).is_err(), "list should reject {bad}");
            assert!(read_file_view(&root, bad).is_err(), "read should reject {bad}");
            assert!(create_entry(&root, bad, false).is_err(), "create should reject {bad}");
            assert!(delete_entry(&root, bad).is_err(), "delete should reject {bad}");
        }
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    }

    #[test]
    fn creates_renames_deletes_and_reads_files_safely() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        assert_eq!(create_entry(root, "src/lib", true).unwrap(), "src/lib");
        assert_eq!(create_entry(root, "src/lib/hello.rs", false).unwrap(), "src/lib/hello.rs");
        assert!(create_entry(root, "src/lib/hello.rs", false).is_err(), "duplicate create must fail");

        fs::write(root.join("src/lib/hello.rs"), "pub fn greet() -> &'static str {\r\n    \"hi\"\r\n}\r\n").unwrap();
        let view = read_file_view(root, "src/lib/hello.rs").unwrap();
        match view {
            FileView::Text { path, language, lines, eol, content, .. } => {
                assert_eq!(path, "src/lib/hello.rs");
                assert_eq!(language.as_deref(), Some("rust"));
                assert_eq!(lines, 3);
                assert_eq!(eol, "CRLF");
                assert!(!content.contains('\r'));
            }
            other => panic!("expected Text view, got {other:?}"),
        }

        let found = search_files(root, "hlrs", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/lib/hello.rs");

        assert_eq!(rename_entry(root, "src/lib/hello.rs", "src/lib/main.rs").unwrap(), "src/lib/main.rs");
        assert!(!root.join("src/lib/hello.rs").exists());
        assert!(root.join("src/lib/main.rs").exists());

        delete_entry(root, "src/lib").unwrap();
        assert!(!root.join("src/lib").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_pointing_outside_cannot_be_read_and_deleting_it_leaves_target_intact() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("outside.txt");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, "keep me").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape.txt")).unwrap();

        assert!(read_file_view(&root, "escape.txt").is_err());
        delete_entry(&root, "escape.txt").unwrap();
        assert!(!root.join("escape.txt").exists());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me");
    }
}
