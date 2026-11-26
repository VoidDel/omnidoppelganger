//! Code navigation through language servers: where a symbol is defined, where it is used, what type it has, a file's
//! outline and its errors, as the language's own server understands the code.
//!
//! A server is started for a workspace and a language the first time `code_nav` asks about a file of that language,
//! and kept for every session working there until the app closes. Servers are spoken to over the Language Server
//! Protocol on their standard streams. Each is set up not to build or run the project's code where it can be:
//! rust-analyzer runs no build scripts, proc macros or `cargo check`, gopls fetches no modules. The TypeScript server
//! loads the project's own TypeScript, which is code from the project; so starting it is asked about first (see
//! [`crate::policy`]).
//!
//! A server is looked for on the `PATH`, and used only if it runs: rustup puts a stand-in for rust-analyzer there
//! that fails when the component is not installed. Where it is not there, a server can be downloaded for the app for
//! Rust, TypeScript and Python, with the user's leave, as the document runtime is: one build, by its checksum, kept
//! in the app's home. Each is a program of its own, needing no Node or Python: rust-analyzer; TypeScript 7, whose
//! `tsc` is a language server itself, and which loads nothing of the project to run, unlike the
//! typescript-language-server on the `PATH`; and ty, Astral's type checker for Python, in place of pyright.

mod client;
mod servers;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use client::{CONTENT_MODIFIED, ProcessGroupGuard, SERVER_CANCELLED};
pub use client::{Client, RpcError};
#[cfg(test)]
use client::{Writer, receive, receive_checked, send};
use servers::missing;
pub use servers::{
    Download, LANGUAGES, Language, Located, downloaded, install, installed, language_by_key, language_for, locate,
    remove, server_tool,
};
#[cfg(test)]
use servers::{RUST_ANALYZER, TY, TYPESCRIPT, executable, install_from, settings, unpack_program};

/// A path as most programs write it: on Windows, without the `\\?\` that marks a path as verbatim (which
/// canonicalizing gives), `\\?\UNC\server\share` written `\\server\share`. Elsewhere, as it is.
pub fn plain(path: &Path) -> PathBuf {
    PathBuf::from(plain_text(&path.to_string_lossy()))
}

fn plain_text(text: &str) -> String {
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.to_string()
    }
}

/// A file's path as a `file:` URI, with what is not plainly part of a path percent-encoded.
pub fn uri(path: &Path) -> String {
    uri_of(&path.to_string_lossy())
}

/// [`uri`], of a path written out, whichever system's it is: `C:\a b` or `\\?\C:\a b` gives `file:///C:/a%20b`,
/// `\\server\share\x` gives `file://server/share/x`.
fn uri_of(text: &str) -> String {
    let text = plain_text(text).replace('\\', "/");
    let mut encoded = String::from(if text.starts_with("//") {
        "file:"
    } else if text.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => encoded.push(byte as char),
            // A drive's colon stays as it is, as servers write it.
            b':' if encoded.len() <= "file:///C".len() => encoded.push(':'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The path a `file:` URI names; `None` for any other kind of URI.
pub fn path_of(uri: &str) -> Option<PathBuf> {
    path_text(uri, cfg!(windows)).map(PathBuf::from)
}

/// [`path_of`], written out as `windows` or another system writes paths. On Windows a drive is written in capitals
/// and with backslashes, whatever the server sent (`file:///c%3A/x` is `C:\x`), so that paths compare as they should.
fn path_text(uri: &str, windows: bool) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::new();
    let raw = rest.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%'
            && at + 3 <= raw.len()
            && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&raw[at + 1..at + 3]).unwrap_or(""), 16)
        {
            bytes.push(byte);
            at += 3;
            continue;
        }
        bytes.push(raw[at]);
        at += 1;
    }
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    if !windows {
        return Some(decoded);
    }
    let drive = decoded.len() > 3
        && decoded.as_bytes()[0] == b'/'
        && decoded.as_bytes()[1].is_ascii_alphabetic()
        && decoded.as_bytes()[2] == b':';
    Some(if drive {
        format!(
            "{}{}",
            decoded[1..2].to_ascii_uppercase(),
            decoded[2..].replace('/', "\\")
        )
    } else if decoded.starts_with('/') {
        decoded.replace('/', "\\")
    } else {
        // `file://server/share/x`: a share on another machine.
        format!(r"\\{}", decoded.replace('/', "\\"))
    })
}

/// A column given in characters (from 0) as LSP counts it, in UTF-16 code units.
fn to_utf16(line: &str, characters: usize) -> usize {
    line.chars().take(characters).map(char::len_utf16).sum()
}

/// An LSP column (UTF-16 code units, from 0) in characters, from 0.
fn from_utf16(line: &str, units: usize) -> usize {
    let mut counted = 0;
    for (index, character) in line.chars().enumerate() {
        if counted >= units {
            return index;
        }
        counted += character.len_utf16();
    }
    line.chars().count()
}

struct InflightStartup {
    cancel: CancellationToken,
    guard: Arc<Mutex<ProcessGroupGuard>>,
    done: Arc<tokio::sync::Notify>,
}

type SlotKey = (PathBuf, &'static str);
type ClientSlot = Arc<tokio::sync::OnceCell<Arc<Client>>>;

/// The language servers running, one per workspace and language, shared by every session.
#[derive(Default)]
pub struct LspHub {
    /// Per-(workspace, language) singleflight slot so slow `locate` + `initialize` on one language
    /// never blocks other languages/workspaces and `shutdown()` never misses an active lock.
    clients: Mutex<HashMap<SlotKey, ClientSlot>>,
    /// Tracks slots currently mid-startup so `shutdown` / `shutdown_language` can cancel and kill them immediately.
    inflight: Mutex<HashMap<SlotKey, InflightStartup>>,
    /// Where servers downloaded for the app are kept; `None` where nothing is downloaded.
    root: Option<PathBuf>,
}

impl LspHub {
    /// A hub whose downloaded servers are kept under `root`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            clients: Mutex::default(),
            inflight: Mutex::default(),
            root: Some(root),
        }
    }

    /// Where servers downloaded for the app are kept.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The server for `language` in `root`: the one running, or a new one, started now in its own singleflight slot.
    pub async fn client(&self, root: &Path, language: &'static Language) -> Result<Arc<Client>, String> {
        let key = (root.to_path_buf(), language.key);
        let cell = {
            let mut clients = self.clients.lock().unwrap();
            if clients
                .get(&key)
                .and_then(|existing| existing.get())
                .is_some_and(|client| client.closed())
            {
                clients.remove(&key);
            }
            clients
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
                .clone()
        };

        let result = cell
            .get_or_try_init(|| async {
                let startup_cancel = CancellationToken::new();
                let group_guard = Arc::new(Mutex::new(ProcessGroupGuard(None)));
                let done = Arc::new(tokio::sync::Notify::new());
                self.inflight.lock().unwrap().insert(
                    key.clone(),
                    InflightStartup {
                        cancel: startup_cancel.clone(),
                        guard: group_guard.clone(),
                        done: done.clone(),
                    },
                );

                let init_outcome: Result<Arc<Client>, String> = async {
                    let located = tokio::select! {
                        biased;
                        () = startup_cancel.cancelled() => return Err("Language server startup cancelled.".to_string()),
                        res = locate(self.root.as_deref(), language) => res,
                    };
                    let (path, name, args) = match (located, language.download) {
                        (Located::System(path), _) => (path, language.program, language.args),
                        (Located::Downloaded(path), Some(download)) => (path, download.program, download.args),
                        (Located::Downloaded(_) | Located::Missing, _) => return Err(missing(language)),
                    };
                    if startup_cancel.is_cancelled() {
                        return Err("Language server startup cancelled.".to_string());
                    }
                    let mut command = tokio::process::Command::new(path);
                    command
                        .args(args)
                        .envs(language.env.iter().copied())
                        .current_dir(root)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true);
                    #[cfg(windows)]
                    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
                    #[cfg(unix)]
                    command.process_group(0);

                    let mut child = command
                        .spawn()
                        .map_err(|error| format!("Cannot start {name}: {error}"))?;
                    group_guard.lock().unwrap().0 = child.id();
                    if startup_cancel.is_cancelled() {
                        group_guard.lock().unwrap().kill_now();
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err("Language server startup cancelled.".to_string());
                    }
                    let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
                        group_guard.lock().unwrap().kill_now();
                        return Err(format!("Cannot talk to {name}."));
                    };
                    // What it says on its error stream, the last of it kept: why it would not start, if it would not.
                    let said = Arc::new(Mutex::new(String::new()));
                    if let Some(stderr) = child.stderr.take() {
                        let said = said.clone();
                        tokio::spawn(async move {
                            let mut lines = BufReader::new(stderr).lines();
                            while let Ok(Some(line)) = lines.next_line().await {
                                let mut said = said.lock().unwrap();
                                said.push_str(&line);
                                said.push('\n');
                                let excess = said.len().saturating_sub(2000);
                                if excess > 0 {
                                    let cut = (excess..said.len())
                                        .find(|at| said.is_char_boundary(*at))
                                        .unwrap_or(said.len());
                                    said.drain(..cut);
                                }
                            }
                        });
                    }
                    match Client::start_with_guard(
                        name,
                        root,
                        Box::new(stdout),
                        Box::new(stdin),
                        Some(child),
                        group_guard.clone(),
                        &startup_cancel,
                    )
                    .await
                    {
                        Ok(client) => Ok(client),
                        Err(error) => {
                            group_guard.lock().unwrap().kill_now();
                            if !startup_cancel.is_cancelled() {
                                // A moment for the last of what it said to be read.
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                            let said = said.lock().unwrap().trim().to_string();
                            let said = if said.is_empty() {
                                String::new()
                            } else {
                                format!(" It said: {said}")
                            };
                            Err(format!("{error}.{said} {}", missing(language)))
                        }
                    }
                }
                .await;

                self.inflight.lock().unwrap().remove(&key);
                done.notify_waiters();
                init_outcome
            })
            .await;

        match result {
            Ok(client) => Ok(client.clone()),
            Err(error) => {
                let mut clients = self.clients.lock().unwrap();
                if clients.get(&key).is_some_and(|current| Arc::ptr_eq(current, &cell)) {
                    clients.remove(&key);
                }
                Err(error)
            }
        }
    }

    /// Deterministically terminate every active and starting language server (including process groups).
    pub fn shutdown(&self) {
        let drained_clients: Vec<Arc<tokio::sync::OnceCell<Arc<Client>>>> = {
            let mut clients = self.clients.lock().unwrap();
            clients.drain().map(|(_, cell)| cell).collect()
        };
        let drained_inflight: Vec<InflightStartup> = {
            let mut inflight = self.inflight.lock().unwrap();
            inflight.drain().map(|(_, entry)| entry).collect()
        };
        for startup in drained_inflight {
            startup.cancel.cancel();
            startup.guard.lock().unwrap().kill_now();
        }
        for cell in drained_clients {
            if let Some(client) = cell.get() {
                client.close_now();
            }
        }
    }

    /// Deterministically shut down all active and starting servers for a single language `key`
    /// (e.g. `"rust"`, `"typescript"`, `"python"`), awaiting child exit so binary files can be removed cleanly.
    pub async fn shutdown_language(&self, key: &str) {
        let matching_cells: Vec<Arc<tokio::sync::OnceCell<Arc<Client>>>> = {
            let mut clients = self.clients.lock().unwrap();
            let keys: Vec<(PathBuf, &'static str)> = clients
                .keys()
                .filter(|(_, lang_key)| *lang_key == key)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| clients.remove(&k)).collect()
        };
        let matching_inflight: Vec<InflightStartup> = {
            let mut inflight = self.inflight.lock().unwrap();
            let keys: Vec<(PathBuf, &'static str)> = inflight
                .keys()
                .filter(|(_, lang_key)| *lang_key == key)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| inflight.remove(&k)).collect()
        };
        for startup in &matching_inflight {
            let notified = startup.done.notified();
            startup.cancel.cancel();
            startup.guard.lock().unwrap().kill_now();
            let _ = tokio::time::timeout(Duration::from_secs(2), notified).await;
        }
        for cell in matching_cells {
            if let Some(client) = cell.get() {
                client.shutdown_and_wait().await;
            }
        }
    }

    #[cfg(test)]
    pub async fn insert(&self, root: &Path, language: &'static Language, client: Arc<Client>) {
        let cell = Arc::new(tokio::sync::OnceCell::new_with(Some(client)));
        self.clients
            .lock()
            .unwrap()
            .insert((root.to_path_buf(), language.key), cell);
    }
}

/// How long the first question to a server waits for it to finish indexing, and how long later ones wait.
const FIRST_SETTLE: Duration = Duration::from_secs(120);
const LATER_SETTLE: Duration = Duration::from_secs(20);
/// The most places listed in one answer.
const LISTED: usize = 100;

/// What `code_nav` was asked: the operation, the file, and where in it.
struct Asked<'a> {
    operation: &'a str,
    path: PathBuf,
    shown: String,
}

/// The operations `code_nav` offers, and whether each needs a place in the file.
pub const OPERATIONS: &[(&str, bool)] = &[
    ("definition", true),
    ("references", true),
    ("implementations", true),
    ("hover", true),
    ("symbols", false),
    ("find_symbol", false),
    ("diagnostics", false),
];

/// The place in a file `code_nav` was pointed at: its line (from 1), and a column found by the symbol named on it,
/// or given (from 1). As LSP wants it: the line from 0, the column in UTF-16 code units.
fn position(text: &str, input: &Value, shown: &str) -> Result<(u64, usize), String> {
    let line = input["line"]
        .as_u64()
        .filter(|line| *line >= 1)
        .ok_or("`line` is required for this operation: the line (from 1) where the symbol is.")?;
    let content = text
        .lines()
        .nth((line - 1) as usize)
        .ok_or_else(|| format!("{shown} has no line {line}; it has {}.", text.lines().count()))?;
    let column = match (
        input["symbol"]
            .as_str()
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty()),
        input["column"].as_u64(),
    ) {
        (Some(symbol), _) => {
            // The symbol as a whole word: `id` is not found inside `width`.
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let found = content.match_indices(symbol).find(|(at, _)| {
                let before = content[..*at].chars().next_back();
                let after = content[at + symbol.len()..].chars().next();
                !before.is_some_and(word) && !after.is_some_and(word)
            });
            let (at, _) = found.or_else(|| content.match_indices(symbol).next()).ok_or_else(|| {
                format!(
                    "`{symbol}` is not on line {line} of {shown}, which reads: {}",
                    content.trim()
                )
            })?;
            content[..at].chars().count()
        }
        (None, Some(column)) if column >= 1 => (column - 1) as usize,
        _ => return Err("Give `symbol`, the name on that line to ask about, or `column` (from 1).".to_string()),
    };
    Ok((line - 1, to_utf16(content, column)))
}

/// Shows places in files by their path in the workspace and their line, with what the line holds.
struct Lines<'a> {
    workspace: &'a Path,
    files: HashMap<PathBuf, Vec<String>>,
}

impl<'a> Lines<'a> {
    fn new(workspace: &'a Path) -> Self {
        Self {
            workspace,
            files: HashMap::new(),
        }
    }

    fn line(&mut self, path: &Path, line: usize) -> String {
        let lines = self.files.entry(path.to_path_buf()).or_insert_with(|| {
            std::fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        lines.get(line).cloned().unwrap_or_default()
    }

    /// `path:line:column  the line`, for a URI and an LSP range or position.
    fn place(&mut self, document: &str, start: &Value) -> Option<(String, String)> {
        let path = path_of(document)?;
        let line = start["line"].as_u64()? as usize;
        let content = self.line(&path, line);
        let column = from_utf16(&content, start["character"].as_u64().unwrap_or(0) as usize);
        let shown = crate::tools::display(self.workspace, &path);
        Some((
            format!("{shown}:{}:{}", line + 1, column + 1),
            content.trim().to_string(),
        ))
    }
}

/// The places an answer of `definition`, `references` or `implementations` gives: `Location`s or `LocationLink`s.
fn locations(answer: &Value) -> Vec<(String, Value)> {
    let listed: Vec<Value> = match answer {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        one => vec![one.clone()],
    };
    listed
        .iter()
        .filter_map(|item| {
            let document = item["uri"].as_str().or(item["targetUri"].as_str())?;
            let start = if item["targetSelectionRange"].is_object() {
                &item["targetSelectionRange"]["start"]
            } else {
                &item["range"]["start"]
            };
            Some((document.to_string(), start.clone()))
        })
        .collect()
}

/// The text a hover answer holds, however the server shaped it.
fn hover_text(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(hover_text)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(part) => match (part.get("value"), part.get("language")) {
            (Some(Value::String(value)), Some(Value::String(language))) => format!("```{language}\n{value}\n```"),
            (Some(Value::String(value)), _) => value.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The LSP name of a symbol kind.
fn kind_name(kind: u64) -> &'static str {
    const KINDS: &[&str] = &[
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum member",
        "struct",
        "event",
        "operator",
        "type parameter",
    ];
    KINDS.get((kind as usize).wrapping_sub(1)).copied().unwrap_or("symbol")
}

/// A file's outline, from `DocumentSymbol`s (nested) or `SymbolInformation`s (flat), one symbol a line.
fn outline(symbols: &[Value], depth: usize, lines: &mut Vec<String>) {
    for symbol in symbols {
        let start = if symbol["selectionRange"].is_object() {
            &symbol["selectionRange"]["start"]
        } else if symbol["range"].is_object() {
            &symbol["range"]["start"]
        } else {
            &symbol["location"]["range"]["start"]
        };
        let line = start["line"].as_u64().unwrap_or(0) + 1;
        let detail = symbol["detail"]
            .as_str()
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(|detail| format!("  {detail}"))
            .unwrap_or_default();
        lines.push(format!(
            "{}{} {}{detail}  (line {line})",
            "  ".repeat(depth),
            kind_name(symbol["kind"].as_u64().unwrap_or(0)),
            symbol["name"].as_str().unwrap_or("?")
        ));
        if let Some(children) = symbol["children"].as_array() {
            outline(children, depth + 1, lines);
        }
    }
}

/// Answer `code_nav`: start or find the server for the file's language, give it the file, and ask.
pub async fn navigate(
    hub: &LspHub,
    workspace: &Path,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<crate::tools::ToolOutput, String> {
    // Paths come back from the server as it writes them, which is plainly: the workspace is compared with them so.
    let workspace = &plain(workspace);
    let operation = input["operation"].as_str().unwrap_or_default();
    let Some((_, placed)) = OPERATIONS.iter().find(|(name, _)| *name == operation) else {
        return Err(format!(
            "`operation` must be one of {}.",
            OPERATIONS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
        ));
    };
    let path = crate::tools::resolve(
        workspace,
        input["path"]
            .as_str()
            .filter(|path| !path.trim().is_empty())
            .ok_or("`path` is required: the file to ask about, or for `find_symbol` any file of the language.")?,
    );
    let asked = Asked {
        operation,
        shown: crate::tools::display(workspace, &path),
        path,
    };
    let language = language_for(&asked.path).ok_or_else(|| {
        let known: Vec<String> = LANGUAGES
            .iter()
            .map(|language| format!("{} (.{})", language.name, language.extensions.join(", .")))
            .collect();
        format!(
            "{} is not of a language navigated here: {}. Use `search` instead.",
            asked.shown,
            known.join("; ")
        )
    })?;
    if !asked.path.is_file() {
        return Err(format!("{} is not a file.", asked.shown));
    }
    let client = hub.client(workspace, language).await?;
    // The first question waits for the project to be indexed; `settled` is set only once indexing truly completes without cancellation.
    let first = !client.settled.load(Ordering::Acquire);
    let ready = client
        .settle(if first { FIRST_SETTLE } else { LATER_SETTLE }, cancel)
        .await;
    if cancel.is_cancelled() {
        return Err("Cancelled.".to_string());
    }
    if ready {
        client.settled.store(true, Ordering::Release);
    }
    let (document, text) = client.sync(&asked.path).await?;
    let place = if *placed {
        Some(position(&text, input, &asked.shown)?)
    } else {
        None
    };
    let at = |(line, character): (u64, usize)| json!({ "textDocument": { "uri": document }, "position": { "line": line, "character": character } });
    let method_params = match asked.operation {
        "definition" => Some(("textDocument/definition", at(place.unwrap()))),
        "implementations" => Some(("textDocument/implementation", at(place.unwrap()))),
        "hover" => Some(("textDocument/hover", at(place.unwrap()))),
        "references" => {
            let mut params = at(place.unwrap());
            params["context"] = json!({ "includeDeclaration": true });
            Some(("textDocument/references", params))
        }
        "symbols" => Some((
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": document } }),
        )),
        "find_symbol" => {
            let query = input["query"]
                .as_str()
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or("`query` is required for `find_symbol`: the name, or part of it, to look for.")?;
            Some(("workspace/symbol", json!({ "query": query })))
        }
        _ => None,
    };
    let mut lines = Lines::new(workspace);
    let mut meta = json!({ "operation": asked.operation, "path": asked.shown, "server": client.program });
    if let Some((line, _)) = place {
        meta["line"] = json!(line + 1);
    }
    let still = if ready {
        ""
    } else {
        "\n[The language server was still indexing the project: the answer may be incomplete. Asking again in a while may find more.]"
    };

    let Some((method, params)) = method_params else {
        // Diagnostics: what the server finds wrong with the file, by its own analysis.
        let found = client.diagnostics(&document, Duration::from_secs(10), cancel).await?;
        let severity = |level: u64| match level {
            1 => "error",
            2 => "warning",
            3 => "info",
            _ => "hint",
        };
        let mut listed: Vec<(u64, String)> = found
            .iter()
            .filter_map(|diagnostic| {
                let level = diagnostic["severity"].as_u64().unwrap_or(1);
                let (place, content) = lines.place(&document, &diagnostic["range"]["start"])?;
                let code = diagnostic["code"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| diagnostic["code"].as_i64().map(|code| code.to_string()))
                    .map(|code| format!(" [{code}]"))
                    .unwrap_or_default();
                let message = diagnostic["message"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .replace('\n', "\n    ");
                Some((
                    level,
                    format!("{} {place}{code}: {message}\n    {content}", severity(level)),
                ))
            })
            .collect();
        listed.sort_by_key(|(level, _)| *level);
        meta["results"] = json!(listed.len());
        meta["errors"] = json!(listed.iter().filter(|(level, _)| *level == 1).count());
        let note = if client.program == "rust-analyzer" {
            "\n[rust-analyzer here runs no `cargo check`, so only what it finds by itself is listed: run `cargo check` for the compiler's full say.]"
        } else {
            ""
        };
        let content = if listed.is_empty() {
            format!(
                "{} has no errors or warnings that {} reports.{note}{still}",
                asked.shown, client.program
            )
        } else {
            format!(
                "{} problems in {}:\n{}{note}{still}",
                listed.len(),
                asked.shown,
                listed
                    .into_iter()
                    .map(|(_, line)| line)
                    .take(LISTED)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        return Ok(crate::tools::ToolOutput::ok(content, meta));
    };

    // The code may change under the question while the server reads it: then it is asked again.
    let mut answer = Value::Null;
    for attempt in 0..3 {
        match client
            .request(method, params.clone(), Duration::from_secs(60), cancel)
            .await
        {
            Ok(got) => {
                answer = got;
                break;
            }
            Err(_) if cancel.is_cancelled() => return Err("Cancelled.".to_string()),
            Err(error) if matches!(error.code, CONTENT_MODIFIED | SERVER_CANCELLED) && attempt < 2 => {
                client.settle(LATER_SETTLE, cancel).await;
                if cancel.is_cancelled() {
                    return Err("Cancelled.".to_string());
                }
            }
            Err(error) => return Err(format!("{} could not answer: {}", client.program, error.message)),
        }
    }

    let content = match asked.operation {
        "hover" => {
            let text = hover_text(&answer["contents"]);
            meta["results"] = json!(usize::from(!text.trim().is_empty()));
            if text.trim().is_empty() {
                format!("{} has nothing to say about that place.{still}", client.program)
            } else {
                let text: String = text.trim().chars().take(4000).collect();
                text + still
            }
        }
        "symbols" => {
            let mut listed = Vec::new();
            outline(answer.as_array().map(Vec::as_slice).unwrap_or_default(), 0, &mut listed);
            meta["results"] = json!(listed.len());
            if listed.is_empty() {
                format!("{} found no symbols in {}.{still}", client.program, asked.shown)
            } else {
                format!("{}:\n{}{still}", asked.shown, listed.join("\n"))
            }
        }
        "find_symbol" => {
            let mut listed = Vec::new();
            for symbol in answer.as_array().into_iter().flatten() {
                let document = symbol["location"]["uri"].as_str().unwrap_or_default();
                // Only what is in the workspace: not the standard library or dependencies.
                if !path_of(document).is_some_and(|path| path.starts_with(workspace)) {
                    continue;
                }
                let start = &symbol["location"]["range"]["start"];
                let place = if start.is_object() {
                    lines.place(document, start).map(|(place, _)| place)
                } else {
                    path_of(document).map(|path| crate::tools::display(workspace, &path))
                };
                let container = symbol["containerName"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(|name| format!(" in {name}"))
                    .unwrap_or_default();
                listed.push(format!(
                    "{} {}{container}  {}",
                    kind_name(symbol["kind"].as_u64().unwrap_or(0)),
                    symbol["name"].as_str().unwrap_or("?"),
                    place.unwrap_or_default()
                ));
                if listed.len() == LISTED {
                    break;
                }
            }
            meta["results"] = json!(listed.len());
            if listed.is_empty() {
                format!(
                    "No symbol in the workspace matches `{}`.{still}",
                    params["query"].as_str().unwrap_or_default()
                )
            } else {
                listed.join("\n") + still
            }
        }
        _ => {
            let found = locations(&answer);
            meta["results"] = json!(found.len());
            let mut listed: Vec<String> = found
                .iter()
                .take(LISTED)
                .filter_map(|(document, start)| lines.place(document, start))
                .map(|(place, content)| format!("{place}  {content}"))
                .collect();
            if found.len() > LISTED {
                listed.push(format!("… and {} more", found.len() - LISTED));
            }
            let what = match asked.operation {
                "definition" => "Defined at",
                "implementations" => "Implemented at",
                _ => "Used at",
            };
            if listed.is_empty() {
                format!("{} found none.{still}", client.program)
            } else {
                format!("{what} ({}):\n{}{still}", found.len(), listed.join("\n"))
            }
        }
    };
    Ok(crate::tools::ToolOutput::ok(content, meta))
}
