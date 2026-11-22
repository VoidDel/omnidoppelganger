//! Interaction, MCP resource, skill, runtime, image generation, and to-do tools.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::events::{Choice, Image, Question, QuestionKind, Reply};

use super::{ToolContext, ToolOutput, display, in_git_directory, inside_workspace, resolve, string_arg};

const TODO_ITEMS: usize = 50;
/// The states a to-do item can be in.
pub const TODO_STATES: &[&str] = &["pending", "in_progress", "done"];

/// The most subagents one `delegate` call sets off.
pub const SUBAGENTS: usize = 8;

/// The most resources listed from one server.
const RESOURCES_LISTED: usize = 200;

/// The name of the tool that presents a plan in plan mode. The agent loop runs it, since it ends the turn.
pub const PRESENT_PLAN: &str = "present_plan";

/// The name of the tool that asks the user questions. The agent loop runs it, since it waits on the user.
pub const ASK_USER: &str = "ask_user";
/// The most questions one `ask_user` call asks, and the most options a question offers.
pub const QUESTIONS: usize = 4;
pub const OPTIONS: usize = 6;

/// Paint a picture with the painter set up, and save it at a path in the workspace that holds nothing yet.
pub(super) async fn generate_image(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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
    if in_git_directory(&context.workspace, &asked) {
        return Err(ToolOutput::error(format!("{} is inside a .git directory: save pictures elsewhere in the workspace.", display(&context.workspace, &asked))));
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
    if !inside_workspace(&context.workspace, &path) || in_git_directory(&context.workspace, &path) {
        return Err(ToolOutput::error(format!("{} cannot be written there: choose another path inside the workspace.", display(&context.workspace, &path))));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ToolOutput::error(format!("Cannot make the folder {}: {error}", parent.display())))?;
    }
    crate::fsutil::write_atomically(&path, &painted.bytes).map_err(|error| ToolOutput::error(format!("Cannot save the picture at {}: {error}", path.display())))?;
    let snapshot = context
        .snapshots
        .as_ref()
        .filter(|_| painted.bytes.len() as u64 <= crate::undo::ARTIFACT_SNAPSHOT_BYTES)
        .and_then(|dir| crate::undo::save(dir, &painted.bytes).ok());
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
    let mut meta = json!({ "file": path.to_string_lossy(), "path": shown, "painter": config.painter.name, "model": config.painter.model, "size": painted.size, "prompt": prompt, "bytes": painted.bytes.len() });
    if let Some(snapshot) = snapshot {
        meta["snapshot"] = json!(snapshot);
    }
    Ok(ToolOutput::ok(content, meta))
}

/// One task handed to `delegate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateTask {
    pub name: String,
    pub task: String,
    pub read_only: bool,
}

/// The tasks `delegate` was given, checked.
pub fn delegate_tasks(input: &Value) -> Result<Vec<DelegateTask>, String> {
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
            let read_only = entry["read_only"].as_bool().unwrap_or(false);
            Ok(DelegateTask { name, task: task.to_string(), read_only })
        })
        .collect()
}

pub(super) async fn list_mcp_resources(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let hub = context.mcp.as_ref().ok_or_else(|| ToolOutput::error("There are no MCP servers here."))?;
    let wanted = input["server"].as_str().filter(|server| !server.is_empty());
    let servers: Vec<_> = hub.resource_servers().into_iter().filter(|(name, _)| wanted.is_none_or(|wanted| wanted == name)).collect();
    if servers.is_empty() {
        return Err(ToolOutput::error(match wanted {
            Some(server) => format!("No connected MCP server `{server}` offers resources."),
            None => "No connected MCP server offers resources.".to_string(),
        }));
    }
    let mut parts = Vec::new();
    let mut counted = 0;
    for (name, server) in servers {
        match server.list_resources(RESOURCES_LISTED).await {
            Ok((resources, templates)) => {
                counted += resources.len();
                let line = |item: &Value, address: &str| {
                    let label = item["title"].as_str().or(item["name"].as_str()).unwrap_or_default();
                    let kind = item["mimeType"].as_str().map(|kind| format!(" ({kind})")).unwrap_or_default();
                    let about = item["description"].as_str().map(str::trim).filter(|about| !about.is_empty()).map(|about| format!(": {about}")).unwrap_or_default();
                    format!("- {}{}{kind}{about}", item[address].as_str().unwrap_or_default(), if label.is_empty() { String::new() } else { format!(" — {label}") })
                };
                let mut part = format!("Resources of `{name}` ({}):", resources.len());
                part.push_str(&resources.iter().map(|resource| format!("\n{}", line(resource, "uri"))).collect::<String>());
                if !templates.is_empty() {
                    part.push_str("\nTemplates of addresses:");
                    part.push_str(&templates.iter().map(|template| format!("\n{}", line(template, "uriTemplate"))).collect::<String>());
                }
                parts.push(part);
            }
            Err(error) => parts.push(format!("`{name}` could not list its resources: {error}")),
        }
    }
    Ok(ToolOutput::ok(parts.join("\n\n"), json!({ "results": counted })))
}

pub(super) async fn read_mcp_resource(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let hub = context.mcp.as_ref().ok_or_else(|| ToolOutput::error("There are no MCP servers here."))?;
    let name = string_arg(input, "server")?;
    let uri = string_arg(input, "uri")?;
    let (_, server) = hub.resource_servers().into_iter().find(|(server, _)| server == name).ok_or_else(|| ToolOutput::error(format!("No connected MCP server `{name}` offers resources.")))?;
    let read = server.read_resource(uri, &context.cancel).await.map_err(|error| ToolOutput::error(format!("`{name}` could not give {uri}: {error}")))?;
    // Pictures go to a model that can see them; to one that cannot, only what they are.
    let seen = context.vision != Some(false);
    let images: Vec<Image> = if seen { read.images.iter().map(|(media_type, data)| Image { media_type: media_type.clone(), data: data.clone() }).collect() } else { Vec::new() };
    let meta = json!({ "server": name, "uri": uri, "pictures": read.images.len() });
    Ok(ToolOutput { content: read.text, is_error: false, meta, images })
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

/// Install a skill from a folder or a Git repository; see [`crate::skills::fetch`].
pub(super) async fn install_skill(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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
pub(super) async fn document_runtime(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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

/// The files a step hands the user: each must lie inside the workspace and exist there, named by its absolute path.
pub(super) fn deliver_files(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let paths: Vec<&str> = input["paths"].as_array().map(|paths| paths.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if paths.is_empty() {
        return Err(ToolOutput::error("`paths` names no file."));
    }
    let mut files = Vec::new();
    let mut outside = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        let file = resolve(&context.workspace, path);
        // Check workspace confinement before probing disk so this cannot reveal whether an outside file exists.
        if !inside_workspace(&context.workspace, &file) {
            outside.push(path.to_string());
        } else if file.is_file() {
            files.push(file.to_string_lossy().into_owned());
        } else {
            missing.push(path.to_string());
        }
    }
    if !outside.is_empty() {
        return Err(ToolOutput::error(format!("Outside the workspace: {}. Nothing was handed over; deliver files inside the workspace.", outside.join(", "))));
    }
    if !missing.is_empty() {
        return Err(ToolOutput::error(format!("Not files that are there: {}. Nothing was handed over; deliver them once they are made.", missing.join(", "))));
    }
    let mut snapshots = std::collections::BTreeMap::new();
    let mut sizes = std::collections::BTreeMap::new();
    if let Some(dir) = &context.snapshots {
        for file in &files {
            if let Ok(Some((snap, len))) = crate::undo::save_file(dir, Path::new(file), crate::undo::ARTIFACT_SNAPSHOT_BYTES) {
                snapshots.insert(file.clone(), snap);
                sizes.insert(file.clone(), len);
            }
        }
    }
    let names: Vec<String> = files.iter().map(|file| display(&context.workspace, Path::new(file))).collect();
    let mut meta = json!({ "files": files });
    if !snapshots.is_empty() {
        meta["snapshots"] = json!(snapshots);
        meta["sizes"] = json!(sizes);
    }
    Ok(ToolOutput::ok(format!("Handed to the user: {}. They can open each from here.", names.join(", ")), meta))
}

/// Add an MCP server to the user's list and connect to it; see [`crate::mcp::McpHub::add`].
pub(super) async fn add_mcp_server(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
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

pub(super) fn update_todos(input: &Value) -> Result<ToolOutput, ToolOutput> {
    let items = todo_items(input).map_err(ToolOutput::error)?;
    let done = items.iter().filter(|(_, status)| status == "done").count();
    let in_progress = items.iter().filter(|(_, status)| status == "in_progress").count();
    let pending = items.iter().filter(|(_, status)| status == "pending").count();
    let todos: Vec<Value> = items.iter().map(|(text, status)| json!({ "text": text, "status": status })).collect();
    let mut content = format!("The list has {} items, {done} done.", items.len());
    // When only the final item(s) remain in progress, models often write their final reply directly and leave the last item unclosed.
    if pending == 0 && in_progress > 0 {
        content.push_str(" Before your final reply, call `update_todos` once more to mark finished items `done`.");
    }
    Ok(ToolOutput::ok(content, json!({ "todos": todos })))
}
