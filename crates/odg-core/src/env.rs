//! The environment the app was started in: the `PATH` it got, which may leave out where the programs it starts live.
//!
//! An app started from a `.app` — the Dock, Finder, Spotlight — is given the `PATH` of the process that started it,
//! and on macOS that is `/usr/bin:/bin:/usr/sbin:/sbin`. The user's shell has their own, with Homebrew's folders in
//! it and the ones nvm, fnm, uv, cargo and pnpm install into. So an MCP server started with `npx`, a language server,
//! or a command the agent runs does not find what the user plainly has, and fails before it says anything else.
//!
//! The user's shell is not run at start to ask it: a start that runs one can wait on a slow or interactive startup
//! file, print what it prints, and change what the app then does. The folders these programs are installed in are
//! added instead, before the ones the app was given, so that a program there is the one the user's shell would pick,
//! and only where they are there.

use std::path::{Path, PathBuf};

/// Widen this process's `PATH` with the folders programs are usually installed in, keeping what it had. Called once,
/// at start, before anything is started that is looked for on the `PATH`.
pub fn adopt_shell_path() {
    let current: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    let widened = widened(&current, usual_folders(&home));
    if widened == current {
        return; // Started from a terminal, which gave it a shell's own `PATH`: there is nothing to add.
    }
    let Ok(joined) = std::env::join_paths(&widened) else {
        return;
    };
    // SAFETY: this is called once, at start, before any thread of the app's own is started that reads the environment.
    // A program started before this call would see the old `PATH`, which is what it would have had anyway.
    unsafe { std::env::set_var("PATH", joined) };
}

/// `current` after `extra`, in that order, each folder once: the ones in `extra` only where they are there, since the
/// list names the usual places for each system. What `current` holds is kept as it was, there or not.
fn widened(current: &[PathBuf], extra: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    for folder in extra.iter().filter(|folder| folder.is_dir()).chain(current.iter()) {
        if !all.contains(folder) {
            all.push(folder.clone());
        }
    }
    all
}

/// Where the programs the app starts — `npx`, `uvx`, `cargo`, the language servers — are usually installed, for every
/// system at once: `widened` keeps only the folders that are there, so naming another system's costs nothing.
fn usual_folders(home: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/local/go/bin",
        "/opt/local/bin",
        "/opt/local/sbin",
        "/home/linuxbrew/.linuxbrew/bin",
        "/snap/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(gobin) = std::env::var_os("GOBIN").filter(|value| !value.is_empty()) {
        folders.push(PathBuf::from(gobin));
    }
    if let Some(gopath) = std::env::var_os("GOPATH").filter(|value| !value.is_empty()) {
        folders.extend(
            std::env::split_paths(&gopath)
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join("bin")),
        );
    }
    // The user's own, as each installer puts them: go, pipx and uv, cargo, and the version managers' shims.
    for folder in [
        "go/bin",
        ".local/bin",
        ".cargo/bin",
        ".local/share/mise/shims",
        ".asdf/shims",
        ".pyenv/shims",
        ".bun/bin",
        ".volta/bin",
        ".deno/bin",
        ".npm-global/bin",
        ".local/share/pnpm",
        "Library/pnpm",
    ] {
        folders.push(home.join(folder));
    }
    folders.extend(node_folders(home));
    folders
}

/// The bins of every Node that nvm and fnm keep: one folder per version, named after it, so the folders cannot be
/// named as they are. A version whose folder is there but holds no `bin` yet is left out, as one being installed is.
fn node_folders(home: &Path) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for (parent, rest) in [
        (".nvm/versions/node", "bin"),
        ("Library/Application Support/fnm/node-versions", "installation/bin"),
        (".local/share/fnm/node-versions", "installation/bin"),
    ] {
        let Ok(entries) = std::fs::read_dir(home.join(parent)) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path().join(rest)))
            .filter(|folder| folder.is_dir())
            .collect();
        versions.sort();
        folders.extend(versions);
    }
    folders
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder_in(root: &Path, path: &str) -> PathBuf {
        let folder = root.join(path);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn keeps_the_order_drops_repeats_and_leaves_out_folders_that_are_not_there() {
        let directory = tempfile::tempdir().unwrap();
        let (first, second) = (
            folder_in(directory.path(), "first"),
            folder_in(directory.path(), "second"),
        );
        let absent = directory.path().join("absent");
        // A folder of the app's own that is not there yet is kept: a program may make it later.
        let later = directory.path().join("later");
        let current = [PathBuf::from("/usr/bin"), later.clone(), PathBuf::from("/bin")];
        let kept = widened(
            &current,
            vec![first.clone(), absent, second.clone(), PathBuf::from("/usr/bin")],
        );
        assert_eq!(
            kept,
            [first, second, PathBuf::from("/usr/bin"), later, PathBuf::from("/bin")]
        );
        // Nothing new to add is told apart from a `PATH` that already had them all.
        assert_eq!(widened(&kept, vec![kept[0].clone()]), kept);
    }

    #[test]
    fn names_the_folders_programs_are_installed_in_and_each_node_version() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        folder_in(home, ".nvm/versions/node/v20.11.0/bin");
        folder_in(home, ".nvm/versions/node/v26.13.1/bin");
        let folders = usual_folders(home);
        for path in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/local/go/bin"] {
            assert!(folders.contains(&PathBuf::from(path)), "{path} is missing");
        }
        for path in [
            "go/bin",
            ".local/bin",
            ".cargo/bin",
            ".local/share/mise/shims",
            ".asdf/shims",
        ] {
            assert!(folders.contains(&home.join(path)), "{path} is missing");
        }
        // Every Node nvm keeps, and only the folder holding its `bin`.
        assert!(
            folders.contains(&home.join(".nvm/versions/node/v20.11.0/bin"))
                && folders.contains(&home.join(".nvm/versions/node/v26.13.1/bin"))
        );
        assert!(!folders.contains(&home.join(".nvm/versions/node/v20.11.0")));
        // A home with none of them costs nothing: the folders it would name are left out.
        let empty = tempfile::tempdir().unwrap();
        assert!(
            !widened(&[], usual_folders(empty.path()))
                .iter()
                .any(|folder| folder.starts_with(empty.path()))
        );
    }
}
