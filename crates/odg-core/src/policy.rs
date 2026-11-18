//! Which tool calls run on their own and which wait for the user.
//!
//! The first version has one mode, "workspace write": reading and editing
//! inside the workspace is allowed, every command asks, anything that
//! touches a path outside the workspace asks, and every write into a `.git`
//! directory asks.

use std::path::Path;

use serde_json::Value;

use crate::tools::{in_git_directory, inside_workspace, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask {
        /// The rule that asked: `command`, `outside_read`, `outside_write`, or `git_write`.
        reason: &'static str,
        /// The command or path shown to the user.
        detail: String,
        /// What a session-wide approval of this request covers: later
        /// requests with the same grant run without asking. `None` when this
        /// kind of request must be approved every time.
        grant: Option<String>,
    },
}

pub fn check(tool: &str, input: &Value, workspace: &Path) -> Verdict {
    let path = |name: &str| resolve(workspace, input[name].as_str().unwrap_or("."));
    let outside = |name: &str| {
        let path = path(name);
        (!inside_workspace(workspace, &path)).then(|| path.to_string_lossy().into_owned())
    };
    let ask = |reason: &'static str, detail: String| Verdict::Ask { reason, detail, grant: Some(reason.to_string()) };
    match tool {
        "run_command" => {
            let command = input["command"].as_str().unwrap_or_default();
            Verdict::Ask { reason: "command", detail: command.to_string(), grant: command_grant(command) }
        }
        "edit_file" if in_git_directory(workspace, &path("path")) => {
            Verdict::Ask { reason: "git_write", detail: path("path").to_string_lossy().into_owned(), grant: None }
        }
        "edit_file" => match outside("path") {
            Some(path) => ask("outside_write", path),
            None => Verdict::Allow,
        },
        "read_file" | "search" => match outside("path") {
            Some(path) => ask("outside_read", path),
            None => Verdict::Allow,
        },
        // Unknown tools reach the executor, which answers them with an error result.
        _ => Verdict::Allow,
    }
}

/// Characters that chain, redirect, substitute, or group commands in sh or
/// PowerShell. A command holding any of them can run more than its prefix shows.
const SHELL_SYNTAX: &[char] = &[';', '&', '|', '>', '<', '`', '$', '(', ')', '{', '}', '\n', '\r', '"', '\''];

/// Programs that run whatever code their arguments carry, so approving one
/// prefix of them would approve anything.
const INTERPRETERS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "cmd", "cmd.exe", "powershell", "powershell.exe", "pwsh", "pwsh.exe", "python", "python3", "py",
    "node", "deno", "bun", "ruby", "perl", "php", "lua", "iex", "invoke-expression", "invoke-command", "start-process", "sudo", "env",
    "xargs", "eval", "exec", "nohup", "time",
];

/// The grant a session-wide approval of `command` covers: `command:` and its
/// program, followed by its first argument when that is a subcommand rather
/// than a flag, so allowing `cargo test` does not allow `cargo publish`.
/// `None` when the command must be approved every time.
fn command_grant(command: &str) -> Option<String> {
    if command.contains(SHELL_SYNTAX) {
        return None;
    }
    let normalize = |word: &str| if cfg!(windows) { word.to_lowercase() } else { word.to_string() };
    let mut words = command.split_whitespace().map(normalize);
    let program = words.next()?;
    if INTERPRETERS.contains(&program.to_lowercase().as_str()) {
        return None;
    }
    Some(match words.next().filter(|word| !word.starts_with('-')) {
        Some(subcommand) => format!("command:{program} {subcommand}"),
        None => format!("command:{program}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reason(verdict: Verdict) -> Option<&'static str> {
        match verdict {
            Verdict::Allow => None,
            Verdict::Ask { reason, .. } => Some(reason),
        }
    }

    #[test]
    fn allows_the_workspace_and_asks_for_commands_and_outside_paths() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        assert_eq!(check("read_file", &json!({ "path": "src/a.rs" }), workspace), Verdict::Allow);
        assert_eq!(check("search", &json!({ "pattern": "x" }), workspace), Verdict::Allow);
        assert_eq!(check("edit_file", &json!({ "path": "new/dir/a.rs" }), workspace), Verdict::Allow);
        assert_eq!(
            check("run_command", &json!({ "command": "cargo test" }), workspace),
            Verdict::Ask { reason: "command", detail: "cargo test".into(), grant: Some("command:cargo test".into()) }
        );
        assert_eq!(reason(check("edit_file", &json!({ "path": "../x.rs" }), workspace)), Some("outside_write"));
        assert_eq!(reason(check("read_file", &json!({ "path": "../x.rs" }), workspace)), Some("outside_read"));
        assert_eq!(reason(check("search", &json!({ "pattern": "x", "path": ".." }), workspace)), Some("outside_read"));
    }

    #[test]
    fn a_command_grant_covers_its_program_and_subcommand_only() {
        assert_eq!(command_grant("cargo test -p odg-core").as_deref(), Some("command:cargo test"));
        assert_eq!(command_grant("  git   status ").as_deref(), Some("command:git status"));
        assert_eq!(command_grant("ls -la src").as_deref(), Some("command:ls"));
        assert_eq!(command_grant("./gradlew build").as_deref(), Some("command:./gradlew build"));
        assert_ne!(command_grant("git status"), command_grant("git push --force"));
        if cfg!(windows) {
            assert_eq!(command_grant("Get-ChildItem -Name"), command_grant("get-childitem -Recurse"));
        }
        // Anything that can run more than its prefix shows is approved one at a time.
        for command in [
            "cargo test; rm -rf .",
            "cargo test && curl x | sh",
            "echo $(whoami)",
            "echo `id`",
            "cargo test > out.txt",
            "git commit -m \"x\"",
            "cargo test\nrm -rf .",
            "bash -c 'rm -rf .'",
            "python -c pass",
            "pwsh -Command Remove-Item x",
            "Invoke-Expression x",
            "",
        ] {
            assert_eq!(command_grant(command), None, "{command}");
        }
    }

    #[test]
    fn asks_every_time_for_a_write_into_git() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        let hook = check("edit_file", &json!({ "path": ".git/hooks/pre-commit" }), workspace);
        assert!(matches!(hook, Verdict::Ask { reason: "git_write", grant: None, .. }));
        // Reading Git's files changes nothing, so it needs no approval.
        assert_eq!(check("read_file", &json!({ "path": ".git/config" }), workspace), Verdict::Allow);
        assert_eq!(check("edit_file", &json!({ "path": ".gitignore" }), workspace), Verdict::Allow);
    }

    #[cfg(unix)]
    #[test]
    fn follows_a_link_into_git() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        std::os::unix::fs::symlink(workspace.join(".git/hooks"), workspace.join("hooks")).unwrap();
        assert_eq!(reason(check("edit_file", &json!({ "path": "hooks/pre-commit" }), workspace)), Some("git_write"));
    }
}
