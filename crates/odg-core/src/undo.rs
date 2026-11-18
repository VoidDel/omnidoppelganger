//! Undoing the agent's file edits.
//!
//! Before `edit_file` writes, it saves what the file held into the session's
//! snapshot directory and records the snapshot's name in its result, with a
//! fingerprint of what it wrote. Undoing from a turn on puts every file those
//! edits touched back to what it held before the first of them. A file that
//! no longer holds what the last edit wrote was changed by someone else
//! since, and is left alone.
//!
//! Only `edit_file` is covered: what a command changed is not known here.

use std::io;
use std::path::{Path, PathBuf};

use crate::events::{Envelope, Event};
use crate::fsutil::write_atomically;

/// Tells two versions of a file apart: its length and a 64-bit FNV-1a hash.
/// It is for noticing a change, not for security.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:x}-{hash:016x}", bytes.len())
}

/// Save `bytes` in `directory` and return the snapshot's name. The same
/// contents are stored once, so a file edited many times does not grow the
/// directory with each edit's unchanged neighbours.
pub fn save(directory: &Path, bytes: &[u8]) -> io::Result<String> {
    std::fs::create_dir_all(directory)?;
    let base = fingerprint(bytes);
    for attempt in 0.. {
        // Two contents can share a fingerprint; the name then gets a suffix rather than the wrong contents.
        let name = if attempt == 0 { base.clone() } else { format!("{base}-{attempt}") };
        let path = directory.join(&name);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(name),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_atomically(&path, bytes)?;
                return Ok(name);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the loop returns")
}

/// What undoing does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUndo {
    pub file: PathBuf,
    /// The path as the tool results show it.
    pub shown: String,
    /// The snapshot to restore; `None` when the agent created the file, which undoing removes.
    pub before: Option<String>,
    /// The fingerprint of what the agent's last edit wrote.
    pub after: String,
}

/// The files to restore to undo every edit made in `turn` and later that has
/// not been undone already, in the order they were first edited.
pub fn plan(events: &[Envelope], turn: u64) -> Vec<FileUndo> {
    let mut current = 0;
    let mut edits: Vec<(u64, FileUndo)> = Vec::new();
    for envelope in events {
        match &envelope.event {
            Event::TurnStart { turn } => current = *turn,
            Event::FilesRestored { turn: from, restored, .. } => edits.retain(|(turn, edit)| !(turn >= from && restored.contains(&edit.shown))),
            Event::ToolResult { tool, is_error: false, meta, .. } if tool == "edit_file" => {
                // An edit logged before snapshots existed has none, and cannot be undone.
                if let (Some(file), Some(shown), Some(after)) = (meta["file"].as_str(), meta["path"].as_str(), meta["after"].as_str()) {
                    let before = meta["before"].as_str().map(str::to_string);
                    edits.push((current, FileUndo { file: PathBuf::from(file), shown: shown.to_string(), before, after: after.to_string() }));
                }
            }
            _ => {}
        }
    }
    let mut files: Vec<FileUndo> = Vec::new();
    for (_, edit) in edits.into_iter().filter(|(edited, _)| *edited >= turn) {
        match files.iter_mut().find(|file| file.file == edit.file) {
            // The first edit knows what the file held before; the last, what it holds now.
            Some(file) => file.after = edit.after,
            None => files.push(edit),
        }
    }
    files
}

/// What an undo did, by the paths the tool results show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    pub restored: Vec<String>,
    /// Files left as they are: changed since the agent's last edit, or their snapshot is gone.
    pub skipped: Vec<String>,
}

/// Restore each file of `plan` from `snapshots`.
pub fn apply(plan: &[FileUndo], snapshots: &Path) -> Outcome {
    let mut outcome = Outcome::default();
    for file in plan {
        let untouched = std::fs::read(&file.file).is_ok_and(|current| fingerprint(&current) == file.after);
        let done = untouched
            && match &file.before {
                Some(name) => std::fs::read(snapshots.join(name)).and_then(|bytes| write_atomically(&file.file, &bytes)).is_ok(),
                None => std::fs::remove_file(&file.file).is_ok(),
            };
        if done { outcome.restored.push(file.shown.clone()) } else { outcome.skipped.push(file.shown.clone()) }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_the_same_contents_once_and_tells_versions_apart() {
        let directory = tempfile::tempdir().unwrap();
        let first = save(directory.path(), b"one").unwrap();
        assert_eq!(save(directory.path(), b"one").unwrap(), first);
        let second = save(directory.path(), b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(directory.path().join(&second)).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        assert_eq!(fingerprint(b""), "0-cbf29ce484222325");
        assert_ne!(fingerprint(b"ab"), fingerprint(b"ba"));
    }
}
