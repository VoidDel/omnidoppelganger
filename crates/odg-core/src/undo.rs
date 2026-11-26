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
use crate::fsutil::{ensure_private_dir, write_atomically, write_private_atomically};

/// The largest deliverable snapshotted for version history (50 MB, matching attached documents).
pub const ARTIFACT_SNAPSHOT_BYTES: u64 = 50 * 1024 * 1024;

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
    ensure_private_dir(directory)?;
    let base = fingerprint(bytes);
    for attempt in 0.. {
        // Two contents can share a fingerprint; the name then gets a suffix rather than the wrong contents.
        let name = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        let path = directory.join(&name);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(name),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_private_atomically(&path, bytes)?;
                return Ok(name);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the loop returns")
}

/// Snapshot a file on disk into `directory` when it is at most `max_bytes`.
/// Returns `Ok(None)` when the file exceeds `max_bytes` or is not a regular file.
pub fn save_file(directory: &Path, file: &Path, max_bytes: u64) -> io::Result<Option<(String, u64)>> {
    let meta = std::fs::metadata(file)?;
    if !meta.is_file() || meta.len() > max_bytes {
        return Ok(None);
    }
    let bytes = std::fs::read(file)?;
    if bytes.len() as u64 > max_bytes {
        return Ok(None);
    }
    let name = save(directory, &bytes)?;
    Ok(Some((name, bytes.len() as u64)))
}

/// Whether `name` is a single safe snapshot filename produced by [`save`].
pub fn valid_snapshot_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Read a named snapshot from `directory`, refusing any unsafe name.
pub fn read_snapshot(directory: &Path, name: &str) -> io::Result<Vec<u8>> {
    if !valid_snapshot_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot name"));
    }
    std::fs::read(directory.join(name))
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
        Some(FileUndo {
            file: PathBuf::from(file),
            shown: shown.to_string(),
            before: change["before"].as_str().map(str::to_string),
            after,
        })
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
            Event::FilesRestored {
                turn: from, restored, ..
            } => edits.retain(|(turn, edit)| !(turn >= from && restored.contains(&edit.shown))),
            Event::ToolResult {
                tool,
                is_error: false,
                meta,
                ..
            } if current > copied && crate::tools::edits_files(tool) => {
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

fn restore_one(file: &FileUndo, snapshots: &Path, workspace: &Path) -> bool {
    if !crate::tools::inside_workspace(workspace, &file.file) || crate::tools::in_git_directory(workspace, &file.file) {
        return false;
    }
    if std::fs::symlink_metadata(&file.file).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let untouched = match (&file.after, std::fs::read(&file.file)) {
        (Some(after), Ok(current)) => fingerprint(&current) == *after,
        // Gone, as the agent left it: nobody has put a file there since.
        (None, Err(error)) => error.kind() == io::ErrorKind::NotFound,
        _ => false,
    };
    if !untouched {
        return false;
    }
    match &file.before {
        Some(name) => read_snapshot(snapshots, name)
            .and_then(|bytes| {
                // A file deleted or moved away may have been the last in its folder, which went with it.
                if let Some(parent) = file.file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if !crate::tools::inside_workspace(workspace, &file.file)
                    || crate::tools::in_git_directory(workspace, &file.file)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "undo target escaped workspace",
                    ));
                }
                if std::fs::symlink_metadata(&file.file).is_ok_and(|meta| meta.file_type().is_symlink()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "refusing to restore symlink",
                    ));
                }
                write_atomically(&file.file, &bytes)
            })
            .is_ok(),
        None => std::fs::remove_file(&file.file).is_ok(),
    }
}

/// Restore each file of `plan` from `snapshots`, enforcing that every target stays
/// inside `workspace`, outside `.git`, is not a symlink, and uses a valid snapshot name.
pub fn apply(plan: &[FileUndo], snapshots: &Path, workspace: &Path) -> Outcome {
    let mut outcome = Outcome::default();
    for file in plan {
        if restore_one(file, snapshots, workspace) {
            outcome.restored.push(file.shown.clone());
        } else {
            outcome.skipped.push(file.shown.clone());
        }
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

    #[test]
    fn apply_in_skips_outside_paths_git_paths_symlinks_and_bad_snapshot_names() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let outside = root.path().join("outside");
        let snapshots = root.path().join("session/snapshots");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&snapshots).unwrap();

        let snap = save(&snapshots, b"original\n").unwrap();
        std::fs::write(root.path().join("session/secret"), b"leaked").unwrap();

        // Valid in-workspace restore succeeds.
        let good_file = workspace.join("src/ok.txt");
        std::fs::write(&good_file, b"edited\n").unwrap();
        let good = FileUndo {
            file: good_file.clone(),
            shown: "src/ok.txt".into(),
            before: Some(snap.clone()),
            after: Some(fingerprint(b"edited\n")),
        };

        // Invalid snapshot traversal `"../secret"` is skipped.
        let bad_snap_file = workspace.join("src/bad_snap.txt");
        let bad_snap = FileUndo {
            file: bad_snap_file.clone(),
            shown: "src/bad_snap.txt".into(),
            before: Some("../secret".into()),
            after: None,
        };

        // Outside workspace file and `.git` file are skipped.
        let outside_file = outside.join("escape.txt");
        let outside_undo = FileUndo {
            file: outside_file.clone(),
            shown: "../outside/escape.txt".into(),
            before: Some(snap.clone()),
            after: None,
        };
        let git_file = workspace.join(".git/hooks/pre-commit");
        let git_undo = FileUndo {
            file: git_file.clone(),
            shown: ".git/hooks/pre-commit".into(),
            before: Some(snap.clone()),
            after: None,
        };

        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut plan = vec![good, bad_snap, outside_undo, git_undo];

        #[cfg(unix)]
        let (symlink_parent_target, symlink_self) = {
            std::os::unix::fs::symlink(&outside, workspace.join("linked_dir")).unwrap();
            let via_parent = workspace.join("linked_dir/from_undo.txt");
            plan.push(FileUndo {
                file: via_parent,
                shown: "linked_dir/from_undo.txt".into(),
                before: Some(snap.clone()),
                after: None,
            });

            let real_in_ws = workspace.join("src/real.txt");
            std::fs::write(&real_in_ws, b"edited\n").unwrap();
            let link_in_ws = workspace.join("src/link.txt");
            std::os::unix::fs::symlink(&real_in_ws, &link_in_ws).unwrap();
            plan.push(FileUndo {
                file: link_in_ws.clone(),
                shown: "src/link.txt".into(),
                before: Some(snap),
                after: Some(fingerprint(b"edited\n")),
            });
            (outside.join("from_undo.txt"), real_in_ws)
        };

        let outcome = apply(&plan, &snapshots, &workspace);
        assert_eq!(outcome.restored, vec!["src/ok.txt"]);
        assert!(outcome.skipped.contains(&"src/bad_snap.txt".to_string()));
        assert!(outcome.skipped.contains(&"../outside/escape.txt".to_string()));
        assert!(outcome.skipped.contains(&".git/hooks/pre-commit".to_string()));
        assert_eq!(std::fs::read(&good_file).unwrap(), b"original\n");
        assert!(!bad_snap_file.exists());
        assert!(!outside_file.exists());
        assert!(!git_file.exists());

        #[cfg(unix)]
        {
            assert!(outcome.skipped.contains(&"linked_dir/from_undo.txt".to_string()));
            assert!(outcome.skipped.contains(&"src/link.txt".to_string()));
            assert!(!symlink_parent_target.exists());
            assert_eq!(std::fs::read(&symlink_self).unwrap(), b"edited\n");
        }
    }
}
