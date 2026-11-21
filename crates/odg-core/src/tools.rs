//! The tools: read a file, search file contents, find files by name, edit a
//! file, write a whole file, run a command, read or stop a command left
//! running in the background, and keep a to-do list.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::events::{Choice, Image, Question, QuestionKind, Reply};
use crate::jobs::{Ended, Jobs, Report};

const READ_LINE_LIMIT: usize = 2000;
pub(crate) const READ_FILE_BYTES: u64 = 10 * 1024 * 1024;
const LINE_CHARS: usize = 2000;
/// Every result is replayed in every later request of the session, so one
/// read must stay far below the context window.
const READ_OUTPUT_CHARS: usize = 100_000;
const SEARCH_RESULTS: usize = 200;
const FIND_RESULTS: usize = 500;
const TODO_ITEMS: usize = 50;
/// The states a to-do item can be in.
pub const TODO_STATES: &[&str] = &["pending", "in_progress", "done"];
const SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const COMMAND_OUTPUT_CHARS: usize = 30_000;
const COMMAND_CAPTURE_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT_SECONDS: u64 = 120;
const COMMAND_TIMEOUT_LIMIT_SECONDS: u64 = 600;
/// The longest `command_output` waits for a background command to print something.
const OUTPUT_WAIT_LIMIT_SECONDS: u64 = 60;
const DIFF_LINES: usize = 400;

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

/// The shell `run_command` uses on this machine: `(program, arguments before the command, name for the model)`.
pub fn shell() -> (&'static str, &'static [&'static str], &'static str) {
    if cfg!(windows) {
        let has_pwsh = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).any(|directory| directory.join("pwsh.exe").is_file()))
            .unwrap_or(false);
        if has_pwsh {
            ("pwsh.exe", &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"], "PowerShell 7")
        } else {
            (
                "powershell.exe",
                &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
                "Windows PowerShell 5.1 (chain commands with `;`, not `&&`)",
            )
        }
    } else {
        ("sh", &["-c"], "sh")
    }
}

/// Tool definitions sent to the model.
pub fn specs() -> Vec<Value> {
    let (_, _, shell_name) = shell();
    vec![
        json!({
            "name": "read_file",
            "description": "Read a text file, or the text of a document: a PDF, Word, Excel, or PowerPoint file. Returns numbered lines. Read a file before editing it. One read returns at most 2000 lines and 100,000 characters; use offset and limit to read the rest. A PNG, JPEG, GIF, or WebP picture is shown to you as a picture, when the model at work can see pictures: use it to look at a screenshot, a chart you drew, or a design to follow.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "offset": { "type": "integer", "description": "First line to return, starting at 1. Default 1." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return. Default 2000." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search",
            "description": "Search file contents with a regular expression, skipping files that .gitignore excludes. Returns `path:line: text` for each match. Use it to find where something is defined or used before reading whole files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression (Rust regex syntax)." },
                    "path": { "type": "string", "description": "Directory or file to search. Default: the workspace." },
                    "glob": { "type": "string", "description": "Only search files whose path matches this glob, for example `**/*.rs`." }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "find_files",
            "description": "List a directory, or find files by name. Without `glob`, lists what `path` holds, one level deep, with a trailing `/` on directories. With `glob`, returns every file below `path` whose path matches, for example `**/*.rs` or `**/test_*.py`, skipping what .gitignore excludes. Use it to learn a project's layout or to locate a file whose name you know; use `search` to look inside files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list or search below. Default: the workspace." },
                    "glob": { "type": "string", "description": "Glob the file's path, relative to the workspace, must match." }
                }
            }
        }),
        json!({
            "name": "edit_file",
            "description": "Replace an exact piece of text in a file, or create a file. `old_string` must match the file exactly and occur once unless `replace_all` is true. To create a new file, pass an empty `old_string` and the full contents as `new_string`. To make several changes to one file in one call, pass `edits` instead of `old_string` and `new_string`: they apply in order, each to the result of the one before, and nothing is written unless all of them apply.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "old_string": { "type": "string", "description": "Exact text to replace. Empty to create a new file." },
                    "new_string": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." },
                    "edits": {
                        "type": "array",
                        "description": "Several replacements for this file, instead of `old_string` and `new_string`.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string", "description": "Exact text to replace." },
                                "new_string": { "type": "string", "description": "Replacement text." },
                                "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Write a whole file: create it, or replace everything it holds with `content`. Use it for a new file or for rewriting most of one. To change part of a file use `edit_file`, which sends less and leaves a smaller diff to review.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." },
                    "content": { "type": "string", "description": "The file's full new contents." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "code_nav",
            "description": "Find your way in code with the language's own server, which reads it as the compiler does: where a symbol is defined, where it is used, what implements it, its type and documentation, a file's outline, symbols by name across the project, and a file's errors and warnings. More precise than `search` for code: it tells a function from a variable of the same name, and follows imports and re-exports. For a place in a file, give `path`, `line`, and the `symbol` on that line. Rust, C and C++, TypeScript and JavaScript, Python, and Go are understood when their server is installed; the first question for a language starts its server, which can take a minute to index a large project.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["definition", "references", "implementations", "hover", "symbols", "find_symbol", "diagnostics"], "description": "definition, references, implementations, and hover ask about the symbol at `line`; symbols outlines the file; find_symbol looks up `query` across the project; diagnostics lists the file's errors and warnings." },
                    "path": { "type": "string", "description": "The file, absolute or relative to the workspace. For find_symbol, any file of the language to search in." },
                    "line": { "type": "integer", "description": "The line the symbol is on, from 1." },
                    "symbol": { "type": "string", "description": "The symbol on that line to ask about, as it is written there." },
                    "column": { "type": "integer", "description": "Instead of `symbol`: the column, from 1." },
                    "query": { "type": "string", "description": "For find_symbol: the name, or part of it." }
                },
                "required": ["operation", "path"]
            }
        }),
        json!({
            "name": "language_server",
            "description": "See whether the language server `code_nav` needs for a language is here, and with `install`, download it for this app. Only rust-analyzer can be downloaded so far, and the user is asked first every time; for the other languages it says how the user installs the server. Use it when `code_nav` says a server is missing.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "language": { "type": "string", "enum": ["rust", "c", "typescript", "python", "go"], "description": "The language whose server to look for." },
                    "install": { "type": "boolean", "description": "Download the server if it is not here. Default false." }
                },
                "required": ["language"]
            }
        }),
        json!({
            "name": "delete_file",
            "description": "Delete one file. A copy is kept so the user can undo it with the turn's other changes; use this rather than `rm` or `del` in a command. It takes a file, not a folder.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the workspace." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "move_file",
            "description": "Move or rename one file. The user can undo it with the turn's other changes; use this rather than `mv` or `move` in a command. It will not replace a file already at `to`, and it takes a file, not a folder. Folders it needs at `to` are created.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file to move, absolute or relative to the workspace." },
                    "to": { "type": "string", "description": "Where it goes, with its name there, absolute or relative to the workspace." }
                },
                "required": ["path", "to"]
            }
        }),
        json!({
            "name": "run_command",
            "description": format!("Run a shell command in the workspace and return its output and exit code. The shell is {shell_name}. The user may have to approve the command before it runs. Use it to build, test, and inspect the project, not to read or edit files. For a command that does not end by itself (a dev server, a watcher) or runs for many minutes, set `background`: it then returns a job id at once, `command_output` reads what the job prints, and `stop_command` ends it."),
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." },
                    "timeout_seconds": { "type": "integer", "description": "Stop the command after this many seconds. Default 120, maximum 600. Ignored with `background`." },
                    "background": { "type": "boolean", "description": "Leave the command running and return at once. Default false." },
                    "unsandboxed": { "type": "boolean", "description": "Only matters when the user has the command sandbox on, in which a command can write only under the workspace and the temporary directories. Set it for a command that must write elsewhere, such as a package manager's cache; the user is then asked every time. Default false." }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "command_output",
            "description": "Read what a background command has printed since you last read it, and whether it is still running. With `wait_seconds` it waits that long for the command to print something or end, which beats asking again and again.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "The job id `run_command` returned, like `b1`." },
                    "wait_seconds": { "type": "integer", "description": "Wait up to this many seconds for new output. Default 0, maximum 60." }
                },
                "required": ["job"]
            }
        }),
        json!({
            "name": "command_input",
            "description": "Type into a background command: write `text` to its standard input, as a line ended by Enter unless `newline` is false, or close its input with `close`. Then returns what it printed in reply, waiting up to `wait_seconds` (default 2) for it. Use it to answer a prompt (`y`, a name, a choice), or to feed a program that reads its input, started with `run_command` and `background`. Its input is a pipe, not a terminal: a program that only works in a terminal (a menu moved with arrow keys, a password prompt) will not take it; give such a program its answers as options or a file instead. The user is asked before each input, which can make the program do anything it does.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "The job id `run_command` returned, like `b1`." },
                    "text": { "type": "string", "description": "What to type. Default: nothing, for `close` alone." },
                    "newline": { "type": "boolean", "description": "End the text with a newline, as pressing Enter does. Default true." },
                    "close": { "type": "boolean", "description": "Close its input after the text: the end of input, as Ctrl-D gives in a terminal. Default false." },
                    "wait_seconds": { "type": "integer", "description": "Wait up to this many seconds for it to print something. Default 2, maximum 60." }
                },
                "required": ["job"]
            }
        }),
        json!({
            "name": "fetch_url",
            "description": "Get a web page and return its text: documentation, an issue, a raw file, a document. HTML comes back as its words, without markup; a PDF, Word, Excel, or PowerPoint file as the text taken out of it. The user may have to approve the site first. A long page is cut; pass `offset` to read on. It reads the one address it is given.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The http or https address." },
                    "offset": { "type": "integer", "description": "Character to start from, for reading on in a long page. Default 0." }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "load_skill",
            "description": "Read a skill's instructions. Skills are how-tos for particular kinds of work; the ones on offer are listed in your instructions by name and description. When a task matches one, load it before you start and follow it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The skill's name, as listed." }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "update_todos",
            "description": "Keep a to-do list for the task, which the user sees beside the conversation. Use it when the work has several steps: write the list before starting, then send it again whenever an item's state changes, marking the item you are on `in_progress` and finished ones `done`. Each call replaces the whole list. Skip it for a task of one or two steps.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The whole list, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "What is to be done, in a few words." },
                                "status": { "type": "string", "enum": TODO_STATES }
                            },
                            "required": ["text", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }
        }),
        json!({
            "name": "install_skill",
            "description": "Install a skill for OmniDoppelganger: a folder holding SKILL.md, from a folder on this computer or from a Git repository (a GitHub address may point into one: https://github.com/<owner>/<repo>/tree/<branch>/<folder>). The user is asked first. With `scope` project it goes in this workspace's .odg/skills, for this project; with user, in the user's skills directory, for every session. It can be read with `load_skill` at once, and is listed from the next turn. To make a new skill instead, write its SKILL.md yourself in one of those places.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "A folder on this computer (absolute or relative to the workspace), or a Git repository's address." },
                    "path": { "type": "string", "description": "The folder within `source` that holds SKILL.md, when it is not at the top. A source holding several skills names them when this is missing." },
                    "scope": { "type": "string", "enum": ["project", "user"], "description": "Default project." },
                    "replace": { "type": "boolean", "description": "Put it in place of an installed skill of the same name. Default false." }
                },
                "required": ["source"]
            }
        }),
        json!({
            "name": "document_runtime",
            "description": "The Python to make and read Word, Excel, PowerPoint and PDF files, and to draw charts and pictures, with: OmniDoppelganger's own, apart from the system's, with python-docx, openpyxl, XlsxWriter, python-pptx, reportlab, pypdf, pandas, Pillow, matplotlib, qrcode and resvg_py (SVG to PNG). Returns the interpreter's absolute path, to run your scripts with through run_command, and where LibreOffice is if it is installed. Not installed yet, it says so: then call it with `install` true, which asks the user first and downloads about 90 MB once. Never install these libraries into another Python, and never run pip on this one.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "install": { "type": "boolean", "description": "Download and set it up if it is not there. The user is asked first. Default false." }
                }
            }
        }),
        json!({
            "name": "deliver_files",
            "description": "Hand the user files you made other than with write_file or edit_file, such as a document a script wrote: they are shown under this step, each with a way to open it and to its folder, and a picture as the picture. Call it once the files are finished and checked.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "The files, absolute or relative to the workspace." }
                },
                "required": ["paths"]
            }
        }),
        json!({
            "name": "add_mcp_server",
            "description": "Add an MCP server to the user's list in OmniDoppelganger (its 法器), as the server's documentation gives its configuration: a program to start (`command`, `args`, `env`) or an address (`url`, `headers`). The user is asked first and sees all of it. It is connected to at once, and this returns its tools, which you are offered from your next step on, each call asking the user. Never edit the list's file yourself, or follow another app's way of adding servers. Put a key or token in `env` or `headers` only when the user gave it to you.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "A short name: letters, digits, `-` and `_`. Its tools are named after it." },
                    "command": { "type": "string", "description": "The program to start, such as npx or uvx." },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Environment variables for the program." },
                    "url": { "type": "string", "description": "The http(s) address of a server reached over the network, instead of a command." },
                    "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Headers sent to the address, such as Authorization." },
                    "replace": { "type": "boolean", "description": "Put it in place of a server of the same name. Default false." }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "stop_command",
            "description": "Stop a background command and the processes it started. Returns what it printed last.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "job": { "type": "string", "description": "The job id `run_command` returned, like `b1`." }
                },
                "required": ["job"]
            }
        }),
    ]
}

/// The definition of `web_search`. It is sent to the model only while a
/// search source is set up, so the model is not offered a tool that cannot work.
pub fn search_spec() -> Value {
    json!({
        "name": "web_search",
        "description": "Search the web and get back the results' titles, addresses, and summaries. Use it for what you cannot know or find in the workspace: current facts, an error message, which page documents something. Then read a result in full with `fetch_url`. Keep the query short and specific, under 70 characters. Each search is a paid call, so do not repeat one.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to search for." },
                "count": { "type": "integer", "description": "How many results to return. Default 8, at most 20." },
                "recency": { "type": "string", "enum": ["oneDay", "oneWeek", "oneMonth", "oneYear"], "description": "Only results this recent. Default: any time." },
                "site": { "type": "string", "description": "Only results from this site, like `docs.rs`." }
            },
            "required": ["query"]
        }
    })
}

/// The definition of `generate_image`. It is sent to the model only while a painter is set up, named in it.
pub fn image_spec(painter: &crate::imagegen::Painter) -> Value {
    json!({
        "name": "generate_image",
        "description": format!("Paint a picture with an image model ({}) and save it in the workspace: what code cannot draw, such as a photo, a realistic rendering, an illustration, a scene, a character, a texture. Charts, diagrams, icons, logos, cards and anything that must be exact or carry much text are drawn with code instead (SVG, or a script with the document runtime), as the `image` skill says. Each picture is a paid call, and the user is asked first. Write the prompt as a full description: what the picture is for, the subject, the style or medium, the composition, the lighting and mood, the colors, and any words it must show, quoted exactly; say what to avoid. You cannot see the picture: the user sees it under this step. Paint one picture per call; for another version, call again with a new path.", painter.name),
        "input_schema": {
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "The full description of the picture." },
                "path": { "type": "string", "description": "Where to save it in the workspace, such as `images/hero.png`. A file that is there is never replaced: choose a new name, such as `hero-v2.png`. The extension is set to the kind of picture that comes back." },
                "shape": { "type": "string", "enum": crate::imagegen::SHAPES, "description": "Default square. landscape is about 3:2, portrait 2:3 (wider on some models)." },
                "quality": { "type": "string", "enum": ["standard", "high"], "description": "Default standard. high takes longer and costs more; use it for a final picture." }
            },
            "required": ["prompt", "path"]
        }
    })
}

/// Paint a picture with the painter set up, and save it at a path in the workspace that holds nothing yet.
async fn generate_image(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let Some(config) = &context.painter else {
        return Err(ToolOutput::error("Painting is not set up: it needs an OpenAI or GLM (Zhipu) API key saved in the settings."));
    };
    let prompt = string_arg(input, "prompt")?.trim();
    if prompt.is_empty() {
        return Err(ToolOutput::error("`prompt` is empty: describe the picture."));
    }
    let shape = input["shape"].as_str().unwrap_or("square");
    if !crate::imagegen::SHAPES.contains(&shape) {
        return Err(ToolOutput::error(format!("`shape` must be one of {}.", crate::imagegen::SHAPES.join(", "))));
    }
    let high = match input["quality"].as_str().unwrap_or("standard") {
        "standard" => false,
        "high" => true,
        _ => return Err(ToolOutput::error("`quality` must be standard or high.")),
    };
    let asked = resolve(&context.workspace, string_arg(input, "path")?);
    if !inside_workspace(&context.workspace, &asked) {
        return Err(ToolOutput::error(format!("{} is outside the workspace: save pictures inside it.", asked.display())));
    }
    // Before anything is paid for: a file that is there stays as it is.
    if asked.exists() {
        let stem = asked.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(ToolOutput::error(format!("{} is there already, and is not replaced: choose a new name, such as `{stem}-v2.png`.", display(&context.workspace, &asked))));
    }
    let painted = crate::imagegen::paint(config, prompt, shape, high, &context.cancel).await.map_err(ToolOutput::error)?;
    // Saved as the kind it is; and, paid for, it is kept under another name rather than lost to one taken since.
    let mut path = asked.with_extension(painted.extension);
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "picture".to_string());
    let mut next = 2;
    while path.exists() {
        path = path.with_file_name(format!("{stem}-{next}.{}", painted.extension));
        next += 1;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot make the folder {}: {error}", parent.display())))?;
    }
    std::fs::write(&path, &painted.bytes).map_err(|error| ToolOutput::error(format!("Cannot save the picture at {}: {error}", path.display())))?;
    let shown = display(&context.workspace, &path);
    let mut content = format!(
        "Painted {shown} ({}, {} KB) with {}. The user sees it under this step. You cannot see it: do not describe it beyond what you asked for; ask the user whether it is what they wanted, and if not, paint again under a new name.",
        painted.size,
        painted.bytes.len().div_ceil(1024),
        config.painter.name
    );
    if path != asked {
        content.push_str(&format!(" It was saved as {shown}, not {}, since that is the kind of picture that came back or the name was taken.", display(&context.workspace, &asked)));
    }
    let meta = json!({ "file": path.to_string_lossy(), "path": shown, "painter": config.painter.name, "model": config.painter.model, "size": painted.size, "prompt": prompt });
    Ok(ToolOutput::ok(content, meta))
}

/// The most subagents one `delegate` call sets off.
pub const SUBAGENTS: usize = 8;

/// The definition of `delegate`. The agent loop runs it, since it starts
/// sessions; a subagent is not offered it, so subagents do not multiply.
pub fn delegate_spec() -> Value {
    json!({
        "name": "delegate",
        "description": format!("Hand parts of the work to subagents. Each task goes to its own subagent, which has the tools you have and works in the same workspace; they all work at the same time, and this call returns once every one has finished, with what each reported. Use it when the work splits into parts that do not depend on each other: looking into several areas of the code at once, or changing files that have nothing to do with each other. A subagent knows nothing of this conversation, so each task must stand on its own: say what to do, where, what is already known, and what to report back. Give tasks that run together different files to change, since they share the workspace. Keep for yourself what is quick, and what depends on something you have yet to learn. At most {SUBAGENTS} tasks a call."),
        "input_schema": {
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "The parts of the work, one subagent each.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "A few words to tell this subagent from the others, shown to the user." },
                            "task": { "type": "string", "description": "Everything the subagent needs to do the part on its own." }
                        },
                        "required": ["name", "task"]
                    }
                }
            },
            "required": ["tasks"]
        }
    })
}

/// The tasks `delegate` was given, checked: `(name, task)` for each.
pub fn delegate_tasks(input: &Value) -> Result<Vec<(String, String)>, String> {
    let tasks = input["tasks"].as_array().filter(|tasks| !tasks.is_empty()).ok_or("`tasks` is required and must be a list of at least one task.")?;
    if tasks.len() > SUBAGENTS {
        return Err(format!("{} tasks are too many for one call; give {SUBAGENTS} or fewer.", tasks.len()));
    }
    tasks
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let task = entry["task"].as_str().map(str::trim).filter(|task| !task.is_empty()).ok_or(format!("Task {} has no `task`.", index + 1))?;
            let name: String = entry["name"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
            let name = if name.is_empty() { format!("subagent {}", index + 1) } else { name };
            Ok((name, task.to_string()))
        })
        .collect()
}

/// The name of the tool that asks the user questions. The agent loop runs it, since it waits on the user.
pub const ASK_USER: &str = "ask_user";
/// The most questions one `ask_user` call asks, and the most options a question offers.
pub const QUESTIONS: usize = 4;
pub const OPTIONS: usize = 6;

/// The definition of `ask_user`. A subagent is not offered it: it reports to the agent that set it off.
pub fn ask_user_spec() -> Value {
    json!({
        "name": ASK_USER,
        "description": format!("Ask the user one or more questions and wait for the answers, shown as a form with one tab per question. Use it when you cannot go on well without the user's decision: choosing between approaches that differ in what the user gets, a preference only they can state, or something missing from the request that you cannot find out yourself. Do not use it for what you can learn by looking, nor to ask leave for something you were already asked to do. Each question is answered by choosing one option (`single`), any number of them (`multiple`), or in words (`text`); a question with options may also be answered in words, so do not add an \"other\" option. Put the option you recommend first, with \"(推荐)\" after its label. Ask everything you need in one call, at most {QUESTIONS} questions with 2 to {OPTIONS} options each. The user may leave a question unanswered or not answer at all, and in auto mode nobody may be there: then decide yourself, taking the cautious reading, and say which you chose."),
        "input_schema": {
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "The questions, in the order they are shown.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": { "type": "string", "description": "A few words naming the question, shown on its tab; at most 12 characters." },
                            "question": { "type": "string", "description": "The whole question, standing on its own, in the language the user writes in." },
                            "kind": { "type": "string", "enum": ["single", "multiple", "text"], "description": "One option, any of them, or an answer in words. Without it, `single` when there are options and `text` when there are none." },
                            "options": {
                                "type": "array",
                                "description": "For `single` and `multiple`: what may be chosen.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "The option in a few words." },
                                        "description": { "type": "string", "description": "What choosing it means, if the label does not say." }
                                    },
                                    "required": ["label"]
                                }
                            }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        }
    })
}

/// The questions `ask_user` was given, checked and tidied: a header for each, a kind, and options
/// that are there and distinct where the kind needs them.
pub fn questions(input: &Value) -> Result<Vec<Question>, String> {
    let given = input["questions"].as_array().filter(|questions| !questions.is_empty()).ok_or("`questions` is required and must be a list of at least one question.")?;
    if given.len() > QUESTIONS {
        return Err(format!("{} questions are too many for one call; ask {QUESTIONS} or fewer.", given.len()));
    }
    let words = |value: &Value| value.as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    given
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let number = index + 1;
            let question = entry["question"].as_str().map(str::trim).filter(|question| !question.is_empty()).ok_or(format!("Question {number} has no `question`."))?.to_string();
            let header: String = words(&entry["header"]).chars().take(12).collect();
            let header = if header.is_empty() { format!("问题 {number}") } else { header };
            let listed = entry["options"].as_array().map(Vec::as_slice).unwrap_or_default();
            let kind = match entry["kind"].as_str() {
                Some("single") => QuestionKind::Single,
                Some("multiple") => QuestionKind::Multiple,
                Some("text") => QuestionKind::Text,
                None if listed.is_empty() => QuestionKind::Text,
                None => QuestionKind::Single,
                Some(other) => return Err(format!("Question {number} has kind `{other}`; give `single`, `multiple`, or `text`.")),
            };
            if kind == QuestionKind::Text {
                return Ok(Question { header, question, kind, options: Vec::new() });
            }
            if !(2..=OPTIONS).contains(&listed.len()) {
                return Err(format!("Question {number} offers {} options; give 2 to {OPTIONS}, or make it a `text` question.", listed.len()));
            }
            let mut options: Vec<Choice> = Vec::new();
            for option in listed {
                let label = words(&option["label"]);
                if label.is_empty() {
                    return Err(format!("An option of question {number} has no `label`."));
                }
                if options.iter().any(|known| known.label == label) {
                    return Err(format!("Question {number} offers `{label}` twice."));
                }
                options.push(Choice { label, description: option["description"].as_str().unwrap_or_default().trim().to_string() });
            }
            Ok(Question { header, question, kind, options })
        })
        .collect()
}

/// What the agent is told came of its questions: each answer under its question, or why there are none.
pub fn answered(questions: &[Question], answers: &[Reply], declined: bool, timed_out: Option<u64>) -> String {
    if let Some(seconds) = timed_out {
        return format!("Nobody answered within {seconds} seconds. Decide yourself, taking the cautious reading, and say in your reply what you chose for each question and why.");
    }
    if declined || answers.iter().all(Reply::is_empty) {
        return "The user chose not to answer and leaves it to you. Decide yourself, and say in your reply what you chose for each question.".to_string();
    }
    let mut told = String::from("The user answered:");
    for (index, question) in questions.iter().enumerate() {
        let reply = answers.get(index).cloned().unwrap_or_default();
        told.push_str(&format!("\n{}. {}\n", index + 1, question.question));
        let text = reply.text.trim();
        let answer = match (reply.chosen.is_empty(), text.is_empty()) {
            (true, true) => "(left unanswered: decide this one yourself, and say what you chose)".to_string(),
            (false, true) => reply.chosen.join("; "),
            (true, false) if question.kind == QuestionKind::Text => text.to_string(),
            (true, false) => format!("None of the options. In their own words: {text}"),
            (false, false) => format!("{}. And in their own words: {text}", reply.chosen.join("; ")),
        };
        told.push_str(&format!("   → {answer}"));
    }
    told
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

/// Where `path` really points: links are followed in the part of the path
/// that exists, and the missing rest is appended unchanged.
fn real_path(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let canonical = loop {
        if let Ok(canonical) = existing.canonicalize() {
            break canonical;
        }
        missing.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?.to_path_buf();
    };
    Some(missing.iter().rev().fold(canonical, |path, name| path.join(name)))
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

fn string_arg<'a>(input: &'a Value, name: &str) -> Result<&'a str, ToolOutput> {
    input[name].as_str().ok_or_else(|| ToolOutput::error(format!("`{name}` is required and must be a string.")))
}

fn clip(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS {
        line.to_string()
    } else {
        format!("{}… [line cut at {LINE_CHARS} characters]", line.chars().take(LINE_CHARS).collect::<String>())
    }
}

/// Run one tool. Unknown tools and bad arguments come back as error results, never as panics.
pub async fn execute(name: &str, input: &Value, context: &ToolContext) -> ToolOutput {
    let result = match name {
        "read_file" => read_file(input, context).await,
        "edit_file" => edit_file(input, context),
        "write_file" => write_file(input, context),
        "delete_file" => delete_file(input, context),
        "language_server" => match &context.lsp {
            Some(hub) => crate::lsp::server_tool(hub, input, &context.cancel).await.map_err(ToolOutput::error),
            None => Err(ToolOutput::error("Language servers are not available here.")),
        },
        "code_nav" => match &context.lsp {
            Some(hub) => crate::lsp::navigate(hub, &context.workspace, input, &context.cancel).await.map_err(ToolOutput::error),
            None => Err(ToolOutput::error("Code navigation is not available here. Use `search` instead.")),
        },
        "move_file" => move_file(input, context),
        "search" => search(input, context).await,
        "find_files" => find_files(input, context).await,
        "run_command" => run_command(input, context).await,
        "command_output" => command_output(input, context).await,
        "command_input" => command_input(input, context).await,
        "stop_command" => stop_command(input, context).await,
        "update_todos" => update_todos(input),
        "install_skill" => install_skill(input, context).await,
        "document_runtime" => document_runtime(input, context).await,
        "deliver_files" => deliver_files(input, context),
        "generate_image" => generate_image(input, context).await,
        "add_mcp_server" => add_mcp_server(input, context).await,
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
                Ok(ToolOutput { content: result.content, is_error: result.is_error, meta: json!({ "mcp": true }), images: Vec::new() })
            }
            None => Err(ToolOutput::error(format!("No MCP server is connected, so `{other}` cannot be called."))),
        },
        other => Err(ToolOutput::error(format!("Unknown tool `{other}`."))),
    };
    result.unwrap_or_else(|error| error)
}

async fn read_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let metadata = std::fs::metadata(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory. Use `find_files` to list it.")));
    }
    // A PDF or an Office file is read for its words, which take a while to take out of a large one: off the thread the agent runs on.
    if let Some(found) = crate::documents::kind_of_file(&path) {
        let kind = found.map_err(|why| ToolOutput::error(format!("{shown} {}.", why.describe())))?;
        if metadata.len() > crate::documents::DOCUMENT_BYTES {
            return Err(ToolOutput::error(format!("{shown} is a document of {} bytes, over the {}-byte limit.", metadata.len(), crate::documents::DOCUMENT_BYTES)));
        }
        let taken = tokio::task::spawn_blocking(move || crate::documents::file_text(&path, kind)).await;
        let read = match taken {
            Ok(Ok(Ok(read))) => read,
            Ok(Ok(Err(why))) => return Err(ToolOutput::error(format!("{shown} {}.", why.describe()))),
            Ok(Err(error)) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
            Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
        };
        let (content, lines, selected) = numbered(&read.text, input, &shown);
        let content = format!("[{shown} is {}]\n{content}", read.describe());
        return Ok(ToolOutput::ok(content, json!({ "path": shown, "lines": lines, "shown": selected, "document": read.kind, "parts": read.parts })));
    }
    let named_picture = crate::events::image_type(&path).is_some();
    if named_picture && metadata.len() > crate::events::IMAGE_FILE_BYTES {
        // Not read whole only to be refused: the picture is said to be too large by its size on disk.
        let megabytes = metadata.len() as f64 / 1_000_000.0;
        return Err(ToolOutput::error(format!(
            "{shown} is a picture of {megabytes:.1} MB, too large to be shown (at most {:.1} MB). Make a smaller copy first, for example with Pillow in the document runtime, and read that.",
            crate::events::IMAGE_FILE_BYTES as f64 / 1_000_000.0
        )));
    }
    if metadata.len() > READ_FILE_BYTES {
        return Err(ToolOutput::error(format!("{shown} is {} bytes, over the {READ_FILE_BYTES}-byte limit.", metadata.len())));
    }
    let bytes = std::fs::read(&path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))?;
    // A picture by its name, or by its first bytes whatever it is called, when they give its size too.
    if named_picture || picture(&bytes).is_some_and(|(_, size)| size.is_some()) {
        return read_picture(&path, &shown, bytes, context);
    }
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err(ToolOutput::error(format!("{shown} is a binary file.")));
    }
    let (content, lines, selected) = numbered(&String::from_utf8_lossy(&bytes), input, &shown);
    Ok(ToolOutput::ok(content, json!({ "path": shown, "lines": lines, "shown": selected })))
}

/// Install a skill from a folder or a Git repository; see [`crate::skills::fetch`].
async fn install_skill(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let source = string_arg(input, "source")?.trim().to_string();
    let within = input["path"].as_str().map(str::trim).filter(|path| !path.is_empty()).map(str::to_string);
    let replace = input["replace"].as_bool().unwrap_or(false);
    let project = context.workspace.join(".odg").join("skills");
    let user = input["scope"] == "user";
    let into = if user {
        context.skills.iter().find(|root| **root != project).cloned().ok_or_else(|| ToolOutput::error("There is no skills directory for every session here: install it for this project instead."))?
    } else {
        project
    };
    let local = resolve(&context.workspace, &source);
    let skill = crate::skills::fetch(&source, local, within.as_deref(), &into, replace, &context.cancel).await.map_err(ToolOutput::error)?;
    let directory = display(&context.workspace, &skill.directory);
    Ok(ToolOutput::ok(
        format!("Installed the skill `{}` ({}) in {directory}. Read it now with load_skill if the task calls for it; it is listed in your instructions from the next turn on.", skill.name, skill.description),
        json!({ "skill": skill.name, "directory": directory, "scope": if user { "user" } else { "project" } }),
    ))
}

/// The document runtime's interpreter, and LibreOffice's; or, asked to, set the runtime up. See [`crate::runtime`].
async fn document_runtime(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let Some(root) = &context.runtime else {
        return Err(ToolOutput::error("There is no document runtime here: use a Python the user has, and ask the user before installing anything into it."));
    };
    let status = crate::runtime::status(root);
    let python = match status {
        crate::runtime::Status::Ready { python, .. } => python,
        // Its Python is there and its libraries are behind: what is missing is installed now, without asking.
        crate::runtime::Status::Stale { .. } => crate::runtime::install(root, &|_| {}, &context.cancel).await.map_err(ToolOutput::error)?,
        crate::runtime::Status::Missing { .. } if input["install"] == true => crate::runtime::install(root, &|_| {}, &context.cancel).await.map_err(ToolOutput::error)?,
        crate::runtime::Status::Missing { outdated } => {
            return Ok(ToolOutput::ok(
                format!(
                    "The document runtime is {}. Call document_runtime with `install` true to set it up: the user is asked first, and about {} MB is downloaded once.",
                    if outdated { "on another version of Python and must be set up again" } else { "not installed" },
                    crate::runtime::DOWNLOAD_MB
                ),
                json!({ "state": "missing" }),
            ));
        }
    };
    let libreoffice = crate::runtime::libreoffice();
    let libraries: Vec<&str> = crate::runtime::MODULES.iter().map(|(_, what)| *what).collect();
    let shown = |path: &Path| path.to_string_lossy().into_owned();
    // How a program at a quoted path is run in this machine's shell: PowerShell takes a quoted string for a value
    // unless it is called with `&`.
    let call = |path: &Path| match crate::safety::Dialect::of_this_machine() {
        crate::safety::Dialect::PowerShell => format!("& \"{}\"", shown(path)),
        crate::safety::Dialect::Posix => format!("\"{}\"", shown(path)),
    };
    Ok(ToolOutput::ok(
        format!(
            "Python {} for documents: {}\nRun scripts with it through run_command, exactly so: {} make_report.py\nLibraries: {}.\nLibreOffice: {}",
            crate::runtime::PYTHON_VERSION,
            shown(&python),
            call(&python),
            libraries.join("; "),
            libreoffice.as_deref().map_or_else(
                || "not installed. Skip the steps that need it, and say so; do not install it.".to_string(),
                |path| format!("{} (run it headless, exactly so: {} --headless --convert-to pdf --outdir <folder> <file>)", shown(path), call(path))
            )
        ),
        json!({ "state": "ready", "python": shown(&python), "libreoffice": libreoffice.as_deref().map(shown) }),
    ))
}

/// The files a step hands the user: each must be there, and is named by its absolute path.
fn deliver_files(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let paths: Vec<&str> = input["paths"].as_array().map(|paths| paths.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if paths.is_empty() {
        return Err(ToolOutput::error("`paths` names no file."));
    }
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        let file = resolve(&context.workspace, path);
        if file.is_file() { files.push(file.to_string_lossy().into_owned()) } else { missing.push(path.to_string()) }
    }
    if !missing.is_empty() {
        return Err(ToolOutput::error(format!("Not files that are there: {}. Nothing was handed over; deliver them once they are made.", missing.join(", "))));
    }
    let names: Vec<String> = files.iter().map(|file| display(&context.workspace, Path::new(file))).collect();
    Ok(ToolOutput::ok(format!("Handed to the user: {}. They can open each from here.", names.join(", ")), json!({ "files": files })))
}

/// Add an MCP server to the user's list and connect to it; see [`crate::mcp::McpHub::add`].
async fn add_mcp_server(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let Some(hub) = &context.mcp else {
        return Err(ToolOutput::error("MCP servers cannot be added here."));
    };
    let name = string_arg(input, "name")?.trim();
    let text = |field: &str| input[field].as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    // Values of `env` and `headers` are strings to the server; a number or a flag given is written as its text.
    let pairs = |field: &str| -> std::collections::BTreeMap<String, String> {
        input[field].as_object().map(|object| object.iter().map(|(key, value)| (key.clone(), value.as_str().map_or_else(|| value.to_string(), str::to_string))).collect()).unwrap_or_default()
    };
    let args: Vec<String> = input["args"].as_array().map(|args| args.iter().map(|arg| arg.as_str().map_or_else(|| arg.to_string(), str::to_string)).collect()).unwrap_or_default();
    let server = crate::mcp::ServerConfig { command: text("command"), args, env: pairs("env"), url: text("url"), headers: pairs("headers"), disabled: false };
    hub.add(name, &server, input["replace"].as_bool().unwrap_or(false)).map_err(|why| ToolOutput::error(format!("The server was not added: {why}.")))?;
    let status = hub.settled(name, crate::mcp::START_TIMEOUT + Duration::from_secs(5), &context.cancel).await;
    let meta = |state: &str, tools: &[String]| json!({ "server": name, "state": state, "tools": tools });
    match status {
        Some(status) if status.state == "ready" => Ok(ToolOutput::ok(
            format!(
                "Added `{name}` to the user's MCP servers, and it is connected. Its tools, offered from your next step on as mcp__{name}__<tool>, each call asking the user first: {}.",
                if status.tools.is_empty() { "none".to_string() } else { status.tools.join(", ") }
            ),
            meta("ready", &status.tools),
        )),
        Some(status) if status.state == "failed" => Err(ToolOutput {
            content: format!(
                "`{name}` was added to the list, but it did not connect: {}. Mend the entry and add it again with `replace`, or tell the user, who can switch it off on the 法器 page.",
                status.error.unwrap_or_default()
            ),
            is_error: true,
            meta: meta("failed", &[]),
            images: Vec::new(),
        }),
        _ => Ok(ToolOutput::ok(format!("Added `{name}` to the user's MCP servers; it is still connecting. Its tools are offered once it is."), meta("connecting", &[]))),
    }
}

/// The lines of `text` that `read_file`'s offset and limit ask for, each with
/// its number, and a note of how to read on when there are more: with how
/// many lines there are in all, and how many are shown.
fn numbered(text: &str, input: &Value, shown: &str) -> (String, usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"].as_u64().map(|limit| limit as usize).unwrap_or(READ_LINE_LIMIT).clamp(1, READ_LINE_LIMIT);
    let mut selected: Vec<String> = Vec::new();
    let mut chars = 0usize;
    let mut capped = false;
    for (index, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let numbered = format!("{:>6}\t{}", index + 1, clip(line));
        let size = numbered.chars().count() + 1;
        // The first line always fits: a line is clipped far below the cap.
        if !selected.is_empty() && chars + size > READ_OUTPUT_CHARS {
            capped = true;
            break;
        }
        chars += size;
        selected.push(numbered);
    }
    let mut content = selected.join("\n");
    let last = offset - 1 + selected.len();
    if selected.is_empty() {
        content = format!("{shown} has {} lines; nothing at line {offset}.", lines.len());
    } else if capped {
        content.push_str(&format!(
            "\n[showing lines {offset}-{last} of {}: output stops at {READ_OUTPUT_CHARS} characters; pass offset {} to read on]",
            lines.len(),
            last + 1
        ));
    } else if last < lines.len() {
        content.push_str(&format!("\n[showing lines {offset}-{last} of {}; pass offset to read more]", lines.len()));
    }
    (content, lines.len(), selected.len())
}

/// The list `update_todos` was given, checked: `(text, status)` per item.
pub fn todo_items(input: &Value) -> Result<Vec<(String, String)>, String> {
    let todos = input["todos"].as_array().ok_or("`todos` is required and must be a list.")?;
    if todos.len() > TODO_ITEMS {
        return Err(format!("The list has {} items; keep it to {TODO_ITEMS} or fewer.", todos.len()));
    }
    todos
        .iter()
        .enumerate()
        .map(|(index, todo)| {
            let text = todo["text"].as_str().map(str::trim).filter(|text| !text.is_empty()).ok_or(format!("Item {} has no `text`.", index + 1))?;
            let status = todo["status"].as_str().filter(|status| TODO_STATES.contains(status)).ok_or(format!("Item {}: `status` must be one of {}.", index + 1, TODO_STATES.join(", ")))?;
            Ok((text.to_string(), status.to_string()))
        })
        .collect()
}

fn update_todos(input: &Value) -> Result<ToolOutput, ToolOutput> {
    let items = todo_items(input).map_err(ToolOutput::error)?;
    let done = items.iter().filter(|(_, status)| status == "done").count();
    let todos: Vec<Value> = items.iter().map(|(text, status)| json!({ "text": text, "status": status })).collect();
    Ok(ToolOutput::ok(format!("The list has {} items, {done} done.", items.len()), json!({ "todos": todos })))
}

/// Whether a tool changes files, and so is decided, snapshotted, and undone like an edit.
pub fn edits_files(tool: &str) -> bool {
    matches!(tool, "edit_file" | "write_file" | "delete_file" | "move_file")
}

/// The largest file `delete_file` and `move_file` take: a copy of it is kept for undoing.
const UNDO_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// A file that is there to delete or move, read whole to keep a copy for undoing. Not a folder or a link,
/// whose undoing a copy of bytes would not do.
fn whole_file(path: &Path, shown: &str, tool: &str) -> Result<Vec<u8>, ToolOutput> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(ToolOutput::error(format!("{shown} does not exist."))),
        Err(error) => return Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    };
    if metadata.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a folder; `{tool}` takes one file. Do the files in it one by one, or use run_command.")));
    }
    if metadata.file_type().is_symlink() {
        return Err(ToolOutput::error(format!("{shown} is a link; `{tool}` takes a file. Use run_command for a link.")));
    }
    if metadata.len() > UNDO_FILE_BYTES {
        let megabytes = metadata.len().div_ceil(1024 * 1024);
        return Err(ToolOutput::error(format!("{shown} is {megabytes} MB, too large to keep a copy of for undoing (at most {} MB). Use run_command if it must be done.", UNDO_FILE_BYTES / 1024 / 1024)));
    }
    std::fs::read(path).map_err(|error| ToolOutput::error(format!("Cannot read {shown}: {error}")))
}

/// Keep `bytes` for undoing, when the session keeps snapshots: the snapshot's name.
fn keep(context: &ToolContext, bytes: &[u8], shown: &str) -> Result<Option<String>, ToolOutput> {
    match &context.snapshots {
        Some(directory) => crate::undo::save(directory, bytes).map(Some).map_err(|error| ToolOutput::error(format!("Cannot keep a copy of {shown} for undoing: {error}"))),
        None => Ok(None),
    }
}

/// What undoing needs of one file a tool changed: where it is, what it held (a snapshot; none when it was not
/// there), and what it holds now (a fingerprint; none when it is gone). See [`crate::undo::plan`].
fn change(path: &Path, shown: &str, before: Option<String>, after: Option<&[u8]>) -> Value {
    json!({ "file": path.to_string_lossy(), "path": shown, "before": before, "after": after.map(crate::undo::fingerprint) })
}

fn delete_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let bytes = whole_file(&path, &shown, "delete_file")?;
    // The copy comes first: what could not be undone is not done.
    let before = keep(context, &bytes, &shown)?;
    std::fs::remove_file(&path).map_err(|error| ToolOutput::error(format!("Cannot delete {shown}: {error}")))?;
    let mut meta = json!({ "path": shown, "deleted": true, "bytes": bytes.len() });
    // A text file shows what went, as a change that takes every line out.
    let text = String::from_utf8(bytes).ok().filter(|text| !text.contains('\0'));
    let summary = match text {
        Some(text) => {
            let diff = line_diff(&text.replace("\r\n", "\n"), "");
            meta["removed"] = json!(diff.removed);
            meta["hunk"] = json!(diff.hunk);
            format!("Deleted {shown} ({} lines).", diff.removed)
        }
        None => format!("Deleted {shown} ({} bytes).", meta["bytes"]),
    };
    if context.snapshots.is_some() {
        meta["changes"] = json!([change(&path, &shown, before, None)]);
    }
    Ok(ToolOutput::ok(summary, meta))
}

fn move_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let from = resolve(&context.workspace, string_arg(input, "path")?);
    let to = resolve(&context.workspace, string_arg(input, "to")?);
    let (shown_from, shown_to) = (display(&context.workspace, &from), display(&context.workspace, &to));
    if from == to {
        return Err(ToolOutput::error(format!("{shown_from} is already there.")));
    }
    let bytes = whole_file(&from, &shown_from, "move_file")?;
    // Only a change of case may find the file itself at `to`, on a file system that ignores case.
    let recased = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
    match std::fs::symlink_metadata(&to) {
        Ok(_) if !recased => {
            return Err(ToolOutput::error(format!("{shown_to} already exists. Delete it first with delete_file if it is to be replaced, or choose another name.")));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ToolOutput::error(format!("Cannot look at {shown_to}: {error}"))),
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let before = keep(context, &bytes, &shown_from)?;
    if std::fs::rename(&from, &to).is_err() {
        // Across volumes a file cannot be renamed: it is written there and taken away here.
        crate::fsutil::write_atomically(&to, &bytes).map_err(|error| ToolOutput::error(format!("Cannot write {shown_to}: {error}")))?;
        if let Err(error) = std::fs::remove_file(&from) {
            let _ = std::fs::remove_file(&to);
            return Err(ToolOutput::error(format!("Cannot move {shown_from}: {error}")));
        }
    }
    let mut meta = json!({ "path": shown_from, "to": shown_to, "moved": true, "bytes": bytes.len() });
    if context.snapshots.is_some() {
        // Where it is now, to be opened; and for undoing, the file that came and the file that went.
        meta["file"] = json!(to.to_string_lossy());
        meta["changes"] = json!([change(&to, &shown_to, None, Some(&bytes)), change(&from, &shown_from, before, None)]);
    }
    Ok(ToolOutput::ok(format!("Moved {shown_from} to {shown_to}."), meta))
}

/// What a picture's first bytes say it is, whatever its name says: its type and, where its header gives them,
/// its width and height. `None` for anything but PNG, JPEG, GIF, and WebP.
pub(crate) fn picture(bytes: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    let u16_be = |at: usize| bytes.get(at..at + 2).map(|two| u32::from(u16::from_be_bytes([two[0], two[1]])));
    let u16_le = |at: usize| bytes.get(at..at + 2).map(|two| u32::from(u16::from_le_bytes([two[0], two[1]])));
    let u24_le = |at: usize| bytes.get(at..at + 3).map(|three| u32::from(three[0]) | u32::from(three[1]) << 8 | u32::from(three[2]) << 16);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let size = bytes.get(16..24).map(|header| (u32::from_be_bytes([header[0], header[1], header[2], header[3]]), u32::from_be_bytes([header[4], header[5], header[6], header[7]])));
        return Some(("image/png", size));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("image/gif", u16_le(6).zip(u16_le(8))));
    }
    if bytes.len() >= 16 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let size = match &bytes[12..16] {
            b"VP8 " => u16_le(26).zip(u16_le(28)).map(|(width, height)| (width & 0x3fff, height & 0x3fff)),
            b"VP8L" => bytes.get(21..25).map(|bits| {
                let [b0, b1, b2, b3] = [bits[0], bits[1], bits[2], bits[3]].map(u32::from);
                (1 + (b0 | (b1 & 0x3f) << 8), 1 + (b1 >> 6 | b2 << 2 | (b3 & 0x0f) << 10))
            }),
            b"VP8X" => u24_le(24).zip(u24_le(27)).map(|(width, height)| (width + 1, height + 1)),
            _ => None,
        };
        return Some(("image/webp", size));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        // The size is in the first start-of-frame segment; the segments before it are stepped over by their lengths.
        let mut at = 2;
        let mut size = None;
        while at + 4 <= bytes.len() && bytes[at] == 0xff {
            let marker = bytes[at + 1];
            if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                size = u16_be(at + 7).zip(u16_be(at + 5));
                break;
            }
            match marker {
                0xff => at += 1,
                0x01 | 0xd0..=0xd9 => at += 2,
                _ => at += 2 + u16_be(at + 2)? as usize,
            }
        }
        return Some(("image/jpeg", size));
    }
    None
}

/// A picture `read_file` was asked for: shown to the model, when it can see pictures, with what it is.
fn read_picture(path: &Path, shown: &str, bytes: Vec<u8>, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let Some((media_type, size)) = picture(&bytes) else {
        return Err(ToolOutput::error(format!("{shown} is named as a picture but is not a PNG, JPEG, GIF, or WebP one.")));
    };
    let kind = media_type.trim_start_matches("image/").to_uppercase();
    let dimensions = size.map(|(width, height)| format!(", {width}×{height}")).unwrap_or_default();
    let described = format!("{shown} is a {kind} picture{dimensions}, {} KB", bytes.len().div_ceil(1024));
    if context.vision == Some(false) {
        return Err(ToolOutput::error(format!("{described}. The model at work cannot see pictures: ask the user what it shows, or to switch to a model that can.")));
    }
    if bytes.len() as u64 > crate::events::IMAGE_FILE_BYTES {
        let most = crate::events::IMAGE_FILE_BYTES as f64 / 1_000_000.0;
        return Err(ToolOutput::error(format!("{described}: too large to be shown, which takes at most {most:.1} MB. Make a smaller copy first, for example with Pillow in the document runtime, and read that.")));
    }
    let (width, height) = size.unzip();
    let meta = json!({ "path": shown, "file": path.to_string_lossy(), "picture": media_type, "bytes": bytes.len(), "width": width, "height": height });
    let image = Image { media_type: media_type.to_string(), data: crate::events::base64(&bytes) };
    Ok(ToolOutput { content: format!("{described}. It is shown to you here."), is_error: false, meta, images: vec![image] })
}

/// A text file as it is on disk: `None` when it does not exist.
fn read_existing(path: &Path, shown: &str) -> Result<Option<String>, ToolOutput> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes).map_err(|_| ToolOutput::error(format!("{shown} is not UTF-8 text.")))?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ToolOutput::error(format!("Cannot read {shown}: {error}"))),
    }
}

/// Write `after` (LF text) over what the file held, after saving that for
/// undo, and describe the change. A file that used CRLF gets CRLF back.
fn write_change(context: &ToolContext, path: &Path, shown: &str, existing: Option<&str>, after: &str) -> Result<ToolOutput, ToolOutput> {
    let crlf = existing.is_some_and(|text| text.contains("\r\n"));
    let before = existing.map(|text| text.replace("\r\n", "\n"));
    if before.as_deref() == Some(after) {
        return Err(ToolOutput::error(format!("The change leaves {shown} as it is.")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot create {}: {error}", parent.display())))?;
    }
    let written = if crlf { after.replace('\n', "\r\n") } else { after.to_string() };
    // The snapshot comes first: a change that could not be undone is not made.
    let snapshot = match (&context.snapshots, existing) {
        (Some(directory), Some(original)) => {
            Some(crate::undo::save(directory, original.as_bytes()).map_err(|error| ToolOutput::error(format!("Cannot save {shown} before changing it: {error}")))?)
        }
        _ => None,
    };
    crate::fsutil::write_atomically(path, written.as_bytes()).map_err(|error| ToolOutput::error(format!("Cannot write {shown}: {error}")))?;

    let created = before.is_none();
    let diff = line_diff(before.as_deref().unwrap_or(""), after);
    let (added, removed) = (diff.added, diff.removed);
    let summary = if created {
        format!("Created {shown} ({added} lines).")
    } else {
        format!("Changed {shown}: {added} lines added, {removed} lines removed.")
    };
    let mut meta = json!({ "path": shown, "created": created, "added": added, "removed": removed, "hunk": diff.hunk });
    if context.snapshots.is_some() {
        // What undoing needs: where the file is, what it held, and what it holds now.
        meta["file"] = json!(path.to_string_lossy());
        meta["before"] = json!(snapshot);
        meta["after"] = json!(crate::undo::fingerprint(written.as_bytes()));
    }
    Ok(ToolOutput::ok(summary, meta))
}

fn edit_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    // One edit in the call's own fields, or several in `edits`.
    let single = [input.clone()];
    let edits: &[Value] = match input["edits"].as_array() {
        Some(edits) if edits.is_empty() => return Err(ToolOutput::error("`edits` is empty.")),
        Some(edits) => edits,
        None => &single,
    };
    let existing = read_existing(&path, &shown)?;
    // Matching happens on LF text.
    let mut text = existing.as_deref().map(|text| text.replace("\r\n", "\n"));
    for (index, edit) in edits.iter().enumerate() {
        // Which edit failed matters only when there are several.
        let failed = |message: String| ToolOutput::error(if edits.len() > 1 { format!("Edit {} of {}: {message} Nothing was written.", index + 1, edits.len()) } else { message });
        let old = string_arg(edit, "old_string")?.replace("\r\n", "\n");
        let new = string_arg(edit, "new_string")?.replace("\r\n", "\n");
        let replace_all = edit["replace_all"].as_bool().unwrap_or(false);
        text = Some(match (&text, old.is_empty()) {
            (None, true) => new,
            (None, false) => return Err(failed(format!("{shown} does not exist. To create it, pass an empty `old_string`."))),
            (Some(current), true) if current.is_empty() => new,
            (Some(_), true) => return Err(failed(format!("{shown} already exists. Pass the text to replace as `old_string`."))),
            (Some(current), false) => match current.matches(old.as_str()).count() {
                0 => return Err(failed(format!("`old_string` was not found in {shown}. Read the file again and copy the text exactly."))),
                1 => current.replacen(old.as_str(), &new, 1),
                _ if replace_all => current.replace(old.as_str(), &new),
                count => return Err(failed(format!("`old_string` occurs {count} times in {shown}. Include more surrounding text to make it unique, or set `replace_all`."))),
            },
        });
    }
    write_change(context, &path, &shown, existing.as_deref(), &text.unwrap_or_default())
}

fn write_file(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let path = resolve(&context.workspace, string_arg(input, "path")?);
    let shown = display(&context.workspace, &path);
    let content = string_arg(input, "content")?.replace("\r\n", "\n");
    if path.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is a directory.")));
    }
    let existing = read_existing(&path, &shown)?;
    write_change(context, &path, &shown, existing.as_deref(), &content)
}

async fn find_files(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let root = match input["path"].as_str() {
        Some(path) => resolve(&context.workspace, path),
        None => context.workspace.clone(),
    };
    let shown = display(&context.workspace, &root);
    let shown = if shown.is_empty() { ".".to_string() } else { shown };
    if !root.is_dir() {
        return Err(ToolOutput::error(format!("{shown} is not a directory.")));
    }
    let Some(glob) = input["glob"].as_str() else {
        // One level, as it is on disk: what .gitignore excludes is listed too.
        let entries = std::fs::read_dir(&root).map_err(|error| ToolOutput::error(format!("Cannot list {shown}: {error}")))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| format!("{}{}", entry.file_name().to_string_lossy(), if entry.file_type().is_ok_and(|kind| kind.is_dir()) { "/" } else { "" }))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        let total = names.len();
        names.truncate(FIND_RESULTS);
        let mut content = if names.is_empty() { format!("{shown} is empty.") } else { names.join("\n") };
        if total > FIND_RESULTS {
            content.push_str(&format!("\n[showing {FIND_RESULTS} of {total} entries]"));
        }
        return Ok(ToolOutput::ok(content, json!({ "path": shown, "entries": names.len(), "truncated": total > FIND_RESULTS })));
    };
    let matcher = globset::GlobBuilder::new(glob)
        .literal_separator(false)
        .build()
        .map_err(|error| ToolOutput::error(format!("Invalid glob: {error}")))?
        .compile_matcher();
    let workspace = context.workspace.clone();
    let cancel = context.cancel.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut found: Vec<String> = Vec::new();
        let mut truncated = false;
        // Dot files count (`.github/workflows/ci.yml` is a file one looks for); Git's own directory does not.
        let walk = ignore::WalkBuilder::new(&root).hidden(false).filter_entry(|entry| entry.file_name() != ".git").build();
        for entry in walk.filter_map(Result::ok) {
            if cancel.is_cancelled() {
                break;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = display(&workspace, entry.path());
            if matcher.is_match(&path) {
                if found.len() == FIND_RESULTS {
                    truncated = true;
                    break;
                }
                found.push(path);
            }
        }
        found.sort_by_key(|path| path.to_lowercase());
        (found, truncated)
    });
    let (found, truncated) = task.await.map_err(|error| ToolOutput::error(format!("Finding files failed: {error}")))?;
    let mut content = if found.is_empty() { format!("No file below {shown} matches `{glob}`.") } else { found.join("\n") };
    if truncated {
        content.push_str(&format!("\n[stopped at {FIND_RESULTS} files; narrow the glob or the path]"));
    }
    Ok(ToolOutput::ok(content, json!({ "path": shown, "glob": glob, "entries": found.len(), "truncated": truncated })))
}

struct LineDiff {
    added: usize,
    removed: usize,
    /// `{ old_start, new_start, lines: [{ kind: " " | "-" | "+", text }] }`, or null when too long to show.
    hunk: Value,
}

/// The changed span between two texts: common leading and trailing lines are
/// dropped, two lines of context are kept on each side.
fn line_diff(before: &str, after: &str) -> LineDiff {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old[prefix..old.len() - suffix];
    let added = &new[prefix..new.len() - suffix];
    let hunk = if removed.len() + added.len() > DIFF_LINES {
        Value::Null
    } else {
        let context_start = prefix.saturating_sub(2);
        let line = |kind: &str, text: &&str| json!({ "kind": kind, "text": text });
        let mut lines: Vec<Value> = old[context_start..prefix].iter().map(|text| line(" ", text)).collect();
        lines.extend(removed.iter().map(|text| line("-", text)));
        lines.extend(added.iter().map(|text| line("+", text)));
        lines.extend(old[old.len() - suffix..].iter().take(2).map(|text| line(" ", text)));
        json!({ "old_start": context_start + 1, "new_start": context_start + 1, "lines": lines })
    };
    LineDiff { added: added.len(), removed: removed.len(), hunk }
}

async fn search(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let pattern = string_arg(input, "pattern")?.to_string();
    let regex = regex::Regex::new(&pattern).map_err(|error| ToolOutput::error(format!("Invalid regular expression: {error}")))?;
    let root = match input["path"].as_str() {
        Some(path) => resolve(&context.workspace, path),
        None => context.workspace.clone(),
    };
    if !root.exists() {
        return Err(ToolOutput::error(format!("{} does not exist.", display(&context.workspace, &root))));
    }
    let glob = match input["glob"].as_str() {
        Some(glob) => Some(
            globset::GlobBuilder::new(glob)
                .literal_separator(false)
                .build()
                .map_err(|error| ToolOutput::error(format!("Invalid glob: {error}")))?
                .compile_matcher(),
        ),
        None => None,
    };
    let workspace = context.workspace.clone();
    let cancel = context.cancel.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut matches: Vec<String> = Vec::new();
        let mut files = 0usize;
        let mut truncated = false;
        'walk: for entry in ignore::WalkBuilder::new(&root).build().filter_map(Result::ok) {
            if cancel.is_cancelled() {
                break;
            }
            let path = entry.path();
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let shown = display(&workspace, path);
            if glob.as_ref().is_some_and(|glob| !glob.is_match(&shown)) {
                continue;
            }
            if entry.metadata().map(|metadata| metadata.len() > SEARCH_FILE_BYTES).unwrap_or(true) {
                continue;
            }
            // Files that are not UTF-8 text are skipped: they are binaries or use an encoding the model cannot read.
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            let mut matched = false;
            for (index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    if matches.len() == SEARCH_RESULTS {
                        truncated = true;
                        break 'walk;
                    }
                    matched = true;
                    let line: String = line.trim_end().chars().take(300).collect();
                    matches.push(format!("{shown}:{}: {line}", index + 1));
                }
            }
            files += usize::from(matched);
        }
        (matches, files, truncated)
    });
    let (matches, files, truncated) = task.await.map_err(|error| ToolOutput::error(format!("Search failed: {error}")))?;
    let mut content = if matches.is_empty() { format!("No matches for `{pattern}`.") } else { matches.join("\n") };
    if truncated {
        content.push_str(&format!("\n[stopped at {SEARCH_RESULTS} matches; narrow the pattern, path, or glob]"));
    }
    Ok(ToolOutput::ok(content, json!({ "pattern": pattern, "matches": matches.len(), "files": files, "truncated": truncated })))
}

/// Read a pipe to its end, keeping at most `COMMAND_CAPTURE_BYTES`.
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    while let Ok(read) = pipe.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let room = COMMAND_CAPTURE_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
    kept
}

/// Remove terminal escape sequences: colors and cursor moves (CSI), links and
/// titles (OSC), and the two-character escapes. They mean nothing to the
/// model, cost tokens in every later request, and clutter the transcript.
pub(crate) fn strip_ansi(text: &str) -> String {
    static ESCAPES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[ -/]*[0-~])").expect("valid pattern"));
    if text.contains('\u{1b}') { ESCAPES.replace_all(text, "").into_owned() } else { text.to_string() }
}

/// Stop a command and the processes it started.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        // `Child::kill` ends only the shell on Windows; taskkill /T takes its children too.
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status()
            .await;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The shell leads its own process group (see `shell_command`), so this
        // reaches what it started as well: a server it launched does not live on.
        let _ = tokio::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }
    let _ = child.kill().await;
}

/// The shell set up to run `command_line` in the workspace, its output piped;
/// confined to `sandbox` when there is one. Fails when it cannot be confined.
fn shell_command(command_line: &str, workspace: &Path, sandbox: Option<&crate::sandbox::Sandbox>) -> std::io::Result<tokio::process::Command> {
    let (program, arguments, _) = shell();
    let mut command = match sandbox {
        Some(sandbox) => crate::sandbox::command(program, sandbox)?,
        None => tokio::process::Command::new(program),
    };
    command.args(arguments);
    if cfg!(windows) {
        // PowerShell writes pipes in the console code page unless told otherwise,
        // and PowerShell 7 colors tables and redirected stderr even into a pipe.
        command.arg(format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; if ($PSStyle) {{ $PSStyle.OutputRendering = 'PlainText' }}; {command_line}"
        ));
    } else {
        command.arg(command_line);
    }
    // Ask the programs the command runs not to color either; whatever still does is stripped from the output.
    command.env("NO_COLOR", "1");
    command.current_dir(workspace).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    // Its own process group, so stopping the command can stop everything it started.
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}

/// What a failed command printed when it was not let to write somewhere, as the systems that confine say it.
const DENIED: &[&str] = &["permission denied", "operation not permitted", "read-only file system", "eacces", "eperm"];

fn job_arg<'a>(input: &'a Value, context: &'a ToolContext) -> Result<(&'a str, &'a Arc<Jobs>), ToolOutput> {
    let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
    Ok((string_arg(input, "job")?, jobs))
}

/// What a background command printed, cut like a foreground command's output, with how it stands.
fn job_report(id: &str, report: Report) -> ToolOutput {
    let mut output = report.output.trim_end().to_string();
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        // The newest output says the most about where a running command stands.
        let tail: String = output.chars().skip(total - COMMAND_OUTPUT_CHARS).collect();
        output = format!("[... {} earlier characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let state = match report.ended {
        None => "still running".to_string(),
        Some(Ended::Stopped) => "stopped".to_string(),
        Some(Ended::Exited(Some(code))) => format!("exited with code {code}"),
        Some(Ended::Exited(None)) => "ended without an exit code".to_string(),
    };
    let content = format!("{}\n[job {id}: {state}]", if output.is_empty() { "(nothing new)" } else { output.as_str() });
    let exit_code = match report.ended {
        Some(Ended::Exited(code)) => code,
        _ => None,
    };
    let failed = matches!(report.ended, Some(Ended::Exited(code)) if code != Some(0));
    ToolOutput {
        content,
        is_error: failed,
        meta: json!({ "job": id, "command": report.command, "running": report.ended.is_none(), "stopped": matches!(report.ended, Some(Ended::Stopped)).then_some("stopped"), "exit_code": exit_code, "output": output }),
        images: Vec::new(),
    }
}

/// A job id that names nothing: jobs do not outlive the app.
fn no_job(id: &str) -> ToolOutput {
    ToolOutput::error(format!("There is no background command `{id}`. Background commands end when the app closes; start it again if you need it."))
}

async fn command_output(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let wait = Duration::from_secs(input["wait_seconds"].as_u64().unwrap_or(0).min(OUTPUT_WAIT_LIMIT_SECONDS));
    let report = jobs.read(id, wait, &context.cancel).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

async fn command_input(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let close = input["close"].as_bool().unwrap_or(false);
    let mut text = input["text"].as_str().unwrap_or_default().to_string();
    if text.is_empty() && !close {
        return Err(ToolOutput::error("Give `text` to type, or `close` to end its input."));
    }
    if !text.is_empty() && input["newline"].as_bool().unwrap_or(true) && !text.ends_with('\n') {
        text.push('\n');
    }
    jobs.write(id, &text, close).await.map_err(ToolOutput::error)?;
    let wait = Duration::from_secs(input["wait_seconds"].as_u64().unwrap_or(2).min(OUTPUT_WAIT_LIMIT_SECONDS));
    let report = jobs.read(id, wait, &context.cancel).await.ok_or_else(|| no_job(id))?;
    let mut output = job_report(id, report);
    let sent = if close && text.is_empty() { "Closed its input.".to_string() } else { format!("Sent {} characters{}.", text.chars().count(), if close { " and closed its input" } else { "" }) };
    output.content = format!("{sent} What it printed since:\n{}", output.content);
    output.meta["sent"] = json!(text);
    output.meta["closed"] = json!(close);
    Ok(output)
}

async fn stop_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let report = jobs.stop(id).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

async fn run_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let command_line = string_arg(input, "command")?.to_string();
    let timeout = Duration::from_secs(input["timeout_seconds"].as_u64().unwrap_or(COMMAND_TIMEOUT_SECONDS).clamp(1, COMMAND_TIMEOUT_LIMIT_SECONDS));
    let (program, _, _) = shell();
    // The model may ask for a command to run unconfined; the user has then been asked (see `policy::check_in`).
    let sandbox = context.sandbox.as_ref().filter(|_| input["unsandboxed"] != true);
    let mut command = shell_command(&command_line, &context.workspace, sandbox).map_err(|error| ToolOutput::error(format!("Cannot confine the command to the sandbox: {error}")))?;
    if input["background"].as_bool().unwrap_or(false) {
        let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
        // Left open, for `command_input`; a command in the foreground gets none, and a prompt fails at once there.
        command.stdin(Stdio::piped());
        let id = jobs.start(&command_line, command).map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
        let content = format!("Started in the background as job {id}. Read what it prints with `command_output`, type into it with `command_input`, and end it with `stop_command` when it is no longer needed.");
        return Ok(ToolOutput::ok(content, json!({ "command": command_line, "job": id, "background": true, "sandboxed": sandbox.is_some() })));
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
    let stdout = tokio::spawn(capture(child.stdout.take().expect("stdout is piped")));
    let stderr = tokio::spawn(capture(child.stderr.take().expect("stderr is piped")));

    let (status, stopped) = tokio::select! {
        status = child.wait() => (status.ok(), None),
        () = tokio::time::sleep(timeout) => (None, Some("timed out")),
        () = context.cancel.cancelled() => (None, Some("cancelled")),
    };
    if stopped.is_some() {
        kill_tree(&mut child).await;
    }
    // A grandchild can keep the pipes open after the kill, so reading gets a deadline.
    let read = |task| async { tokio::time::timeout(Duration::from_secs(2), task).await.ok().and_then(Result::ok).unwrap_or_default() };
    let (stdout, stderr): (Vec<u8>, Vec<u8>) = (read(stdout).await, read(stderr).await);

    let mut output = strip_ansi(&String::from_utf8_lossy(&stdout).replace("\r\n", "\n")).trim_end().to_string();
    let stderr = strip_ansi(&String::from_utf8_lossy(&stderr).replace("\r\n", "\n"));
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("[stderr]\n{}", stderr.trim_end()));
    }
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        let half = COMMAND_OUTPUT_CHARS / 2;
        let head: String = output.chars().take(half).collect();
        let tail: String = output.chars().skip(total - half).collect();
        output = format!("{head}\n[... {} characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let exit_code = status.and_then(|status| status.code());
    let mut content = if output.is_empty() { "(no output)".to_string() } else { output.clone() };
    match (stopped, exit_code) {
        (Some("timed out"), _) => content.push_str(&format!("\n[stopped after {} seconds: timed out]", timeout.as_secs())),
        (Some(reason), _) => content.push_str(&format!("\n[stopped: {reason}]")),
        (None, Some(0)) => {}
        (None, Some(code)) => content.push_str(&format!("\n[exit code {code}]")),
        (None, None) => content.push_str("\n[ended without an exit code]"),
    }
    // A confined command that was refused a write is told why, and the way out.
    if let Some(sandbox) = sandbox
        && exit_code != Some(0)
        && DENIED.iter().any(|said| output.to_lowercase().contains(said))
    {
        let roots: Vec<String> = sandbox.writable.iter().map(|root| root.display().to_string()).collect();
        content.push_str(&format!(
            "\n[The command ran in the sandbox the user switched on: it could write only under {}. If that is why it failed and it must write elsewhere, run it again with `unsandboxed` set; the user is asked first.]",
            roots.join(", ")
        ));
    }
    Ok(ToolOutput {
        content,
        is_error: stopped.is_some() || exit_code != Some(0),
        meta: json!({
            "command": command_line,
            "exit_code": exit_code,
            "stopped": stopped,
            "duration_ms": started.elapsed().as_millis() as u64,
            "output": output,
            "sandboxed": sandbox.is_some(),
        }),
        images: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (tempfile::TempDir, ToolContext) {
        let directory = tempfile::tempdir().unwrap();
        let context = ToolContext { workspace: directory.path().to_path_buf(), cancel: CancellationToken::new(), snapshots: None, jobs: None, mcp: None, skills: Vec::new(), search: None, sandbox: None, runtime: None, painter: None, vision: Some(true), lsp: None };
        (directory, context)
    }

    #[tokio::test]
    async fn paints_a_picture_into_the_workspace_as_the_kind_it_is_and_never_over_a_file() {
        let (directory, context) = context();
        let unset = execute("generate_image", &json!({ "prompt": "一只白鹤", "path": "crane.png" }), &context).await;
        assert!(unset.is_error && unset.content.contains("API key"), "{}", unset.content);

        let answer = format!(r#"{{"data":[{{"b64_json":"{}"}}]}}"#, crate::events::base64(crate::imagegen::tests::PNG));
        let (address, _sent) = crate::imagegen::tests::serve(vec![(200, "application/json", answer.into_bytes())]);
        let painter = crate::imagegen::painter("gpt-image-2").unwrap();
        let context = ToolContext { painter: Some(crate::imagegen::ImageConfig { painter, url: format!("{address}/v1/images/generations"), key: "sk".into() }), ..context };
        // Asked for as a JPEG, it comes back a PNG and is saved as one, in a folder made for it.
        let painted = execute("generate_image", &json!({ "prompt": "一只白鹤", "path": "images/crane.jpg", "shape": "landscape" }), &context).await;
        assert!(!painted.is_error, "{}", painted.content);
        let saved = directory.path().join("images").join("crane.png");
        assert_eq!(std::fs::read(&saved).unwrap(), crate::imagegen::tests::PNG);
        assert!(painted.content.contains("images/crane.png") && painted.content.contains("1536x1024") && painted.content.contains("saved as"), "{}", painted.content);
        assert_eq!((painted.meta["file"].as_str(), painted.meta["model"].as_str()), (Some(saved.to_string_lossy().as_ref()), Some("gpt-image-2")));

        // A file that is there is not painted over, and nothing is asked for; nor is a picture put outside.
        let taken = execute("generate_image", &json!({ "prompt": "又一只", "path": "images/crane.png" }), &context).await;
        assert!(taken.is_error && taken.content.contains("crane-v2.png"), "{}", taken.content);
        let outside = execute("generate_image", &json!({ "prompt": "x", "path": "../crane.png" }), &context).await;
        assert!(outside.is_error && outside.content.contains("outside the workspace"), "{}", outside.content);
        let shaped = execute("generate_image", &json!({ "prompt": "x", "path": "a.png", "shape": "round" }), &context).await;
        assert!(shaped.is_error && shaped.content.contains("square"), "{}", shaped.content);
    }

    #[tokio::test]
    async fn says_where_the_document_runtime_is_or_how_to_set_it_up_and_hands_over_files() {
        let (directory, context) = context();
        // Not offered here: a Python the user has is to be used, with leave.
        assert!(execute("document_runtime", &json!({}), &context).await.is_error);
        let root = directory.path().join("runtime");
        let context = ToolContext { runtime: Some(root.clone()), ..context };
        let missing = execute("document_runtime", &json!({}), &context).await;
        assert!(!missing.is_error && missing.content.contains("`install` true") && missing.meta["state"] == "missing", "{}", missing.content);
        // Set up as an install leaves it: the interpreter is named, quoted, to run scripts with.
        let python = if cfg!(windows) { root.join("python").join("python.exe") } else { root.join("python").join("bin").join("python3") };
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::write(root.join("runtime.json"), serde_json::to_vec(&crate::runtime::Installed::current("t")).unwrap()).unwrap();
        let ready = execute("document_runtime", &json!({}), &context).await;
        let call = if cfg!(windows) { format!("& \"{}\" make_report.py", python.display()) } else { format!("\"{}\" make_report.py", python.display()) };
        assert!(ready.content.contains(&call) && ready.meta["python"] == python.to_string_lossy().as_ref(), "{}", ready.content);
        assert!(ready.content.contains("LibreOffice:"));

        std::fs::write(directory.path().join("report.docx"), "x").unwrap();
        let handed = execute("deliver_files", &json!({ "paths": ["report.docx"] }), &context).await;
        assert_eq!(handed.meta["files"], json!([directory.path().join("report.docx").to_string_lossy()]));
        let refused = execute("deliver_files", &json!({ "paths": ["report.docx", "missing.pdf"] }), &context).await;
        assert!(refused.is_error && refused.content.contains("missing.pdf"));
        assert!(execute("deliver_files", &json!({ "paths": [] }), &context).await.is_error);
    }

    #[tokio::test]
    async fn installs_a_skill_from_a_folder_or_a_repository() {
        let (directory, mut context) = context();
        let elsewhere = tempfile::tempdir().unwrap();
        let user = elsewhere.path().join("user-skills");
        context.skills = vec![user.clone(), directory.path().join(".odg/skills")];
        // A skill in a folder, for this project.
        let made = elsewhere.path().join("made/release");
        std::fs::create_dir_all(&made).unwrap();
        std::fs::write(made.join("SKILL.md"), "---\nname: release\ndescription: How to cut a release.\n---\nTag it.\n").unwrap();
        let done = execute("install_skill", &json!({ "source": made.to_string_lossy() }), &context).await;
        assert!(!done.is_error && done.content.starts_with("Installed the skill `release` (How to cut a release.)"), "{}", done.content);
        assert_eq!((done.meta["scope"].as_str(), done.meta["directory"].as_str()), (Some("project"), Some(".odg/skills/release")));
        assert!(crate::skills::load(&context.skills, "release").unwrap().starts_with("Tag it."));

        // From a Git repository, a folder in it, for every session.
        let repository = elsewhere.path().join("skills.git");
        std::fs::create_dir_all(repository.join("skills/pdf")).unwrap();
        std::fs::write(repository.join("skills/pdf/SKILL.md"), "---\nname: pdf\ndescription: Work with PDFs.\n---\nSplit them.\n").unwrap();
        for args in [&["init", "--quiet"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "--quiet", "-m", "skills"]] {
            assert!(std::process::Command::new("git").arg("-C").arg(&repository).args(args).status().unwrap().success());
        }
        let cloned = execute("install_skill", &json!({ "source": repository.to_string_lossy(), "path": "skills/pdf", "scope": "user" }), &context).await;
        assert!(!cloned.is_error, "{}", cloned.content);
        assert!(user.join("pdf/SKILL.md").is_file() && !user.join("pdf/.git").exists());
        // Not naming the folder, the repository's skills are named; and nothing is left of the copies fetched.
        let missed = execute("install_skill", &json!({ "source": repository.to_string_lossy() }), &context).await;
        assert!(missed.is_error && missed.content.contains("skills/pdf") && missed.content.contains("`path`"), "{}", missed.content);
        let left = std::fs::read_dir(std::env::temp_dir()).unwrap().filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().starts_with("odg-skill-")).count();
        assert_eq!(left, 0, "a fetched repository was left in the temporary directory");
        // Again, not over what is there.
        assert!(execute("install_skill", &json!({ "source": made.to_string_lossy() }), &context).await.content.contains("replace"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adds_an_mcp_server_and_says_what_it_offers() {
        let (directory, mut context) = context();
        let hub = Arc::new(crate::mcp::McpHub::new(directory.path().join("home/mcp.json"), tokio::runtime::Handle::current()));
        context.mcp = Some(hub.clone());
        // Neither a command nor an address: nothing is added.
        let empty = execute("add_mcp_server", &json!({ "name": "notes" }), &context).await;
        assert!(empty.is_error && empty.content.starts_with("The server was not added"), "{}", empty.content);
        // One that cannot start is added and says why it did not connect.
        let broken = execute("add_mcp_server", &json!({ "name": "broken", "command": "odg-no-such-program" }), &context).await;
        assert!(broken.is_error && broken.content.contains("did not connect") && broken.content.contains("`replace`"), "{}", broken.content);
        assert_eq!(broken.meta["state"], "failed");
        // One that starts is connected, and its tools are named. The stand-in server needs only Node.
        let node = std::process::Command::new("node").arg("--version").output().is_ok_and(|output| output.status.success());
        if node {
            let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/mcp-server.mjs");
            let added = execute("add_mcp_server", &json!({ "name": "notes", "command": "node", "args": [server.to_string_lossy()], "env": { "LEVEL": 2 } }), &context).await;
            assert!(!added.is_error && added.content.contains("mcp__notes__<tool>") && added.content.ends_with(": shout."), "{}", added.content);
            assert_eq!(added.meta["tools"], json!(["shout"]));
            let listed: Value = serde_json::from_str(&std::fs::read_to_string(directory.path().join("home/mcp.json")).unwrap()).unwrap();
            assert_eq!(listed["mcpServers"]["notes"]["env"], json!({ "LEVEL": "2" }));
            assert!(hub.specs().iter().any(|spec| spec["name"] == "mcp__notes__shout"));
        }
        context.mcp = None;
        assert!(execute("add_mcp_server", &json!({ "name": "x", "url": "https://x" }), &context).await.content.contains("cannot be added here"));
    }

    #[tokio::test]
    async fn reads_numbered_lines_with_offset_and_limit() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let all = execute("read_file", &json!({ "path": "a.txt" }), &context).await;
        assert!(!all.is_error);
        assert_eq!(all.content, "     1\tone\n     2\ttwo\n     3\tthree\n     4\tfour");
        assert_eq!(all.meta, json!({ "path": "a.txt", "lines": 4, "shown": 4 }));

        let part = execute("read_file", &json!({ "path": "a.txt", "offset": 2, "limit": 2 }), &context).await;
        assert_eq!(part.content, "     2\ttwo\n     3\tthree\n[showing lines 2-3 of 4; pass offset to read more]");

        assert!(execute("read_file", &json!({ "path": "missing.txt" }), &context).await.is_error);
        assert!(execute("read_file", &json!({}), &context).await.is_error);
        std::fs::write(directory.path().join("b.bin"), [1u8, 0, 2]).unwrap();
        assert!(execute("read_file", &json!({ "path": "b.bin" }), &context).await.content.contains("binary"));
    }

    #[tokio::test]
    async fn reads_the_text_of_a_document_in_numbered_lines() {
        let (directory, context) = context();
        // Named as anything: a PDF is known by its first bytes.
        std::fs::write(directory.path().join("sheet.dat"), crate::pdf::tests::sample(&[&["Pin 1 is VIN", "Pin 2 is GND"], &["Absolute maximum 42 V"]])).unwrap();
        let all = execute("read_file", &json!({ "path": "sheet.dat" }), &context).await;
        assert!(!all.is_error, "{}", all.content);
        assert!(all.content.starts_with("[sheet.dat is a PDF of 2 pages. This is its text"), "{}", all.content);
        assert!(all.content.contains("     1\t[page 1 of 2]\n     2\tPin 1 is VIN") && all.content.contains("\t[page 2 of 2]\n"), "{}", all.content);
        assert_eq!((all.meta["document"].as_str(), all.meta["parts"].as_u64()), (Some("pdf"), Some(2)));
        // Read on from a line, as a text file is: here from where the second page starts.
        let second = all.content.lines().find(|line| line.ends_with("\t[page 2 of 2]")).and_then(|line| line.trim().split('\t').next()?.parse::<u64>().ok()).unwrap();
        let part = execute("read_file", &json!({ "path": "sheet.dat", "offset": second, "limit": 2 }), &context).await;
        assert!(part.content.starts_with("[sheet.dat is a PDF of 2 pages."), "{}", part.content);
        assert!(part.content.ends_with(&format!("{second:>6}\t[page 2 of 2]\n{:>6}\tAbsolute maximum 42 V", second + 1)), "{}", part.content);
        // A PDF without words says so, rather than that it is a binary file.
        std::fs::write(directory.path().join("scan.pdf"), crate::pdf::tests::sample(&[&[]])).unwrap();
        let scan = execute("read_file", &json!({ "path": "scan.pdf" }), &context).await;
        assert!(scan.is_error && scan.content.contains("no text in it"), "{}", scan.content);
        // A Word document, and an old one that is not read, with what would make it readable.
        use crate::documents::tests::{paragraph, word_document};
        std::fs::write(directory.path().join("说明.docx"), word_document(&[paragraph(Some("1"), "安装"), paragraph(None, "先装驱动。")].concat())).unwrap();
        let word = execute("read_file", &json!({ "path": "说明.docx" }), &context).await;
        assert_eq!(word.content, "[说明.docx is a Word document. This is its text: paragraphs in order, headings marked with #, list items with -, and tables as rows of cells split by |. Its pictures, charts, and formatting are not kept.]\n     1\t# 安装\n     2\t先装驱动。");
        std::fs::write(directory.path().join("old.doc"), [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).unwrap();
        let old = execute("read_file", &json!({ "path": "old.doc" }), &context).await;
        assert!(old.is_error && old.content.contains("saved as .docx or .pptx"), "{}", old.content);
    }

    #[tokio::test]
    async fn stops_a_read_of_long_lines_at_the_output_limit() {
        let (directory, context) = context();
        // 2000 lines of 1500 characters: within the line and file limits, but 3 MB of text.
        let line = "x".repeat(1500);
        std::fs::write(directory.path().join("wide.txt"), format!("{line}\n").repeat(2000)).unwrap();
        let read = execute("read_file", &json!({ "path": "wide.txt" }), &context).await;
        assert!(!read.is_error);
        assert!(read.content.chars().count() < READ_OUTPUT_CHARS + 200);
        let shown = read.meta["shown"].as_u64().unwrap();
        assert!(shown > 0 && shown < 2000);
        assert!(read.content.ends_with(&format!("pass offset {} to read on]", shown + 1)), "{}", &read.content[read.content.len() - 200..]);

        let next = execute("read_file", &json!({ "path": "wide.txt", "offset": shown + 1 }), &context).await;
        assert!(next.content.starts_with(&format!("{:>6}\t", shown + 1)));
    }

    #[tokio::test]
    async fn edits_exactly_one_occurrence_and_keeps_crlf() {
        let (directory, context) = context();
        let file = directory.path().join("src").join("lib.rs");
        let created = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "", "new_string": "fn a() {}\nfn b() {}\n" }), &context).await;
        assert!(!created.is_error, "{}", created.content);
        assert_eq!(created.meta["created"], true);

        std::fs::write(&file, "fn a() {}\r\nfn b() {}\r\nfn a() {}\r\n").unwrap();
        let ambiguous = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn a() {}", "new_string": "fn c() {}" }), &context).await;
        assert!(ambiguous.is_error && ambiguous.content.contains("2 times"));
        let missing = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn z()", "new_string": "x" }), &context).await;
        assert!(missing.is_error && missing.content.contains("not found"));

        let edited = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn b() {}\nfn a() {}", "new_string": "fn b() {\n    1;\n}" }), &context).await;
        assert!(!edited.is_error, "{}", edited.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn a() {}\r\nfn b() {\r\n    1;\r\n}\r\n");
        assert_eq!((edited.meta["added"].as_u64(), edited.meta["removed"].as_u64()), (Some(3), Some(2)));
        let kinds: String = edited.meta["hunk"]["lines"].as_array().unwrap().iter().map(|line| line["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, " --+++");

        let all = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn", "new_string": "pub fn", "replace_all": true }), &context).await;
        assert!(!all.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap().matches("pub fn").count(), 2);
    }

    #[tokio::test]
    async fn searches_text_files_and_honours_gitignore_and_glob() {
        let (directory, context) = context();
        let root = directory.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "target\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    read_events();\n}\n").unwrap();
        std::fs::write(root.join("notes.md"), "call read_events here\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "read_events\n").unwrap();

        let found = execute("search", &json!({ "pattern": r"read_events\b" }), &context).await;
        assert!(!found.is_error);
        let mut lines: Vec<&str> = found.content.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines, vec!["notes.md:1: call read_events here", "src/main.rs:2:     read_events();"]);
        assert_eq!(found.meta["files"], 2);

        let rust_only = execute("search", &json!({ "pattern": "read_events", "glob": "**/*.rs" }), &context).await;
        assert_eq!(rust_only.content, "src/main.rs:2:     read_events();");
        assert_eq!(execute("search", &json!({ "pattern": "nothing_like_this" }), &context).await.content, "No matches for `nothing_like_this`.");
        assert!(execute("search", &json!({ "pattern": "(" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn runs_commands_and_reports_exit_code_timeout_and_cancel() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
        let listed = execute("run_command", &json!({ "command": if cfg!(windows) { "Get-ChildItem -Name" } else { "ls" } }), &context).await;
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("marker.txt"));
        assert_eq!(listed.meta["exit_code"], 0);

        let unicode = execute("run_command", &json!({ "command": "echo 分身" }), &context).await;
        assert_eq!(unicode.content.trim(), "分身");

        let failed = execute("run_command", &json!({ "command": "exit 3" }), &context).await;
        assert!(failed.is_error);
        assert!(failed.content.ends_with("[exit code 3]"), "{}", failed.content);

        let sleep = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
        let started = Instant::now();
        let timed_out = execute("run_command", &json!({ "command": sleep, "timeout_seconds": 1 }), &context).await;
        assert!(timed_out.is_error && timed_out.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(15));

        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let cancelled = execute("run_command", &json!({ "command": sleep }), &context).await;
        assert!(cancelled.content.contains("[stopped: cancelled]"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sandboxed_command_is_told_why_a_write_was_refused_and_can_ask_to_leave() {
        if let Err(why) = crate::sandbox::support() {
            eprintln!("not run: {why}");
            return;
        }
        let (directory, context) = context();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("out.txt");
        // Only the workspace: the test's other directory is under the system's temporary one, which a session would allow.
        let context = ToolContext { sandbox: Some(crate::sandbox::Sandbox { writable: vec![directory.path().to_path_buf()] }), jobs: Some(Arc::new(Jobs::default())), ..context };

        let inside = execute("run_command", &json!({ "command": "echo in > in.txt && cat in.txt" }), &context).await;
        assert!(!inside.is_error && inside.content == "in" && inside.meta["sandboxed"] == true, "{}", inside.content);

        let refused = execute("run_command", &json!({ "command": format!("echo out > {}", target.display()) }), &context).await;
        assert!(refused.is_error && !target.exists(), "{}", refused.content);
        assert!(refused.content.contains("ran in the sandbox") && refused.content.contains("`unsandboxed`") && refused.content.contains(&directory.path().display().to_string()), "{}", refused.content);
        // A command that fails for a reason of its own is not told about the sandbox.
        let failed = execute("run_command", &json!({ "command": "exit 3" }), &context).await;
        assert!(failed.is_error && !failed.content.contains("sandbox"), "{}", failed.content);

        // One left running is confined too.
        let started = execute("run_command", &json!({ "command": format!("echo out > {}; echo done", target.display()), "background": true }), &context).await;
        assert_eq!(started.meta["sandboxed"], true);
        let printed = execute("command_output", &json!({ "job": started.meta["job"], "wait_seconds": 20 }), &context).await;
        assert!(printed.content.contains("done") && !target.exists(), "{}", printed.content);

        // Asked to run unconfined, it does: whether it may is the policy's to decide before this.
        let free = execute("run_command", &json!({ "command": format!("echo out > {}", target.display()), "unsandboxed": true }), &context).await;
        assert!(!free.is_error && free.meta["sandboxed"] == false && target.is_file(), "{}", free.content);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn leaves_a_command_running_reads_what_it_prints_and_stops_it() {
        let (_directory, mut context) = context();
        assert!(execute("run_command", &json!({ "command": "echo x", "background": true }), &context).await.is_error);
        let jobs = Arc::new(Jobs::default());
        context.jobs = Some(jobs.clone());

        // A command that prints, then stays: a server.
        let serve = if cfg!(windows) { "'ready'; Start-Sleep -Seconds 60; 'never'" } else { "echo ready; sleep 60; echo never" };
        let started = execute("run_command", &json!({ "command": serve, "background": true }), &context).await;
        assert!(!started.is_error && started.meta["job"] == "b1" && started.meta["background"] == true, "{}", started.content);
        let read = execute("command_output", &json!({ "job": "b1", "wait_seconds": 30 }), &context).await;
        assert_eq!(read.content, "ready\n[job b1: still running]");
        assert_eq!((read.meta["running"].as_bool(), read.is_error), (Some(true), false));
        // Nothing new: the read returns at once and says so.
        assert_eq!(execute("command_output", &json!({ "job": "b1" }), &context).await.content, "(nothing new)\n[job b1: still running]");
        assert_eq!(jobs.running(), [("b1".to_string(), serve.to_string())]);

        let begun = Instant::now();
        let stopped = execute("stop_command", &json!({ "job": "b1" }), &context).await;
        assert!(stopped.content.ends_with("[job b1: stopped]") && !stopped.content.contains("never"), "{}", stopped.content);
        assert!(begun.elapsed() < Duration::from_secs(20));
        assert!(jobs.running().is_empty());

        // Stopping takes what the shell started with it, not only the shell.
        #[cfg(unix)]
        {
            // The shell starts a child of its own and prints the child's process id.
            let child = execute("run_command", &json!({ "command": "sleep 300 & echo $!; wait", "background": true }), &context).await;
            let id = child.meta["job"].as_str().unwrap().to_string();
            let printed = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
            let pid = printed.content.lines().next().unwrap().trim().to_string();
            // A process that is gone has no state; one that was killed and not yet collected shows Z.
            let alive = || {
                let state = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid]).output().unwrap().stdout;
                !state.is_empty() && !state.starts_with(b"Z")
            };
            assert!(alive(), "{}", printed.content);
            execute("stop_command", &json!({ "job": id }), &context).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!alive(), "the command's child outlived it");
        }

        // A command that ends by itself reports its exit code, and a failure is an error result.
        let fail = execute("run_command", &json!({ "command": "echo 分身; exit 3", "background": true }), &context).await;
        let id = fail.meta["job"].as_str().unwrap().to_string();
        let ended = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
        // The exit can arrive a moment after the output; a second read settles it.
        let ended = if ended.meta["running"] == true { execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await } else { ended };
        assert!(ended.is_error && ended.content.ends_with(&format!("[job {id}: exited with code 3]")), "{}", ended.content);
        assert_eq!(ended.meta["exit_code"], 3);

        assert!(execute("command_output", &json!({ "job": "b9" }), &context).await.content.starts_with("There is no background command `b9`."));
        assert!(execute("stop_command", &json!({ "job": "b9" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn command_output_reaches_the_model_without_terminal_colors() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
        let commands: &[&str] = if cfg!(windows) {
            &[
                // PowerShell 7 colors a table's header.
                "Get-ChildItem | Select-Object Name, Length",
                // ...and a native program's stderr redirected into the pipeline.
                "cmd /c \"echo oops 1>&2\" 2>&1",
                // A program that colors its output regardless.
                "Write-Output \"$([char]27)[31mred$([char]27)[0m $([char]27)]8;;https://example.com$([char]7)link$([char]27)]8;;$([char]7)\"",
            ]
        } else {
            &["printf '\\033[31mred\\033[0m \\033]8;;https://example.com\\007link\\033]8;;\\007\\n'"]
        };
        for command in commands {
            let output = execute("run_command", &json!({ "command": command }), &context).await;
            assert!(!output.content.contains('\u{1b}'), "{command}: {:?}", output.content);
            assert!(!output.meta["output"].as_str().unwrap().contains('\u{1b}'), "{command}");
        }
        let colored = execute("run_command", &json!({ "command": commands.last().unwrap() }), &context).await;
        assert_eq!(colored.content.trim(), "red link");
    }

    #[test]
    fn strips_terminal_escape_sequences() {
        assert_eq!(strip_ansi("\u{1b}[32;1mMode\u{1b}[0m plain"), "Mode plain");
        assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
        assert_eq!(strip_ansi("a\u{1b}(Bb\u{1b}[?25lc"), "abc");
        assert_eq!(strip_ansi("no escapes, 分身"), "no escapes, 分身");
    }

    #[tokio::test]
    async fn finds_files_by_listing_a_directory_or_by_glob() {
        let (directory, context) = context();
        let root = directory.path();
        for file in ["src/main.rs", "src/lexer/mod.rs", "src/lexer/notes.txt", ".github/workflows/ci.yml", ".git/config", "target/debug/app.rs", "README.md"] {
            std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            std::fs::write(root.join(file), "x").unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

        // A listing is one level, as it is on disk, directories marked.
        let listed = execute("find_files", &json!({}), &context).await;
        assert_eq!(listed.content, ".git/\n.github/\n.gitignore\nREADME.md\nsrc/\ntarget/");
        assert_eq!(execute("find_files", &json!({ "path": "src/lexer" }), &context).await.content, "mod.rs\nnotes.txt");

        // A glob walks below, skipping what Git ignores and Git's own directory, but not other dot directories.
        let rust = execute("find_files", &json!({ "glob": "**/*.rs" }), &context).await;
        assert_eq!(rust.content, "src/lexer/mod.rs\nsrc/main.rs");
        assert_eq!(rust.meta["entries"], 2);
        assert_eq!(execute("find_files", &json!({ "glob": "**/*.yml" }), &context).await.content, ".github/workflows/ci.yml");
        assert_eq!(execute("find_files", &json!({ "glob": "*config*" }), &context).await.content, "No file below . matches `*config*`.");
        assert_eq!(execute("find_files", &json!({ "path": "src", "glob": "**/mod.rs" }), &context).await.content, "src/lexer/mod.rs");

        assert!(execute("find_files", &json!({ "path": "README.md" }), &context).await.is_error);
        assert!(execute("find_files", &json!({ "glob": "[" }), &context).await.is_error);
    }

    #[tokio::test]
    async fn writes_a_whole_file_and_applies_several_edits_or_none() {
        let (directory, mut context) = context();
        let snapshots = directory.path().join("snapshots");
        context.snapshots = Some(snapshots.clone());
        let file = directory.path().join("notes/a.txt");

        let created = execute("write_file", &json!({ "path": "notes/a.txt", "content": "one\ntwo\nthree\n" }), &context).await;
        assert!(!created.is_error && created.meta["created"] == true && created.meta["before"].is_null(), "{}", created.content);
        // Overwriting saves what was there, like an edit does.
        let rewritten = execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await;
        assert_eq!((rewritten.meta["added"].as_u64(), rewritten.meta["removed"].as_u64()), (Some(3), Some(3)));
        let saved = snapshots.join(rewritten.meta["before"].as_str().unwrap());
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "one\ntwo\nthree\n");
        assert!(execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await.is_error);
        assert!(execute("write_file", &json!({ "path": "notes", "content": "x" }), &context).await.is_error);

        // Several edits apply in order, each to the result of the one before.
        let edits = json!({ "path": "notes/a.txt", "edits": [
            { "old_string": "alpha", "new_string": "first" },
            { "old_string": "first\ntwo", "new_string": "first\nsecond" },
        ] });
        let edited = execute("edit_file", &edits, &context).await;
        assert!(!edited.is_error, "{}", edited.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");

        // One that does not apply stops all of them: the file stays as it was.
        let failing = json!({ "path": "notes/a.txt", "edits": [
            { "old_string": "omega", "new_string": "last" },
            { "old_string": "missing", "new_string": "x" },
        ] });
        let failed = execute("edit_file", &failing, &context).await;
        assert!(failed.is_error && failed.content.starts_with("Edit 2 of 2: `old_string` was not found") && failed.content.ends_with("Nothing was written."), "{}", failed.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");
        assert!(execute("edit_file", &json!({ "path": "notes/a.txt", "edits": [] }), &context).await.is_error);
    }

    #[tokio::test]
    async fn keeps_a_checked_todo_list() {
        let (_directory, context) = context();
        let list = json!({ "todos": [{ "text": " read the parser ", "status": "done" }, { "text": "fix the lexer", "status": "in_progress" }, { "text": "run tests", "status": "pending" }] });
        let kept = execute("update_todos", &list, &context).await;
        assert_eq!(kept.content, "The list has 3 items, 1 done.");
        assert_eq!(kept.meta["todos"][0], json!({ "text": "read the parser", "status": "done" }));
        for bad in [json!({}), json!({ "todos": [{ "text": "x", "status": "started" }] }), json!({ "todos": [{ "text": "  ", "status": "done" }] })] {
            assert!(execute("update_todos", &bad, &context).await.is_error, "{bad}");
        }
        assert!(execute("update_todos", &json!({ "todos": [] }), &context).await.content.starts_with("The list has 0 items"));
    }

    #[test]
    fn reads_the_checked_out_branch_from_gits_files() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("project");
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        assert_eq!(git_branch(directory.path()), None);
        std::fs::write(workspace.join(".git/HEAD"), "ref: refs/heads/feat/providers\n").unwrap();
        assert_eq!(git_branch(&workspace).as_deref(), Some("feat/providers"));
        // No branch checked out: the commit, shortened.
        std::fs::write(workspace.join(".git/HEAD"), "7cbbed4a96c5740b9387e85f1f58fabcdef01234\n").unwrap();
        assert_eq!(git_branch(&workspace).as_deref(), Some("7cbbed4"));
        // A linked worktree keeps a file that points at its real directory.
        let worktree = directory.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(workspace.join(".git/worktrees/w")).unwrap();
        std::fs::write(workspace.join(".git/worktrees/w/HEAD"), "ref: refs/heads/side\n").unwrap();
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", workspace.join(".git/worktrees/w").display())).unwrap();
        assert_eq!(git_branch(&worktree).as_deref(), Some("side"));
    }

    #[test]
    fn resolves_paths_and_detects_what_is_outside_the_workspace() {
        let (directory, context) = context();
        let workspace = &context.workspace;
        std::fs::create_dir_all(directory.path().join("src")).unwrap();
        assert_eq!(resolve(workspace, "src/../a.txt"), workspace.join("a.txt"));
        assert!(inside_workspace(workspace, &resolve(workspace, "src/new/file.rs")));
        assert!(inside_workspace(workspace, workspace));
        assert!(!inside_workspace(workspace, &resolve(workspace, "../elsewhere.txt")));
        assert!(!inside_workspace(workspace, &std::env::temp_dir()));

        std::fs::create_dir_all(directory.path().join(".git/hooks")).unwrap();
        assert!(in_git_directory(workspace, &resolve(workspace, ".git/hooks/pre-commit")));
        assert!(in_git_directory(workspace, &resolve(workspace, "vendor/lib/.GIT/config")));
        assert!(!in_git_directory(workspace, &resolve(workspace, "src/.gitignore")));
        assert!(!in_git_directory(workspace, &resolve(workspace, "src/new/file.rs")));
    }

    #[test]
    fn questions_are_checked_and_tidied_before_the_user_sees_them() {
        let asked = questions(&json!({ "questions": [
            { "header": "  一个很长很长很长的标签名字啊  ", "question": " 用哪个？ ", "options": [{ "label": " 方案  一 ", "description": " 改得少 " }, { "label": "方案二" }] },
            { "question": "哪些要测？", "kind": "multiple", "options": [{ "label": "解析" }, { "label": "渲染" }, { "label": "导出" }] },
            { "question": "还有什么？" }
        ] }))
        .unwrap();
        // A header cut to fit its tab, or made up; the kind from the options when it is not given.
        assert_eq!(asked.iter().map(|question| (question.header.as_str(), question.kind)).collect::<Vec<_>>(), [("一个很长很长很长的标签名", QuestionKind::Single), ("问题 2", QuestionKind::Multiple), ("问题 3", QuestionKind::Text)]);
        assert_eq!((asked[0].question.as_str(), asked[0].options[0].label.as_str(), asked[0].options[0].description.as_str()), ("用哪个？", "方案 一", "改得少"));
        // A text question takes no options, even if some were given.
        let text = questions(&json!({ "questions": [{ "question": "说说？", "kind": "text", "options": [{ "label": "a" }, { "label": "b" }] }] })).unwrap();
        assert!(text[0].options.is_empty());

        let refused = |input: Value| questions(&input).unwrap_err();
        assert!(refused(json!({})).contains("`questions` is required"));
        assert!(refused(json!({ "questions": [] })).contains("`questions` is required"));
        assert!(refused(json!({ "questions": vec![json!({ "question": "?" }); 5] })).contains("5 questions are too many"));
        assert!(refused(json!({ "questions": [{ "question": " " }] })).contains("Question 1 has no `question`"));
        assert!(refused(json!({ "questions": [{ "question": "?", "kind": "rank" }] })).contains("kind `rank`"));
        assert!(refused(json!({ "questions": [{ "question": "?", "kind": "single" }] })).contains("offers 0 options"));
        assert!(refused(json!({ "questions": [{ "question": "?", "options": vec![json!({ "label": "x" }); 7] }] })).contains("offers 7 options"));
        assert!(refused(json!({ "questions": [{ "question": "?", "options": [{ "label": "a" }, { "label": "" }] }] })).contains("has no `label`"));
        assert!(refused(json!({ "questions": [{ "question": "?", "options": [{ "label": "a" }, { "label": " a " }] }] })).contains("offers `a` twice"));
    }

    #[test]
    fn the_agent_is_told_each_answer_under_its_question() {
        let asked = questions(&json!({ "questions": [
            { "question": "用哪个？", "options": [{ "label": "一" }, { "label": "二" }] },
            { "question": "哪些要测？", "kind": "multiple", "options": [{ "label": "解析" }, { "label": "渲染" }] },
            { "question": "还有什么？" },
            { "question": "叫什么？", "options": [{ "label": "甲" }, { "label": "乙" }] }
        ] }))
        .unwrap();
        let reply = |chosen: &[&str], text: &str| Reply { chosen: chosen.iter().map(|label| label.to_string()).collect(), text: text.to_string() };
        let told = answered(&asked, &[reply(&["一"], ""), reply(&["解析", "渲染"], "先测解析"), reply(&[], "别动接口"), reply(&[], "")], false, None);
        assert_eq!(
            told,
            "The user answered:\n1. 用哪个？\n   → 一\n2. 哪些要测？\n   → 解析; 渲染. And in their own words: 先测解析\n3. 还有什么？\n   → 别动接口\n4. 叫什么？\n   → (left unanswered: decide this one yourself, and say what you chose)"
        );
        // Words in place of an option.
        assert!(answered(&asked[..1], &[reply(&[], "都不要")], false, None).ends_with("→ None of the options. In their own words: 都不要"));
        // Nothing answered, left to the agent, or nobody there in time.
        assert!(answered(&asked, &[], true, None).starts_with("The user chose not to answer"));
        assert!(answered(&asked, &[reply(&[], " ")], false, None).starts_with("The user chose not to answer"));
        assert!(answered(&asked, &[], false, Some(30)).starts_with("Nobody answered within 30 seconds"));
    }

    /// The smallest headers of each kind that say what a picture is and how big.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0]);
        bytes
    }

    #[test]
    fn tells_a_picture_and_its_size_by_its_first_bytes() {
        assert_eq!(picture(&png(640, 480)), Some(("image/png", Some((640, 480)))));
        assert_eq!(picture(b"GIF89a\x20\x03\x58\x02rest"), Some(("image/gif", Some((800, 600)))));
        // A JPEG's size is in its first frame header, after the segments before it.
        let jpeg = [&[0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0][..], &[0xff, 0xc0, 0, 11, 8, 0x01, 0xe0, 0x02, 0x80, 3][..]].concat();
        assert_eq!(picture(&jpeg), Some(("image/jpeg", Some((640, 480)))));
        assert_eq!(picture(&[0xff, 0xd8, 0xff, 0xd9]), Some(("image/jpeg", None)));
        let webp = |chunk: &[u8], header: &[u8]| [b"RIFF\0\0\0\0WEBP".as_slice(), chunk, &[0; 4], header].concat();
        // VP8X keeps width and height less one in three bytes each, from the 24th byte.
        assert_eq!(picture(&webp(b"VP8X", &[0, 0, 0, 0, 0x7f, 0x02, 0, 0xdf, 0x01, 0])), Some(("image/webp", Some((640, 480)))));
        // VP8L packs them less one in fourteen bits each, from the 21st byte.
        let (width, height) = (639u32, 479u32);
        let bits = width | height << 14;
        assert_eq!(picture(&webp(b"VP8L", &[0x2f, bits as u8, (bits >> 8) as u8, (bits >> 16) as u8, (bits >> 24) as u8])), Some(("image/webp", Some((640, 480)))));
        assert_eq!(picture(b"plain text"), None);
    }

    #[tokio::test]
    async fn shows_a_picture_to_a_model_that_can_see_and_says_why_not_otherwise() {
        let (directory, context) = context();
        std::fs::write(directory.path().join("shot.png"), png(2, 3)).unwrap();
        let read = execute("read_file", &json!({ "path": "shot.png" }), &context).await;
        assert!(!read.is_error && read.content == "shot.png is a PNG picture, 2×3, 1 KB. It is shown to you here.", "{}", read.content);
        assert_eq!(read.images, [Image { media_type: "image/png".into(), data: crate::events::base64(&png(2, 3)) }]);
        assert_eq!((read.meta["picture"].as_str(), read.meta["width"].as_u64(), read.meta["path"].as_str()), (Some("image/png"), Some(2), Some("shot.png")));
        // What it is, by its bytes: a picture under another name is still one; a text file named as a picture is not.
        std::fs::write(directory.path().join("shot.bin"), png(2, 3)).unwrap();
        assert_eq!(execute("read_file", &json!({ "path": "shot.bin" }), &context).await.images.len(), 1);
        std::fs::write(directory.path().join("fake.jpg"), "not a picture").unwrap();
        let fake = execute("read_file", &json!({ "path": "fake.jpg" }), &context).await;
        assert!(fake.is_error && fake.content.contains("is not a PNG, JPEG, GIF, or WebP one"), "{}", fake.content);
        // A model that cannot see is told what the picture is, and what to do instead.
        let blind = ToolContext { vision: Some(false), workspace: context.workspace.clone(), cancel: CancellationToken::new(), ..context };
        let refused = execute("read_file", &json!({ "path": "shot.png" }), &blind).await;
        assert!(refused.is_error && refused.images.is_empty() && refused.content.contains("2×3") && refused.content.contains("cannot see pictures"), "{}", refused.content);
        // One too large to send is refused by its size, before it is read.
        let mut large = png(4000, 4000);
        large.resize(crate::events::IMAGE_FILE_BYTES as usize + 1, 0);
        std::fs::write(directory.path().join("large.png"), large).unwrap();
        let large = execute("read_file", &json!({ "path": "large.png" }), &ToolContext { vision: Some(true), ..blind }).await;
        assert!(large.is_error && large.content.contains("too large to be shown"), "{}", large.content);
    }

    #[tokio::test]
    async fn deletes_and_moves_one_file_keeping_a_copy_to_undo_it() {
        let (directory, mut context) = context();
        let workspace = directory.path().join("work");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        context.workspace = workspace.clone();
        let snapshots = directory.path().join("snapshots");
        context.snapshots = Some(snapshots.clone());
        std::fs::write(workspace.join("old.txt"), "one\ntwo\n").unwrap();
        let deleted = execute("delete_file", &json!({ "path": "old.txt" }), &context).await;
        assert!(!deleted.is_error && deleted.content == "Deleted old.txt (2 lines).", "{}", deleted.content);
        assert!(!workspace.join("old.txt").exists());
        // What went is shown as lines taken out, and kept for undoing; the file is not there to open.
        assert_eq!((deleted.meta["deleted"].as_bool(), deleted.meta["removed"].as_u64(), deleted.meta["file"].is_null()), (Some(true), Some(2), true));
        let change = &deleted.meta["changes"][0];
        assert_eq!((change["path"].as_str(), change["after"].is_null()), (Some("old.txt"), true));
        assert_eq!(std::fs::read(snapshots.join(change["before"].as_str().unwrap())).unwrap(), b"one\ntwo\n");
        // A file that is not text is deleted all the same, by its bytes.
        std::fs::write(workspace.join("blob.bin"), [0u8, 1, 2]).unwrap();
        assert_eq!(execute("delete_file", &json!({ "path": "blob.bin" }), &context).await.content, "Deleted blob.bin (3 bytes).");

        std::fs::write(workspace.join("a.txt"), "moved\n").unwrap();
        let moved = execute("move_file", &json!({ "path": "a.txt", "to": "lib/deep/b.txt" }), &context).await;
        assert!(!moved.is_error && moved.content == "Moved a.txt to lib/deep/b.txt.", "{}", moved.content);
        assert!(!workspace.join("a.txt").exists());
        assert_eq!(std::fs::read_to_string(workspace.join("lib/deep/b.txt")).unwrap(), "moved\n");
        // For undoing: the file that came, and the file that went.
        let changes: Vec<(&str, bool, bool)> = moved.meta["changes"].as_array().unwrap().iter().map(|change| (change["path"].as_str().unwrap(), change["before"].is_null(), change["after"].is_null())).collect();
        assert_eq!(changes, [("lib/deep/b.txt", true, false), ("a.txt", false, true)]);
        assert_eq!(moved.meta["to"], "lib/deep/b.txt");

        // What it will not do.
        std::fs::write(workspace.join("c.txt"), "c").unwrap();
        let refused = |input: Value| {
            let context = &context;
            async move { execute(input["tool"].as_str().unwrap(), &input, context).await }
        };
        let taken = refused(json!({ "tool": "move_file", "path": "c.txt", "to": "lib/deep/b.txt" })).await;
        assert!(taken.is_error && taken.content.contains("lib/deep/b.txt already exists"), "{}", taken.content);
        assert!(workspace.join("c.txt").exists());
        let folder = refused(json!({ "tool": "delete_file", "path": "src" })).await;
        assert!(folder.is_error && folder.content.contains("is a folder"), "{}", folder.content);
        let missing = refused(json!({ "tool": "delete_file", "path": "gone.txt" })).await;
        assert!(missing.is_error && missing.content.contains("does not exist"), "{}", missing.content);
        let same = refused(json!({ "tool": "move_file", "path": "c.txt", "to": "./c.txt" })).await;
        assert!(same.is_error && same.content.contains("already there"), "{}", same.content);
    }

    /// A background program that answers each line it reads, and says goodbye at the end of its input.
    fn echoing() -> &'static str {
        if cfg!(windows) {
            "while (($line = [Console]::In.ReadLine()) -ne $null) { \"got $line\" }; 'bye'"
        } else {
            "while read line; do echo \"got $line\"; done; echo bye"
        }
    }

    #[tokio::test]
    async fn types_into_a_background_command_and_reads_what_it_answers() {
        let (_directory, mut context) = context();
        context.jobs = Some(Arc::new(Jobs::default()));
        let started = execute("run_command", &json!({ "command": echoing(), "background": true }), &context).await;
        let job = started.meta["job"].as_str().unwrap().to_string();
        assert!(started.content.contains("`command_input`"), "{}", started.content);

        // A line, ended by Enter unless told otherwise; what the program says back comes with it.
        let answered = execute("command_input", &json!({ "job": job, "text": "hello", "wait_seconds": 10 }), &context).await;
        assert!(!answered.is_error && answered.content.starts_with("Sent 6 characters. What it printed since:\ngot hello"), "{}", answered.content);
        assert_eq!((answered.meta["sent"].as_str(), answered.meta["running"].as_bool()), (Some("hello\n"), Some(true)));

        // Closed, its input ends, and so does the program.
        let closed = execute("command_input", &json!({ "job": job, "text": "last", "close": true, "wait_seconds": 10 }), &context).await;
        assert!(closed.content.starts_with("Sent 5 characters and closed its input."), "{}", closed.content);
        let mut ended = closed;
        for _ in 0..20 {
            if ended.meta["running"] == false {
                break;
            }
            ended = execute("command_output", &json!({ "job": job, "wait_seconds": 1 }), &context).await;
        }
        assert_eq!((ended.meta["running"].as_bool(), ended.meta["exit_code"].as_i64()), (Some(false), Some(0)), "{}", ended.content);

        // What it will not do.
        let late = execute("command_input", &json!({ "job": job, "text": "more" }), &context).await;
        assert!(late.is_error && late.content.contains("has ended"), "{}", late.content);
        let nothing = execute("command_input", &json!({ "job": job }), &context).await;
        assert!(nothing.is_error && nothing.content.contains("Give `text`"), "{}", nothing.content);
        let unknown = execute("command_input", &json!({ "job": "b99", "text": "x" }), &context).await;
        assert!(unknown.is_error && unknown.content.contains("b99"), "{}", unknown.content);
    }

    #[tokio::test]
    async fn a_command_in_the_foreground_reads_no_input_and_does_not_wait_for_any() {
        let (_directory, context) = context();
        let reading = if cfg!(windows) { "$line = [Console]::In.ReadLine(); if ($line -eq $null) { 'none' } else { $line }" } else { "read line || echo none" };
        let ran = execute("run_command", &json!({ "command": reading, "timeout_seconds": 20 }), &context).await;
        assert!(ran.content.contains("none") && ran.meta["stopped"].is_null(), "{}", ran.content);
    }
}
