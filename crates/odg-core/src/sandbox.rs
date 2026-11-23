//! Confining the commands the agent runs. A confined command reads what the
//! user can read, and writes only under the directories it is given: the
//! workspace, the temporary directories, and whatever the user adds. What a
//! build script or a test does by mistake, or because a file told the model
//! to, then stays inside the project.
//!
//! - Linux: Landlock, in the kernel since 5.13. The rules are made here and
//!   taken on by the command's shell just before it starts, and by everything
//!   the shell starts. Landlock does not cover changing a file's mode, owner,
//!   or times, so those are not confined.
//! - macOS: `sandbox-exec`, which every Mac has.
//! - Windows: nothing yet.
//!
//! Only writes are confined. A command can still read any file the user can
//! and reach the network.

use std::path::{Path, PathBuf};

/// Where confined commands may write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// Directories they may write under; each must exist to count.
    pub writable: Vec<PathBuf>,
}

/// Says whether commands are to be confined right now: `None` when the user
/// has that switched off, else the directories they added to write under.
/// Asked at every command, so the switch takes effect at once.
pub type SandboxSource = std::sync::Arc<dyn Fn() -> Option<Vec<PathBuf>> + Send + Sync>;

/// What confines commands on this machine, or why nothing can.
pub fn support() -> Result<&'static str, String> {
    #[cfg(target_os = "linux")]
    return linux::abi().map(|_| "Landlock");
    #[cfg(target_os = "macos")]
    return if Path::new(macos::SANDBOX_EXEC).is_file() { Ok("sandbox-exec") } else { Err(format!("{} is missing", macos::SANDBOX_EXEC)) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err("commands cannot be confined on this system yet; only on Linux and macOS".to_string());
}

/// The directories a session's commands may write under: its workspace, the
/// system's temporary directories, and `extra`. Each as the system names it
/// with links followed, which is how the rules match; those that do not
/// exist are left out.
pub fn roots(workspace: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let temporary = [std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from("/var/tmp"), PathBuf::from("/dev/shm")];
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in std::iter::once(workspace.to_path_buf()).chain(temporary).chain(extra.iter().cloned()) {
        if let Ok(real) = path.canonicalize()
            && !roots.contains(&real)
        {
            roots.push(real);
        }
    }
    roots
}

/// A command that runs `program` confined to `sandbox`; the caller adds the
/// arguments and the rest. Fails where [`support`] does.
pub fn command(program: &str, sandbox: &Sandbox) -> std::io::Result<tokio::process::Command> {
    #[cfg(target_os = "linux")]
    {
        let mut command = tokio::process::Command::new(program);
        linux::confine(&mut command, linux::ruleset(&sandbox.writable)?);
        Ok(command)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(macos::command(program, &sandbox.writable))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (program, sandbox);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, support().unwrap_err()))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};

    // The kernel's Landlock interface: include/uapi/linux/landlock.h.
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;
    const WRITE_FILE: u64 = 1 << 1;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    /// Moving or linking a file into another directory. Since the second version of the interface;
    /// before it, such a move is refused whatever the rules say, and `mv` copies instead.
    const REFER: u64 = 1 << 13;
    /// Since the third version; before it, truncating a file is not confined.
    const TRUNCATE: u64 = 1 << 14;

    /// The first version's attributes, which every later kernel still takes.
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: libc::c_int,
    }

    /// The version of Landlock the running kernel has, or why it has none.
    pub fn abi() -> Result<i64, String> {
        // SAFETY: with this flag the call reads nothing and returns a number.
        let version = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<RulesetAttr>(), 0usize, CREATE_RULESET_VERSION) };
        if version >= 1 { Ok(version) } else { Err(format!("this kernel has no Landlock to confine commands with ({})", io::Error::last_os_error())) }
    }

    /// Every kind of write the kernel at hand can confine.
    fn writes(abi: i64) -> u64 {
        let first = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM;
        first | if abi >= 2 { REFER } else { 0 } | if abi >= 3 { TRUNCATE } else { 0 }
    }

    /// Rules that refuse every write except under `writable`, and to the
    /// devices in `/dev`, which a shell needs for `> /dev/null`. Making them
    /// confines nothing yet: see [`confine`].
    pub fn ruleset(writable: &[PathBuf]) -> io::Result<OwnedFd> {
        let handled = writes(abi().map_err(io::Error::other)?);
        let attributes = RulesetAttr { handled_access_fs: handled };
        // SAFETY: the pointer and size describe `attributes`, which outlives the call.
        let made = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attributes, std::mem::size_of::<RulesetAttr>(), 0u32) };
        if made < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call returned a new descriptor, owned by nothing else. It closes itself across exec.
        let ruleset = unsafe { OwnedFd::from_raw_fd(made as libc::c_int) };
        let allow = |path: &Path, access: u64| -> io::Result<()> {
            // A directory that is not there has nothing to be written under.
            let Ok(place) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_CLOEXEC).open(path) else { return Ok(()) };
            // Only a directory can hold the rights to make and remove things.
            let access = if place.metadata()?.is_dir() { access } else { access & (WRITE_FILE | TRUNCATE) };
            let rule = PathBeneathAttr { allowed_access: access & handled, parent_fd: place.as_raw_fd() };
            // SAFETY: the pointer describes `rule`, and both descriptors are open for the length of the call.
            let added = unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &rule, 0u32) };
            if added < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        };
        for root in writable {
            allow(root, handled)?;
        }
        allow(Path::new("/dev"), WRITE_FILE | TRUNCATE)?;
        Ok(ruleset)
    }

    /// Have `command` take on `ruleset` as it starts: the last thing done in
    /// the new process before the program replaces it, so the program and
    /// everything it starts are confined, and nothing here is.
    pub fn confine(command: &mut tokio::process::Command, ruleset: OwnedFd) {
        // SAFETY: the closure runs between fork and exec, where little is safe. It makes two system
        // calls and reads the error number: no allocation, no locks.
        unsafe {
            command.pre_exec(move || {
                // Required before a process without privileges may confine itself. It also stops
                // setuid programs such as `sudo` from gaining privileges inside.
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 || libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    /// Everything is allowed but writing, which is allowed under the given
    /// directories and to the devices a shell writes to. Later rules win.
    fn profile(roots: usize) -> String {
        let allowed: String = (0..roots).map(|index| format!(" (subpath (param \"ROOT_{index}\"))")).collect();
        format!(
            "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*{allowed}\n  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\")\n  (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/fd/[0-9]+$\") (regex #\"^/dev/ttys[0-9]+$\"))\n"
        )
    }

    pub fn command(program: &str, writable: &[PathBuf]) -> tokio::process::Command {
        // The rules match a path as the system names it: /tmp is /private/tmp.
        let roots: Vec<PathBuf> = writable.iter().filter_map(|root| root.canonicalize().ok()).collect();
        let mut command = tokio::process::Command::new(SANDBOX_EXEC);
        command.arg("-p").arg(profile(roots.len()));
        for (index, root) in roots.iter().enumerate() {
            let mut parameter = std::ffi::OsString::from(format!("ROOT_{index}="));
            parameter.push(root);
            command.arg("-D").arg(parameter);
        }
        command.arg(program);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_and_the_temporary_directories_are_where_commands_may_write() {
        let directory = tempfile::tempdir().unwrap();
        let (workspace, extra) = (directory.path().join("work"), directory.path().join("cache"));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&extra).unwrap();
        let roots = roots(&workspace, &[extra.clone(), directory.path().join("gone"), workspace.clone()]);
        assert_eq!(roots[0], workspace.canonicalize().unwrap());
        assert!(roots.contains(&std::env::temp_dir().canonicalize().unwrap()) && roots.contains(&extra.canonicalize().unwrap()));
        // Named once each, and only when they exist.
        assert_eq!(roots.iter().filter(|root| **root == roots[0]).count(), 1);
        assert!(!roots.iter().any(|root| root.ends_with("gone")));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn says_so_where_commands_cannot_be_confined() {
        assert!(support().unwrap_err().contains("Linux and macOS"));
        assert!(command("sh", &Sandbox { writable: Vec::new() }).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn a_confined_command_writes_under_its_directories_and_nowhere_else() {
        // A machine whose kernel has no Landlock cannot run this.
        if let Err(why) = support() {
            eprintln!("not run: {why}");
            return;
        }
        // Not under the system's temporary directory in the rules: only `inside` is writable.
        let directory = tempfile::tempdir().unwrap();
        let (inside, outside) = (directory.path().join("inside"), directory.path().join("outside"));
        for path in [&inside, &outside] {
            std::fs::create_dir_all(path.join("sub")).unwrap();
            std::fs::write(path.join("kept.txt"), "kept\n").unwrap();
        }
        let sandbox = Sandbox { writable: vec![inside.clone()] };
        let run = |script: String| {
            let sandbox = sandbox.clone();
            let inside = inside.clone();
            async move {
                let output = command("sh", &sandbox)
                    .unwrap()
                    .arg("-c")
                    .arg(script)
                    .current_dir(&inside)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .await
                    .unwrap();
                (output.status.success(), String::from_utf8_lossy(&output.stderr).into_owned())
            }
        };
        let out = outside.display();

        // Inside: make, change, move between directories, and remove; and read what is outside.
        let (ok, said) = run(format!("echo new > a.txt && echo more >> kept.txt && mv a.txt sub/a.txt && rm kept.txt && mkdir made && cat {out}/kept.txt > /dev/null")).await;
        assert!(ok, "{said}");
        assert!(inside.join("sub").join("a.txt").is_file() && inside.join("made").is_dir() && !inside.join("kept.txt").exists());

        // Outside: none of that, whichever way it is tried. What the command starts is confined with it.
        for script in [
            format!("echo new > {out}/a.txt"),
            format!("echo more >> {out}/kept.txt"),
            format!("rm {out}/kept.txt"),
            format!("mkdir {out}/made"),
            format!("mv sub/a.txt {out}/a.txt"),
            format!("sh -c 'sh -c \"echo deep > {out}/deep.txt\"'"),
        ] {
            let (ok, _) = run(script.clone()).await;
            assert!(!ok, "`{script}` wrote outside the sandbox");
        }
        assert_eq!(std::fs::read_to_string(outside.join("kept.txt")).unwrap(), "kept\n");
        let left: Vec<String> = std::fs::read_dir(&outside).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(inside.join("sub").join("a.txt").is_file(), "a move that was refused left the file where it was");

        // The same commands run unconfined, so it was the sandbox that stopped them.
        let free = tokio::process::Command::new("sh").arg("-c").arg(format!("echo new > {out}/a.txt")).output().await.unwrap();
        assert!(free.status.success() && outside.join("a.txt").is_file());
    }
}
