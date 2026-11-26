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
    let metadata =
        std::fs::metadata(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!(
            "{shown} is a directory. Use `find_files` to list it."
        )));
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
    // Enforce the 50 MB ceiling BEFORE `kind_of_file` so oversized `.zip` / datasets never open `ZipArchive`.
    if metadata.len() > crate::documents::DOCUMENT_BYTES {
        return Err(ToolOutput::error(format!(
            "{shown} is {} bytes, over the {}-byte limit.",
            metadata.len(),
            crate::documents::DOCUMENT_BYTES
        )));
    }
    // Sniff archive/document headers and extract words off the async runtime thread with `catch_unwind` protection.
    let probe_path = path.clone();
    let doc_probe = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| match crate::documents::kind_of_file(&probe_path) {
            Some(Ok(kind)) => Some(Ok(crate::documents::file_text(&probe_path, kind))),
            Some(Err(why)) => Some(Err(why)),
            None => None,
        })
        .unwrap_or(Some(Err(crate::documents::DocError::Unreadable)))
    })
    .await
    .map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;

    if let Some(found) = doc_probe {
        let taken = found.map_err(|why| ToolOutput::error(format!("{shown} {}.", why.describe())))?;
        let read = match taken {
            Ok(Ok(read)) => read,
            Ok(Err(why)) => return Err(ToolOutput::error(format!("{shown} {}.", why.describe()))),
            Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
        };
        let (content, lines, selected) = numbered(&read.text, input, &shown);
        let content = format!("[{shown} is {}]\n{content}", read.describe());
        return Ok(ToolOutput::ok(
            content,
            json!({ "path": shown, "lines": lines, "shown": selected, "document": read.kind, "parts": read.parts }),
        ));
    }
    if metadata.len() > READ_FILE_BYTES {
        return Err(ToolOutput::error(format!(
            "{shown} is {} bytes, over the {READ_FILE_BYTES}-byte limit.",
            metadata.len()
        )));
    }
    let read_path = path.clone();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&read_path))
        .await
        .map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?
        .map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    // A picture by its name, or by its first bytes whatever it is called, when they give its size too.
    if named_picture || picture(&bytes).is_some_and(|(_, size)| size.is_some()) {
        return read_picture(&path, &shown, bytes, context);
    }
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ToolOutput::error(format!("{shown} is a binary file.")));
    }
    let (content, lines, selected) = numbered(&String::from_utf8_lossy(&bytes), input, &shown);
    Ok(ToolOutput::ok(
        content,
        json!({ "path": shown, "lines": lines, "shown": selected }),
    ))
}

/// The lines of `text` that `read_file`'s offset and limit ask for, each with
/// its number, and a note of how to read on when there are more: with how
/// many lines there are in all, and how many are shown.
fn numbered(text: &str, input: &Value, shown: &str) -> (String, usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"]
        .as_u64()
        .map(|limit| limit as usize)
        .unwrap_or(READ_LINE_LIMIT)
        .clamp(1, READ_LINE_LIMIT);
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
        content.push_str(&format!(
            "\n[showing lines {offset}-{last} of {}; pass offset to read more]",
            lines.len()
        ));
    }
    (content, lines.len(), selected.len())
}

/// A file that is there to delete or move, read whole to keep a copy for undoing. Not a folder or a link,
/// whose undoing a copy of bytes would not do.
fn whole_file(path: &Path, shown: &str, tool: &str) -> Result<Vec<u8>, ToolOutput> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolOutput::error(format!("{shown} does not exist.")));
        }
        Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    };
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!(
            "{shown} is a folder; `{tool}` takes one file. Do the files in it one by one, or use run_command."
        )));
    }
    if metadata.file_type().is_symlink() {
        return Err(ToolOutput::error(format!(
            "{shown} is a link; `{tool}` takes a file. Use run_command for a link."
        )));
    }
    if metadata.len() > UNDO_FILE_BYTES {
        let megabytes = metadata.len().div_ceil(1024 * 1024);
        return Err(ToolOutput::error(format!(
            "{shown} is {megabytes} MB, too large to keep a copy of for undoing (at most {} MB). Use run_command if it must be done.",
            UNDO_FILE_BYTES / 1024 / 1024
        )));
    }
    std::fs::read(path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))
}

/// Keep `bytes` for undoing, when the session keeps snapshots: the snapshot's name.
fn keep(context: &ToolContext, bytes: &[u8], shown: &str) -> Result<Option<String>, ToolOutput> {
    match &context.snapshots {
        Some(directory) => crate::undo::save(directory, bytes)
            .map(Some)
            .map_err(|error| ToolOutput::error(format!("Cannot keep a copy of {shown} for undoing: {error}"))),
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

/// Whether `a` and `b` refer to the exact same file on disk (not merely two paths whose names match ignoring case).
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!((std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    }
}

pub(super) fn move_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let from = resolve(&context.workspace, string_arg(input, "path")?);
    let to = resolve(&context.workspace, string_arg(input, "to")?);
    let (shown_from, shown_to) = (display(&context.workspace, &from), display(&context.workspace, &to));
    if from == to {
        return Err(ToolOutput::error(format!("{shown_from} is already there.")));
    }
    let bytes = whole_file(&from, &shown_from, "move_file")?;
    // Only a change of case on the same file (on a case-insensitive filesystem) may find an entry at `to`;
    // on a case-sensitive filesystem, a separate file differing only in case must not be overwritten.
    let recased = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase() && same_file(&from, &to);
    match std::fs::symlink_metadata(&to) {
        Ok(_) if !recased => {
            return Err(ToolOutput::error(format!(
                "{shown_to} already exists. Delete it first with delete_file if it is to be replaced, or choose another name."
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ToolOutput::error(format!("Cannot look at {shown_to}: {error}"))),
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let before = keep(context, &bytes, &shown_from)?;
    if std::fs::rename(&from, &to).is_err() {
        // Across volumes a file cannot be renamed: it is written there and taken away here.
        crate::fsutil::write_atomically(&to, &bytes)
            .map_err(|error| ToolOutput::error(format!("Cannot write {shown_to}: {error}")))?;
        if let Err(error) = std::fs::remove_file(&from) {
            let _ = std::fs::remove_file(&to);
            return Err(ToolOutput::error(format!("Cannot move {shown_from}: {error}")));
        }
    }
    let mut meta = json!({ "path": shown_from, "to": shown_to, "moved": true, "bytes": bytes.len() });
    if context.snapshots.is_some() {
        // Where it is now, to be opened; and for undoing, the file that came and the file that went.
        meta["file"] = json!(to.to_string_lossy());
        meta["changes"] = json!([
            change(&to, &shown_to, None, Some(&bytes)),
            change(&from, &shown_from, before, None)
        ]);
    }
    Ok(ToolOutput::ok(format!("Moved {shown_from} to {shown_to}."), meta))
}

/// What a picture's first bytes say it is, whatever its name says: its type and, where its header gives them,
/// its width and height. `None` for anything but PNG, JPEG, GIF, and WebP.
pub(super) fn picture(bytes: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    let u16_be = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|two| u32::from(u16::from_be_bytes([two[0], two[1]])))
    };
    let u16_le = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|two| u32::from(u16::from_le_bytes([two[0], two[1]])))
    };
    let u24_le = |at: usize| {
        bytes
            .get(at..at + 3)
            .map(|three| u32::from(three[0]) | u32::from(three[1]) << 8 | u32::from(three[2]) << 16)
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let size = bytes.get(16..24).map(|header| {
            (
                u32::from_be_bytes([header[0], header[1], header[2], header[3]]),
                u32::from_be_bytes([header[4], header[5], header[6], header[7]]),
            )
        });
        return Some(("image/png", size));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("image/gif", u16_le(6).zip(u16_le(8))));
    }
    if bytes.len() >= 16 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let size = match &bytes[12..16] {
            b"VP8 " => u16_le(26)
                .zip(u16_le(28))
                .map(|(width, height)| (width & 0x3fff, height & 0x3fff)),
            b"VP8L" => bytes.get(21..25).map(|bits| {
                let [b0, b1, b2, b3] = [bits[0], bits[1], bits[2], bits[3]].map(u32::from);
                (1 + (b0 | (b1 & 0x3f) << 8), 1 + (b1 >> 6 | b2 << 2 | (b3 & 0x0f) << 10))
            }),
            b"VP8X" => u24_le(24)
                .zip(u24_le(27))
                .map(|(width, height)| (width + 1, height + 1)),
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
        return Err(ToolOutput::error(format!(
            "{shown} is named as a picture but is not a PNG, JPEG, GIF, or WebP one."
        )));
    };
    let kind = media_type.trim_start_matches("image/").to_uppercase();
    let dimensions = size
        .map(|(width, height)| format!(", {width}×{height}"))
        .unwrap_or_default();
    let described = format!(
        "{shown} is a {kind} picture{dimensions}, {} KB",
        bytes.len().div_ceil(1024)
    );
    if context.vision == Some(false) {
        return Err(ToolOutput::error(format!(
            "{described}. The model at work cannot see pictures: ask the user what it shows, or to switch to a model that can."
        )));
    }
    if bytes.len() as u64 > crate::events::IMAGE_FILE_BYTES {
        let most = crate::events::IMAGE_FILE_BYTES as f64 / 1_000_000.0;
        return Err(ToolOutput::error(format!(
            "{described}: too large to be shown, which takes at most {most:.1} MB. Make a smaller copy first, for example with Pillow in the document runtime, and read that."
        )));
    }
    let (width, height) = size.unzip();
    let meta = json!({ "path": shown, "file": path.to_string_lossy(), "picture": media_type, "bytes": bytes.len(), "width": width, "height": height });
    let image = Image {
        media_type: media_type.to_string(),
        data: crate::events::base64(&bytes),
    };
    Ok(ToolOutput {
        content: format!("{described}. It is shown to you here."),
        is_error: false,
        meta,
        images: vec![image],
    })
}

/// A text file as it is on disk: `None` when it does not exist.
fn read_existing(path: &Path, shown: &str) -> Result<Option<String>, ToolOutput> {
    match std::fs::read(path) {
        Ok(bytes) => {
            Ok(Some(String::from_utf8(bytes).map_err(|_| {
                ToolOutput::error(format!("{shown} is not UTF-8 text."))
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    }
}

/// Write `after` (LF text) over what the file held, after saving that for
/// undo, and describe the change. A file that used CRLF gets CRLF back.
fn write_change(
    context: &ToolContext,
    path: &Path,
    shown: &str,
    existing: Option<&str>,
    after: &str,
) -> Result<ToolOutput, ToolOutput> {
    let crlf = existing.is_some_and(|text| text.contains("\r\n"));
    let before = existing.map(|text| text.replace("\r\n", "\n"));
    if before.as_deref() == Some(after) {
        return Err(ToolOutput::error(format!("The change leaves {shown} as it is.")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let written = if crlf {
        after.replace('\n', "\r\n")
    } else {
        after.to_string()
    };
    // The snapshot comes first: a change that could not be undone is not made.
    let snapshot = match (&context.snapshots, existing) {
        (Some(directory), Some(original)) => Some(
            crate::undo::save(directory, original.as_bytes())
                .map_err(|error| ToolOutput::error(format!("Cannot save {shown} before changing it: {error}")))?,
        ),
        _ => None,
    };
    crate::fsutil::write_atomically(path, written.as_bytes())
        .map_err(|error| ToolOutput::error(format!("Cannot write {shown}: {error}")))?;

    let created = before.is_none();
    let diff = line_diff(before.as_deref().unwrap_or(""), after);
    let (added, removed) = (diff.added, diff.removed);
    let summary = if created {
        format!("Created {shown} ({added} lines).")
    } else {
        format!("Changed {shown}: {added} lines added, {removed} lines removed.")
    };
    let mut meta = json!({ "path": shown, "created": created, "added": added, "removed": removed, "hunk": diff.hunk });
    if !diff.hunks.is_empty() {
        meta["hunks"] = json!(diff.hunks);
    }
    if context.snapshots.is_some() {
        let after_snapshot = context
            .snapshots
            .as_ref()
            .filter(|_| written.len() as u64 <= crate::undo::ARTIFACT_SNAPSHOT_BYTES)
            .and_then(|dir| crate::undo::save(dir, written.as_bytes()).ok());
        // What undoing needs: where the file is, what it held, and what it holds now.
        meta["file"] = json!(path.to_string_lossy());
        meta["before"] = json!(snapshot);
        meta["after"] = json!(crate::undo::fingerprint(written.as_bytes()));
        if let Some(after_snapshot) = after_snapshot {
            meta["snapshot"] = json!(after_snapshot);
        }
        meta["bytes"] = json!(written.len());
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
        let failed = |message: String| {
            ToolOutput::error(if edits.len() > 1 {
                format!("Edit {} of {}: {message} Nothing was written.", index + 1, edits.len())
            } else {
                message
            })
        };
        let old = string_arg(edit, "old_string")?.replace("\r\n", "\n");
        let new = string_arg(edit, "new_string")?.replace("\r\n", "\n");
        let replace_all = edit["replace_all"].as_bool().unwrap_or(false);
        text = Some(match (&text, old.is_empty()) {
            (None, true) => new,
            (None, false) => {
                return Err(failed(format!(
                    "{shown} does not exist. To create it, pass an empty `old_string`."
                )));
            }
            (Some(current), true) if current.is_empty() => new,
            (Some(_), true) => {
                return Err(failed(format!(
                    "{shown} already exists. Pass the text to replace as `old_string`."
                )));
            }
            (Some(current), false) => match current.matches(old.as_str()).count() {
                0 => {
                    return Err(failed(format!(
                        "`old_string` was not found in {shown}. Read the file again and copy the text exactly."
                    )));
                }
                1 => current.replacen(old.as_str(), &new, 1),
                _ if replace_all => current.replace(old.as_str(), &new),
                count => {
                    return Err(failed(format!(
                        "`old_string` occurs {count} times in {shown}. Include more surrounding text to make it unique, or set `replace_all`."
                    )));
                }
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
        let entries =
            std::fs::read_dir(&root).map_err(|error| ToolOutput::error(format!("Cannot list {shown}: {error}")))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| {
                format!(
                    "{}{}",
                    entry.file_name().to_string_lossy(),
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        "/"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        let total = names.len();
        names.truncate(FIND_RESULTS);
        let mut content = if names.is_empty() {
            format!("{shown} is empty.")
        } else {
            names.join("\n")
        };
        if total > FIND_RESULTS {
            content.push_str(&format!("\n[showing {FIND_RESULTS} of {total} entries]"));
        }
        return Ok(ToolOutput::ok(
            content,
            json!({ "path": shown, "entries": names.len(), "truncated": total > FIND_RESULTS }),
        ));
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
        let walk = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build();
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
    let (found, truncated) = task
        .await
        .map_err(|error| ToolOutput::error(format!("Finding files failed: {error}")))?;
    let mut content = if found.is_empty() {
        format!("No file below {shown} matches `{glob}`.")
    } else {
        found.join("\n")
    };
    if truncated {
        content.push_str(&format!(
            "\n[stopped at {FIND_RESULTS} files; narrow the glob or the path]"
        ));
    }
    Ok(ToolOutput::ok(
        content,
        json!({ "path": shown, "glob": glob, "entries": found.len(), "truncated": truncated }),
    ))
}

pub(crate) struct LineDiff {
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// First hunk (`{ old_start, new_start, lines: [{ kind: " " | "-" | "+", text }] }`), or null when too long to show.
    pub(crate) hunk: Value,
    /// All hunks when a change spans more than one non-adjacent region; empty otherwise.
    pub(crate) hunks: Vec<Value>,
}

/// One contiguous changed region in full-file 0-based line coordinates.
#[derive(Clone, Copy)]
struct ChangeSpan {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

/// Myers shortest-edit-script line diff on `old` vs `new`, capped at `max_d` edits.
/// Returns `None` when more than `max_d` line edits separate the two slices.
fn myers_spans(
    old: &[&str],
    new: &[&str],
    offset_old: usize,
    offset_new: usize,
    max_d: usize,
) -> Option<Vec<ChangeSpan>> {
    let (n, m) = (old.len(), new.len());
    if n == 0 && m == 0 {
        return Some(Vec::new());
    }
    if n == 0 || m == 0 {
        return (n + m <= max_d).then(|| {
            vec![ChangeSpan {
                old_start: offset_old,
                old_end: offset_old + n,
                new_start: offset_new,
                new_end: offset_new + m,
            }]
        });
    }
    let shift = max_d as isize;
    let mut v = vec![0usize; 2 * max_d + 1];
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=max_d {
        trace.push(v.clone());
        let d_isize = d as isize;
        let mut k = -d_isize;
        while k <= d_isize {
            let down = k == -d_isize || (k != d_isize && v[(k - 1 + shift) as usize] < v[(k + 1 + shift) as usize]);
            let mut x = if down {
                v[(k + 1 + shift) as usize]
            } else {
                v[(k - 1 + shift) as usize] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && old[x] == new[y] {
                x += 1;
                y += 1;
            }
            v[(k + shift) as usize] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let found = found?;

    // Backtrack into reverse-ordered edit operations: true = equal, false = delete/insert.
    #[derive(Clone, Copy)]
    enum Op {
        Equal,
        Delete,
        Insert,
    }
    let mut ops: Vec<Op> = Vec::with_capacity(n + m);
    let (mut x, mut y) = (n, m);
    for d in (1..=found).rev() {
        let prev = &trace[d];
        let k = x as isize - y as isize;
        let d_isize = d as isize;
        let prev_k =
            if k == -d_isize || (k != d_isize && prev[(k - 1 + shift) as usize] < prev[(k + 1 + shift) as usize]) {
                k + 1
            } else {
                k - 1
            };
        let prev_x = prev[(prev_k + shift) as usize];
        let prev_y = (prev_x as isize - prev_k) as usize;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            ops.push(Op::Insert);
            y -= 1;
        } else {
            ops.push(Op::Delete);
            x -= 1;
        }
    }
    while x > 0 && y > 0 {
        ops.push(Op::Equal);
        x -= 1;
        y -= 1;
    }
    ops.reverse();

    // Collapse adjacent deletes and inserts into clean change spans.
    let mut spans: Vec<ChangeSpan> = Vec::new();
    let (mut oi, mut ni) = (offset_old, offset_new);
    let mut at = 0;
    while at < ops.len() {
        if matches!(ops[at], Op::Equal) {
            oi += 1;
            ni += 1;
            at += 1;
            continue;
        }
        let (old_start, new_start) = (oi, ni);
        while at < ops.len() && !matches!(ops[at], Op::Equal) {
            match ops[at] {
                Op::Delete => oi += 1,
                Op::Insert => ni += 1,
                Op::Equal => unreachable!(),
            }
            at += 1;
        }
        spans.push(ChangeSpan {
            old_start,
            old_end: oi,
            new_start,
            new_end: ni,
        });
    }
    Some(spans)
}

/// Multi-hunk line diff between two texts, keeping two lines of context around
/// each changed region and merging nearby hunks whose context meets.
pub(crate) fn line_diff(before: &str, after: &str) -> LineDiff {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];
    let Some(spans) = myers_spans(old_mid, new_mid, prefix, prefix, DIFF_LINES) else {
        return LineDiff {
            added: new_mid.len(),
            removed: old_mid.len(),
            hunk: Value::Null,
            hunks: Vec::new(),
        };
    };
    let removed: usize = spans.iter().map(|span| span.old_end - span.old_start).sum();
    let added: usize = spans.iter().map(|span| span.new_end - span.new_start).sum();
    if spans.is_empty() || removed + added > DIFF_LINES {
        return LineDiff {
            added,
            removed,
            hunk: Value::Null,
            hunks: Vec::new(),
        };
    }
    // Group spans whose 2-line context windows touch or overlap into one hunk.
    let mut groups: Vec<Vec<ChangeSpan>> = Vec::new();
    for span in spans {
        match groups.last_mut() {
            Some(group) if span.old_start <= group.last().unwrap().old_end + 4 => group.push(span),
            _ => groups.push(vec![span]),
        }
    }
    let line = |kind: &str, text: &&str| json!({ "kind": kind, "text": text });
    let mut built: Vec<Value> = Vec::with_capacity(groups.len());
    for group in groups {
        let first = group[0];
        let last = *group.last().unwrap();
        let ctx_before = first.old_start.saturating_sub(2);
        let ctx_after = (last.old_end + 2).min(old.len());
        let old_start = ctx_before + 1;
        let new_start = first.new_start - (first.old_start - ctx_before) + 1;
        let mut lines: Vec<Value> = old[ctx_before..first.old_start]
            .iter()
            .map(|text| line(" ", text))
            .collect();
        for (at, span) in group.iter().enumerate() {
            if at > 0 {
                let prev = group[at - 1];
                lines.extend(old[prev.old_end..span.old_start].iter().map(|text| line(" ", text)));
            }
            lines.extend(old[span.old_start..span.old_end].iter().map(|text| line("-", text)));
            lines.extend(new[span.new_start..span.new_end].iter().map(|text| line("+", text)));
        }
        lines.extend(old[last.old_end..ctx_after].iter().map(|text| line(" ", text)));
        built.push(json!({ "old_start": old_start, "new_start": new_start, "lines": lines }));
    }
    let hunk = built.first().cloned().unwrap_or(Value::Null);
    let hunks = if built.len() > 1 { built } else { Vec::new() };
    LineDiff {
        added,
        removed,
        hunk,
        hunks,
    }
}

const SEARCH_CONTEXT_LIMIT: usize = 10;

pub(super) async fn search(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let pattern = string_arg(input, "pattern")?.to_string();
    let case_insensitive = input["case_insensitive"].as_bool().unwrap_or(false);
    let files_only = input["files_only"].as_bool().unwrap_or(false);
    let context_lines = input["context"]
        .as_u64()
        .map(|lines| (lines as usize).min(SEARCH_CONTEXT_LIMIT))
        .unwrap_or(0);
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| ToolOutput::error(format!("Invalid regular expression: {error}")))?;
    let root = match input["path"].as_str() {
        Some(path) => resolve(&context.workspace, path),
        None => context.workspace.clone(),
    };
    if !root.exists() {
        return Err(ToolOutput::error(format!(
            "{} does not exist.",
            display(&context.workspace, &root)
        )));
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
        let mut output: Vec<String> = Vec::new();
        let mut match_count = 0usize;
        let mut files = 0usize;
        let mut chars = 0usize;
        let mut truncated = false;
        // Include dot files (such as `.github/workflows/ci.yml`), as `find_files` does, while skipping Git's own directory.
        let walk = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
            .build();
        'walk: for entry in walk.filter_map(Result::ok) {
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
            if entry
                .metadata()
                .map(|metadata| metadata.len() > SEARCH_FILE_BYTES)
                .unwrap_or(true)
            {
                continue;
            }
            // Files that are not UTF-8 text are skipped: they are binaries or use an encoding the model cannot read.
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            if files_only {
                if text.lines().any(|line| regex.is_match(line)) {
                    if output.len() == FIND_RESULTS {
                        truncated = true;
                        break 'walk;
                    }
                    output.push(shown);
                    files += 1;
                }
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            let mut hits: Vec<usize> = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                if regex.is_match(line) {
                    if match_count == SEARCH_RESULTS {
                        truncated = true;
                        break;
                    }
                    match_count += 1;
                    hits.push(index);
                }
            }
            if hits.is_empty() {
                if truncated {
                    break 'walk;
                }
                continue;
            }
            files += 1;
            let clip_line = |line: &str| -> String { line.trim_end().chars().take(300).collect() };
            if context_lines == 0 {
                for index in hits {
                    let formatted = format!("{shown}:{}: {}", index + 1, clip_line(lines[index]));
                    let size = formatted.chars().count() + 1;
                    if !output.is_empty() && chars + size > READ_OUTPUT_CHARS {
                        truncated = true;
                        break 'walk;
                    }
                    chars += size;
                    output.push(formatted);
                }
            } else {
                // Merge overlapping or adjacent context windows so no line is printed twice.
                let mut ranges: Vec<(usize, usize)> = Vec::new();
                for &hit in &hits {
                    let start = hit.saturating_sub(context_lines);
                    let end = (hit + context_lines).min(lines.len() - 1);
                    match ranges.last_mut() {
                        Some((_, last_end)) if start <= *last_end + 1 => *last_end = (*last_end).max(end),
                        _ => ranges.push((start, end)),
                    }
                }
                let mut cursor = 0usize;
                for (start, end) in ranges {
                    if !output.is_empty() {
                        chars += 3;
                        output.push("--".to_string());
                    }
                    for (index, line) in lines.iter().enumerate().take(end + 1).skip(start) {
                        while cursor < hits.len() && hits[cursor] < index {
                            cursor += 1;
                        }
                        let sep = if hits.get(cursor) == Some(&index) { ':' } else { '-' };
                        let formatted = format!("{shown}:{}{sep} {}", index + 1, clip_line(line));
                        let size = formatted.chars().count() + 1;
                        if !output.is_empty() && chars + size > READ_OUTPUT_CHARS {
                            truncated = true;
                            break 'walk;
                        }
                        chars += size;
                        output.push(formatted);
                    }
                }
            }
            if truncated {
                break 'walk;
            }
        }
        if files_only {
            output.sort_by_key(|path| path.to_lowercase());
        }
        (output, match_count, files, truncated)
    });
    let (output, match_count, files, truncated) = task
        .await
        .map_err(|error| ToolOutput::error(format!("Search failed: {error}")))?;
    let mut content = if output.is_empty() {
        format!("No matches for `{pattern}`.")
    } else {
        output.join("\n")
    };
    if truncated {
        if files_only {
            content.push_str(&format!(
                "\n[stopped at {FIND_RESULTS} files; narrow the pattern, path, or glob]"
            ));
        } else if match_count >= SEARCH_RESULTS {
            content.push_str(&format!(
                "\n[stopped at {SEARCH_RESULTS} matches; narrow the pattern, path, or glob]"
            ));
        } else {
            content.push_str(&format!(
                "\n[stopped at {READ_OUTPUT_CHARS} characters; narrow the pattern, path, or glob]"
            ));
        }
    }
    let meta = if files_only {
        json!({ "pattern": pattern, "files": files, "files_only": true, "truncated": truncated })
    } else {
        json!({ "pattern": pattern, "matches": match_count, "files": files, "truncated": truncated })
    };
    Ok(ToolOutput::ok(content, meta))
}

const BATCH_MAX_QUERIES: usize = 16;
const BATCH_DEFAULT_ITEM_CHARS: usize = 4_000;
const BATCH_MAX_ITEM_CHARS: usize = 20_000;
const BATCH_DEFAULT_TOTAL_CHARS: usize = 32_000;
const BATCH_MAX_TOTAL_CHARS: usize = 80_000;

pub(super) async fn batch_inspect(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let queries = input["queries"]
        .as_array()
        .ok_or_else(|| ToolOutput::error("`queries` is required and must be an array of inspection objects."))?;
    if queries.is_empty() {
        return Err(ToolOutput::error("`queries` must contain at least 1 query."));
    }
    if queries.len() > BATCH_MAX_QUERIES {
        return Err(ToolOutput::error(format!(
            "`queries` has {} items, exceeding the maximum of {BATCH_MAX_QUERIES} per call.",
            queries.len()
        )));
    }
    let max_total = input["max_total_chars"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(BATCH_DEFAULT_TOTAL_CHARS)
        .clamp(1_000, BATCH_MAX_TOTAL_CHARS);

    let total = queries.len();
    let mut sections: Vec<String> = Vec::with_capacity(total);
    let mut used_chars = 0usize;
    let mut errors = 0usize;
    let mut any_truncated = false;

    for (idx, query) in queries.iter().enumerate() {
        if context.cancel.is_cancelled() {
            break;
        }
        let kind = query["kind"].as_str().unwrap_or_default();
        let max_item = query["max_chars"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(BATCH_DEFAULT_ITEM_CHARS)
            .clamp(200, BATCH_MAX_ITEM_CHARS);

        let label = match kind {
            "read_file" => format!("read_file {}", query["path"].as_str().unwrap_or_default()),
            "search" => {
                let pat = query["pattern"].as_str().unwrap_or_default();
                match query["path"].as_str().filter(|p| !p.is_empty()) {
                    Some(p) => format!("search `{pat}` in {p}"),
                    None => format!("search `{pat}`"),
                }
            }
            "find_files" => {
                let p = query["path"].as_str().unwrap_or(".");
                match query["glob"].as_str().filter(|g| !g.is_empty()) {
                    Some(g) => format!("find_files {p} ({g})"),
                    None => format!("find_files {p}"),
                }
            }
            other => format!("unknown `{other}`"),
        };

        let out = match kind {
            "read_file" => read_file(query, context).await.unwrap_or_else(|e| e),
            "search" => search(query, context).await.unwrap_or_else(|e| e),
            "find_files" => find_files(query, context).await.unwrap_or_else(|e| e),
            other => ToolOutput::error(format!(
                "Unsupported query kind `{other}`; expected `read_file`, `search`, or `find_files`."
            )),
        };

        if out.is_error {
            errors += 1;
        }
        let body_chars = out.content.chars().count();
        let body = if body_chars > max_item {
            any_truncated = true;
            let kept: String = out.content.chars().take(max_item).collect();
            format!("{kept}\n[... truncated at {max_item} of {body_chars} characters ...]")
        } else {
            out.content
        };

        let header = format!(
            "=== [{}/{}] {}{} ===",
            idx + 1,
            total,
            label.trim(),
            if out.is_error { " [ERROR]" } else { "" }
        );
        let section = format!("{header}\n{body}");
        let section_chars = section.chars().count() + 2;
        if !sections.is_empty() && used_chars + section_chars > max_total {
            any_truncated = true;
            let remaining = max_total.saturating_sub(used_chars);
            if remaining > header.chars().count() + 60 {
                let take = remaining - header.chars().count() - 48;
                let partial: String = body.chars().take(take).collect();
                sections.push(format!(
                    "{header}\n{partial}\n[... batch digest stopped at {max_total} total characters ...]"
                ));
            } else {
                sections.push(format!(
                    "[... batch digest stopped at {max_total} characters; {} remaining quer{} omitted ...]",
                    total - idx,
                    if total - idx == 1 { "y" } else { "ies" }
                ));
            }
            break;
        }
        used_chars += section_chars;
        sections.push(section);
    }

    let content = sections.join("\n\n");
    let meta = json!({
        "queries": total,
        "errors": errors,
        "truncated": any_truncated,
    });
    if errors == total {
        Err(ToolOutput {
            content,
            is_error: true,
            meta,
            images: Vec::new(),
        })
    } else {
        Ok(ToolOutput::ok(content, meta))
    }
}

#[cfg(test)]
mod size_gate_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn oversized_zip_is_rejected_before_zip_archive_sniffing() {
        let dir = tempfile::tempdir().unwrap();
        let huge_zip = dir.path().join("dataset.zip");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&huge_zip).unwrap();
            f.write_all(b"PK\x03\x04corrupt-central-directory").unwrap();
            f.set_len(crate::documents::DOCUMENT_BYTES + 4096).unwrap();
        }
        let ctx = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
            snapshots: None,
            jobs: None,
            mcp: None,
            skills: Vec::new(),
            search: None,
            sandbox: None,
            runtime: None,
            painter: None,
            vision: Some(true),
            lsp: None,
        };
        let t0 = std::time::Instant::now();
        let err = read_file(&json!({ "path": "dataset.zip" }), &ctx).await.unwrap_err();
        assert!(t0.elapsed() < std::time::Duration::from_millis(200));
        assert!(err.is_error);
        assert!(
            err.content
                .contains(&format!("over the {}-byte limit", crate::documents::DOCUMENT_BYTES)),
            "{}",
            err.content
        );
    }
}
