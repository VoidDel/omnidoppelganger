//! The tools: read a file, search file contents, find files by name, edit a
//! file, write a whole file, run a command, and read or stop a command left
//! running in the background.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::jobs::{Ended, Jobs, Report};

const READ_LINE_LIMIT: usize = 2000;
const READ_FILE_BYTES: u64 = 10 * 1024 * 1024;
const LINE_CHARS: usize = 2000;
/// Every result is replayed in every later request of the session, so one
/// read must stay far below the context window.
const READ_OUTPUT_CHARS: usize = 100_000;
const SEARCH_RESULTS: usize = 200;
const FIND_RESULTS: usize = 500;
const SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const COMMAND_OUTPUT_CHARS: usize = 30_000;
const COMMAND_CAPTURE_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT_SECONDS: u64 = 120;
const COMMAND_TIMEOUT_LIMIT_SECONDS: u64 = 600;
/// The longest `command_output` waits for a background command to print something.
const OUTPUT_WAIT_LIMIT_SECONDS: u64 = 60;
const DIFF_LINES: usize = 400;

pub struct ToolContext {
    /// Directory relative paths resolve against and commands run in.
    pub workspace: PathBuf,
    pub cancel: CancellationToken,
    /// Where `edit_file` saves what a file held before changing it, so the
    /// edit can be undone; see [`crate::undo`]. `None` saves nothing.
    pub snapshots: Option<PathBuf>,
    /// The session's background commands. `None` means commands cannot be left running.
    pub jobs: Option<Arc<Jobs>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// Text returned to the model.
    pub content: String,
    pub is_error: bool,
    /// Presentation data for the client; never sent to the model.
    pub meta: Value,
}

impl ToolOutput {
    fn ok(content: impl Into<String>, meta: Value) -> Self {
        Self { content: content.into(), is_error: false, meta }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true, meta: Value::Null }
    }
}

/// The shell `run_command` uses on this machine: `(program, arguments before the command, name for the model)`.
pub fn shell() -> (&'static str, &'static [&'static str], &'static str) {
    if cfg!(windows) {
        let has_pwsh = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).any(|directory| directory.join("pwsh.exe").is_file()))
            .unwrap_or(false);
        if has_pwsh {
            ("pwsh.exe", &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"], "PowerShell 7")
        } else {
            (
                "powershell.exe",
                &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
                "Windows PowerShell 5.1 (chain commands with `;`, not `&&`)",
            )
        }
    } else {
        ("sh", &["-c"], "sh")
    }
}

/// Tool definitions sent to the model.
pub fn specs() -> Vec<Value> {
    let (_, _, shell_name) = shell();
    vec![
        json!({
            "name": "read_file",
            "description": "Read a text file. Returns numbered lines. Read a file before editing it. One read returns at most 2000 lines and 100,000 characters; use offset and limit to read the rest.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "offset": { "type": "integer", "description": "First line to return, starting at 1. Default 1." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return. Default 2000." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search",
            "description": "Search file contents with a regular expression, skipping files that .gitignore excludes. Returns `path:line: text` for each match. Use it to find where something is defined or used before reading whole files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression (Rust regex syntax)." },
                    "path": { "type": "string", "description": "Directory or file to search. Default: the workspace." },
                    "glob": { "type": "string", "description": "Only search files whose path matches this glob, for example `**/*.rs`." }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "find_files",
            "description": "List a directory, or find files by name. Without `glob`, lists what `path` holds, one level deep, with a trailing `/` on directories. With `glob`, returns every file below `path` whose path matches, for example `**/*.rs` or `**/test_*.py`, skipping what .gitignore excludes. Use it to learn a project's layout or to locate a file whose name you know; use `search` to look inside files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list or search below. Default: the workspace." },
                    "glob": { "type": "string", "description": "Glob the file's path, relative to the workspace, must match." }
                }
            }
        }),
        json!({
            "name": "edit_file",
            "description": "Replace an exact piece of text in a file, or create a file. `old_string` must match the file exactly and occur once unless `replace_all` is true. To create a new file, pass an empty `old_string` and the full contents as `new_string`. To make several changes to one file in one call, pass `edits` instead of `old_string` and `new_string`: they apply in order, each to the result of the one before, and nothing is written unless all of them apply.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "old_string": { "type": "string", "description": "Exact text to replace. Empty to create a new file." },
                    "new_string": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." },
                    "edits": {
                        "type": "array",
                        "description": "Several replacements for this file, instead of `old_string` and `new_string`.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string", "description": "Exact text to replace." },
                                "new_string": { "type": "string", "description": "Replacement text." },
                                "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Write a whole file: create it, or replace everything it holds with `content`. Use it for a new file or for rewriting most of one. To change part of a file use `edit_file`, which sends less and leaves a smaller diff to review.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "content": { "type": "string", "description": "The file's full new contents." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "run_command",
            "description": format!("Run a shell command in the workspace and return its output and exit code. The shell is {shell_name}. The user may have to approve the command before it runs. Use it to build, test, and inspect the project, not to read or edit files. For a command that does not end by itself (a dev server, a watcher) or runs for many minutes, set `background`: it then returns a job id at once, `command_output` reads what the job prints, and `stop_command` ends it."),
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." },
                    "timeout_seconds": { "type": "integer", "description": "Stop the command after this many seconds. Default 120, maximum 600. Ignored with `background`." },
                    "background": { "type": "boolean", "description": "Leave the command running and return at once. Default false." }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "command_output",
            "description": "Read what a background command has printed since you last read it, and whether it is still running. With `wait_seconds` it waits that long for the command to print something or end, which beats asking again and again.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "The job id `run_command` returned, like `b1`." },
                    "wait_seconds": { "type": "integer", "description": "Wait up to this many seconds for new output. Default 0, maximum 60." }
                },
                "required": ["job"]
            }
        }),
        json!({
            "name": "stop_command",
            "description": "Stop a background command and the processes it started. Returns what it printed last.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "The job id `run_command` returned, like `b1`." }
                },
                "required": ["job"]
            }
        }),
    ]
}

/// Resolve a tool's path argument against the workspace, folding `.` and `..` without touching the disk.
pub fn resolve(workspace: &Path, path: &str) -> PathBuf {
    let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { workspace.join(path) };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// Where `path` really points: links are followed in the part of the path
/// that exists, and the missing rest is appended unchanged.
fn real_path(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let canonical = loop {
        if let Ok(canonical) = existing.canonicalize() {
            break canonical;
        }
        missing.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?.to_path_buf();
    };
    Some(missing.iter().rev().fold(canonical, |path, name| path.join(name)))
}

/// Whether `path` is the workspace or below it, following links in the part of the path that exists.
pub fn inside_workspace(workspace: &Path, path: &Path) -> bool {
    match (workspace.canonicalize(), real_path(path)) {
        (Ok(root), Some(full)) => full.starts_with(root),
        _ => false,
    }
}

/// Whether `path`, links followed, lies in a `.git` directory below the
/// workspace (or anywhere, when it is outside the workspace). Git runs hooks
/// and reads configuration from there, so writing to it can run code later.
pub fn in_git_directory(workspace: &Path, path: &Path) -> bool {
    let Some(full) = real_path(path) else { return true };
    let below = workspace.canonicalize().ok().and_then(|root| full.strip_prefix(root).ok().map(Path::to_path_buf)).unwrap_or(full);
    below.components().any(|component| component.as_os_str().to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
}

fn display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn string_arg<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolOutput> {
    input[name].as_str().ok_or_else(|| ToolOutput::error(format!("`{name}` is required and must be a string.")))
}

fn clip(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS {
        line.to_string()
    } else {
        format!("{}… [line cut at {LINE_CHARS} characters]", line.chars().take(LINE_CHARS).collect::<String>())
    }
}

/// Run one tool. Unknown tools and bad arguments come back as error results, never as panics.
pub async fn execute(name: &str, input: &Value, context: &ToolContext) -> ToolOutput {
    let result = match name {
        "read_file" => read_file(input, context),
        "edit_file" => edit_file(input, context),
        "write_file" => write_file(input, context),
        "search" => search(input, context).await,
        "find_files" => find_files(input, context).await,
        "run_command" => run_command(input, context).await,
        "command_output" => command_output(input, context).await,
        "stop_command" => stop_command(input, context).await,
        other => Err(ToolOutput::error(format!("Unknown tool `{other}`."))),
    };
    result.unwrap_or_else(|error| error)
}

fn read_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let metadata = std::fs::metadata(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory. Use `find_files` to list it.")));
    }
    if metadata.len() > READ_FILE_BYTES {
        return Err(ToolOutput::error(format!("{shown} is {} bytes, over the {READ_FILE_BYTES}-byte limit.", metadata.len())));
    }
    let bytes = std::fs::read(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ToolOutput::error(format!("{shown} is a binary file.")));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"].as_u64().map(|limit| limit as usize).unwrap_or(READ_LINE_LIMIT).clamp(1, READ_LINE_LIMIT);
    let mut selected: Vec<String> = Vec::new();
    let mut chars = 0usize;
    let mut capped = false;
    for (index, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let numbered = format!("{:>6}\t{}", index + 1, clip(line));
        let size = numbered.chars().count() + 1;
        // The first line always fits: a line is clipped far below the cap.
        if !selected.is_empty() && chars + size > READ_OUTPUT_CHARS {
            capped = true;
            break;
        }
        chars += size;
        selected.push(numbered);
    }
    let mut content = selected.join("\n");
    let last = offset - 1 + selected.len();
    if selected.is_empty() {
        content = format!("{shown} has {} lines; nothing at line {offset}.", lines.len());
    } else if capped {
        content.push_str(&format!(
            "\n[showing lines {offset}-{last} of {}: output stops at {READ_OUTPUT_CHARS} characters; pass offset {} to read on]",
            lines.len(),
            last + 1
        ));
    } else if last < lines.len() {
        content.push_str(&format!("\n[showing lines {offset}-{last} of {}; pass offset to read more]", lines.len()));
    }
    Ok(ToolOutput::ok(content, json!({ "path": shown, "lines": lines.len(), "shown": selected.len() })))
}

/// Whether a tool changes files, and so is decided, snapshotted, and undone like an edit.
pub fn edits_files(tool: &str) -> bool {
    matches!(tool, "edit_file" | "write_file")
}

/// A text file as it is on disk: `None` when it does not exist.
fn read_existing(path: &Path, shown: &str) -> Result<Option<String>, ToolOutput> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes).map_err(|_| ToolOutput::error(format!("{shown} is not UTF-8 text.")))?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    }
}

/// Write `after` (LF text) over what the file held, after saving that for
/// undo, and describe the change. A file that used CRLF gets CRLF back.
fn write_change(context: &ToolContext, path: &Path, shown: &str, existing: Option<&str>, after: &str) -> Result<ToolOutput, ToolOutput> {
    let crlf = existing.is_some_and(|text| text.contains("\r\n"));
    let before = existing.map(|text| text.replace("\r\n", "\n"));
    if before.as_deref() == Some(after) {
        return Err(ToolOutput::error(format!("The change leaves {shown} as it is.")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let written = if crlf { after.replace('\n', "\r\n") } else { after.to_string() };
    // The snapshot comes first: a change that could not be undone is not made.
    let snapshot = match (&context.snapshots, existing) {
        (Some(directory), Some(original)) => {
            Some(crate::undo::save(directory, original.as_bytes()).map_err(|error| ToolOutput::error(format!("Cannot save {shown} before changing it: {error}")))?)
        }
        _ => None,
    };
    crate::fsutil::write_atomically(path, written.as_bytes()).map_err(|error| ToolOutput::error(format!("Cannot write {shown}: {error}")))?;

    let created = before.is_none();
    let diff = line_diff(before.as_deref().unwrap_or(""), after);
    let (added, removed) = (diff.added, diff.removed);
    let summary = if created {
        format!("Created {shown} ({added} lines).")
    } else {
        format!("Changed {shown}: {added} lines added, {removed} lines removed.")
    };
    let mut meta = json!({ "path": shown, "created": created, "added": added, "removed": removed, "hunk": diff.hunk });
    if context.snapshots.is_some() {
        // What undoing needs: where the file is, what it held, and what it holds now.
        meta["file"] = json!(path.to_string_lossy());
        meta["before"] = json!(snapshot);
        meta["after"] = json!(crate::undo::fingerprint(written.as_bytes()));
    }
    Ok(ToolOutput::ok(summary, meta))
}

fn edit_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    // One edit in the call's own fields, or several in `edits`.
    let single = [input.clone()];
    let edits: &[Value] = match input["edits"].as_array() {
        Some(edits) if edits.is_empty() => return Err(ToolOutput::error("`edits` is empty.")),
        Some(edits) => edits,
        None => &single,
    };
    let existing = read_existing(&path, &shown)?;
    // Matching happens on LF text.
    let mut text = existing.as_deref().map(|text| text.replace("\r\n", "\n"));
    for (index, edit) in edits.iter().enumerate() {
        // Which edit failed matters only when there are several.
        let failed = |message: String| ToolOutput::error(if edits.len() > 1 { format!("Edit {} of {}: {message} Nothing was written.", index + 1, edits.len()) } else { message });
        let old = string_arg(edit, "old_string")?.replace("\r\n", "\n");
        let new = string_arg(edit, "new_string")?.replace("\r\n", "\n");
        let replace_all = edit["replace_all"].as_bool().unwrap_or(false);
        text = Some(match (&text, old.is_empty()) {
            (None, true) => new,
            (None, false) => return Err(failed(format!("{shown} does not exist. To create it, pass an empty `old_string`."))),
            (Some(current), true) if current.is_empty() => new,
            (Some(_), true) => return Err(failed(format!("{shown} already exists. Pass the text to replace as `old_string`."))),
            (Some(current), false) => match current.matches(old.as_str()).count() {
                0 => return Err(failed(format!("`old_string` was not found in {shown}. Read the file again and copy the text exactly."))),
                1 => current.replacen(old.as_str(), &new, 1),
                _ if replace_all => current.replace(old.as_str(), &new),
                count => return Err(failed(format!("`old_string` occurs {count} times in {shown}. Include more surrounding text to make it unique, or set `replace_all`."))),
            },
        });
    }
    write_change(context, &path, &shown, existing.as_deref(), &text.unwrap_or_default())
}

fn write_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let content = string_arg(input, "content")?.replace("\r\n", "\n");
    if path.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory.")));
    }
    let existing = read_existing(&path, &shown)?;
    write_change(context, &path, &shown, existing.as_deref(), &content)
}

async fn find_files(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let root = match input["path"].as_str() {
        Some(path) => resolve(&context.workspace, path),
        None => context.workspace.clone(),
    };
    let shown = display(&context.workspace, &root);
    let shown = if shown.is_empty() { ".".to_string() } else { shown };
    if !root.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is not a directory.")));
    }
    let Some(glob) = input["glob"].as_str() else {
        // One level, as it is on disk: what .gitignore excludes is listed too.
        let entries = std::fs::read_dir(&root).map_err(|error| ToolOutput::error(format!("Cannot list {shown}: {error}")))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| format!("{}{}", entry.file_name().to_string_lossy(), if entry.file_type().is_ok_and(|kind| kind.is_dir()) { "/" } else { "" }))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        let total = names.len();
        names.truncate(FIND_RESULTS);
        let mut content = if names.is_empty() { format!("{shown} is empty.") } else { names.join("\n") };
        if total > FIND_RESULTS {
            content.push_str(&format!("\n[showing {FIND_RESULTS} of {total} entries]"));
        }
        return Ok(ToolOutput::ok(content, json!({ "path": shown, "entries": names.len(), "truncated": total > FIND_RESULTS })));
    };
    let matcher = globset::GlobBuilder::new(glob)
        .literal_separator(false)
        .build()
        .map_err(|error| ToolOutput::error(format!("Invalid glob: {error}")))?
        .compile_matcher();
    let workspace = context.workspace.clone();
    let cancel = context.cancel.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut found: Vec<String> = Vec::new();
        let mut truncated = false;
        // Dot files count (`.github/workflows/ci.yml` is a file one looks for); Git's own directory does not.
        let walk = ignore::WalkBuilder::new(&root).hidden(false).filter_entry(|entry| entry.file_name() != ".git").build();
        for entry in walk.filter_map(Result::ok) {
            if cancel.is_cancelled() {
                break;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = display(&workspace, entry.path());
            if matcher.is_match(&path) {
                if found.len() == FIND_RESULTS {
                    truncated = true;
                    break;
                }
                found.push(path);
            }
        }
        found.sort_by_key(|path| path.to_lowercase());
        (found, truncated)
    });
    let (found, truncated) = task.await.map_err(|error| ToolOutput::error(format!("Finding files failed: {error}")))?;
    let mut content = if found.is_empty() { format!("No file below {shown} matches `{glob}`.") } else { found.join("\n") };
    if truncated {
        content.push_str(&format!("\n[stopped at {FIND_RESULTS} files; narrow the glob or the path]"));
    }
    Ok(ToolOutput::ok(content, json!({ "path": shown, "glob": glob, "entries": found.len(), "truncated": truncated })))
}

struct LineDiff {
    added: usize,
    removed: usize,
    /// `{ old_start, new_start, lines: [{ kind: " " | "-" | "+", text }] }`, or null when too long to show.
    hunk: Value,
}

/// The changed span between two texts: common leading and trailing lines are
/// dropped, two lines of context are kept on each side.
fn line_diff(before: &str, after: &str) -> LineDiff {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    let hunk = if removed.len() + added.len() > DIFF_LINES {
        Value::Null
    } else {
        let context_start = prefix.saturating_sub(2);
        let line = |kind: &str, text: &&str| json!({ "kind": kind, "text": text });
        let mut lines: Vec<Value> = old[context_start..prefix].iter().map(|text| line(" ", text)).collect();
        lines.extend(removed.iter().map(|text| line("-", text)));
        lines.extend(added.iter().map(|text| line("+", text)));
        lines.extend(old[old.len() - suffix..].iter().take(2).map(|text| line(" ", text)));
        json!({ "old_start": context_start + 1, "new_start": context_start + 1, "lines": lines })
    };
    LineDiff { added: added.len(), removed: removed.len(), hunk }
}

async fn search(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let pattern = string_arg(input, "pattern")?.to_string();
    let regex = regex::Regex::new(&pattern).map_err(|error| ToolOutput::error(format!("Invalid regular expression: {error}")))?;
    let root = match input["path"].as_str() {
        Some(path) => resolve(&context.workspace, path),
        None => context.workspace.clone(),
    };
    if !root.exists() {
        return Err(ToolOutput::error(format!("{} does not exist.", display(&context.workspace, &root))));
    }
    let glob = match input["glob"].as_str() {
        Some(glob) => Some(
            globset::GlobBuilder::new(glob)
                .literal_separator(false)
                .build()
                .map_err(|error| ToolOutput::error(format!("Invalid glob: {error}")))?
                .compile_matcher(),
        ),
        None => None,
    };
    let workspace = context.workspace.clone();
    let cancel = context.cancel.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut matches: Vec<String> = Vec::new();
        let mut files = 0usize;
        let mut truncated = false;
        'walk: for entry in ignore::WalkBuilder::new(&root).build().filter_map(Result::ok) {
            if cancel.is_cancelled() {
                break;
            }
            let path = entry.path();
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let shown = display(&workspace, path);
            if glob.as_ref().is_some_and(|glob| !glob.is_match(&shown)) {
                continue;
            }
            if entry.metadata().map(|metadata| metadata.len() > SEARCH_FILE_BYTES).unwrap_or(true) {
                continue;
            }
            // Files that are not UTF-8 text are skipped: they are binaries or use an encoding the model cannot read.
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            let mut matched = false;
            for (index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    if matches.len() == SEARCH_RESULTS {
                        truncated = true;
                        break 'walk;
                    }
                    matched = true;
                    let line: String = line.trim_end().chars().take(300).collect();
                    matches.push(format!("{shown}:{}: {line}", index + 1));
                }
            }
            files += usize::from(matched);
        }
        (matches, files, truncated)
    });
    let (matches, files, truncated) = task.await.map_err(|error| ToolOutput::error(format!("Search failed: {error}")))?;
    let mut content = if matches.is_empty() { format!("No matches for `{pattern}`.") } else { matches.join("\n") };
    if truncated {
        content.push_str(&format!("\n[stopped at {SEARCH_RESULTS} matches; narrow the pattern, path, or glob]"));
    }
    Ok(ToolOutput::ok(content, json!({ "pattern": pattern, "matches": matches.len(), "files": files, "truncated": truncated })))
}

/// Read a pipe to its end, keeping at most `COMMAND_CAPTURE_BYTES`.
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    while let Ok(read) = pipe.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let room = COMMAND_CAPTURE_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
    kept
}

/// Remove terminal escape sequences: colors and cursor moves (CSI), links and
/// titles (OSC), and the two-character escapes. They mean nothing to the
/// model, cost tokens in every later request, and clutter the transcript.
pub(crate) fn strip_ansi(text: &str) -> String {
    static ESCAPES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[ -/]*[0-~])").expect("valid pattern"));
    if text.contains('\u{1b}') { ESCAPES.replace_all(text, "").into_owned() } else { text.to_string() }
}

/// Stop a command and the processes it started.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        // `Child::kill` ends only the shell on Windows; taskkill /T takes its children too.
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status()
            .await;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The shell leads its own process group (see `shell_command`), so this
        // reaches what it started as well: a server it launched does not live on.
        let _ = tokio::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }
    let _ = child.kill().await;
}

/// The shell set up to run `command_line` in the workspace, its output piped.
fn shell_command(command_line: &str, workspace: &Path) -> tokio::process::Command {
    let (program, arguments, _) = shell();
    let mut command = tokio::process::Command::new(program);
    command.args(arguments);
    if cfg!(windows) {
        // PowerShell writes pipes in the console code page unless told otherwise,
        // and PowerShell 7 colors tables and redirected stderr even into a pipe.
        command.arg(format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; if ($PSStyle) {{ $PSStyle.OutputRendering = 'PlainText' }}; {command_line}"
        ));
    } else {
        command.arg(command_line);
    }
    // Ask the programs the command runs not to color either; whatever still does is stripped from the output.
    command.env("NO_COLOR", "1");
    command.current_dir(workspace).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    // Its own process group, so stopping the command can stop everything it started.
    #[cfg(unix)]
    command.process_group(0);
    command
}

fn job_arg<'a>(input: &'a Value, context: &'a ToolContext) -> Result<(&'a str, &'a Arc<Jobs>), ToolOutput> {
    let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
    Ok((string_arg(input, "job")?, jobs))
}

/// What a background command printed, cut like a foreground command's output, with how it stands.
fn job_report(id: &str, report: Report) -> ToolOutput {
    let mut output = report.output.trim_end().to_string();
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        // The newest output says the most about where a running command stands.
        let tail: String = output.chars().skip(total - COMMAND_OUTPUT_CHARS).collect();
        output = format!("[... {} earlier characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let state = match report.ended {
        None => "still running".to_string(),
        Some(Ended::Stopped) => "stopped".to_string(),
        Some(Ended::Exited(Some(code))) => format!("exited with code {code}"),
        Some(Ended::Exited(None)) => "ended without an exit code".to_string(),
    };
    let content = format!("{}\n[job {id}: {state}]", if output.is_empty() { "(nothing new)" } else { output.as_str() });
    let exit_code = match report.ended {
        Some(Ended::Exited(code)) => code,
        _ => None,
    };
    let failed = matches!(report.ended, Some(Ended::Exited(code)) if code != Some(0));
    ToolOutput {
        content,
        is_error: failed,
        meta: json!({ "job": id, "command": report.command, "running": report.ended.is_none(), "stopped": matches!(report.ended, Some(Ended::Stopped)).then_some("stopped"), "exit_code": exit_code, "output": output }),
    }
}

/// A job id that names nothing: jobs do not outlive the app.
fn no_job(id: &str) -> ToolOutput {
    ToolOutput::error(format!("There is no background command `{id}`. Background commands end when the app closes; start it again if you need it."))
}

async fn command_output(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let wait = Duration::from_secs(input["wait_seconds"].as_u64().unwrap_or(0).min(OUTPUT_WAIT_LIMIT_SECONDS));
    let report = jobs.read(id, wait, &context.cancel).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

async fn stop_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let report = jobs.stop(id).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

async fn run_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let command_line = string_arg(input, "command")?.to_string();
    let timeout = Duration::from_secs(input["timeout_seconds"].as_u64().unwrap_or(COMMAND_TIMEOUT_SECONDS).clamp(1, COMMAND_TIMEOUT_LIMIT_SECONDS));
    let (program, _, _) = shell();
    let mut command = shell_command(&command_line, &context.workspace);
    if input["background"].as_bool().unwrap_or(false) {
        let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
        let id = jobs.start(&command_line, command).map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
        let content = format!("Started in the background as job {id}. Read what it prints with `command_output`, and end it with `stop_command` when it is no longer needed.");
        return Ok(ToolOutput::ok(content, json!({ "command": command_line, "job": id, "background": true })));
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
    let stdout = tokio::spawn(capture(child.stdout.take().expect("stdout is piped")));
    let stderr = tokio::spawn(capture(child.stderr.take().expect("stderr is piped")));

    let (status, stopped) = tokio::select! {
        status = child.wait() => (status.ok(), None),
        () = tokio::time::sleep(timeout) => (None, Some("timed out")),
        () = context.cancel.cancelled() => (None, Some("cancelled")),
    };
    if stopped.is_some() {
        kill_tree(&mut child).await;
    }
    // A grandchild can keep the pipes open after the kill, so reading gets a deadline.
    let read = |task| async { tokio::time::timeout(Duration::from_secs(2), task).await.ok().and_then(Result::ok).unwrap_or_default() };
    let (stdout, stderr): (Vec<u8>, Vec<u8>) = (read(stdout).await, read(stderr).await);

    let mut output = strip_ansi(&String::from_utf8_lossy(&stdout).replace("\r\n", "\n")).trim_end().to_string();
    let stderr = strip_ansi(&String::from_utf8_lossy(&stderr).replace("\r\n", "\n"));
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("[stderr]\n{}", stderr.trim_end()));
    }
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        let half = COMMAND_OUTPUT_CHARS / 2;
        let head: String = output.chars().take(half).collect();
        let tail: String = output.chars().skip(total - half).collect();
        output = format!("{head}\n[... {} characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let exit_code = status.and_then(|status| status.code());
    let mut content = if output.is_empty() { "(no output)".to_string() } else { output.clone() };
    match (stopped, exit_code) {
        (Some("timed out"), _) => content.push_str(&format!("\n[stopped after {} seconds: timed out]", timeout.as_secs())),
        (Some(reason), _) => content.push_str(&format!("\n[stopped: {reason}]")),
        (None, Some(0)) => {}
        (None, Some(code)) => content.push_str(&format!("\n[exit code {code}]")),
        (None, None) => content.push_str("\n[ended without an exit code]"),
    }
    Ok(ToolOutput {
        content,
        is_error: stopped.is_some() || exit_code != Some(0),
        meta: json!({
            "command": command_line,
            "exit_code": exit_code,
            "stopped": stopped,
            "duration_ms": started.elapsed().as_millis() as u64,
            "output": output,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (tempfile::TempDir, ToolContext) {
        let directory = tempfile::tempdir().unwrap();
        let context = ToolContext { workspace: directory.path().to_path_buf(), cancel: CancellationToken::new(), snapshots: None, jobs: None };
        (directory, context)
    }

    #[tokio::test]
    async fn reads_numbered_lines_with_offset_and_limit() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let all = execute("read_file", &json!({ "path": "a.txt" }), &context).await;
        assert!(!all.is_error);
        assert_eq!(all.content, "     1\tone\n     2\ttwo\n     3\tthree\n     4\tfour");
        assert_eq!(all.meta, json!({ "path": "a.txt", "lines": 4, "shown": 4 }));

        let part = execute("read_file", &json!({ "path": "a.txt", "offset": 2, "limit": 2 }), &context).await;
        assert_eq!(part.content, "     2\ttwo\n     3\tthree\n[showing lines 2-3 of 4; pass offset to read more]");

        assert!(execute("read_file", &json!({ "path": "missing.txt" }), &context).await.is_error);
        assert!(execute("read_file", &json!({}), &context).await.is_error);
        std::fs::write(directory.path().join("b.bin"), [1u8, 0, 2]).unwrap();
        assert!(execute("read_file", &json!({ "path": "b.bin" }), &context).await.content.contains("binary"));
    }

    #[tokio::test]
    async fn stops_a_read_of_long_lines_at_the_output_limit() {
        let (directory, context) = context();
        // 2000 lines of 1500 characters: within the line and file limits, but 3 MB of text.
        let line = "x".repeat(1500);
        std::fs::write(directory.path().join("wide.txt"), format!("{line}\n").repeat(2000)).unwrap();
        let read = execute("read_file", &json!({ "path": "wide.txt" }), &context).await;
        assert!(!read.is_error);
        assert!(read.content.chars().count() < READ_OUTPUT_CHARS + 200);
        let shown = read.meta["shown"].as_u64().unwrap();
        assert!(shown > 0 && shown < 2000);
        assert!(read.content.ends_with(&format!("pass offset {} to read on]", shown + 1)), "{}", &read.content[read.content.len() - 200..]);

        let next = execute("read_file", &json!({ "path": "wide.txt", "offset": shown + 1 }), &context).await;
        assert!(next.content.starts_with(&format!("{:>6}\t", shown + 1)));
    }

    #[tokio::test]
    async fn edits_exactly_one_occurrence_and_keeps_crlf() {
        let (directory, context) = context();
        let file = directory.path().join("src").join("lib.rs");
        let created = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "", "new_string": "fn a() {}\nfn b() {}\n" }), &context).await;
        assert!(!created.is_error, "{}", created.content);
        assert_eq!(created.meta["created"], true);

        std::fs::write(&file, "fn a() {}\r\nfn b() {}\r\nfn a() {}\r\n").unwrap();
        let ambiguous = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn a() {}", "new_string": "fn c() {}" }), &context).await;
        assert!(ambiguous.is_error && ambiguous.content.contains("2 times"));
        let missing = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn z()", "new_string": "x" }), &context).await;
        assert!(missing.is_error && missing.content.contains("not found"));

        let edited = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn b() {}\nfn a() {}", "new_string": "fn b() {\n    1;\n}" }), &context).await;
        assert!(!edited.is_error, "{}", edited.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn a() {}\r\nfn b() {\r\n    1;\r\n}\r\n");
        assert_eq!((edited.meta["added"].as_u64(), edited.meta["removed"].as_u64()), (Some(3), Some(2)));
        let kinds: String = edited.meta["hunk"]["lines"].as_array().unwrap().iter().map(|line| line["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, " --+++");

        let all = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn", "new_string": "pub fn", "replace_all": true }), &context).await;
        assert!(!all.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap().matches("pub fn").count(), 2);
    }

    #[tokio::test]
    async fn searches_text_files_and_honours_gitignore_and_glob() {
        let (directory, context) = context();
        let root = directory.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "target\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    read_events();\n}\n").unwrap();
        std::fs::write(root.join("notes.md"), "call read_events here\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "read_events\n").unwrap();

        let found = execute("search", &json!({ "pattern": r"read_events\b" }), &context).await;
        assert!(!found.is_error);
        let mut lines: Vec<&str> = found.content.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, vec!["notes.md:1: call read_events here", "src/main.rs:2:     read_events();"]);
        assert_eq!(found.meta["files"], 2);

        let rust_only = execute("search", &json!({ "pattern": "read_events", "glob": "**/*.rs" }), &context).await;
        assert_eq!(rust_only.content, "src/main.rs:2:     read_events();");
        assert_eq!(execute("search", &json!({ "pattern": "nothing_like_this" }), &context).await.content, "No matches for `nothing_like_this`.");
        assert!(execute("search", &json!({ "pattern": "(" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn runs_commands_and_reports_exit_code_timeout_and_cancel() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
        let listed = execute("run_command", &json!({ "command": if cfg!(windows) { "Get-ChildItem -Name" } else { "ls" } }), &context).await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("marker.txt"));
        assert_eq!(listed.meta["exit_code"], 0);

        let unicode = execute("run_command", &json!({ "command": "echo 分身" }), &context).await;
        assert_eq!(unicode.content.trim(), "分身");

        let failed = execute("run_command", &json!({ "command": "exit 3" }), &context).await;
        assert!(failed.is_error);
        assert!(failed.content.ends_with("[exit code 3]"), "{}", failed.content);

        let sleep = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
        let started = Instant::now();
        let timed_out = execute("run_command", &json!({ "command": sleep, "timeout_seconds": 1 }), &context).await;
        assert!(timed_out.is_error && timed_out.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(15));

        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let cancelled = execute("run_command", &json!({ "command": sleep }), &context).await;
        assert!(cancelled.content.contains("[stopped: cancelled]"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn leaves_a_command_running_reads_what_it_prints_and_stops_it() {
        let (_directory, mut context) = context();
        assert!(execute("run_command", &json!({ "command": "echo x", "background": true }), &context).await.is_error);
        let jobs = Arc::new(Jobs::default());
        context.jobs = Some(jobs.clone());

        // A command that prints, then stays: a server.
        let serve = if cfg!(windows) { "'ready'; Start-Sleep -Seconds 60; 'never'" } else { "echo ready; sleep 60; echo never" };
        let started = execute("run_command", &json!({ "command": serve, "background": true }), &context).await;
        assert!(!started.is_error && started.meta["job"] == "b1" && started.meta["background"] == true, "{}", started.content);
        let read = execute("command_output", &json!({ "job": "b1", "wait_seconds": 30 }), &context).await;
        assert_eq!(read.content, "ready\n[job b1: still running]");
        assert_eq!((read.meta["running"].as_bool(), read.is_error), (Some(true), false));
        // Nothing new: the read returns at once and says so.
        assert_eq!(execute("command_output", &json!({ "job": "b1" }), &context).await.content, "(nothing new)\n[job b1: still running]");
        assert_eq!(jobs.running(), [("b1".to_string(), serve.to_string())]);

        let begun = Instant::now();
        let stopped = execute("stop_command", &json!({ "job": "b1" }), &context).await;
        assert!(stopped.content.ends_with("[job b1: stopped]") && !stopped.content.contains("never"), "{}", stopped.content);
        assert!(begun.elapsed() < Duration::from_secs(20));
        assert!(jobs.running().is_empty());

        // Stopping takes what the shell started with it, not only the shell.
        #[cfg(unix)]
        {
            // The shell starts a child of its own and prints the child's process id.
            let child = execute("run_command", &json!({ "command": "sleep 300 & echo $!; wait", "background": true }), &context).await;
            let id = child.meta["job"].as_str().unwrap().to_string();
            let printed = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
            let pid = printed.content.lines().next().unwrap().trim().to_string();
            // A process that is gone has no state; one that was killed and not yet collected shows Z.
            let alive = || {
                let state = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid]).output().unwrap().stdout;
                !state.is_empty() && !state.starts_with(b"Z")
            };
            assert!(alive(), "{}", printed.content);
            execute("stop_command", &json!({ "job": id }), &context).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!alive(), "the command's child outlived it");
        }

        // A command that ends by itself reports its exit code, and a failure is an error result.
        let fail = execute("run_command", &json!({ "command": "echo 分身; exit 3", "background": true }), &context).await;
        let id = fail.meta["job"].as_str().unwrap().to_string();
        let ended = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
        // The exit can arrive a moment after the output; a second read settles it.
        let ended = if ended.meta["running"] == true { execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await } else { ended };
        assert!(ended.is_error && ended.content.ends_with(&format!("[job {id}: exited with code 3]")), "{}", ended.content);
        assert_eq!(ended.meta["exit_code"], 3);

        assert!(execute("command_output", &json!({ "job": "b9" }), &context).await.content.starts_with("There is no background command `b9`."));
        assert!(execute("stop_command", &json!({ "job": "b9" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn command_output_reaches_the_model_without_terminal_colors() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
        let commands: &[&str] = if cfg!(windows) {
            &[
                // PowerShell 7 colors a table's header.
                "Get-ChildItem | Select-Object Name, Length",
                // ...and a native program's stderr redirected into the pipeline.
                "cmd /c \"echo oops 1>&2\" 2>&1",
                // A program that colors its output regardless.
                "Write-Output \"$([char]27)[31mred$([char]27)[0m $([char]27)]8;;https://example.com$([char]7)link$([char]27)]8;;$([char]7)\"",
            ]
        } else {
            &["printf '\\033[31mred\\033[0m \\033]8;;https://example.com\\007link\\033]8;;\\007\\n'"]
        };
        for command in commands {
            let output = execute("run_command", &json!({ "command": command }), &context).await;
            assert!(!output.content.contains('\u{1b}'), "{command}: {:?}", output.content);
            assert!(!output.meta["output"].as_str().unwrap().contains('\u{1b}'), "{command}");
        }
        let colored = execute("run_command", &json!({ "command": commands.last().unwrap() }), &context).await;
        assert_eq!(colored.content.trim(), "red link");
    }

    #[test]
    fn strips_terminal_escape_sequences() {
        assert_eq!(strip_ansi("\u{1b}[32;1mMode\u{1b}[0m plain"), "Mode plain");
        assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
        assert_eq!(strip_ansi("a\u{1b}(Bb\u{1b}[?25lc"), "abc");
        assert_eq!(strip_ansi("no escapes, 分身"), "no escapes, 分身");
    }

    #[tokio::test]
    async fn finds_files_by_listing_a_directory_or_by_glob() {
        let (directory, context) = context();
        let root = directory.path();
        for file in ["src/main.rs", "src/lexer/mod.rs", "src/lexer/notes.txt", ".github/workflows/ci.yml", ".git/config", "target/debug/app.rs", "README.md"] {
            std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            std::fs::write(root.join(file), "x").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

        // A listing is one level, as it is on disk, directories marked.
        let listed = execute("find_files", &json!({}), &context).await;
        assert_eq!(listed.content, ".git/\n.github/\n.gitignore\nREADME.md\nsrc/\ntarget/");
        assert_eq!(execute("find_files", &json!({ "path": "src/lexer" }), &context).await.content, "mod.rs\nnotes.txt");

        // A glob walks below, skipping what Git ignores and Git's own directory, but not other dot directories.
        let rust = execute("find_files", &json!({ "glob": "**/*.rs" }), &context).await;
        assert_eq!(rust.content, "src/lexer/mod.rs\nsrc/main.rs");
        assert_eq!(rust.meta["entries"], 2);
        assert_eq!(execute("find_files", &json!({ "glob": "**/*.yml" }), &context).await.content, ".github/workflows/ci.yml");
        assert_eq!(execute("find_files", &json!({ "glob": "*config*" }), &context).await.content, "No file below . matches `*config*`.");
        assert_eq!(execute("find_files", &json!({ "path": "src", "glob": "**/mod.rs" }), &context).await.content, "src/lexer/mod.rs");

        assert!(execute("find_files", &json!({ "path": "README.md" }), &context).await.is_error);
        assert!(execute("find_files", &json!({ "glob": "[" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn writes_a_whole_file_and_applies_several_edits_or_none() {
        let (directory, mut context) = context();
        let snapshots = directory.path().join("snapshots");
        context.snapshots = Some(snapshots.clone());
        let file = directory.path().join("notes/a.txt");

        let created = execute("write_file", &json!({ "path": "notes/a.txt", "content": "one\ntwo\nthree\n" }), &context).await;
        assert!(!created.is_error && created.meta["created"] == true && created.meta["before"].is_null(), "{}", created.content);
        // Overwriting saves what was there, like an edit does.
        let rewritten = execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await;
        assert_eq!((rewritten.meta["added"].as_u64(), rewritten.meta["removed"].as_u64()), (Some(3), Some(3)));
        let saved = snapshots.join(rewritten.meta["before"].as_str().unwrap());
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "one\ntwo\nthree\n");
        assert!(execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await.is_error);
        assert!(execute("write_file", &json!({ "path": "notes", "content": "x" }), &context).await.is_error);

        // Several edits apply in order, each to the result of the one before.
        let edits = json!({ "path": "notes/a.txt", "edits": [
            { "old_string": "alpha", "new_string": "first" },
            { "old_string": "first\ntwo", "new_string": "first\nsecond" },
        ] });
        let edited = execute("edit_file", &edits, &context).await;
        assert!(!edited.is_error, "{}", edited.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");

        // One that does not apply stops all of them: the file stays as it was.
        let failing = json!({ "path": "notes/a.txt", "edits": [
            { "old_string": "omega", "new_string": "last" },
            { "old_string": "missing", "new_string": "x" },
        ] });
        let failed = execute("edit_file", &failing, &context).await;
        assert!(failed.is_error && failed.content.starts_with("Edit 2 of 2: `old_string` was not found") && failed.content.ends_with("Nothing was written."), "{}", failed.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");
        assert!(execute("edit_file", &json!({ "path": "notes/a.txt", "edits": [] }), &context).await.is_error);
    }

    #[test]
    fn resolves_paths_and_detects_what_is_outside_the_workspace() {
        let (directory, context) = context();
        let workspace = &context.workspace;
        std::fs::create_dir_all(directory.path().join("src")).unwrap();
        assert_eq!(resolve(workspace, "src/../a.txt"), workspace.join("a.txt"));
        assert!(inside_workspace(workspace, &resolve(workspace, "src/new/file.rs")));
        assert!(inside_workspace(workspace, workspace));
        assert!(!inside_workspace(workspace, &resolve(workspace, "../elsewhere.txt")));
        assert!(!inside_workspace(workspace, &std::env::temp_dir()));

        std::fs::create_dir_all(directory.path().join(".git/hooks")).unwrap();
        assert!(in_git_directory(workspace, &resolve(workspace, ".git/hooks/pre-commit")));
        assert!(in_git_directory(workspace, &resolve(workspace, "vendor/lib/.GIT/config")));
        assert!(!in_git_directory(workspace, &resolve(workspace, "src/.gitignore")));
        assert!(!in_git_directory(workspace, &resolve(workspace, "src/new/file.rs")));
    }
}
