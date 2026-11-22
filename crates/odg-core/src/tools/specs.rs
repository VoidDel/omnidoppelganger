//! Tool JSON schemas sent to model providers.

use serde_json::{Value, json};

use super::commands::shell;
use super::extras::{ASK_USER, OPTIONS, PRESENT_PLAN, QUESTIONS, SUBAGENTS, TODO_STATES};

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
            "description": "See whether the language server `code_nav` needs for a language is here, and with `install`, download it for this app. Rust (rust-analyzer), TypeScript and JavaScript (TypeScript 7), and Python (ty) can be downloaded, and the user is asked first every time; for C, C++ and Go it says how the user installs the server. Use it when `code_nav` says a server is missing.",
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

/// The definitions of `list_mcp_resources` and `read_mcp_resource`, offered while a connected MCP server has resources.
pub fn mcp_resource_specs(servers: &[String]) -> Vec<Value> {
    let named = servers.iter().map(|server| format!("`{server}`")).collect::<Vec<_>>().join(", ");
    vec![
        json!({
            "name": "list_mcp_resources",
            "description": format!("List what the user's MCP servers offer to be read: documents, files, records, schemas, each by an address (`uri`), with the templates some servers name addresses by. Servers with resources: {named}. Read one with `read_mcp_resource`."),
            "input_schema": {
                "type": "object",
                "properties": { "server": { "type": "string", "description": "Only this server's. Default: every server's." } }
            }
        }),
        json!({
            "name": "read_mcp_resource",
            "description": "Read a resource of one of the user's MCP servers by its address, as `list_mcp_resources` gave it or a template makes it. The user is asked first: the address goes to the server.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "server": { "type": "string", "description": "The server the resource is on." },
                    "uri": { "type": "string", "description": "The resource's address." }
                },
                "required": ["server", "uri"]
            }
        }),
    ]
}

/// The definition of `present_plan`. A subagent is not offered it: it reports to the agent that set it off.
pub fn present_plan_spec() -> Value {
    json!({
        "name": PRESENT_PLAN,
        "description": "In plan mode, present your plan to the user: what you would change, where, and how you would verify it, in Markdown. It is shown as the plan, with a way for the user to have it carried out, and your turn ends with it: say nothing after it. Use it once the plan is complete, in place of writing the plan as your reply. Outside plan mode it is not needed: do the work.",
        "input_schema": {
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "The plan in a few words, shown over it." },
                "plan": { "type": "string", "description": "The whole plan, in Markdown: the steps, the files each touches, and how the result is checked." }
            },
            "required": ["plan"]
        }
    })
}

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
