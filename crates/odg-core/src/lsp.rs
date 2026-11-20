//! Code navigation through language servers: where a symbol is defined, where it is used, what type it has, a file's
//! outline and its errors, as the language's own server understands the code.
//!
//! A server is started for a workspace and a language the first time `code_nav` asks about a file of that language,
//! and kept for every session working there until the app closes. Servers are spoken to over the Language Server
//! Protocol on their standard streams. Each is set up not to build or run the project's code where it can be:
//! rust-analyzer runs no build scripts, proc macros or `cargo check`, gopls fetches no modules. The TypeScript server
//! loads the project's own TypeScript, which is code from the project; so starting it is asked about first (see
//! [`crate::policy`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

/// A language a server is known for, and how that server is started.
#[derive(Debug)]
pub struct Language {
    /// The language, as the agent and the user are told of it.
    pub name: &'static str,
    /// The server's program, looked for on the `PATH`.
    pub program: &'static str,
    args: &'static [&'static str],
    extensions: &'static [&'static str],
    /// How to install the server, said when it is missing.
    pub install: &'static str,
    /// The server runs code from the project, so starting it is asked about first.
    pub runs_project_code: bool,
    env: &'static [(&'static str, &'static str)],
}

pub const LANGUAGES: &[Language] = &[
    Language { name: "Rust", program: "rust-analyzer", args: &[], extensions: &["rs"], install: "rustup component add rust-analyzer", runs_project_code: false, env: &[] },
    Language {
        name: "C/C++",
        program: "clangd",
        args: &["--log=error"],
        extensions: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm"],
        install: "install clangd (it comes with LLVM, and with Xcode's command line tools on macOS)",
        runs_project_code: false,
        env: &[],
    },
    Language {
        name: "TypeScript/JavaScript",
        program: "typescript-language-server",
        args: &["--stdio"],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        install: "npm install -g typescript-language-server typescript",
        runs_project_code: true,
        env: &[],
    },
    Language { name: "Python", program: "pyright-langserver", args: &["--stdio"], extensions: &["py", "pyi"], install: "npm install -g pyright", runs_project_code: false, env: &[] },
    Language {
        name: "Go",
        program: "gopls",
        args: &[],
        extensions: &["go"],
        install: "go install golang.org/x/tools/gopls@latest",
        runs_project_code: false,
        // No module is fetched, and no other Go is downloaded to run.
        env: &[("GOPROXY", "off"), ("GOTOOLCHAIN", "local")],
    },
];

/// The language of a file, by its extension.
pub fn language_for(path: &Path) -> Option<&'static Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES.iter().find(|language| language.extensions.contains(&extension.as_str()))
}

/// The LSP language id of a file, by its extension.
fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("rs") => "rust",
        Some("c" | "h") => "c",
        Some("m") => "objective-c",
        Some("mm") => "objective-cpp",
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx") => "cpp",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("jsx") => "javascriptreact",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("py" | "pyi") => "python",
        Some("go") => "go",
        _ => "plaintext",
    }
}

/// What a server is told it may be set to, as `initializationOptions` and as the answer to `workspace/configuration`.
fn settings(language: &Language) -> Value {
    match language.program {
        // Analysis only: nothing of the project is built or run.
        "rust-analyzer" => json!({ "cargo": { "buildScripts": { "enable": false } }, "procMacro": { "enable": false }, "checkOnSave": false }),
        _ => Value::Null,
    }
}

/// Where a program is on the `PATH`, if it is there.
pub fn installed(program: &str) -> Option<PathBuf> {
    let directories = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect::<Vec<_>>()).unwrap_or_default();
    let names: Vec<String> = if cfg!(windows) {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        extensions.split(';').filter(|extension| !extension.is_empty()).map(|extension| format!("{program}{extension}")).collect()
    } else {
        vec![program.to_string()]
    };
    directories.iter().flat_map(|directory| names.iter().map(move |name| directory.join(name))).find(|candidate| candidate.is_file())
}

/// A file's path as a `file:` URI, with what is not plainly part of a path percent-encoded.
pub fn uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::from(if text.starts_with('/') { "file://" } else { "file:///" });
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
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::new();
    let raw = rest.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' && at + 3 <= raw.len() && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&raw[at + 1..at + 3]).unwrap_or(""), 16) {
            bytes.push(byte);
            at += 3;
            continue;
        }
        bytes.push(raw[at]);
        at += 1;
    }
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    // `file:///C:/x` on Windows names `C:/x`.
    let windows_drive = decoded.len() > 3 && decoded.as_bytes()[0] == b'/' && decoded.as_bytes()[2] == b':';
    Some(PathBuf::from(if cfg!(windows) && windows_drive { decoded[1..].to_string() } else { decoded }))
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

/// What a server said went wrong with a request.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// The server answered that the code changed under the request: worth asking again.
const CONTENT_MODIFIED: i64 = -32801;
const SERVER_CANCELLED: i64 = -32802;

/// What the reader has learned of the server, for those who wait on it.
#[derive(Default)]
struct Watch {
    /// The work the server reported under way (`$/progress`), by token.
    progress: HashSet<String>,
    /// rust-analyzer's own word on whether it is done for now.
    quiescent: Option<bool>,
    /// The last diagnostics published for each document, by URI.
    diagnostics: HashMap<String, Vec<Value>>,
    /// The server is gone, and why.
    closed: Option<String>,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;
type Writer = Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// One running language server.
pub struct Client {
    language: &'static Language,
    writer: Writer,
    next: AtomicI64,
    pending: Pending,
    watch: Arc<Mutex<Watch>>,
    changed: Arc<Notify>,
    /// The documents the server holds open, by URI: the version and the text it was last sent.
    open: tokio::sync::Mutex<HashMap<String, (i64, String)>>,
    capabilities: Mutex<Value>,
    /// Whether the server has once been waited on until it was done indexing.
    settled: AtomicBool,
    _child: Option<tokio::process::Child>,
}

/// Write one LSP message: a `Content-Length` header and the JSON.
async fn send(writer: &Writer, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    let mut writer = writer.lock().await;
    writer.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.map_err(|error| format!("the server stopped reading: {error}"))?;
    writer.flush().await.map_err(|error| format!("the server stopped reading: {error}"))
}

/// Read one LSP message; `None` when the stream ended.
async fn receive(reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>) -> Option<Value> {
    loop {
        let mut length: Option<usize> = None;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).await.ok()? == 0 {
                return None;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().ok();
            }
        }
        // A message without its length cannot be read; the next one may be.
        let Some(length) = length else { continue };
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await.ok()?;
        if let Ok(message) = serde_json::from_slice(&body) {
            return Some(message);
        }
    }
}

impl Client {
    /// Start talking to a server on these streams: answer what it asks, note what it says, and initialize it for
    /// `root`. `child` is the process, kept so that dropping the client ends it.
    pub async fn start(
        language: &'static Language,
        root: &Path,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<tokio::process::Child>,
    ) -> Result<Arc<Self>, String> {
        let client = Arc::new(Client {
            language,
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            next: AtomicI64::new(1),
            pending: Arc::default(),
            watch: Arc::default(),
            changed: Arc::default(),
            open: tokio::sync::Mutex::default(),
            capabilities: Mutex::new(Value::Null),
            settled: AtomicBool::new(false),
            _child: child,
        });
        let (pending, watch, changed, writer) = (client.pending.clone(), client.watch.clone(), client.changed.clone(), client.writer.clone());
        let folder = json!([{ "uri": uri(root), "name": root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default() }]);
        let answered_folder = folder.clone();
        tokio::spawn(async move {
            let folder = answered_folder;
            let mut reader = BufReader::new(reader);
            while let Some(message) = receive(&mut reader).await {
                let method = message["method"].as_str();
                match (method, message.get("id")) {
                    // An answer to a request of ours.
                    (None, Some(id)) => {
                        let Some(sender) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { continue };
                        let answer = match message.get("error") {
                            Some(error) => Err(RpcError { code: error["code"].as_i64().unwrap_or(0), message: error["message"].as_str().unwrap_or_default().to_string() }),
                            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = sender.send(answer);
                    }
                    // A request of the server's: answered as a client that keeps nothing of its own would.
                    (Some(method), Some(id)) => {
                        let answer = match method {
                            "workspace/configuration" => {
                                let items = message["params"]["items"].as_array().cloned().unwrap_or_default();
                                json!({ "jsonrpc": "2.0", "id": id, "result": items.iter().map(|item| if item["section"].as_str() == Some(language.program) { settings(language) } else { Value::Null }).collect::<Vec<_>>() })
                            }
                            "workspace/workspaceFolders" => json!({ "jsonrpc": "2.0", "id": id, "result": folder }),
                            "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" | "window/showMessageRequest" | "workspace/diagnostic/refresh" | "workspace/semanticTokens/refresh" | "workspace/inlayHint/refresh" | "workspace/codeLens/refresh" => {
                                json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })
                            }
                            _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} is not supported") } }),
                        };
                        let _ = send(&writer, &answer).await;
                    }
                    // What the server tells of itself.
                    (Some(method), None) => {
                        let params = &message["params"];
                        let mut watch = watch.lock().unwrap();
                        match method {
                            "$/progress" => {
                                let token = params["token"].to_string();
                                match params["value"]["kind"].as_str() {
                                    Some("begin") => {
                                        watch.progress.insert(token);
                                    }
                                    Some("end") => {
                                        watch.progress.remove(&token);
                                    }
                                    _ => {}
                                }
                            }
                            "experimental/serverStatus" => watch.quiescent = params["quiescent"].as_bool(),
                            "textDocument/publishDiagnostics" => {
                                if let Some(document) = params["uri"].as_str() {
                                    watch.diagnostics.insert(document.to_string(), params["diagnostics"].as_array().cloned().unwrap_or_default());
                                }
                            }
                            _ => {}
                        }
                        drop(watch);
                        changed.notify_waiters();
                    }
                    (None, None) => {}
                }
            }
            watch.lock().unwrap().closed = Some("the language server stopped".to_string());
            for (_, sender) in pending.lock().unwrap().drain() {
                let _ = sender.send(Err(RpcError { code: 0, message: "the language server stopped".to_string() }));
            }
            changed.notify_waiters();
        });

        let capabilities = json!({
            "general": { "positionEncodings": ["utf-16"] },
            "window": { "workDoneProgress": true },
            "workspace": { "configuration": true, "workspaceFolders": true, "symbol": {} },
            "textDocument": {
                "synchronization": { "didSave": false },
                "definition": { "linkSupport": true },
                "implementation": { "linkSupport": true },
                "references": {},
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "publishDiagnostics": { "relatedInformation": false },
                "diagnostic": {},
            },
            // rust-analyzer says when it is done for now, which is the surest sign it can answer.
            "experimental": { "serverStatusNotification": true },
        });
        let initialized = client
            .request(
                "initialize",
                json!({ "processId": std::process::id(), "rootUri": uri(root), "workspaceFolders": folder, "capabilities": capabilities, "initializationOptions": settings(language), "clientInfo": { "name": "OmniDoppelganger" } }),
                Duration::from_secs(60),
            )
            .await
            .map_err(|error| format!("{} would not start: {}", language.program, error.message))?;
        *client.capabilities.lock().unwrap() = initialized["capabilities"].clone();
        send(&client.writer, &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })).await?;
        Ok(client)
    }

    /// Ask the server something and wait for its answer, at most `wait`.
    pub async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, RpcError> {
        if let Some(why) = self.watch.lock().unwrap().closed.clone() {
            return Err(RpcError { code: 0, message: why });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        if let Err(message) = send(&self.writer, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError { code: 0, message });
        }
        match tokio::time::timeout(wait, receiver).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(RpcError { code: 0, message: "the language server stopped".to_string() }),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let _ = send(&self.writer, &json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } })).await;
                Err(RpcError { code: 0, message: format!("no answer to {method} within {} seconds", wait.as_secs()) })
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        send(&self.writer, &json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    pub fn closed(&self) -> bool {
        self.watch.lock().unwrap().closed.is_some()
    }

    /// Whether the server is at work: indexing, loading the project, or not yet said to be done.
    fn busy(&self) -> bool {
        let watch = self.watch.lock().unwrap();
        watch.closed.is_none() && (!watch.progress.is_empty() || watch.quiescent == Some(false) || (self.language.program == "rust-analyzer" && watch.quiescent.is_none()))
    }

    /// Wait, at most `most`, until the server has been quiet a moment: no work under way. Returns whether it is.
    pub async fn settle(&self, most: Duration, cancel: &CancellationToken) -> bool {
        const QUIET: Duration = Duration::from_millis(400);
        let deadline = tokio::time::Instant::now() + most;
        loop {
            let notified = self.changed.notified();
            let wait = if self.busy() { deadline.saturating_duration_since(tokio::time::Instant::now()) } else { QUIET.min(deadline.saturating_duration_since(tokio::time::Instant::now())) };
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(wait) => {
                    if !self.busy() || tokio::time::Instant::now() >= deadline {
                        return !self.busy();
                    }
                }
                () = cancel.cancelled() => return false,
            }
            if tokio::time::Instant::now() >= deadline {
                return !self.busy();
            }
        }
    }

    /// Give the server the file as it is on disk: opened the first time, its whole text again when it changed.
    /// Returns the document's URI and text.
    async fn sync(&self, path: &Path) -> Result<(String, String), String> {
        let text = String::from_utf8_lossy(&std::fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?).into_owned();
        let document = uri(path);
        let mut open = self.open.lock().await;
        match open.get(&document) {
            Some((_, sent)) if *sent == text => {}
            Some((version, _)) => {
                let version = version + 1;
                // What the server published for the old text no longer holds.
                self.watch.lock().unwrap().diagnostics.remove(&document);
                self.notify("textDocument/didChange", json!({ "textDocument": { "uri": document, "version": version }, "contentChanges": [{ "text": text }] })).await?;
                open.insert(document.clone(), (version, text.clone()));
            }
            None => {
                self.notify("textDocument/didOpen", json!({ "textDocument": { "uri": document, "languageId": language_id(path), "version": 1, "text": text } })).await?;
                open.insert(document.clone(), (1, text.clone()));
            }
        }
        Ok((document, text))
    }

    /// The diagnostics of a document: asked for where the server answers that, or else as it publishes them, waited
    /// for at most `most`.
    async fn diagnostics(&self, document: &str, most: Duration, cancel: &CancellationToken) -> Result<Vec<Value>, String> {
        let pulled = self.capabilities.lock().unwrap().get("diagnosticProvider").is_some_and(|provider| !provider.is_null());
        if pulled {
            let answer = self.request("textDocument/diagnostic", json!({ "textDocument": { "uri": document } }), Duration::from_secs(30)).await.map_err(|error| error.message)?;
            return Ok(answer["items"].as_array().cloned().unwrap_or_default());
        }
        let deadline = tokio::time::Instant::now() + most;
        loop {
            let notified = self.changed.notified();
            if let Some(found) = self.watch.lock().unwrap().diagnostics.get(document) {
                return Ok(found.clone());
            }
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep_until(deadline) => return Ok(Vec::new()),
                () = cancel.cancelled() => return Ok(Vec::new()),
            }
        }
    }
}

/// The language servers running, one per workspace and language, shared by every session.
#[derive(Default)]
pub struct LspHub {
    clients: tokio::sync::Mutex<HashMap<(PathBuf, &'static str), Arc<Client>>>,
}

impl LspHub {
    /// The server for `language` in `root`: the one running, or a new one, started now.
    pub async fn client(&self, root: &Path, language: &'static Language) -> Result<Arc<Client>, String> {
        let mut clients = self.clients.lock().await;
        let key = (root.to_path_buf(), language.program);
        if let Some(client) = clients.get(&key).filter(|client| !client.closed()) {
            return Ok(client.clone());
        }
        let program = installed(language.program).ok_or_else(|| {
            format!("{} is not installed, so {} code cannot be navigated here. Ask the user to install it ({}), or use `search` instead.", language.program, language.name, language.install)
        })?;
        let mut command = tokio::process::Command::new(program);
        command.args(language.args).envs(language.env.iter().copied()).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
        let mut child = command.spawn().map_err(|error| format!("Cannot start {}: {error}", language.program))?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(format!("Cannot talk to {}.", language.program));
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
                        let cut = (excess..said.len()).find(|at| said.is_char_boundary(*at)).unwrap_or(said.len());
                        said.drain(..cut);
                    }
                }
            });
        }
        let client = match Client::start(language, root, Box::new(stdout), Box::new(stdin), Some(child)).await {
            Ok(client) => client,
            Err(error) => {
                // A moment for the last of what it said to be read.
                tokio::time::sleep(Duration::from_millis(200)).await;
                let said = said.lock().unwrap().trim().to_string();
                let said = if said.is_empty() { String::new() } else { format!(" It said: {said}") };
                return Err(format!("{error}.{said} If it is not installed, ask the user to install it ({}), or use `search` instead.", language.install));
            }
        };
        clients.insert(key, client.clone());
        Ok(client)
    }

    /// End every server.
    pub fn shutdown(&self) {
        if let Ok(mut clients) = self.clients.try_lock() {
            clients.clear();
        }
    }

    #[cfg(test)]
    pub async fn insert(&self, root: &Path, client: Arc<Client>) {
        self.clients.lock().await.insert((root.to_path_buf(), client.language.program), client);
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
pub const OPERATIONS: &[(&str, bool)] = &[("definition", true), ("references", true), ("implementations", true), ("hover", true), ("symbols", false), ("find_symbol", false), ("diagnostics", false)];

/// The place in a file `code_nav` was pointed at: its line (from 1), and a column found by the symbol named on it,
/// or given (from 1). As LSP wants it: the line from 0, the column in UTF-16 code units.
fn position(text: &str, input: &Value, shown: &str) -> Result<(u64, usize), String> {
    let line = input["line"].as_u64().filter(|line| *line >= 1).ok_or("`line` is required for this operation: the line (from 1) where the symbol is.")?;
    let content = text.lines().nth((line - 1) as usize).ok_or_else(|| format!("{shown} has no line {line}; it has {}.", text.lines().count()))?;
    let column = match (input["symbol"].as_str().map(str::trim).filter(|symbol| !symbol.is_empty()), input["column"].as_u64()) {
        (Some(symbol), _) => {
            // The symbol as a whole word: `id` is not found inside `width`.
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let found = content.match_indices(symbol).find(|(at, _)| {
                let before = content[..*at].chars().next_back();
                let after = content[at + symbol.len()..].chars().next();
                !before.is_some_and(word) && !after.is_some_and(word)
            });
            let (at, _) = found.or_else(|| content.match_indices(symbol).next()).ok_or_else(|| format!("`{symbol}` is not on line {line} of {shown}, which reads: {}", content.trim()))?;
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
        Self { workspace, files: HashMap::new() }
    }

    fn line(&mut self, path: &Path, line: usize) -> String {
        let lines = self.files.entry(path.to_path_buf()).or_insert_with(|| std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).lines().map(str::to_string).collect()).unwrap_or_default());
        lines.get(line).cloned().unwrap_or_default()
    }

    /// `path:line:column  the line`, for a URI and an LSP range or position.
    fn place(&mut self, document: &str, start: &Value) -> Option<(String, String)> {
        let path = path_of(document)?;
        let line = start["line"].as_u64()? as usize;
        let content = self.line(&path, line);
        let column = from_utf16(&content, start["character"].as_u64().unwrap_or(0) as usize);
        let shown = crate::tools::display(self.workspace, &path);
        Some((format!("{shown}:{}:{}", line + 1, column + 1), content.trim().to_string()))
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
            let start = if item["targetSelectionRange"].is_object() { &item["targetSelectionRange"]["start"] } else { &item["range"]["start"] };
            Some((document.to_string(), start.clone()))
        })
        .collect()
}

/// The text a hover answer holds, however the server shaped it.
fn hover_text(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().map(hover_text).filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n"),
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
        "file", "module", "namespace", "package", "class", "method", "property", "field", "constructor", "enum", "interface", "function", "variable", "constant", "string", "number", "boolean", "array", "object", "key", "null", "enum member", "struct", "event", "operator", "type parameter",
    ];
    KINDS.get((kind as usize).wrapping_sub(1)).copied().unwrap_or("symbol")
}

/// A file's outline, from `DocumentSymbol`s (nested) or `SymbolInformation`s (flat), one symbol a line.
fn outline(symbols: &[Value], depth: usize, lines: &mut Vec<String>) {
    for symbol in symbols {
        let start = if symbol["selectionRange"].is_object() { &symbol["selectionRange"]["start"] } else if symbol["range"].is_object() { &symbol["range"]["start"] } else { &symbol["location"]["range"]["start"] };
        let line = start["line"].as_u64().unwrap_or(0) + 1;
        let detail = symbol["detail"].as_str().map(str::trim).filter(|detail| !detail.is_empty()).map(|detail| format!("  {detail}")).unwrap_or_default();
        lines.push(format!("{}{} {}{detail}  (line {line})", "  ".repeat(depth), kind_name(symbol["kind"].as_u64().unwrap_or(0)), symbol["name"].as_str().unwrap_or("?")));
        if let Some(children) = symbol["children"].as_array() {
            outline(children, depth + 1, lines);
        }
    }
}

/// Answer `code_nav`: start or find the server for the file's language, give it the file, and ask.
pub async fn navigate(hub: &LspHub, workspace: &Path, input: &Value, cancel: &CancellationToken) -> Result<crate::tools::ToolOutput, String> {
    let operation = input["operation"].as_str().unwrap_or_default();
    let Some((_, placed)) = OPERATIONS.iter().find(|(name, _)| *name == operation) else {
        return Err(format!("`operation` must be one of {}.", OPERATIONS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")));
    };
    let path = crate::tools::resolve(workspace, input["path"].as_str().filter(|path| !path.trim().is_empty()).ok_or("`path` is required: the file to ask about, or for `find_symbol` any file of the language.")?);
    let asked = Asked { operation, shown: crate::tools::display(workspace, &path), path };
    let language = language_for(&asked.path).ok_or_else(|| {
        let known: Vec<String> = LANGUAGES.iter().map(|language| format!("{} (.{})", language.name, language.extensions.join(", ."))).collect();
        format!("{} is not of a language navigated here: {}. Use `search` instead.", asked.shown, known.join("; "))
    })?;
    if !asked.path.is_file() {
        return Err(format!("{} is not a file.", asked.shown));
    }
    let client = hub.client(workspace, language).await?;
    // The first question waits for the project to be indexed; later ones only for the server to catch up.
    let first = !client.settled.swap(true, Ordering::Relaxed);
    let ready = client.settle(if first { FIRST_SETTLE } else { LATER_SETTLE }, cancel).await;
    if cancel.is_cancelled() {
        return Err("Cancelled.".to_string());
    }
    let (document, text) = client.sync(&asked.path).await?;
    let place = if *placed { Some(position(&text, input, &asked.shown)?) } else { None };
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
        "symbols" => Some(("textDocument/documentSymbol", json!({ "textDocument": { "uri": document } }))),
        "find_symbol" => {
            let query = input["query"].as_str().map(str::trim).filter(|query| !query.is_empty()).ok_or("`query` is required for `find_symbol`: the name, or part of it, to look for.")?;
            Some(("workspace/symbol", json!({ "query": query })))
        }
        _ => None,
    };
    let mut lines = Lines::new(workspace);
    let mut meta = json!({ "operation": asked.operation, "path": asked.shown, "server": language.program });
    if let Some((line, _)) = place {
        meta["line"] = json!(line + 1);
    }
    let still = if ready { "" } else { "\n[The language server was still indexing the project: the answer may be incomplete. Asking again in a while may find more.]" };

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
                let code = diagnostic["code"].as_str().map(str::to_string).or_else(|| diagnostic["code"].as_i64().map(|code| code.to_string())).map(|code| format!(" [{code}]")).unwrap_or_default();
                let message = diagnostic["message"].as_str().unwrap_or_default().trim().replace('\n', "\n    ");
                Some((level, format!("{} {place}{code}: {message}\n    {content}", severity(level))))
            })
            .collect();
        listed.sort_by_key(|(level, _)| *level);
        meta["results"] = json!(listed.len());
        meta["errors"] = json!(listed.iter().filter(|(level, _)| *level == 1).count());
        let note = if language.program == "rust-analyzer" { "\n[rust-analyzer here runs no `cargo check`, so only what it finds by itself is listed: run `cargo check` for the compiler's full say.]" } else { "" };
        let content = if listed.is_empty() {
            format!("{} has no errors or warnings that {} reports.{note}{still}", asked.shown, language.program)
        } else {
            format!("{} problems in {}:\n{}{note}{still}", listed.len(), asked.shown, listed.into_iter().map(|(_, line)| line).take(LISTED).collect::<Vec<_>>().join("\n"))
        };
        return Ok(crate::tools::ToolOutput::ok(content, meta));
    };

    // The code may change under the question while the server reads it: then it is asked again.
    let mut answer = Value::Null;
    for attempt in 0..3 {
        match client.request(method, params.clone(), Duration::from_secs(60)).await {
            Ok(got) => {
                answer = got;
                break;
            }
            Err(error) if matches!(error.code, CONTENT_MODIFIED | SERVER_CANCELLED) && attempt < 2 => {
                client.settle(LATER_SETTLE, cancel).await;
            }
            Err(error) => return Err(format!("{} could not answer: {}", language.program, error.message)),
        }
    }

    let content = match asked.operation {
        "hover" => {
            let text = hover_text(&answer["contents"]);
            meta["results"] = json!(usize::from(!text.trim().is_empty()));
            if text.trim().is_empty() {
                format!("{} has nothing to say about that place.{still}", language.program)
            } else {
                let text: String = text.trim().chars().take(4000).collect();
                text + still
            }
        }
        "symbols" => {
            let mut listed = Vec::new();
            outline(answer.as_array().map(Vec::as_slice).unwrap_or_default(), 0, &mut listed);
            meta["results"] = json!(listed.len());
            if listed.is_empty() { format!("{} found no symbols in {}.{still}", language.program, asked.shown) } else { format!("{}:\n{}{still}", asked.shown, listed.join("\n")) }
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
                let place = if start.is_object() { lines.place(document, start).map(|(place, _)| place) } else { path_of(document).map(|path| crate::tools::display(workspace, &path)) };
                let container = symbol["containerName"].as_str().filter(|name| !name.is_empty()).map(|name| format!(" in {name}")).unwrap_or_default();
                listed.push(format!("{} {}{container}  {}", kind_name(symbol["kind"].as_u64().unwrap_or(0)), symbol["name"].as_str().unwrap_or("?"), place.unwrap_or_default()));
                if listed.len() == LISTED {
                    break;
                }
            }
            meta["results"] = json!(listed.len());
            if listed.is_empty() { format!("No symbol in the workspace matches `{}`.{still}", params["query"].as_str().unwrap_or_default()) } else { listed.join("\n") + still }
        }
        _ => {
            let found = locations(&answer);
            meta["results"] = json!(found.len());
            let mut listed: Vec<String> = found.iter().take(LISTED).filter_map(|(document, start)| lines.place(document, start)).map(|(place, content)| format!("{place}  {content}")).collect();
            if found.len() > LISTED {
                listed.push(format!("… and {} more", found.len() - LISTED));
            }
            let what = match asked.operation {
                "definition" => "Defined at",
                "implementations" => "Implemented at",
                _ => "Used at",
            };
            if listed.is_empty() { format!("{} found none.{still}", language.program) } else { format!("{what} ({}):\n{}{still}", found.len(), listed.join("\n")) }
        }
    };
    Ok(crate::tools::ToolOutput::ok(content, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_paths_into_uris_and_back() {
        let path = Path::new("/work/my project/src/日志.rs");
        let document = uri(path);
        assert_eq!(document, "file:///work/my%20project/src/%E6%97%A5%E5%BF%97.rs");
        assert_eq!(path_of(&document).unwrap(), path);
        assert_eq!(path_of("untitled:Untitled-1"), None);
        assert_eq!(language_for(Path::new("a/B.TSX")).map(|language| language.program), Some("typescript-language-server"));
        assert!(language_for(Path::new("README.md")).is_none());
    }

    #[test]
    fn counts_columns_in_utf16_as_lsp_does() {
        // An emoji is two UTF-16 units, a Chinese character one.
        let line = "let 名 = \"😀\"; x";
        let x = line.chars().count() - 1;
        assert_eq!(to_utf16(line, x), x + 1);
        assert_eq!(from_utf16(line, x + 1), x);
        assert_eq!(from_utf16(line, 999), line.chars().count());
    }

    #[test]
    fn finds_the_place_by_the_symbol_named_on_the_line() {
        let text = "fn width() {}\nlet w = width(id);\n";
        // The whole word: `id` in `width` is passed over for the `id` after it.
        assert_eq!(position(text, &json!({ "line": 2, "symbol": "id" }), "a.rs"), Ok((1, 14)));
        assert_eq!(position(text, &json!({ "line": 2, "symbol": "width" }), "a.rs"), Ok((1, 8)));
        assert_eq!(position(text, &json!({ "line": 1, "column": 4 }), "a.rs"), Ok((0, 3)));
        assert!(position(text, &json!({ "line": 2, "symbol": "height" }), "a.rs").unwrap_err().contains("which reads: let w = width(id);"));
        assert!(position(text, &json!({ "line": 9, "symbol": "w" }), "a.rs").unwrap_err().contains("has no line 9"));
        assert!(position(text, &json!({ "symbol": "w" }), "a.rs").unwrap_err().contains("`line` is required"));
    }

    #[test]
    fn reads_every_shape_an_answer_comes_in() {
        let link = json!({ "targetUri": "file:///w/a.rs", "targetRange": {}, "targetSelectionRange": { "start": { "line": 3, "character": 1 } } });
        let location = json!({ "uri": "file:///w/b.rs", "range": { "start": { "line": 0, "character": 0 } } });
        assert_eq!(locations(&json!([link, location])).len(), 2);
        assert_eq!(locations(&location)[0].0, "file:///w/b.rs");
        assert!(locations(&Value::Null).is_empty());
        assert_eq!(hover_text(&json!({ "kind": "markdown", "value": "```rust\nfn f()\n```" })), "```rust\nfn f()\n```");
        assert_eq!(hover_text(&json!([{ "language": "rust", "value": "fn f()" }, "docs"])), "```rust\nfn f()\n```\n\ndocs");
        let mut lines = Vec::new();
        outline(&[json!({ "name": "S", "kind": 23, "selectionRange": { "start": { "line": 0 } }, "children": [{ "name": "f", "kind": 6, "detail": "fn()", "selectionRange": { "start": { "line": 1 } } }] })], 0, &mut lines);
        assert_eq!(lines, ["struct S  (line 1)", "  method f  fn()  (line 2)"]);
    }

    /// A server on the other end of an in-memory pipe: it says it is indexing, then done, publishes a diagnostic for
    /// the file opened, asks its client one thing, and answers `definition` with the file's first line.
    async fn scripted(root: &Path, configured: Arc<Mutex<Value>>) -> Arc<Client> {
        let (client_side, server_side) = tokio::io::duplex(1 << 16);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, server_write) = tokio::io::split(server_side);
        let writer: Writer = Arc::new(tokio::sync::Mutex::new(Box::new(server_write)));
        tokio::spawn(async move {
            let mut reader = BufReader::new(Box::new(server_read) as Box<dyn AsyncRead + Send + Unpin>);
            while let Some(message) = receive(&mut reader).await {
                let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
                match message["method"].as_str() {
                    Some("initialize") => send(&writer, &reply(json!({ "capabilities": { "definitionProvider": true } }))).await.unwrap(),
                    Some("initialized") => {
                        send(&writer, &json!({ "jsonrpc": "2.0", "id": 900, "method": "workspace/configuration", "params": { "items": [{ "section": "rust-analyzer" }] } })).await.unwrap();
                        send(&writer, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "index", "value": { "kind": "begin", "title": "Indexing" } } })).await.unwrap();
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        send(&writer, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "index", "value": { "kind": "end" } } })).await.unwrap();
                        send(&writer, &json!({ "jsonrpc": "2.0", "method": "experimental/serverStatus", "params": { "quiescent": true, "health": "ok" } })).await.unwrap();
                    }
                    Some("textDocument/didOpen") => {
                        let document = message["params"]["textDocument"]["uri"].clone();
                        let diagnostic = json!({ "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 5 } }, "severity": 1, "code": "E0425", "message": "cannot find value `y`" });
                        send(&writer, &json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": document, "diagnostics": [diagnostic] } })).await.unwrap();
                    }
                    Some("textDocument/definition") => {
                        let document = message["params"]["textDocument"]["uri"].clone();
                        send(&writer, &reply(json!([{ "uri": document, "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 4 } } }]))).await.unwrap();
                    }
                    // The answer to the server's own question, kept for the test to see.
                    None if message["id"] == 900 => *configured.lock().unwrap() = message["result"].clone(),
                    _ => {}
                }
            }
        });
        let rust = &LANGUAGES[0];
        Client::start(rust, root, Box::new(client_read), Box::new(client_write), None).await.unwrap()
    }

    #[tokio::test]
    async fn waits_for_indexing_then_answers_with_the_line_it_points_at() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("a.rs"), "fn f() {}\nfn g() { y; f(); }\n").unwrap();
        let hub = LspHub::default();
        let configured = Arc::new(Mutex::new(Value::Null));
        let client = scripted(root, configured.clone()).await;
        hub.insert(root, client.clone()).await;
        let cancel = CancellationToken::new();
        // rust-analyzer's own word counts: until it says it is done, it is at work.
        assert!(client.busy());
        let defined = navigate(&hub, root, &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }), &cancel).await.unwrap();
        assert_eq!(defined.content, "Defined at (1):\na.rs:1:4  fn f() {}");
        assert_eq!((defined.meta["results"].as_u64(), defined.meta["line"].as_u64()), (Some(1), Some(2)));
        assert!(!client.busy());
        let found = navigate(&hub, root, &json!({ "operation": "diagnostics", "path": "a.rs" }), &cancel).await.unwrap();
        assert!(found.content.starts_with("1 problems in a.rs:\nerror a.rs:2:5 [E0425]: cannot find value `y`\n    fn g() { y; f(); }"), "{}", found.content);
        assert_eq!(found.meta["errors"], 1);
        // What it asked for its settings was answered with them: no build scripts, no proc macros, no `cargo check`.
        assert_eq!(configured.lock().unwrap()[0]["procMacro"]["enable"], false);
        assert_eq!(configured.lock().unwrap()[0]["checkOnSave"], false);
        let refused = navigate(&hub, root, &json!({ "operation": "rename", "path": "a.rs" }), &cancel).await.unwrap_err();
        assert!(refused.contains("`operation` must be one of"), "{refused}");
        let other = navigate(&hub, root, &json!({ "operation": "symbols", "path": "notes.md" }), &cancel).await.unwrap_err();
        assert!(other.contains("is not of a language navigated here"), "{other}");
    }

    /// Whether a language server can be run here: on the `PATH`, and answering (rustup's stand-in for a component
    /// not installed is on the `PATH` but fails).
    fn runs(program: &str) -> bool {
        installed(program).is_some() && std::process::Command::new(program).arg("--version").output().is_ok_and(|output| output.status.success())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn navigates_a_real_rust_project_with_rust_analyzer() {
        if !runs("rust-analyzer") {
            eprintln!("rust-analyzer is not installed here: skipped");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n").unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "mod broken;\n\npub struct Counter {\n    pub count: u32,\n}\n\n/// Adds one.\npub fn helper(counter: &Counter) -> u32 {\n    counter.count + 1\n}\n\npub fn caller() -> u32 {\n    let counter = Counter { count: 1 };\n    helper(&counter) + helper(&counter)\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/broken.rs"), "pub fn oops( {\n}\n").unwrap();
        let hub = LspHub::default();
        let cancel = CancellationToken::new();
        let ask = |input: Value| {
            let (hub, root, cancel) = (&hub, &root, &cancel);
            async move { navigate(hub, root, &input, cancel).await.unwrap() }
        };

        let defined = ask(json!({ "operation": "definition", "path": "src/lib.rs", "line": 14, "symbol": "helper" })).await;
        assert!(defined.content.starts_with("Defined at (1):\nsrc/lib.rs:8:8  pub fn helper(counter: &Counter) -> u32 {"), "{}", defined.content);
        let used = ask(json!({ "operation": "references", "path": "src/lib.rs", "line": 8, "symbol": "helper" })).await;
        assert_eq!(used.meta["results"], 3, "{}", used.content);
        let hover = ask(json!({ "operation": "hover", "path": "src/lib.rs", "line": 14, "symbol": "helper" })).await;
        assert!(hover.content.contains("fn helper(counter: &Counter) -> u32") && hover.content.contains("Adds one."), "{}", hover.content);
        let outline = ask(json!({ "operation": "symbols", "path": "src/lib.rs" })).await;
        assert!(outline.content.contains("struct Counter  (line 3)") && outline.content.contains("function helper"), "{}", outline.content);
        let found = ask(json!({ "operation": "find_symbol", "path": "src/lib.rs", "query": "Counter" })).await;
        assert!(found.content.contains("struct Counter") && found.content.contains("src/lib.rs:3:"), "{}", found.content);
        let broken = ask(json!({ "operation": "diagnostics", "path": "src/broken.rs" })).await;
        assert!(broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error src/broken.rs:1:"), "{}", broken.content);
        // A file changed on disk is given to the server again before it is asked.
        std::fs::write(root.join("src/broken.rs"), "pub fn oops() {}\n").unwrap();
        let mended = ask(json!({ "operation": "diagnostics", "path": "src/broken.rs" })).await;
        assert_eq!(mended.meta["errors"], 0, "{}", mended.content);
        hub.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn navigates_a_real_c_project_with_clangd() {
        if !runs("clangd") {
            eprintln!("clangd is not installed here: skipped");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(
            root.join("counter.c"),
            "struct counter {\n    int count;\n};\n\n/* Adds one. */\nint helper(struct counter *c) {\n    return c->count + 1;\n}\n\nint caller(void) {\n    struct counter c = { 1 };\n    return helper(&c) + helper(&c);\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("broken.c"), "int oops( {\n}\n").unwrap();
        let hub = LspHub::default();
        let cancel = CancellationToken::new();
        let ask = |input: Value| {
            let (hub, root, cancel) = (&hub, &root, &cancel);
            async move { navigate(hub, root, &input, cancel).await.unwrap() }
        };
        let defined = ask(json!({ "operation": "definition", "path": "counter.c", "line": 12, "symbol": "helper" })).await;
        assert!(defined.content.starts_with("Defined at (1):\ncounter.c:6:5  int helper(struct counter *c) {"), "{}", defined.content);
        let used = ask(json!({ "operation": "references", "path": "counter.c", "line": 6, "symbol": "helper" })).await;
        assert_eq!(used.meta["results"], 3, "{}", used.content);
        let hover = ask(json!({ "operation": "hover", "path": "counter.c", "line": 12, "symbol": "helper" })).await;
        assert!(hover.content.contains("int helper(struct counter *c)") && hover.content.contains("Adds one."), "{}", hover.content);
        let outline = ask(json!({ "operation": "symbols", "path": "counter.c" })).await;
        // clangd calls a C struct a class, with `struct` as its detail.
        assert!(outline.content.contains("counter  struct  (line 1)") && outline.content.contains("  field count  int  (line 2)") && outline.content.contains("function helper"), "{}", outline.content);
        let broken = ask(json!({ "operation": "diagnostics", "path": "broken.c" })).await;
        assert!(broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error broken.c:1:"), "{}", broken.content);
        // A file changed on disk is given to the server again before it is asked.
        std::fs::write(root.join("broken.c"), "int oops(void) {\n    return 0;\n}\n").unwrap();
        let mended = ask(json!({ "operation": "diagnostics", "path": "broken.c" })).await;
        assert_eq!(mended.meta["errors"], 0, "{}", mended.content);
        hub.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_will_not_start_says_why_and_how_to_install_it() {
        // rustup's stand-in for rust-analyzer when the component is not installed is the case to cover; a program
        // that exits at once stands in for it everywhere.
        static FAILING: Language = Language { name: "Test", program: "false", args: &[], extensions: &["test"], install: "install it somehow", runs_project_code: false, env: &[] };
        if installed("false").is_none() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let error = LspHub::default().client(directory.path(), &FAILING).await.err().unwrap();
        assert!(error.contains("false would not start") && error.contains("install it somehow"), "{error}");
    }
}
