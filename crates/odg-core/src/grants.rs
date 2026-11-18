//! Approvals the user gave for good: "always allow `cargo test` in this
//! workspace". They are kept in one file, by workspace, and outlive the app.
//!
//! Only a command's grant can be kept: it names a program and subcommand
//! (see `policy::command_grant`), which stays meaningful from one day to the
//! next. An approval of edits or of paths outside the workspace lasts for
//! the session it was given in.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::fsutil::write_atomically;

/// Grants by workspace path.
pub type Grants = BTreeMap<String, BTreeSet<String>>;

pub struct GrantStore {
    path: PathBuf,
    /// Held across a read-change-write of the file.
    writing: Mutex<()>,
}

impl GrantStore {
    /// A store kept in the file at `path`, which need not exist yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), writing: Mutex::new(()) }
    }

    /// Whether an approval of this grant can be kept for good.
    pub fn keeps(grant: &str) -> bool {
        grant.starts_with("command:")
    }

    /// Every kept grant. A missing or unreadable file holds none.
    pub fn all(&self) -> Grants {
        std::fs::read_to_string(&self.path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn allows(&self, workspace: &Path, grant: &str) -> bool {
        self.all().get(workspace.to_string_lossy().as_ref()).is_some_and(|grants| grants.contains(grant))
    }

    fn change(&self, change: impl FnOnce(&mut Grants)) -> io::Result<()> {
        let _guard = self.writing.lock().unwrap();
        let mut grants = self.all();
        change(&mut grants);
        grants.retain(|_, kept| !kept.is_empty());
        if let Some(directory) = self.path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        write_atomically(&self.path, &serde_json::to_vec_pretty(&grants).map_err(io::Error::other)?)
    }

    /// Keep `grant` for `workspace`. A grant that cannot be kept is refused.
    pub fn add(&self, workspace: &Path, grant: &str) -> io::Result<()> {
        if !Self::keeps(grant) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("`{grant}` cannot be allowed for good")));
        }
        self.change(|grants| {
            grants.entry(workspace.to_string_lossy().into_owned()).or_default().insert(grant.to_string());
        })
    }

    /// Take back a kept grant. Taking back one that is not there succeeds.
    pub fn remove(&self, workspace: &str, grant: &str) -> io::Result<()> {
        self.change(|grants| {
            if let Some(kept) = grants.get_mut(workspace) {
                kept.remove(grant);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_command_grants_by_workspace_and_gives_them_back() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("home/grants.json");
        let store = GrantStore::new(&file);
        let (one, two) = (Path::new("/work/one"), Path::new("/work/two"));
        assert!(store.all().is_empty() && !store.allows(one, "command:cargo test"));

        store.add(one, "command:cargo test").unwrap();
        store.add(one, "command:git status").unwrap();
        store.add(one, "command:cargo test").unwrap();
        // Another store on the same file sees it: the grant outlives the app.
        let reopened = GrantStore::new(&file);
        assert!(reopened.allows(one, "command:cargo test") && !reopened.allows(two, "command:cargo test"));
        assert!(!reopened.allows(one, "command:cargo publish"));
        assert_eq!(reopened.all()["/work/one"].len(), 2);

        // Only a command's grant is kept.
        assert!(store.add(one, "edit").is_err() && store.add(one, "outside_write").is_err());

        store.remove("/work/one", "command:cargo test").unwrap();
        store.remove("/work/one", "command:never given").unwrap();
        assert!(!store.allows(one, "command:cargo test") && store.allows(one, "command:git status"));
        // A workspace with nothing left is dropped from the file.
        store.remove("/work/one", "command:git status").unwrap();
        assert!(store.all().is_empty());
    }
}
