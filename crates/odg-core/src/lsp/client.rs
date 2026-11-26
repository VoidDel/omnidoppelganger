//! JSON-RPC transport and running LSP client state.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::sync_ext::MutexExt;

use super::servers::{language_id, settings};
use super::uri;

/// Maximum JSON-RPC message body size accepted from a language server (64 MB).
pub(super) const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// Maximum single source file size synchronized to a language server via `didOpen`/`didChange` (10 MB).
pub(super) const MAX_SYNC_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum number of documents kept open simultaneously per language server before LRU `didClose`.
pub(super) const MAX_OPEN_DOCUMENTS: usize = 64;

/// Synchronous guard that terminates an entire LSP child process group (including grandchildren
/// such as `node tsserver.js`) when shut down or dropped.
pub(super) struct ProcessGroupGuard(pub(super) Option<u32>);

impl ProcessGroupGuard {
    pub(super) fn kill_now(&mut self) {
        let Some(pid) = self.0.take() else { return };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// What a server said went wrong with a request.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// The server answered that the code changed under the request: worth asking again.
pub(super) const CONTENT_MODIFIED: i64 = -32801;
pub(super) const SERVER_CANCELLED: i64 = -32802;

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

struct OpenDoc {
    version: i64,
    text: String,
    last_used: u64,
}

#[derive(Default)]
struct OpenState {
    tick: u64,
    docs: HashMap<String, OpenDoc>,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;
#[cfg(test)]
pub(super) type Writer = Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// One running language server.
pub struct Client {
    /// The program that serves: the language's own, or the one downloaded in its place.
    pub(super) program: &'static str,
    outbound_tx: mpsc::UnboundedSender<Value>,
    next: AtomicI64,
    pending: Pending,
    watch: Arc<Mutex<Watch>>,
    changed: Arc<Notify>,
    /// The documents the server holds open, by URI, bounded by [`MAX_OPEN_DOCUMENTS`] via LRU `didClose`.
    open: tokio::sync::Mutex<OpenState>,
    capabilities: Mutex<Value>,
    /// Whether the server has once been waited on until it was done indexing.
    pub(super) settled: AtomicBool,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    group_guard: Arc<Mutex<ProcessGroupGuard>>,
}

/// Write one LSP message on a test stream: a `Content-Length` header and the JSON.
#[cfg(test)]
pub(super) async fn send(writer: &Writer, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    let mut writer = writer.lock().await;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .map_err(|error| format!("the server stopped reading: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("the server stopped reading: {error}"))
}

/// Read one LSP message, enforcing [`MAX_FRAME_BYTES`] (64 MB) on `Content-Length`.
pub(super) async fn receive_checked(
    reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
) -> Result<Option<Value>, String> {
    loop {
        let mut length: Option<usize> = None;
        loop {
            let mut header = String::new();
            let read = reader
                .read_line(&mut header)
                .await
                .map_err(|e| format!("the language server stopped: {e}"))?;
            if read == 0 {
                return Ok(None);
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                let raw_len = value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "invalid LSP Content-Length header".to_string())?;
                if raw_len > MAX_FRAME_BYTES as u64 {
                    return Err(format!(
                        "LSP message Content-Length ({raw_len} bytes) exceeds the {MAX_FRAME_BYTES}-byte limit"
                    ));
                }
                length = Some(raw_len as usize);
            }
        }
        // A message without its length cannot be read; the next one may be.
        let Some(length) = length else { continue };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).await.is_err() {
            return Ok(None);
        }
        if let Ok(message) = serde_json::from_slice(&body) {
            return Ok(Some(message));
        }
    }
}

/// Read one LSP message; `None` when the stream ended or exceeded [`MAX_FRAME_BYTES`].
#[cfg(test)]
pub(super) async fn receive(reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>) -> Option<Value> {
    receive_checked(reader).await.ok().flatten()
}

impl Client {
    /// Start talking to a server on these streams: answer what it asks, note what it says, and initialize it for
    /// `root`. `child` is the process, kept so that dropping the client ends it.
    pub async fn start(
        program: &'static str,
        root: &Path,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<tokio::process::Child>,
    ) -> Result<Arc<Self>, String> {
        let pid = child.as_ref().and_then(tokio::process::Child::id);
        let guard = Arc::new(Mutex::new(ProcessGroupGuard(pid)));
        Self::start_with_guard(program, root, reader, writer, child, guard, &CancellationToken::new()).await
    }

    pub(super) async fn start_with_guard(
        program: &'static str,
        root: &Path,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        mut writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<tokio::process::Child>,
        group_guard: Arc<Mutex<ProcessGroupGuard>>,
        cancel: &CancellationToken,
    ) -> Result<Arc<Self>, String> {
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();
        // Dedicated writer task serializes all outbound frames onto `stdin`, eliminating full-duplex pipe deadlocks
        // when the reader task answers server-initiated requests concurrently with `sync` / `request`.
        tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                let body = message.to_string();
                let header = format!("Content-Length: {}\r\n\r\n", body.len());
                if writer.write_all(header.as_bytes()).await.is_err()
                    || writer.write_all(body.as_bytes()).await.is_err()
                    || writer.flush().await.is_err()
                {
                    break;
                }
            }
        });

        let client = Arc::new(Client {
            program,
            outbound_tx: outbound_tx.clone(),
            next: AtomicI64::new(1),
            pending: Arc::default(),
            watch: Arc::default(),
            changed: Arc::default(),
            open: tokio::sync::Mutex::default(),
            capabilities: Mutex::new(Value::Null),
            settled: AtomicBool::new(false),
            child: tokio::sync::Mutex::new(child),
            group_guard,
        });
        let (pending, watch, changed) = (client.pending.clone(), client.watch.clone(), client.changed.clone());
        let folder = json!([{ "uri": uri(root), "name": root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default() }]);
        let answered_folder = folder.clone();
        let reply_tx = outbound_tx;
        tokio::spawn(async move {
            let folder = answered_folder;
            let mut reader = BufReader::new(reader);
            let close_reason = loop {
                match receive_checked(&mut reader).await {
                    Ok(Some(message)) => {
                        let method = message["method"].as_str();
                        match (method, message.get("id")) {
                            // An answer to a request of ours.
                            (None, Some(id)) => {
                                let Some(sender) = id.as_i64().and_then(|id| pending.lock_unpoisoned().remove(&id))
                                else {
                                    continue;
                                };
                                let answer = match message.get("error") {
                                    Some(error) => Err(RpcError {
                                        code: error["code"].as_i64().unwrap_or(0),
                                        message: error["message"].as_str().unwrap_or_default().to_string(),
                                    }),
                                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                                };
                                let _ = sender.send(answer);
                            }
                            // A request of the server's: enqueued non-blockingly onto `outbound_tx`.
                            (Some(method), Some(id)) => {
                                let answer = match method {
                                    "workspace/configuration" => {
                                        let items = message["params"]["items"].as_array().cloned().unwrap_or_default();
                                        json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "result": items.iter().map(|item| if item["section"].as_str() == Some(program) { settings(program) } else { Value::Null }).collect::<Vec<_>>()
                                        })
                                    }
                                    "workspace/workspaceFolders" => {
                                        json!({ "jsonrpc": "2.0", "id": id, "result": folder })
                                    }
                                    "window/workDoneProgress/create"
                                    | "client/registerCapability"
                                    | "client/unregisterCapability"
                                    | "window/showMessageRequest"
                                    | "workspace/diagnostic/refresh"
                                    | "workspace/semanticTokens/refresh"
                                    | "workspace/inlayHint/refresh"
                                    | "workspace/codeLens/refresh" => {
                                        json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })
                                    }
                                    _ => {
                                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} is not supported") } })
                                    }
                                };
                                let _ = reply_tx.send(answer);
                            }
                            // What the server tells of itself.
                            (Some(method), None) => {
                                let params = &message["params"];
                                let mut watch = watch.lock_unpoisoned();
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
                                            watch.diagnostics.insert(
                                                document.to_string(),
                                                params["diagnostics"].as_array().cloned().unwrap_or_default(),
                                            );
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
                    Ok(None) => break "the language server stopped".to_string(),
                    Err(reason) => break reason,
                }
            };
            {
                let mut w = watch.lock_unpoisoned();
                if w.closed.is_none() {
                    w.closed = Some(close_reason.clone());
                }
            }
            for (_, sender) in pending.lock_unpoisoned().drain() {
                let _ = sender.send(Err(RpcError {
                    code: 0,
                    message: close_reason.clone(),
                }));
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
                json!({
                    "processId": std::process::id(),
                    "rootUri": uri(root),
                    "workspaceFolders": folder,
                    "capabilities": capabilities,
                    "initializationOptions": settings(program),
                    "clientInfo": { "name": "OmniDoppelganger" }
                }),
                Duration::from_secs(60),
                cancel,
            )
            .await
            .map_err(|error| {
                client.close_now();
                format!("{program} would not start: {}", error.message)
            })?;
        *client.capabilities.lock_unpoisoned() = initialized["capabilities"].clone();
        client.enqueue(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }))?;
        Ok(client)
    }

    fn enqueue(&self, message: Value) -> Result<(), String> {
        self.outbound_tx
            .send(message)
            .map_err(|_| "the server stopped reading".to_string())
    }

    /// Synchronously mark the client closed and kill its process group immediately.
    pub(super) fn close_now(&self) {
        {
            let mut watch = self.watch.lock_unpoisoned();
            if watch.closed.is_none() {
                watch.closed = Some("the language server was shut down".to_string());
            }
        }
        for (_, sender) in self.pending.lock_unpoisoned().drain() {
            let _ = sender.send(Err(RpcError {
                code: 0,
                message: "the language server was shut down".to_string(),
            }));
        }
        self.changed.notify_waiters();
        self.group_guard.lock_unpoisoned().kill_now();
    }

    /// Shut down the client and await child process termination so executable file locks are released.
    pub(super) async fn shutdown_and_wait(&self) {
        self.close_now();
        let child = self.child.lock().await.take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    /// Ask the server something and wait for its answer, at most `wait`, aborting immediately on `cancel`.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        if cancel.is_cancelled() {
            return Err(RpcError {
                code: 0,
                message: "Cancelled.".to_string(),
            });
        }
        if let Some(why) = self.watch.lock_unpoisoned().closed.clone() {
            return Err(RpcError { code: 0, message: why });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock_unpoisoned().insert(id, sender);
        if let Err(message) = self.enqueue(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            self.pending.lock_unpoisoned().remove(&id);
            return Err(RpcError { code: 0, message });
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                self.pending.lock_unpoisoned().remove(&id);
                let _ = self.enqueue(json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } }));
                Err(RpcError { code: 0, message: "Cancelled.".to_string() })
            }
            outcome = tokio::time::timeout(wait, receiver) => match outcome {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err(RpcError { code: 0, message: "the language server stopped".to_string() }),
                Err(_) => {
                    self.pending.lock_unpoisoned().remove(&id);
                    let _ = self.enqueue(json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } }));
                    Err(RpcError { code: 0, message: format!("no answer to {method} within {} seconds", wait.as_secs()) })
                }
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.enqueue(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    pub fn closed(&self) -> bool {
        self.watch.lock_unpoisoned().closed.is_some()
    }

    #[cfg(test)]
    pub(super) async fn open_document_count(&self) -> usize {
        self.open.lock().await.docs.len()
    }

    /// Whether the server is at work: indexing, loading the project, or not yet said to be done.
    pub(super) fn busy(&self) -> bool {
        let watch = self.watch.lock_unpoisoned();
        watch.closed.is_none()
            && (!watch.progress.is_empty()
                || watch.quiescent == Some(false)
                || (self.program == "rust-analyzer" && watch.quiescent.is_none()))
    }

    /// Wait, at most `most`, until the server has been quiet a moment: no work under way. Returns whether it is.
    pub async fn settle(&self, most: Duration, cancel: &CancellationToken) -> bool {
        const QUIET: Duration = Duration::from_millis(400);
        let deadline = tokio::time::Instant::now() + most;
        loop {
            let notified = self.changed.notified();
            let wait = if self.busy() {
                deadline.saturating_duration_since(tokio::time::Instant::now())
            } else {
                QUIET.min(deadline.saturating_duration_since(tokio::time::Instant::now()))
            };
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
    /// Reads under the `open` lock, caps single-file size at [`MAX_SYNC_FILE_BYTES`], clears stale diagnostics
    /// before `didOpen` and `didChange`, and evicts least-recently-used documents past [`MAX_OPEN_DOCUMENTS`].
    pub(super) async fn sync(&self, path: &Path) -> Result<(String, String), String> {
        let mut open = self.open.lock().await;
        let owned_path = path.to_path_buf();
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let meta = std::fs::metadata(&owned_path)
                .map_err(|error| format!("Cannot read {}: {error}", owned_path.display()))?;
            if meta.len() > MAX_SYNC_FILE_BYTES {
                return Err(format!(
                    "{} is {} bytes, over the {MAX_SYNC_FILE_BYTES}-byte language server sync limit.",
                    owned_path.display(),
                    meta.len()
                ));
            }
            std::fs::read(&owned_path).map_err(|error| format!("Cannot read {}: {error}", owned_path.display()))
        })
        .await
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))??;

        let text = String::from_utf8_lossy(&bytes).into_owned();
        let document = uri(path);
        open.tick = open.tick.wrapping_add(1);
        let now_tick = open.tick;

        match open.docs.get_mut(&document) {
            Some(doc) if doc.text == text => {
                doc.last_used = now_tick;
            }
            Some(doc) => {
                let version = doc.version + 1;
                // What the server published for the old text no longer holds.
                self.watch.lock_unpoisoned().diagnostics.remove(&document);
                self.notify(
                    "textDocument/didChange",
                    json!({ "textDocument": { "uri": document, "version": version }, "contentChanges": [{ "text": text }] }),
                )?;
                doc.version = version;
                doc.text = text.clone();
                doc.last_used = now_tick;
            }
            None => {
                self.watch.lock_unpoisoned().diagnostics.remove(&document);
                self.notify(
                    "textDocument/didOpen",
                    json!({ "textDocument": { "uri": document, "languageId": language_id(path), "version": 1, "text": text } }),
                )?;
                open.docs.insert(
                    document.clone(),
                    OpenDoc {
                        version: 1,
                        text: text.clone(),
                        last_used: now_tick,
                    },
                );
                while open.docs.len() > MAX_OPEN_DOCUMENTS {
                    let lru_uri = open
                        .docs
                        .iter()
                        .filter(|(u, _)| *u != &document)
                        .min_by_key(|(_, doc)| doc.last_used)
                        .map(|(u, _)| u.clone());
                    let Some(evicted) = lru_uri else { break };
                    open.docs.remove(&evicted);
                    let _ = self.notify("textDocument/didClose", json!({ "textDocument": { "uri": evicted } }));
                }
            }
        }
        Ok((document, text))
    }

    /// The diagnostics of a document: asked for where the server answers that, or else as it publishes them, waited
    /// for at most `most`.
    pub(super) async fn diagnostics(
        &self,
        document: &str,
        most: Duration,
        cancel: &CancellationToken,
    ) -> Result<Vec<Value>, String> {
        let pulled = self
            .capabilities
            .lock()
            .unwrap()
            .get("diagnosticProvider")
            .is_some_and(|provider| !provider.is_null());
        if pulled {
            let answer = self
                .request(
                    "textDocument/diagnostic",
                    json!({ "textDocument": { "uri": document } }),
                    Duration::from_secs(30),
                    cancel,
                )
                .await
                .map_err(|error| error.message)?;
            return Ok(answer["items"].as_array().cloned().unwrap_or_default());
        }
        let deadline = tokio::time::Instant::now() + most;
        loop {
            let notified = self.changed.notified();
            if let Some(found) = self.watch.lock_unpoisoned().diagnostics.get(document) {
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
