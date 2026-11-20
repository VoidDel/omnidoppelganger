//! Undoing the agent's file edits.
//!
//! Before `edit_file`, `write_file`, `delete_file`, or `move_file` changes a file, it saves what the
//! file held into the session's snapshot directory and records the snapshot's name in its result,
//! with a fingerprint of what the file holds after, or that it holds nothing, being gone. Undoing
//! from a turn on puts every file those edits touched back to what it held before the first of
//! them, or takes it away if it was not there. A file that no longer holds what the last edit left
//! was changed by someone else since, and is left alone.
//!
//! Only those tools are covered: what a command changed is not known here.

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
    /// The fingerprint of what the agent's last edit left; `None` when it left no file, having deleted or moved it.
    pub after: Option<String>,
}

/// The files one edit's result says it changed: those in its `changes`, or the one file of an edit
/// that changes one. An edit logged before snapshots existed has none, and cannot be undone.
fn changed(meta: &serde_json::Value) -> Vec<FileUndo> {
    let one = |change: &serde_json::Value, gone_allowed: bool| {
        let (file, shown) = (change["file"].as_str()?, change["path"].as_str()?);
        let after = change["after"].as_str().map(str::to_string);
        // Of a one-file edit, only a written file is known: one without `after` predates snapshots.
        if after.is_none() && !gone_allowed {
            return None;
        }
        Some(FileUndo { file: PathBuf::from(file), shown: shown.to_string(), before: change["before"].as_str().map(str::to_string), after })
    };
    match meta["changes"].as_array() {
        Some(changes) => changes.iter().filter_map(|change| one(change, true)).collect(),
        None => one(meta, false).into_iter().collect(),
    }
}

/// The files to restore to undo every edit made in `turn` and later that has
/// not been undone already, in the order they were first edited.
///
/// In a fork, the edits of the turns it copied are left out: their snapshots
/// are kept by the session that made them, which is where to undo them.
pub fn plan(events: &[Envelope], turn: u64) -> Vec<FileUndo> {
    let copied = crate::session::forked_from(events).map_or(0, |fork| fork.turn);
    let mut current = 0;
    let mut edits: Vec<(u64, FileUndo)> = Vec::new();
    for envelope in events {
        match &envelope.event {
            Event::TurnStart { turn } => current = *turn,
            Event::FilesRestored { turn: from, restored, .. } => edits.retain(|(turn, edit)| !(turn >= from && restored.contains(&edit.shown))),
            Event::ToolResult { tool, is_error: false, meta, .. } if current > copied && crate::tools::edits_files(tool) => {
                edits.extend(changed(meta).into_iter().map(|edit| (current, edit)));
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
        let untouched = match (&file.after, std::fs::read(&file.file)) {
            (Some(after), Ok(current)) => fingerprint(&current) == *after,
            // Gone, as the agent left it: nobody has put a file there since.
            (None, Err(error)) => error.kind() == io::ErrorKind::NotFound,
            _ => false,
        };
        let done = untouched
            && match &file.before {
                Some(name) => std::fs::read(snapshots.join(name))
                    .and_then(|bytes| {
                        // A file deleted or moved away may have been the last in its folder, which went with it.
                        if let Some(parent) = file.file.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        write_atomically(&file.file, &bytes)
                    })
                    .is_ok(),
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
