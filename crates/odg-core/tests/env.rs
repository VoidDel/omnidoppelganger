//! The `PATH` an app started from a `.app` is given, and what the app does about it. A process of its own, since it
//! changes its own environment.

use std::path::{Path, PathBuf};

#[test]
fn adds_the_folders_programs_are_installed_in_before_the_ones_it_was_given_and_only_once() {
    // As macOS starts an app from the Dock or Finder.
    let given = "/usr/bin:/bin:/usr/sbin:/sbin";
    // SAFETY: this is a test binary of its own, and nothing in it reads the environment while the test runs.
    unsafe { std::env::set_var("PATH", given) };
    odg_core::env::adopt_shell_path();
    let folders: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();

    // What it was given is kept, in its own order, after what was added.
    let given: Vec<PathBuf> = std::env::split_paths(given).collect();
    assert_eq!(folders[folders.len() - given.len()..], given[..], "{folders:?}");
    let place = |path: &str| folders.iter().position(|folder| folder == Path::new(path));
    // Homebrew's folder is now there, before the system's own, where this machine has it.
    if Path::new("/opt/homebrew/bin").is_dir() {
        let (homebrew, usr) = (place("/opt/homebrew/bin").expect("Homebrew's folder was not added"), place("/usr/bin").unwrap());
        assert!(homebrew < usr, "{folders:?}");
        // And so a program there is found, where before it was not.
        let found = std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|folder| folder.join("npx")).find(|path| path.is_file());
        assert!(found.is_some(), "{folders:?}");
    }
    // Nothing twice, and the like of it done again changes nothing.
    let mut once = folders.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), folders.len(), "{folders:?}");
    odg_core::env::adopt_shell_path();
    assert_eq!(std::env::var_os("PATH").unwrap(), std::env::join_paths(&folders).unwrap());
}
