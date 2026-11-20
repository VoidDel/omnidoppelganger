//! What a turn left in the workspace for the user: the documents, pictures and such that were new or changed when it
//! ended. A script or a command makes them, and no tool says so; they are found by looking, so the user can open them
//! from the conversation all the same.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The kinds of file a 分身 makes for the user, as against the code and build output a turn also leaves.
pub const KINDS: &[&str] = &[
    "docx", "doc", "xlsx", "xls", "csv", "pptx", "ppt", "pdf", "odt", "ods", "odp", "rtf", "epub", "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "mp3", "wav",
    "mp4", "mov", "zip", "7z",
];

/// How many entries are looked at, at most: a workspace bigger than that is looked at only so far.
const LOOKED_AT: usize = 20_000;
/// How many files are kept, at most.
const KEPT: usize = 50;
/// How deep below the workspace.
const DEPTH: usize = 8;
/// A file's time is the file system's: a little slack for a clock that keeps it coarsely.
const SLACK: Duration = Duration::from_secs(2);

/// The files of [`KINDS`] below `root` changed since `since`, newest first, as absolute paths. What `.gitignore`
/// leaves out and hidden folders are not looked in, nor OmniDoppelganger's own.
pub fn since(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let from = since.checked_sub(SLACK).unwrap_or(since);
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    // A `.gitignore` counts in a folder that is not a repository too; and the heaps of packages and build output
    // that are not always ignored are not looked through.
    let walk = ignore::WalkBuilder::new(root)
        .max_depth(Some(DEPTH))
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some("node_modules" | "target" | "__pycache__" | "venv")))
        .build();
    for entry in walk.take(LOOKED_AT).flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|kind| kind.is_file()) || !of_a_kind(path) {
            continue;
        }
        // Office's own lock files beside an open document (`~$report.docx`) are not made for anyone.
        if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("~$")) {
            continue;
        }
        if let Some(changed) = entry.metadata().ok().and_then(|metadata| metadata.modified().ok())
            && changed >= from
        {
            found.push((changed, path.to_path_buf()));
        }
    }
    found.sort_by(|one, other| other.0.cmp(&one.0).then_with(|| one.1.cmp(&other.1)));
    found.into_iter().take(KEPT).map(|(_, path)| path).collect()
}

fn of_a_kind(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| KINDS.contains(&extension.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_documents_a_turn_left_and_not_its_code_what_git_ignores_or_what_was_there_before() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old.pdf");
        std::fs::write(&old, "old").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(long_ago).unwrap();
        // Not a repository, and its .gitignore counts all the same.
        std::fs::write(root.path().join(".gitignore"), "out/\n").unwrap();
        let started = SystemTime::now();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::create_dir_all(root.path().join("out")).unwrap();
        std::fs::create_dir_all(root.path().join("node_modules").join("pkg")).unwrap();
        std::fs::write(root.path().join("node_modules").join("pkg").join("logo.png"), "a package's").unwrap();
        std::fs::write(root.path().join("docs").join("手册.DOCX"), "made").unwrap();
        std::fs::write(root.path().join("chart.png"), "made").unwrap();
        std::fs::write(root.path().join("build.py"), "code").unwrap();
        std::fs::write(root.path().join("~$手册.docx"), "lock").unwrap();
        std::fs::write(root.path().join("out").join("ignored.pdf"), "ignored").unwrap();
        let found = since(root.path(), started);
        let names: Vec<String> = found.iter().map(|path| path.strip_prefix(root.path()).unwrap().display().to_string().replace('\\', "/")).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, ["chart.png", "docs/手册.DOCX"]);
        assert!(found.iter().all(|path| path.is_absolute()));
    }
}
