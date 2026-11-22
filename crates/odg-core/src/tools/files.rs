//! File reading, editing, moving, deleting, listing, and searching tools.

use std::path::Path;

use serde_json::{Value, json};

use crate::events::Image;

use super::{READ_FILE_BYTES, READ_OUTPUT_CHARS, ToolContext, ToolOutput, clip, display, resolve, string_arg};

const READ_LINE_LIMIT: usize = 2000;
const SEARCH_RESULTS: usize = 200;
const FIND_RESULTS: usize = 500;
const SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DIFF_LINES: usize = 400;
/// The largest file `delete_file` and `move_file` take: a copy of it is kept for undoing.
const UNDO_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Whether a tool changes files, and so is decided, snapshotted, and undone like an edit.
pub fn edits_files(tool: &str) -> bool {
    matches!(tool, "edit_file" | "write_file" | "delete_file" | "move_file")
}

pub(super) async fn read_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let metadata = std::fs::metadata(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory. Use `find_files` to list it.")));
    }
    // A PDF or an Office file is read for its words, which take a while to take out of a large one: off the thread the agent runs on.
    if let Some(found) = crate::documents::kind_of_file(&path) {
        let kind = found.map_err(|why| ToolOutput::error(format!("{shown} {}.", why.describe())))?;
        if metadata.len() > crate::documents::DOCUMENT_BYTES {
            return Err(ToolOutput::error(format!("{shown} is a document of {} bytes, over the {}-byte limit.", metadata.len(), crate::documents::DOCUMENT_BYTES)));
        }
        let taken = tokio::task::spawn_blocking(move || crate::documents::file_text(&path, kind)).await;
        let read = match taken {
            Ok(Ok(Ok(read))) => read,
            Ok(Ok(Err(why))) => return Err(ToolOutput::error(format!("{shown} {}.", why.describe()))),
            Ok(Err(error)) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
            Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
        };
        let (content, lines, selected) = numbered(&read.text, input, &shown);
        let content = format!("[{shown} is {}]\n{content}", read.describe());
        return Ok(ToolOutput::ok(content, json!({ "path": shown, "lines": lines, "shown": selected, "document": read.kind, "parts": read.parts })));
    }
    let named_picture = crate::events::image_type(&path).is_some();
    if named_picture && metadata.len() > crate::events::IMAGE_FILE_BYTES {
        // Not read whole only to be refused: the picture is said to be too large by its size on disk.
        let megabytes = metadata.len() as f64 / 1_000_000.0;
        return Err(ToolOutput::error(format!(
            "{shown} is a picture of {megabytes:.1} MB, too large to be shown (at most {:.1} MB). Make a smaller copy first, for example with Pillow in the document runtime, and read that.",
            crate::events::IMAGE_FILE_BYTES as f64 / 1_000_000.0
        )));
    }
    if metadata.len() > READ_FILE_BYTES {
        return Err(ToolOutput::error(format!("{shown} is {} bytes, over the {READ_FILE_BYTES}-byte limit.", metadata.len())));
    }
    let bytes = std::fs::read(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    // A picture by its name, or by its first bytes whatever it is called, when they give its size too.
    if named_picture || picture(&bytes).is_some_and(|(_, size)| size.is_some()) {
        return read_picture(&path, &shown, bytes, context);
    }
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ToolOutput::error(format!("{shown} is a binary file.")));
    }
    let (content, lines, selected) = numbered(&String::from_utf8_lossy(&bytes), input, &shown);
    Ok(ToolOutput::ok(content, json!({ "path": shown, "lines": lines, "shown": selected })))
}

/// The lines of `text` that `read_file`'s offset and limit ask for, each with
/// its number, and a note of how to read on when there are more: with how
/// many lines there are in all, and how many are shown.
fn numbered(text: &str, input: &Value, shown: &str) -> (String, usize, usize) {
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
    (content, lines.len(), selected.len())
}

/// A file that is there to delete or move, read whole to keep a copy for undoing. Not a folder or a link,
/// whose undoing a copy of bytes would not do.
fn whole_file(path: &Path, shown: &str, tool: &str) -> Result<Vec<u8>, ToolOutput> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(ToolOutput::error(format!("{shown} does not exist."))),
        Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    };
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a folder; `{tool}` takes one file. Do the files in it one by one, or use run_command.")));
    }
    if metadata.file_type().is_symlink() {
        return Err(ToolOutput::error(format!("{shown} is a link; `{tool}` takes a file. Use run_command for a link.")));
    }
    if metadata.len() > UNDO_FILE_BYTES {
        let megabytes = metadata.len().div_ceil(1024 * 1024);
        return Err(ToolOutput::error(format!("{shown} is {megabytes} MB, too large to keep a copy of for undoing (at most {} MB). Use run_command if it must be done.", UNDO_FILE_BYTES / 1024 / 1024)));
    }
    std::fs::read(path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))
}

/// Keep `bytes` for undoing, when the session keeps snapshots: the snapshot's name.
fn keep(context: &ToolContext, bytes: &[u8], shown: &str) -> Result<Option<String>, ToolOutput> {
    match &context.snapshots {
        Some(directory) => crate::undo::save(directory, bytes).map(Some).map_err(|error| ToolOutput::error(format!("Cannot keep a copy of {shown} for undoing: {error}"))),
        None => Ok(None),
    }
}

/// What undoing needs of one file a tool changed: where it is, what it held (a snapshot; none when it was not
/// there), and what it holds now (a fingerprint; none when it is gone). See [`crate::undo::plan`].
fn change(path: &Path, shown: &str, before: Option<String>, after: Option<&[u8]>) -> Value {
    json!({ "file": path.to_string_lossy(), "path": shown, "before": before, "after": after.map(crate::undo::fingerprint) })
}

pub(super) fn delete_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let bytes = whole_file(&path, &shown, "delete_file")?;
    // The copy comes first: what could not be undone is not done.
    let before = keep(context, &bytes, &shown)?;
    std::fs::remove_file(&path).map_err(|error| ToolOutput::error(format!("Cannot delete {shown}: {error}")))?;
    let mut meta = json!({ "path": shown, "deleted": true, "bytes": bytes.len() });
    // A text file shows what went, as a change that takes every line out.
    let text = String::from_utf8(bytes).ok().filter(|text| !text.contains('\0'));
    let summary = match text {
        Some(text) => {
            let diff = line_diff(&text.replace("\r\n", "\n"), "");
            meta["removed"] = json!(diff.removed);
            meta["hunk"] = json!(diff.hunk);
            format!("Deleted {shown} ({} lines).", diff.removed)
        }
        None => format!("Deleted {shown} ({} bytes).", meta["bytes"]),
    };
    if context.snapshots.is_some() {
        meta["changes"] = json!([change(&path, &shown, before, None)]);
    }
    Ok(ToolOutput::ok(summary, meta))
}

pub(super) fn move_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let from = resolve(&context.workspace, string_arg(input, "path")?);
    let to = resolve(&context.workspace, string_arg(input, "to")?);
    let (shown_from, shown_to) = (display(&context.workspace, &from), display(&context.workspace, &to));
    if from == to {
        return Err(ToolOutput::error(format!("{shown_from} is already there.")));
    }
    let bytes = whole_file(&from, &shown_from, "move_file")?;
    // Only a change of case may find the file itself at `to`, on a file system that ignores case.
    let recased = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
    match std::fs::symlink_metadata(&to) {
        Ok(_) if !recased => {
            return Err(ToolOutput::error(format!("{shown_to} already exists. Delete it first with delete_file if it is to be replaced, or choose another name.")));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ToolOutput::error(format!("Cannot look at {shown_to}: {error}"))),
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let before = keep(context, &bytes, &shown_from)?;
    if std::fs::rename(&from, &to).is_err() {
        // Across volumes a file cannot be renamed: it is written there and taken away here.
        crate::fsutil::write_atomically(&to, &bytes).map_err(|error| ToolOutput::error(format!("Cannot write {shown_to}: {error}")))?;
        if let Err(error) = std::fs::remove_file(&from) {
            let _ = std::fs::remove_file(&to);
            return Err(ToolOutput::error(format!("Cannot move {shown_from}: {error}")));
        }
    }
    let mut meta = json!({ "path": shown_from, "to": shown_to, "moved": true, "bytes": bytes.len() });
    if context.snapshots.is_some() {
        // Where it is now, to be opened; and for undoing, the file that came and the file that went.
        meta["file"] = json!(to.to_string_lossy());
        meta["changes"] = json!([change(&to, &shown_to, None, Some(&bytes)), change(&from, &shown_from, before, None)]);
    }
    Ok(ToolOutput::ok(format!("Moved {shown_from} to {shown_to}."), meta))
}

/// What a picture's first bytes say it is, whatever its name says: its type and, where its header gives them,
/// its width and height. `None` for anything but PNG, JPEG, GIF, and WebP.
pub(super) fn picture(bytes: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    let u16_be = |at: usize| bytes.get(at..at + 2).map(|two| u32::from(u16::from_be_bytes([two[0], two[1]])));
    let u16_le = |at: usize| bytes.get(at..at + 2).map(|two| u32::from(u16::from_le_bytes([two[0], two[1]])));
    let u24_le = |at: usize| bytes.get(at..at + 3).map(|three| u32::from(three[0]) | u32::from(three[1]) << 8 | u32::from(three[2]) << 16);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let size = bytes.get(16..24).map(|header| (u32::from_be_bytes([header[0], header[1], header[2], header[3]]), u32::from_be_bytes([header[4], header[5], header[6], header[7]])));
        return Some(("image/png", size));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("image/gif", u16_le(6).zip(u16_le(8))));
    }
    if bytes.len() >= 16 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let size = match &bytes[12..16] {
            b"VP8 " => u16_le(26).zip(u16_le(28)).map(|(width, height)| (width & 0x3fff, height & 0x3fff)),
            b"VP8L" => bytes.get(21..25).map(|bits| {
                let [b0, b1, b2, b3] = [bits[0], bits[1], bits[2], bits[3]].map(u32::from);
                (1 + (b0 | (b1 & 0x3f) << 8), 1 + (b1 >> 6 | b2 << 2 | (b3 & 0x0f) << 10))
            }),
            b"VP8X" => u24_le(24).zip(u24_le(27)).map(|(width, height)| (width + 1, height + 1)),
            _ => None,
        };
        return Some(("image/webp", size));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        // The size is in the first start-of-frame segment; the segments before it are stepped over by their lengths.
        let mut at = 2;
        let mut size = None;
        while at + 4 <= bytes.len() && bytes[at] == 0xff {
            let marker = bytes[at + 1];
            if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                size = u16_be(at + 7).zip(u16_be(at + 5));
                break;
            }
            match marker {
                0xff => at += 1,
                0x01 | 0xd0..=0xd9 => at += 2,
                _ => at += 2 + u16_be(at + 2)? as usize,
            }
        }
        return Some(("image/jpeg", size));
    }
    None
}

/// A picture `read_file` was asked for: shown to the model, when it can see pictures, with what it is.
fn read_picture(path: &Path, shown: &str, bytes: Vec<u8>, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let Some((media_type, size)) = picture(&bytes) else {
        return Err(ToolOutput::error(format!("{shown} is named as a picture but is not a PNG, JPEG, GIF, or WebP one.")));
    };
    let kind = media_type.trim_start_matches("image/").to_uppercase();
    let dimensions = size.map(|(width, height)| format!(", {width}×{height}")).unwrap_or_default();
    let described = format!("{shown} is a {kind} picture{dimensions}, {} KB", bytes.len().div_ceil(1024));
    if context.vision == Some(false) {
        return Err(ToolOutput::error(format!("{described}. The model at work cannot see pictures: ask the user what it shows, or to switch to a model that can.")));
    }
    if bytes.len() as u64 > crate::events::IMAGE_FILE_BYTES {
        let most = crate::events::IMAGE_FILE_BYTES as f64 / 1_000_000.0;
        return Err(ToolOutput::error(format!("{described}: too large to be shown, which takes at most {most:.1} MB. Make a smaller copy first, for example with Pillow in the document runtime, and read that.")));
    }
    let (width, height) = size.unzip();
    let meta = json!({ "path": shown, "file": path.to_string_lossy(), "picture": media_type, "bytes": bytes.len(), "width": width, "height": height });
    let image = Image { media_type: media_type.to_string(), data: crate::events::base64(&bytes) };
    Ok(ToolOutput { content: format!("{described}. It is shown to you here."), is_error: false, meta, images: vec![image] })
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

pub(super) fn edit_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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

pub(super) fn write_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let content = string_arg(input, "content")?.replace("\r\n", "\n");
    if path.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory.")));
    }
    let existing = read_existing(&path, &shown)?;
    write_change(context, &path, &shown, existing.as_deref(), &content)
}

pub(super) async fn find_files(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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

pub(super) async fn search(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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
