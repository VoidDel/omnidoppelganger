//! Which tool calls run on their own and which wait for the user.
//!
//! In every mode, reading inside the workspace runs, anything that touches a
//! path outside the workspace asks, and every write into a `.git` directory
//! asks. The modes differ in edits and commands:
//!
//! - manual: every edit asks, and every command.
//! - edit: edits inside the workspace run; every command asks.
//! - plan: edits are refused, so the model explores and proposes; every command asks.
//! - auto: edits inside the workspace run, and so does a command that passes
//!   [`crate::safety`]; any other command asks.
//!
//! Fetching a web page asks in every mode, once per site: the request leaves
//! the machine, and its address can carry whatever the model puts in it. A
//! web search asks too: the query goes to the search service, and each
//! search is paid for.
//!
//! A tool of an MCP server asks in every mode: what it does is the server's
//! business, and nothing here can tell a harmless one from another.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::safety::{self, Dialect};
use crate::tools::{command_cwd, display, edits_files, in_git_directory, inside_workspace, resolve};

/// How much the agent does without asking. A session starts in the mode
/// chosen in settings, and the user can switch it at any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Manual,
    /// The only mode before there were modes, so logs and settings without one read as this.
    #[default]
    Edit,
    Plan,
    Auto,
    /// Nothing asks: commands of every kind, paths outside the workspace, `.git`, the web, MCP tools.
    /// Only a session in a worktree of its own may be in it, and only by the user's hand in that
    /// session: it is never a default, nor a schedule's. Since format 11.
    Bypass,
}

impl Mode {
    pub fn is_edit(&self) -> bool {
        *self == Mode::Edit
    }
}

/// The tool result an edit gets in plan mode.
pub const PLAN_REFUSAL: &str = "Plan mode is on, so files cannot be changed. Keep exploring, then present your plan and stop; the user switches modes to carry it out.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Not run and not asked about: the tool gets this message as an error result.
    Refuse(&'static str),
    Ask {
        /// The rule that asked: `command`, `unsandboxed`, `edit`, `mcp`, `web`, `search`, `outside_read`, `outside_write`,
        /// `git_write`, `install_skill`, or `add_mcp_server`.
        reason: &'static str,
        /// The command or path shown to the user.
        detail: String,
        /// What a session-wide approval of this request covers: later
        /// requests with the same grant run without asking. `None` when this
        /// kind of request must be approved every time.
        grant: Option<String>,
    },
}

/// Format a command for an approval prompt, prefixing its working directory when `cwd` differs from `workspace`.
fn command_detail(command: &str, input: &Value, workspace: &Path) -> String {
    match command_cwd(input, workspace) {
        Some(dir) => {
            let shown = if !inside_workspace(workspace, &dir) {
                dir.canonicalize().unwrap_or(dir).to_string_lossy().into_owned()
            } else {
                display(workspace, &dir)
            };
            if shown.is_empty() { command.to_string() } else { format!("[in {shown}]\n{command}") }
        }
        None => command.to_string(),
    }
}

/// [`check`] for a session whose commands are confined when `sandboxed`; see
/// [`crate::sandbox`]. A command that asks to run unconfined is the user's to
/// decide each time, whatever the mode and whatever was allowed before.
pub fn check_in(mode: Mode, tool: &str, input: &Value, workspace: &Path, sandboxed: bool) -> Verdict {
    if let Some(verdict) = installing(mode, tool, input) {
        return verdict;
    }
    if mode == Mode::Bypass {
        return Verdict::Allow;
    }
    if sandboxed && tool == "run_command" && input["unsandboxed"] == true {
        let command = input["command"].as_str().unwrap_or_default();
        return Verdict::Ask { reason: "unsandboxed", detail: command_detail(command, input, workspace), grant: None };
    }
    check(mode, tool, input, workspace)
}

/// What the agent installs adds to what every later turn can do: a skill's
/// instructions, which it will follow, or an MCP server, a program it can
/// have run. So it is the user's to decide each time, whatever the mode, and
/// in plan mode it is not done. The user is shown all of it: where a skill
/// comes from and where it goes, and the whole of a server's entry.
fn installing(mode: Mode, tool: &str, input: &Value) -> Option<Verdict> {
    let detail = match tool {
        "install_skill" => {
            let path = input["path"].as_str().filter(|path| !path.is_empty()).map(|path| format!("  ({path})")).unwrap_or_default();
            let into = if input["scope"] == "user" { "~/.odg/skills" } else { ".odg/skills" };
            format!("{}{path}\n→ {into}", input["source"].as_str().unwrap_or_default())
        }
        // Only a server the app can download is installed by it; for the others the tool only says how.
        "language_server" if input["install"] == true => {
            let language = crate::lsp::language_by_key(input["language"].as_str().unwrap_or_default())?;
            let download = language.download?;
            let megabytes = download.megabytes()?;
            format!("下载 {} {}（约 {megabytes} MB，只下载一次）\n→ ~/.odg/lsp", download.name, download.version)
        }
        "document_runtime" if input["install"] == true => {
            let modules: Vec<&str> = crate::runtime::MODULES.iter().map(|(_, what)| what.split(':').next().unwrap_or(what)).collect();
            format!(
                "下载 Python {} 及 {}（约 {} MB，只下载一次）\n→ ~/.odg/runtime",
                crate::runtime::PYTHON_VERSION,
                modules.join("、"),
                crate::runtime::DOWNLOAD_MB
            )
        }
        "add_mcp_server" => {
            let entry: serde_json::Map<String, Value> =
                ["command", "args", "env", "url", "headers"].iter().filter(|field| !input[**field].is_null()).map(|field| (field.to_string(), input[*field].clone())).collect();
            let name = input["name"].as_str().unwrap_or_default();
            serde_json::to_string_pretty(&serde_json::json!({ name: entry })).unwrap_or_default()
        }
        _ => return None,
    };
    if mode == Mode::Plan {
        return Some(Verdict::Refuse("Nothing is installed in plan mode. Put the installation in the plan, for the user to carry out."));
    }
    let reason = match tool {
        "install_skill" => "install_skill",
        "document_runtime" => "install_runtime",
        "language_server" => "install_lsp",
        _ => "add_mcp_server",
    };
    Some(Verdict::Ask { reason, detail, grant: None })
}

pub fn check(mode: Mode, tool: &str, input: &Value, workspace: &Path) -> Verdict {
    if let Some(verdict) = installing(mode, tool, input) {
        return verdict;
    }
    if mode == Mode::Bypass {
        return Verdict::Allow;
    }
    let path = |name: &str| resolve(workspace, input[name].as_str().unwrap_or("."));
    let outside = |name: &str| {
        let path = path(name);
        (!inside_workspace(workspace, &path)).then(|| path.to_string_lossy().into_owned())
    };
    let ask = |reason: &'static str, detail: String| Verdict::Ask { reason, detail, grant: Some(reason.to_string()) };
    match tool {
        "run_command" => {
            let command = input["command"].as_str().unwrap_or_default();
            let detail = command_detail(command, input, workspace);
            if let Some(dir) = command_cwd(input, workspace)
                && (!inside_workspace(workspace, &dir) || in_git_directory(workspace, &dir))
            {
                return Verdict::Ask { reason: "outside_command", detail, grant: None };
            }
            if mode == Mode::Auto && safety::check(command, Dialect::of_this_machine(), workspace).is_ok() {
                return Verdict::Allow;
            }
            // In auto mode only what the check does not pass needs approving, and so a standing grant.
            let grant = safety::grants_needed(command, Dialect::of_this_machine(), workspace, mode == Mode::Auto).ok().filter(|grants| !grants.is_empty()).map(|grants| grants.join("\n"));
            Verdict::Ask { reason: "command", detail, grant }
        }
        tool if edits_files(tool) && mode == Mode::Plan => Verdict::Refuse(PLAN_REFUSAL),
        // An edit is judged by every file it changes: a move by where the file is and where it goes.
        tool if edits_files(tool) => {
            let touched: Vec<PathBuf> = if tool == "move_file" { vec![path("path"), path("to")] } else { vec![path("path")] };
            let detail = touched.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" → ");
            if touched.iter().any(|path| in_git_directory(workspace, path)) {
                Verdict::Ask { reason: "git_write", detail, grant: None }
            } else if touched.iter().any(|path| !inside_workspace(workspace, path)) {
                ask("outside_write", detail)
            } else if mode == Mode::Manual {
                ask("edit", detail)
            } else {
                Verdict::Allow
            }
        }
        "read_file" | "search" | "find_files" => match outside("path") {
            Some(path) => ask("outside_read", path),
            None => Verdict::Allow,
        },
        "batch_inspect" => {
            let mut outside_paths: Vec<String> = Vec::new();
            if let Some(queries) = input["queries"].as_array() {
                for q in queries {
                    let target = resolve(workspace, q["path"].as_str().unwrap_or("."));
                    if !inside_workspace(workspace, &target) {
                        let shown = target.to_string_lossy().into_owned();
                        if !outside_paths.contains(&shown) {
                            outside_paths.push(shown);
                        }
                    }
                }
            }
            if outside_paths.is_empty() {
                Verdict::Allow
            } else {
                ask("outside_read", outside_paths.join(", "))
            }
        }
        // Reading code through its language's server: outside the workspace as reading there is; and the one server
        // that loads code of the project to run (see `crate::lsp`) is asked about, once for good if the user says so.
        // Only where that server is on the `PATH`: the one downloaded in its place runs nothing of the project.
        "code_nav" => match (outside("path"), crate::lsp::language_for(&path("path"))) {
            (Some(path), _) => ask("outside_read", path),
            (None, Some(language)) if language.runs_project_code && crate::lsp::installed(language.program).is_some() => Verdict::Ask {
                reason: "lsp",
                detail: format!("{} ({})", language.program, workspace.display()),
                grant: Some(format!("lsp:{}", language.program)),
            },
            (None, _) => Verdict::Allow,
        },
        // What is typed into a running program can make it do anything it does: each input is asked about, and
        // allowing it for the session covers that one job.
        "command_input" => Verdict::Ask { reason: "input", detail: typed(input), grant: Some(format!("input:{}", input["job"].as_str().unwrap_or_default())) },
        "web_search" => Verdict::Ask { reason: "search", detail: input["query"].as_str().unwrap_or_default().to_string(), grant: Some("search".to_string()) },
        // A picture writes a file and is paid for: what it shows, and where it goes, are put to the user.
        "generate_image" if mode == Mode::Plan => Verdict::Refuse("Nothing is painted in plan mode: a picture writes a file and is paid for. Put it in the plan."),
        "generate_image" if in_git_directory(workspace, &path("path")) => {
            Verdict::Refuse("Pictures cannot be saved inside a .git directory. Choose a path elsewhere in the workspace.")
        }
        "generate_image" => Verdict::Ask {
            reason: "image",
            detail: format!("{}\n→ {}", input["prompt"].as_str().unwrap_or_default().trim(), path("path").to_string_lossy()),
            grant: Some("image".to_string()),
        },
        "fetch_url" => {
            let url = input["url"].as_str().unwrap_or_default();
            match crate::web::host(url) {
                // The grant is the site, so its other pages do not ask again.
                Some(host) => Verdict::Ask { reason: "web", detail: url.to_string(), grant: Some(format!("web:{host}")) },
                None => Verdict::Refuse("Only valid http:// or https:// addresses without credentials or backslashes can be fetched."),
            }
        }
        // Reading a server's resource sends the address to it, which may be on another machine: asked about, as
        // a call is, and allowed for that server for good if the user says so.
        "read_mcp_resource" => {
            let server = input["server"].as_str().unwrap_or_default();
            Verdict::Ask { reason: "mcp_resource", detail: format!("{server} · {}", input["uri"].as_str().unwrap_or_default()), grant: Some(format!("mcp-resource:{server}")) }
        }
        tool if crate::mcp::split_name(tool).is_some() => {
            let (server, name) = crate::mcp::split_name(tool).unwrap_or_default();
            // What the user is shown: whose tool, which one, and with what, cut to a readable length.
            let arguments: String = input.to_string().chars().take(600).collect();
            Verdict::Ask { reason: "mcp", detail: format!("{server} · {name}  {arguments}"), grant: Some(format!("mcp:{tool}")) }
        }
        // Unknown tools reach the executor, which answers them with an error result.
        _ => Verdict::Allow,
    }
}

/// What `command_input` types, as the user is shown it: the text, and whether its input is then closed.
pub fn typed(input: &Value) -> String {
    let text = input["text"].as_str().unwrap_or_default();
    let close = if input["close"] == true { "（然后结束输入）" } else { "" };
    format!("← {}{close}", if text.is_empty() { "（不输入文字）" } else { text })
}

/// What a standing approval of `command` covers outside auto mode: one grant per command in it,
/// joined by newlines. `None` when it can only be approved one time at a time. See
/// [`safety::grants_needed`]; [`check`] gives the same, as the grant of the request it makes.
#[cfg(test)]
fn command_grant(command: &str, workspace: &Path) -> Option<String> {
    safety::grants_needed(command, Dialect::of_this_machine(), workspace, false).ok().filter(|grants| !grants.is_empty()).map(|grants| grants.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reason(verdict: Verdict) -> Option<&'static str> {
        match verdict {
            Verdict::Allow => None,
            Verdict::Refuse(_) => Some("refused"),
            Verdict::Ask { reason, .. } => Some(reason),
        }
    }

    fn check(tool: &str, input: &Value, workspace: &Path) -> Verdict {
        super::check(Mode::Edit, tool, input, workspace)
    }

    #[test]
    fn bypass_asks_about_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        for (tool, input) in [
            ("run_command", json!({ "command": "Remove-Item -Recurse ../other; node -e 'x' > out.txt" })),
            ("run_command", json!({ "command": "rm -rf ~/x", "unsandboxed": true })),
            ("edit_file", json!({ "path": "../outside.txt", "old_string": "", "new_string": "x" })),
            ("write_file", json!({ "path": ".git/hooks/pre-commit", "content": "x" })),
            ("read_file", json!({ "path": "../secret.txt" })),
            ("fetch_url", json!({ "url": "https://example.com" })),
            ("web_search", json!({ "query": "x" })),
            ("mcp__notes__shout", json!({ "text": "x" })),
        ] {
            assert_eq!(check_in(Mode::Bypass, tool, &input, workspace, true), Verdict::Allow, "{tool} {input}");
        }
    }

    #[test]
    fn setting_up_the_document_runtime_asks_every_time_and_asking_about_it_does_not() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        for mode in [Mode::Manual, Mode::Edit, Mode::Auto, Mode::Bypass, Mode::Plan] {
            assert_eq!(check_in(mode, "document_runtime", &json!({}), workspace, false), Verdict::Allow, "{mode:?}");
        }
        for mode in [Mode::Manual, Mode::Edit, Mode::Auto, Mode::Bypass] {
            let Verdict::Ask { reason, detail, grant } = check_in(mode, "document_runtime", &json!({ "install": true }), workspace, false) else { panic!("{mode:?} did not ask") };
            assert_eq!((reason, grant), ("install_runtime", None), "{mode:?}");
            assert!(detail.contains("python-docx") && detail.contains("约 90 MB") && detail.contains("matplotlib") && detail.contains("~/.odg/runtime"), "{detail}");
        }
        assert_eq!(reason(check_in(Mode::Plan, "document_runtime", &json!({ "install": true }), workspace, false)), Some("refused"));
    }

    #[test]
    fn a_picture_is_asked_for_with_what_it_shows_and_where_it_goes_and_not_painted_in_plan_mode() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let input = json!({ "prompt": " 雪中的白鹤 ", "path": "images/crane.png" });
        for mode in [Mode::Manual, Mode::Edit, Mode::Auto] {
            let Verdict::Ask { reason, detail, grant } = check_in(mode, "generate_image", &input, workspace, false) else { panic!("{mode:?} did not ask") };
            assert_eq!((reason, grant.as_deref()), ("image", Some("image")), "{mode:?}");
            assert!(detail.starts_with("雪中的白鹤\n→ ") && detail.contains("crane.png"), "{detail}");
        }
        assert_eq!(check_in(Mode::Bypass, "generate_image", &input, workspace, false), Verdict::Allow);
        assert_eq!(reason(check_in(Mode::Plan, "generate_image", &input, workspace, false)), Some("refused"));
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        let into_git = json!({ "prompt": "x", "path": ".git/hooks/pre-commit.png" });
        for mode in [Mode::Manual, Mode::Edit, Mode::Auto, Mode::Plan] {
            assert_eq!(reason(check_in(mode, "generate_image", &into_git, workspace, false)), Some("refused"), "{mode:?}");
        }
    }

    #[test]
    fn installing_asks_every_time_in_every_mode_and_shows_all_of_it() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let skill = json!({ "source": "https://github.com/anthropics/skills", "path": "skills/pdf", "scope": "user" });
        let server = json!({ "name": "notes", "command": "npx", "args": ["-y", "notes-mcp"], "env": { "TOKEN": "abc" }, "replace": true });
        for mode in [Mode::Manual, Mode::Edit, Mode::Auto, Mode::Bypass] {
            let asked = check_in(mode, "install_skill", &skill, workspace, false);
            assert_eq!(asked, Verdict::Ask { reason: "install_skill", detail: "https://github.com/anthropics/skills  (skills/pdf)\n→ ~/.odg/skills".into(), grant: None }, "{mode:?}");
            let Verdict::Ask { reason, detail, grant } = check_in(mode, "add_mcp_server", &server, workspace, false) else { panic!("{mode:?} did not ask") };
            assert_eq!((reason, grant), ("add_mcp_server", None));
            // The entry as it will be written, its secrets too, for the user to see; not the flag that only says how.
            let shown: Value = serde_json::from_str(&detail).unwrap();
            assert_eq!(shown, json!({ "notes": { "command": "npx", "args": ["-y", "notes-mcp"], "env": { "TOKEN": "abc" } } }));
        }
        assert_eq!(reason(check_in(Mode::Plan, "install_skill", &json!({ "source": "./x" }), workspace, false)), Some("refused"));
        assert_eq!(reason(check_in(Mode::Plan, "add_mcp_server", &server, workspace, false)), Some("refused"));
        let project = check_in(Mode::Edit, "install_skill", &json!({ "source": "D:\\skills\\pdf" }), workspace, false);
        assert!(matches!(project, Verdict::Ask { ref detail, .. } if detail == "D:\\skills\\pdf\n→ .odg/skills"), "{project:?}");
    }

    #[test]
    fn each_mode_decides_edits_and_commands_its_own_way() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let edit = json!({ "path": "src/a.rs", "old_string": "", "new_string": "x" });
        let safe = json!({ "command": "git status" });
        let risky = json!({ "command": "git push" });
        let read = json!({ "path": "src/a.rs" });
        let decide = |mode, tool, input: &Value| reason(super::check(mode, tool, input, workspace));
        // (mode, edit, safe command, risky command)
        for (mode, edits, safe_command, risky_command) in [
            (Mode::Manual, Some("edit"), Some("command"), Some("command")),
            (Mode::Edit, None, Some("command"), Some("command")),
            (Mode::Plan, Some("refused"), Some("command"), Some("command")),
            (Mode::Auto, None, None, Some("command")),
        ] {
            assert_eq!(decide(mode, "edit_file", &edit), edits, "{mode:?}");
            // Writing a whole file is decided like an edit; finding files like a read.
            assert_eq!(decide(mode, "write_file", &json!({ "path": "src/a.rs", "content": "x" })), edits, "{mode:?}");
            assert_eq!(decide(mode, "find_files", &json!({ "glob": "**/*.rs" })), None, "{mode:?}");
            assert_eq!(decide(mode, "find_files", &json!({ "path": ".." })), Some("outside_read"), "{mode:?}");
            assert_eq!(decide(mode, "run_command", &safe), safe_command, "{mode:?}");
            assert_eq!(decide(mode, "run_command", &risky), risky_command, "{mode:?}");
            // Reading inside runs and reading outside asks, whatever the mode.
            assert_eq!(decide(mode, "read_file", &read), None, "{mode:?}");
            assert_eq!(decide(mode, "read_file", &json!({ "path": "../x" })), Some("outside_read"), "{mode:?}");
        }
        // Fetching a page asks whatever the mode, and its grant is the site.
        for mode in [Mode::Manual, Mode::Edit, Mode::Plan, Mode::Auto] {
            let asked = super::check(mode, "fetch_url", &json!({ "url": "https://docs.rs/serde/latest/serde/" }), workspace);
            assert_eq!(asked, Verdict::Ask { reason: "web", detail: "https://docs.rs/serde/latest/serde/".into(), grant: Some("web:docs.rs".into()) });
        }
        assert_eq!(reason(super::check(Mode::Edit, "fetch_url", &json!({ "url": "file:///etc/passwd" }), workspace)), Some("refused"));
        for bad_url in ["https://evil.com\\@docs.rs/x", "https://user:pass@docs.rs/", ""] {
            for mode in [Mode::Manual, Mode::Edit, Mode::Plan, Mode::Auto] {
                assert_eq!(reason(super::check(mode, "fetch_url", &json!({ "url": bad_url }), workspace)), Some("refused"), "{mode:?} {bad_url}");
            }
        }
        // A search asks in every mode too, with one grant for all searches.
        let searched = super::check(Mode::Auto, "web_search", &json!({ "query": "tokio release notes" }), workspace);
        assert_eq!(searched, Verdict::Ask { reason: "search", detail: "tokio release notes".into(), grant: Some("search".into()) });
        // A tool of an MCP server asks whatever the mode, and its grant names that one tool.
        for mode in [Mode::Manual, Mode::Edit, Mode::Plan, Mode::Auto] {
            let asked = super::check(mode, "mcp__files__read_file", &json!({ "path": "a" }), workspace);
            assert_eq!(asked, Verdict::Ask { reason: "mcp", detail: "files · read_file  {\"path\":\"a\"}".into(), grant: Some("mcp:mcp__files__read_file".into()) });
        }
        // A session-wide approval of edits in manual mode covers every later edit.
        assert!(matches!(super::check(Mode::Manual, "edit_file", &edit, workspace), Verdict::Ask { grant: Some(grant), .. } if grant == "edit"));
        // Auto mode still asks before writing outside the workspace or into .git.
        assert_eq!(decide(Mode::Auto, "edit_file", &json!({ "path": "../x.rs" })), Some("outside_write"));
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        assert_eq!(decide(Mode::Auto, "edit_file", &json!({ "path": ".git/config" })), Some("git_write"));
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
        for mode in [Mode::Manual, Mode::Edit, Mode::Plan, Mode::Auto] {
            assert_eq!(
                super::check(
                    mode,
                    "batch_inspect",
                    &json!({ "queries": [{ "kind": "find_files", "glob": "**/*.rs" }, { "kind": "read_file", "path": "src/a.rs" }] }),
                    workspace
                ),
                Verdict::Allow,
                "{mode:?}"
            );
            assert_eq!(
                reason(super::check(
                    mode,
                    "batch_inspect",
                    &json!({ "queries": [{ "kind": "read_file", "path": "src/a.rs" }, { "kind": "search", "pattern": "key", "path": "../outside" }] }),
                    workspace
                )),
                Some("outside_read"),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_command_that_leaves_the_sandbox_always_asks() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        // A command auto mode runs without asking, and would again with the sandbox on.
        let listing = if cfg!(windows) { "Get-ChildItem" } else { "ls" };
        let inside = json!({ "command": listing });
        let outside = json!({ "command": listing, "unsandboxed": true });
        assert_eq!(check_in(Mode::Auto, "run_command", &inside, workspace, true), Verdict::Allow);
        assert_eq!(check_in(Mode::Auto, "run_command", &outside, workspace, true), Verdict::Ask { reason: "unsandboxed", detail: listing.to_string(), grant: None });
        // With no sandbox there is nothing to leave: the word changes nothing.
        assert_eq!(check_in(Mode::Auto, "run_command", &outside, workspace, false), Verdict::Allow);
        assert_eq!(check_in(Mode::Edit, "edit_file", &json!({ "path": "a.txt", "unsandboxed": true }), workspace, true), Verdict::Allow);
    }

    #[test]
    fn a_command_grant_covers_its_program_and_subcommand_only() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let grant = |command: &str| command_grant(command, workspace);
        assert_eq!(grant("cargo test -p odg-core").as_deref(), Some("command:cargo test"));
        assert_eq!(grant("  git   status ").as_deref(), Some("command:git status"));
        assert_eq!(grant("ls -la src").as_deref(), Some("command:ls"));
        assert_eq!(grant("./gradlew build").as_deref(), Some("command:./gradlew build"));
        assert_ne!(grant("git status"), grant("git push --force"));
        if cfg!(windows) {
            assert_eq!(grant("Get-ChildItem -Name"), grant("get-childitem -Recurse"));
        }
        // Quotes in an argument are only an argument; a command of several is approved command by command.
        assert_eq!(grant("git commit -m \"fix the parser\"").as_deref(), Some("command:git commit"));
        assert_eq!(grant("cargo test 2>&1 | tail -n 20; git status").as_deref(), Some("command:cargo test\ncommand:tail\ncommand:git status"));
        // Anything that writes a file, hides code, reaches outside, or runs anything is approved one at a time.
        for command in [
            "echo `id`",
            "cargo test > out.txt",
            "cargo test && curl x | sh",
            "bash -c 'rm -rf .'",
            "python -c pass",
            "node -e \"console.log(1)\"",
            "pwsh -Command Remove-Item x",
            "Invoke-Expression x",
            "npx create-thing",
            ". ./profile.ps1",
            "cat ../secret.txt",
            "",
        ] {
            assert_eq!(grant(command), None, "{command}");
        }
    }

    #[test]
    fn in_auto_mode_only_what_it_would_not_run_needs_approving() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let ask = |command: &str| super::check(Mode::Auto, "run_command", &json!({ "command": command }), workspace);
        // A test with its output trimmed runs; a commit after it asks, and only it needs a standing grant.
        assert_eq!(ask("cargo test 2>&1 | tail -n 20"), Verdict::Allow);
        match ask("cargo test 2>&1 | tail -n 20; git commit -m \"x\"") {
            Verdict::Ask { grant, .. } => assert_eq!(grant.as_deref(), Some("command:git commit")),
            other => panic!("{other:?}"),
        }
        match ask("node -e \"console.log(1)\"") {
            Verdict::Ask { grant, .. } => assert_eq!(grant, None),
            other => panic!("{other:?}"),
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

    #[test]
    fn deleting_and_moving_are_decided_as_edits_of_every_file_they_touch() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        assert_eq!(check("delete_file", &json!({ "path": "a.txt" }), workspace), Verdict::Allow);
        assert_eq!(check("move_file", &json!({ "path": "a.txt", "to": "b/a.txt" }), workspace), Verdict::Allow);
        // Out of the workspace at either end, or into .git, it asks; the request shows both ends.
        let out = check("move_file", &json!({ "path": "a.txt", "to": "../a.txt" }), workspace);
        assert!(matches!(&out, Verdict::Ask { reason: "outside_write", detail, .. } if detail.ends_with(&(" → ".to_string() + &resolve(workspace, "../a.txt").to_string_lossy()))), "{out:?}");
        assert_eq!(reason(check("move_file", &json!({ "path": "../a.txt", "to": "a.txt" }), workspace)), Some("outside_write"));
        assert_eq!(reason(check("delete_file", &json!({ "path": ".git/config" }), workspace)), Some("git_write"));
        assert_eq!(reason(check("move_file", &json!({ "path": "hook.sh", "to": ".git/hooks/pre-commit" }), workspace)), Some("git_write"));
        // Asked about every time in 步步请示, and not done at all in 先谋后动.
        assert_eq!(reason(super::check(Mode::Manual, "delete_file", &json!({ "path": "a.txt" }), workspace)), Some("edit"));
        assert_eq!(reason(super::check(Mode::Plan, "move_file", &json!({ "path": "a.txt", "to": "b.txt" }), workspace)), Some("refused"));
    }

    #[test]
    fn navigating_code_asks_only_outside_the_workspace_and_before_running_project_code() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        assert_eq!(check("code_nav", &json!({ "operation": "definition", "path": "src/a.rs" }), workspace), Verdict::Allow);
        assert_eq!(super::check(Mode::Plan, "code_nav", &json!({ "operation": "symbols", "path": "a.py" }), workspace), Verdict::Allow);
        assert_eq!(reason(check("code_nav", &json!({ "operation": "hover", "path": "../x.rs" }), workspace)), Some("outside_read"));
        // The TypeScript server on the `PATH` loads the project's own TypeScript: asked about, and allowed for good by
        // its grant. Where it is not there, the TypeScript downloaded in its place runs nothing of the project.
        let typescript = check("code_nav", &json!({ "operation": "definition", "path": "src/a.ts" }), workspace);
        if crate::lsp::installed("typescript-language-server").is_some() {
            assert!(matches!(&typescript, Verdict::Ask { reason: "lsp", grant: Some(grant), .. } if grant == "lsp:typescript-language-server"), "{typescript:?}");
        } else {
            assert_eq!(typescript, Verdict::Allow);
        }
        assert!(crate::grants::GrantStore::keeps("lsp:typescript-language-server"));
    }

    #[test]
    fn downloading_a_language_server_is_asked_about_every_time_and_not_done_in_plan_mode() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let install = json!({ "language": "rust", "install": true });
        if crate::lsp::language_by_key("rust").and_then(|rust| rust.download).and_then(|download| download.megabytes()).is_some() {
            let asked = super::check(Mode::Bypass, "language_server", &install, workspace);
            assert!(matches!(&asked, Verdict::Ask { reason: "install_lsp", detail, grant: None } if detail.starts_with("下载 rust-analyzer 2026-11-16（约 ") && detail.ends_with("→ ~/.odg/lsp")), "{asked:?}");
            assert_eq!(reason(super::check(Mode::Plan, "language_server", &install, workspace)), Some("refused"));
        }
        // What is downloaded in the place of a language's own server is named as itself.
        if crate::lsp::language_by_key("python").and_then(|python| python.download).and_then(|download| download.megabytes()).is_some() {
            let asked = super::check(Mode::Edit, "language_server", &json!({ "language": "python", "install": true }), workspace);
            assert!(matches!(&asked, Verdict::Ask { reason: "install_lsp", detail, grant: None } if detail.starts_with("下载 ty 0.0.107（约 ")), "{asked:?}");
            let asked = super::check(Mode::Edit, "language_server", &json!({ "language": "typescript", "install": true }), workspace);
            assert!(matches!(&asked, Verdict::Ask { reason: "install_lsp", detail, grant: None } if detail.starts_with("下载 TypeScript 7.0.2（约 ")), "{asked:?}");
        }
        // Only looking, or a server the app does not download, asks nothing.
        assert_eq!(check("language_server", &json!({ "language": "rust" }), workspace), Verdict::Allow);
        assert_eq!(check("language_server", &json!({ "language": "go", "install": true }), workspace), Verdict::Allow);
    }

    #[test]
    fn typing_into_a_background_command_is_asked_about_with_what_is_typed() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        let input = json!({ "job": "b2", "text": "y", "close": true });
        assert_eq!(check("command_input", &input, workspace), Verdict::Ask { reason: "input", detail: "← y（然后结束输入）".into(), grant: Some("input:b2".into()) });
        assert_eq!(reason(super::check(Mode::Auto, "command_input", &input, workspace)), Some("input"));
        assert_eq!(super::check(Mode::Bypass, "command_input", &input, workspace), Verdict::Allow);
        // Allowed for the session, it covers that job only, and is never kept for good.
        assert!(!crate::grants::GrantStore::keeps("input:b2"));
    }

    #[test]
    fn reading_an_mcp_resource_is_asked_about_and_listing_them_is_not() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path();
        assert_eq!(
            check("read_mcp_resource", &json!({ "server": "notes", "uri": "notes://today" }), workspace),
            Verdict::Ask { reason: "mcp_resource", detail: "notes · notes://today".into(), grant: Some("mcp-resource:notes".into()) }
        );
        assert!(crate::grants::GrantStore::keeps("mcp-resource:notes"));
        assert_eq!(check("list_mcp_resources", &json!({}), workspace), Verdict::Allow);
        assert_eq!(check("present_plan", &json!({ "plan": "x" }), workspace), Verdict::Allow);
    }

    #[test]
    fn command_cwd_inside_workspace_follows_normal_rules_while_outside_or_git_always_asks_without_grant() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("project");
        std::fs::create_dir_all(workspace.join("crates/sub")).unwrap();
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        let outside = directory.path().join("other");
        std::fs::create_dir_all(&outside).unwrap();

        let listing = if cfg!(windows) { "Get-ChildItem" } else { "ls" };
        // Inside workspace: auto mode allows safe commands; edit mode asks with `[in ...]` detail and standing grant.
        assert_eq!(super::check(Mode::Auto, "run_command", &json!({ "command": listing, "cwd": "crates/sub" }), &workspace), Verdict::Allow);
        let edit_sub = check("run_command", &json!({ "command": "cargo test", "cwd": "crates/sub" }), &workspace);
        assert_eq!(
            edit_sub,
            Verdict::Ask { reason: "command", detail: "[in crates/sub]\ncargo test".into(), grant: Some("command:cargo test".into()) }
        );
        // Workspace root itself (`cwd: "."`) keeps the plain command detail.
        assert_eq!(
            check("run_command", &json!({ "command": "cargo test", "cwd": "." }), &workspace),
            Verdict::Ask { reason: "command", detail: "cargo test".into(), grant: Some("command:cargo test".into()) }
        );
        // Outside workspace or inside `.git`: even auto mode asks with reason `outside_command` and no standing grant.
        for cwd in ["..", "../other", ".git", ".git/hooks"] {
            let asked = super::check(Mode::Auto, "run_command", &json!({ "command": listing, "cwd": cwd }), &workspace);
            assert!(matches!(&asked, Verdict::Ask { reason: "outside_command", grant: None, detail } if detail.starts_with("[in ") && detail.ends_with(listing)), "{cwd}: {asked:?}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, workspace.join("escape")).unwrap();
            let escaped = super::check(Mode::Auto, "run_command", &json!({ "command": listing, "cwd": "escape" }), &workspace);
            assert!(matches!(&escaped, Verdict::Ask { reason: "outside_command", grant: None, .. }), "{escaped:?}");
        }
    }
}
