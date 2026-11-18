//! Git worktrees for sessions that must not step on each other's files. Such
//! a session works in a checkout of its own, on a branch of its own, made
//! from the repository's current commit; its work goes back by a merge.
//!
//! Everything here runs `git`, so it needs Git installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use crate::events::Worktree;

/// The name commits are made under when the user has set Git no identity.
const FALLBACK_IDENTITY: [&str; 4] = ["-c", "user.name=OmniDoppelganger", "-c", "user.email=odg@localhost"];

/// Run `git` in `directory`. Returns what it printed, or what it said went wrong.
fn git(directory: &Path, config: &[&str], arguments: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(directory).args(config).args(arguments).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    }
    let output = command.output().map_err(|error| format!("cannot run git: {error}"))?;
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    if output.status.success() {
        return Ok(text(&output.stdout));
    }
    // A failed merge names the conflicts on standard output and little on standard error.
    let said: Vec<String> = [text(&output.stdout), text(&output.stderr)].into_iter().filter(|part| !part.is_empty()).collect();
    Err(if said.is_empty() { format!("git {} failed", arguments.first().copied().unwrap_or_default()) } else { said.join("\n") })
}

/// What lets a commit be made in `directory`: nothing when the user has an identity set, a stand-in when not.
fn identity(directory: &Path) -> &'static [&'static str] {
    if git(directory, &[], &["config", "user.email"]).is_ok_and(|email| !email.is_empty()) { &[] } else { &FALLBACK_IDENTITY }
}

/// The top of the repository `directory` is in, if it is in one.
pub fn repository(directory: &Path) -> Option<PathBuf> {
    git(directory, &[], &["rev-parse", "--show-toplevel"]).ok().filter(|top| !top.is_empty()).map(|top| PathBuf::from(top).components().collect())
}

/// Make a worktree for a session that is to work on `workspace` without
/// sharing its files: a checkout under `root` on a new branch, from the
/// commit checked out now. Returns the directory in it that stands for
/// `workspace` (the same place inside the repository), and what was made.
///
/// Changes not yet committed in `workspace` are not in the worktree.
pub fn create(workspace: &Path, root: &Path) -> Result<(PathBuf, Worktree), String> {
    let top = repository(workspace).ok_or_else(|| format!("{} is not in a Git repository, so it cannot have a worktree", workspace.display()))?;
    git(&top, &[], &["rev-parse", "--verify", "HEAD"]).map_err(|_| "the repository has no commit yet; a worktree starts from one".to_string())?;
    // Where the workspace sits inside the repository, to stand in the same place in the worktree.
    let canonical = |path: &Path| path.canonicalize().map_err(|error| format!("cannot read {}: {error}", path.display()));
    let within = canonical(workspace)?.strip_prefix(canonical(&top)?).map(Path::to_path_buf).unwrap_or_default();

    let id = uuid::Uuid::now_v7().simple().to_string();
    let tag = &id[id.len() - 8..];
    let name = top.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "repository".to_string());
    let checkout = root.join(format!("{name}-{tag}"));
    let branch = format!("odg/{tag}");
    std::fs::create_dir_all(root).map_err(|error| format!("cannot create {}: {error}", root.display()))?;
    git(&top, &[], &["worktree", "add", "-b", &branch, &checkout.to_string_lossy(), "HEAD"])?;
    let worktree = Worktree { origin: workspace.to_string_lossy().into_owned(), branch, checkout: checkout.to_string_lossy().into_owned() };
    Ok((checkout.join(within), worktree))
}

/// What bringing a worktree's work back did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Landed {
    /// How many commits were merged; none when the worktree held nothing new.
    pub commits: usize,
    /// The branch they were merged into: the one checked out in the repository.
    pub into: String,
}

/// Bring a worktree's work back: commit whatever it holds uncommitted, under
/// `message`, and merge its branch into what the repository has checked out.
/// A merge that conflicts is undone, and the conflicts reported; the
/// worktree and its branch stay as they are either way, to go on from.
pub fn land(worktree: &Worktree, message: &str) -> Result<Landed, String> {
    let checkout = Path::new(&worktree.checkout);
    let top = repository(Path::new(&worktree.origin)).ok_or_else(|| format!("{} is no longer a Git repository", worktree.origin))?;
    git(checkout, &[], &["add", "-A"])?;
    // `diff --quiet` fails when there is a difference: something is staged.
    if git(checkout, &[], &["diff", "--cached", "--quiet"]).is_err() {
        git(checkout, identity(checkout), &["commit", "-m", message])?;
    }
    let into = git(&top, &[], &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let commits: usize = git(&top, &[], &["rev-list", "--count", &format!("HEAD..{}", worktree.branch)])?.parse().unwrap_or(0);
    if commits == 0 {
        return Ok(Landed { commits, into });
    }
    if let Err(error) = git(&top, identity(&top), &["merge", "--no-edit", &worktree.branch]) {
        // Nothing is left half merged; a merge that never started has nothing to abort.
        let _ = git(&top, &[], &["merge", "--abort"]);
        return Err(format!("the work could not be merged into {into}, and nothing was changed there:\n{error}"));
    }
    Ok(Landed { commits, into })
}

/// Take a worktree away: its checkout, with anything uncommitted in it, and
/// its branch when that is merged. A branch with commits that were never
/// brought back is kept, so that work is not lost with the session.
pub fn remove(worktree: &Worktree) -> Result<(), String> {
    let checkout = Path::new(&worktree.checkout);
    let Some(top) = repository(Path::new(&worktree.origin)) else {
        // The repository is gone; what is left of the checkout is only a folder.
        return if checkout.exists() { std::fs::remove_dir_all(checkout).map_err(|error| format!("cannot remove {}: {error}", checkout.display())) } else { Ok(()) };
    };
    if checkout.exists() {
        git(&top, &[], &["worktree", "remove", "--force", &worktree.checkout])?;
    } else {
        git(&top, &[], &["worktree", "prune"])?;
    }
    let _ = git(&top, &[], &["branch", "-d", &worktree.branch]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one commit, and a directory to keep worktrees in.
    fn repository_with_a_commit() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let top = directory.path().join("project");
        std::fs::create_dir_all(top.join("src")).unwrap();
        std::fs::write(top.join("src").join("a.txt"), "one\n").unwrap();
        git(&top, &[], &["init", "--quiet", "--initial-branch", "main"]).unwrap();
        git(&top, &[], &["add", "-A"]).unwrap();
        git(&top, &FALLBACK_IDENTITY, &["commit", "--quiet", "-m", "first"]).unwrap();
        let root = directory.path().join("worktrees");
        (directory, top, root)
    }

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
    }

    #[test]
    fn a_worktree_holds_the_repository_apart_and_its_work_comes_back_by_a_merge() {
        let (_directory, top, root) = repository_with_a_commit();
        let (workspace, worktree) = create(&top, &root).unwrap();
        assert!(worktree.branch.starts_with("odg/") && workspace.starts_with(&root));
        assert_eq!(crate::tools::git_branch(&workspace).as_deref(), Some(worktree.branch.as_str()));
        assert_eq!(read(workspace.join("src").join("a.txt")), "one\n");

        // Work in the worktree leaves the repository's own files alone until it is brought back.
        std::fs::write(workspace.join("src").join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(workspace.join("new.txt"), "new\n").unwrap();
        assert_eq!(read(top.join("src").join("a.txt")), "one\n");
        assert_eq!(land(&worktree, "odg: add two").unwrap(), Landed { commits: 1, into: "main".to_string() });
        assert_eq!(read(top.join("src").join("a.txt")), "one\ntwo\n");
        assert_eq!(read(top.join("new.txt")), "new\n");
        // With nothing new there is nothing to bring back.
        assert_eq!(land(&worktree, "odg: again").unwrap().commits, 0);

        remove(&worktree).unwrap();
        assert!(!Path::new(&worktree.checkout).exists());
        assert!(git(&top, &[], &["rev-parse", "--verify", &worktree.branch]).is_err(), "a merged branch goes with its worktree");
    }

    #[test]
    fn a_merge_that_conflicts_changes_nothing_and_keeps_the_work_on_its_branch() {
        let (_directory, top, root) = repository_with_a_commit();
        let (workspace, worktree) = create(&top, &root).unwrap();
        std::fs::write(workspace.join("src").join("a.txt"), "theirs\n").unwrap();
        std::fs::write(top.join("src").join("a.txt"), "ours\n").unwrap();
        git(&top, &FALLBACK_IDENTITY, &["commit", "--quiet", "-am", "ours"]).unwrap();

        let error = land(&worktree, "odg: theirs").unwrap_err();
        assert!(error.contains("could not be merged into main") && error.contains("a.txt"), "{error}");
        assert_eq!(read(top.join("src").join("a.txt")), "ours\n");
        assert_eq!(git(&top, &[], &["status", "--porcelain"]).unwrap(), "");

        // The checkout goes with the session; the commits that never came back stay under the branch.
        remove(&worktree).unwrap();
        assert!(!Path::new(&worktree.checkout).exists());
        assert!(git(&top, &[], &["rev-parse", "--verify", &worktree.branch]).is_ok());
    }

    #[test]
    fn a_workspace_inside_a_repository_stands_in_the_same_place_in_the_worktree() {
        let (_directory, top, root) = repository_with_a_commit();
        let (workspace, worktree) = create(&top.join("src"), &root).unwrap();
        assert_eq!(workspace, Path::new(&worktree.checkout).join("src"));
        assert!(workspace.join("a.txt").is_file());
        remove(&worktree).unwrap();
    }

    #[test]
    fn refuses_a_folder_that_is_not_a_repository_or_has_no_commit() {
        let directory = tempfile::tempdir().unwrap();
        let plain = directory.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        // A folder under a repository of the machine's own would count as inside it; this one is checked to be outside.
        if repository(&plain).is_none() {
            assert!(create(&plain, &directory.path().join("worktrees")).unwrap_err().contains("not in a Git repository"));
        }
        let empty = directory.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        git(&empty, &[], &["init", "--quiet"]).unwrap();
        assert!(create(&empty, &directory.path().join("worktrees")).unwrap_err().contains("no commit yet"));
    }
}
