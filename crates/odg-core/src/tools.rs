//! The tools: read a file, search file contents, find files by name, edit a
//! file, write a whole file, run a command, read or stop a command left
//! running in the background, and keep a to-do list.

mod commands;
mod extras;
mod files;
mod specs;
#[cfg(test)]
mod tests;

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::events::Image;
use crate::jobs::Jobs;

pub use commands::{InlineShellOutput, SENSITIVE_ENV_VARS, run_inline_shell, shell};
pub(crate) use commands::{kill_tree, strip_ansi};
pub use extras::{ASK_USER, DelegateTask, OPTIONS, PRESENT_PLAN, QUESTIONS, SUBAGENTS, TODO_STATES, answered, delegate_tasks, questions, todo_items};
pub use files::edits_files;
pub(crate) use files::{LineDiff, line_diff};
#[cfg(test)]
use files::picture;
pub use specs::{ask_user_spec, delegate_spec, image_spec, mcp_resource_specs, present_plan_spec, search_spec, specs};

pub(crate) const READ_FILE_BYTES: u64 = 10 * 1024 * 1024;
const LINE_CHARS: usize = 2000;
/// Every result is replayed in every later request of the session, so one
/// read must stay far below the context window.
pub(super) const READ_OUTPUT_CHARS: usize = 100_000;

pub struct ToolContext {
    /// Directory relative paths resolve against and commands run in.
    pub workspace: PathBuf,
    pub cancel: CancellationToken,
    /// Where `edit_file` saves what a file held before changing it, so the
    /// edit can be undone; see [`crate::undo`]. `None` saves nothing.
    pub snapshots: Option<PathBuf>,
    /// The session's background commands. `None` means commands cannot be left running.
    pub jobs: Option<Arc<Jobs>>,
    /// The MCP servers whose tools the model may call. `None` means there are none.
    pub mcp: Option<Arc<crate::mcp::McpHub>>,
    /// The directories skills are looked for in, the workspace's last; see [`crate::skills`].
    pub skills: Vec<PathBuf>,
    /// Where `web_search` sends a search. `None` means searching is not set up.
    pub search: Option<crate::web::SearchConfig>,
    /// What confines the commands that run. `None` means they are not confined.
    pub sandbox: Option<crate::sandbox::Sandbox>,
    /// Where the document runtime is kept; see [`crate::runtime`]. `None` means it is not offered.
    pub runtime: Option<PathBuf>,
    /// Where `generate_image` paints. `None` means painting is not set up.
    pub painter: Option<crate::imagegen::ImageConfig>,
    /// Whether the model at work can see pictures: `None` when that is not known, and a picture is tried.
    pub vision: Option<bool>,
    /// The language servers `code_nav` asks. `None` means code is not navigated.
    pub lsp: Option<Arc<crate::lsp::LspHub>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// Text returned to the model.
    pub content: String,
    pub is_error: bool,
    /// Presentation data for the client; never sent to the model.
    pub meta: Value,
    /// Pictures the model is shown with the text: an image `read_file` read.
    pub images: Vec<Image>,
}

impl ToolOutput {
    pub(crate) fn ok(content: impl Into<String>, meta: Value) -> Self {
        Self { content: content.into(), is_error: false, meta, images: Vec::new() }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true, meta: Value::Null, images: Vec::new() }
    }
}

/// Resolve a tool's path argument against the workspace, folding `.` and `..` without touching the disk.
pub fn resolve(workspace: &Path, path: &str) -> PathBuf {
    let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { workspace.join(path) };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// Maximum number of symlink hops followed when resolving a path whose leaf does not exist yet.
const MAX_SYMLINK_HOPS: usize = 32;

/// Where `path` really points: links (including dangling symlinks) are followed
/// component by component up to [`MAX_SYMLINK_HOPS`], and any missing tail is
/// appended unchanged onto the resolved target.
pub(crate) fn real_path(path: &Path) -> Option<PathBuf> {
    let owned;
    let abs = if path.is_absolute() {
        path
    } else {
        owned = std::env::current_dir().ok()?.join(path);
        &owned
    };
    real_path_inner(abs, 0)
}

fn real_path_inner(path: &Path, hops: usize) -> Option<PathBuf> {
    if hops > MAX_SYMLINK_HOPS {
        return None;
    }
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let mut current = PathBuf::new();
    let mut missing = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !missing
                    && let Ok(canonical) = current.canonicalize()
                {
                    current = canonical;
                }
                let _ = current.pop();
            }
            Component::Normal(name) => {
                current.push(name);
                if missing {
                    continue;
                }
                match std::fs::symlink_metadata(&current) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        let target = std::fs::read_link(&current).ok()?;
                        let next = if target.is_absolute() {
                            target
                        } else {
                            current.pop();
                            current.join(target)
                        };
                        current = real_path_inner(&next, hops + 1)?;
                        if !current.exists() && std::fs::symlink_metadata(&current).is_err() {
                            missing = true;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        current.pop();
                        if !current.as_os_str().is_empty()
                            && let Ok(canonical) = current.canonicalize()
                        {
                            current = canonical;
                        }
                        current.push(name);
                        missing = true;
                    }
                }
            }
        }
    }
    if !missing
        && let Ok(canonical) = current.canonicalize()
    {
        return Some(canonical);
    }
    (!current.as_os_str().is_empty()).then_some(current)
}

/// Whether `path` is the workspace or below it, following links in the part of the path that exists.
pub fn inside_workspace(workspace: &Path, path: &Path) -> bool {
    match (workspace.canonicalize(), real_path(path)) {
        (Ok(root), Some(full)) => full.starts_with(root),
        _ => false,
    }
}

/// Whether `path`, links followed, lies in a `.git` directory below the
/// workspace (or anywhere, when it is outside the workspace). Git runs hooks
/// and reads configuration from there, so writing to it can run code later.
pub fn in_git_directory(workspace: &Path, path: &Path) -> bool {
    let Some(full) = real_path(path) else { return true };
    let below = workspace.canonicalize().ok().and_then(|root| full.strip_prefix(root).ok().map(Path::to_path_buf)).unwrap_or(full);
    below.components().any(|component| component.as_os_str().to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
}

/// The branch checked out in the workspace, read from Git's own files so no
/// program has to run: a branch name, the first seven characters of the
/// commit when no branch is checked out, or `None` outside a repository.
pub fn git_branch(workspace: &Path) -> Option<String> {
    let mut git = workspace.join(".git");
    if git.is_file() {
        // A linked worktree or a submodule: the file points at the real directory.
        let pointer = std::fs::read_to_string(&git).ok()?;
        git = workspace.join(pointer.trim().strip_prefix("gitdir:")?.trim());
    }
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_string()),
        None => (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_string()),
    }
}

pub(crate) fn display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Resolved working directory requested by `run_command`'s `cwd` parameter, when given and non-empty.
pub(crate) fn command_cwd(input: &Value, workspace: &Path) -> Option<PathBuf> {
    let raw = input["cwd"].as_str().map(str::trim).filter(|cwd| !cwd.is_empty())?;
    Some(resolve(workspace, raw))
}

pub(super) fn string_arg<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolOutput> {
    input[name].as_str().ok_or_else(|| ToolOutput::error(format!("`{name}` is required and must be a string.")))
}

pub(super) fn clip(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS {
        line.to_string()
    } else {
        format!("{}… [line cut at {LINE_CHARS} characters]", line.chars().take(LINE_CHARS).collect::<String>())
    }
}

fn blocking_context(context: &ToolContext) -> ToolContext {
    ToolContext {
        workspace: context.workspace.clone(),
        cancel: context.cancel.clone(),
        snapshots: context.snapshots.clone(),
        jobs: None,
        mcp: None,
        skills: Vec::new(),
        search: None,
        sandbox: None,
        runtime: None,
        painter: None,
        vision: context.vision,
        lsp: None,
    }
}

async fn run_blocking_tool(
    input: &Value,
    context: &ToolContext,
    tool_fn: fn(&Value, &ToolContext) -> Result<ToolOutput, ToolOutput>,
) -> Result<ToolOutput, ToolOutput> {
    let owned_input = input.clone();
    let owned_context = blocking_context(context);
    tokio::task::spawn_blocking(move || tool_fn(&owned_input, &owned_context))
        .await
        .map_err(|error| ToolOutput::error(format!("Tool execution failed: {error}")))?
}

/// Run one tool. Unknown tools and bad arguments come back as error results, never as panics.
pub async fn execute(name: &str, input: &Value, context: &ToolContext) -> ToolOutput {
    let result = match name {
        "read_file" => files::read_file(input, context).await,
        "edit_file" => run_blocking_tool(input, context, files::edit_file).await,
        "write_file" => run_blocking_tool(input, context, files::write_file).await,
        "delete_file" => run_blocking_tool(input, context, files::delete_file).await,
        "language_server" => match &context.lsp {
            Some(hub) => crate::lsp::server_tool(hub, input, &context.cancel).await.map_err(ToolOutput::error),
            None => Err(ToolOutput::error("Language servers are not available here.")),
        },
        "code_nav" => match &context.lsp {
            Some(hub) => crate::lsp::navigate(hub, &context.workspace, input, &context.cancel).await.map_err(ToolOutput::error),
            None => Err(ToolOutput::error("Code navigation is not available here. Use `search` instead.")),
        },
        "move_file" => run_blocking_tool(input, context, files::move_file).await,
        "search" => files::search(input, context).await,
        "find_files" => files::find_files(input, context).await,
        "batch_inspect" => files::batch_inspect(input, context).await,
        "run_command" => commands::run_command(input, context).await,
        "command_output" => commands::command_output(input, context).await,
        "command_input" => commands::command_input(input, context).await,
        "list_mcp_resources" => extras::list_mcp_resources(input, context).await,
        "read_mcp_resource" => extras::read_mcp_resource(input, context).await,
        "stop_command" => commands::stop_command(input, context).await,
        "update_todos" => extras::update_todos(input),
        "install_skill" => extras::install_skill(input, context).await,
        "document_runtime" => extras::document_runtime(input, context).await,
        "deliver_files" => run_blocking_tool(input, context, extras::deliver_files).await,
        "generate_image" => extras::generate_image(input, context).await,
        "add_mcp_server" => extras::add_mcp_server(input, context).await,
        "web_search" => match (string_arg(input, "query"), &context.search) {
            (Ok(query), Some(config)) => {
                let found = crate::web::search(config, query, input, &context.cancel).await;
                Ok(ToolOutput { content: found.content, is_error: found.is_error, meta: found.meta, images: Vec::new() })
            }
            (Ok(_), None) => Err(ToolOutput::error("Web search is not set up: it needs a GLM (Zhipu) API key saved in the settings.")),
            (Err(error), _) => Err(error),
        },
        "fetch_url" => match string_arg(input, "url") {
            Ok(url) => {
                let offset = usize::try_from(input["offset"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX);
                let fetched = crate::web::fetch(url, offset, &context.cancel).await;
                Ok(ToolOutput { content: fetched.content, is_error: fetched.is_error, meta: fetched.meta, images: Vec::new() })
            }
            Err(error) => Err(error),
        },
        "load_skill" => string_arg(input, "name").and_then(|name| match crate::skills::load(&context.skills, name) {
            Ok(instructions) => Ok(ToolOutput::ok(instructions, json!({ "skill": name }))),
            Err(error) => Err(ToolOutput::error(error)),
        }),
        // A tool of an MCP server: what it does and returns is the server's.
        other if crate::mcp::split_name(other).is_some() => match &context.mcp {
            Some(hub) => {
                let result = hub.call(other, input, &context.cancel).await;
                let seen = context.vision != Some(false);
                let images: Vec<Image> = if seen { result.images.iter().map(|(media_type, data)| Image { media_type: media_type.clone(), data: data.clone() }).collect() } else { Vec::new() };
                let meta = if result.images.is_empty() { json!({ "mcp": true }) } else { json!({ "mcp": true, "pictures": result.images.len() }) };
                Ok(ToolOutput { content: result.content, is_error: result.is_error, meta, images })
            }
            None => Err(ToolOutput::error(format!("No MCP server is connected, so `{other}` cannot be called."))),
        },
        other => Err(ToolOutput::error(format!("Unknown tool `{other}`."))),
    };
    result.unwrap_or_else(|error| error)
}

#[cfg(all(test, unix))]
mod symlink_boundary_tests {
    use super::*;

    #[test]
    fn inside_workspace_and_in_git_directory_follow_dangling_and_chained_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
        std::fs::create_dir_all(outside.join("dir")).unwrap();

        // Direct dangling absolute symlink -> outside non-existent target.
        let dangling_abs = workspace.join("dangling_abs");
        std::os::unix::fs::symlink(outside.join("not_yet_created.txt"), &dangling_abs).unwrap();
        assert!(!inside_workspace(&workspace, &dangling_abs));

        // Direct dangling relative symlink -> ../outside/rel_new.txt.
        let dangling_rel = workspace.join("dangling_rel");
        std::os::unix::fs::symlink("../outside/rel_new.txt", &dangling_rel).unwrap();
        assert!(!inside_workspace(&workspace, &dangling_rel));

        // Two-hop symlink with `..` after symlinked directory (`sub_link -> /outside/dir`, `escape -> sub_link/../pwn`).
        std::os::unix::fs::symlink(outside.join("dir"), workspace.join("sub_link")).unwrap();
        let chained = workspace.join("escape");
        std::os::unix::fs::symlink("sub_link/../pwn.txt", &chained).unwrap();
        assert!(!inside_workspace(&workspace, &chained));

        // Dangling symlink into `.git/hooks/pre-commit` (target hook not created yet).
        let hook_link = workspace.join("hook_link");
        std::os::unix::fs::symlink(".git/hooks/pre-commit", &hook_link).unwrap();
        assert!(inside_workspace(&workspace, &hook_link));
        assert!(in_git_directory(&workspace, &hook_link));

        // Symlink loop fails closed without infinite recursion.
        std::os::unix::fs::symlink("loop_b", workspace.join("loop_a")).unwrap();
        std::os::unix::fs::symlink("loop_a", workspace.join("loop_b")).unwrap();
        assert!(!inside_workspace(&workspace, &workspace.join("loop_a")));
        assert!(in_git_directory(&workspace, &workspace.join("loop_a")));
    }
}
