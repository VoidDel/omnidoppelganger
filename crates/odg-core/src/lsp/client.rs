//! JSON-RPC transport and running LSP client state.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use super::servers::{language_id, settings};
use super::uri;

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

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;
pub(super) type Writer = Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// One running language server.
pub struct Client {
    /// The program that serves: the language's own, or the one downloaded in its place.
    pub(super) program: &'static str,
    writer: Writer,
    next: AtomicI64,
    pending: Pending,
    watch: Arc<Mutex<Watch>>,
    changed: Arc<Notify>,
    /// The documents the server holds open, by URI: the version and the text it was last sent.
    open: tokio::sync::Mutex<HashMap<String, (i64, String)>>,
    capabilities: Mutex<Value>,
    /// Whether the server has once been waited on until it was done indexing.
    pub(super) settled: AtomicBool,
    _child: Option<tokio::process::Child>,
}

/// Write one LSP message: a `Content-Length` header and the JSON.
pub(super) async fn send(writer: &Writer, message: &Value) -> Result<(), String> {
    let body = message.to_string();
    let mut writer = writer.lock().await;
    writer.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.map_err(|error| format!("the server stopped reading: {error}"))?;
    writer.flush().await.map_err(|error| format!("the server stopped reading: {error}"))
}

/// Read one LSP message; `None` when the stream ended.
pub(super) async fn receive(reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>) -> Option<Value> {
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
        program: &'static str,
        root: &Path,
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        child: Option<tokio::process::Child>,
    ) -> Result<Arc<Self>, String> {
        let client = Arc::new(Client {
            program,
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
                                json!({ "jsonrpc": "2.0", "id": id, "result": items.iter().map(|item| if item["section"].as_str() == Some(program) { settings(program) } else { Value::Null }).collect::<Vec<_>>() })
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
                json!({ "processId": std::process::id(), "rootUri": uri(root), "workspaceFolders": folder, "capabilities": capabilities, "initializationOptions": settings(program), "clientInfo": { "name": "OmniDoppelganger" } }),
                Duration::from_secs(60),
            )
            .await
            .map_err(|error| format!("{program} would not start: {}", error.message))?;
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
    pub(super) fn busy(&self) -> bool {
        let watch = self.watch.lock().unwrap();
        watch.closed.is_none() && (!watch.progress.is_empty() || watch.quiescent == Some(false) || (self.program == "rust-analyzer" && watch.quiescent.is_none()))
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
    pub(super) async fn sync(&self, path: &Path) -> Result<(String, String), String> {
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
    pub(super) async fn diagnostics(&self, document: &str, most: Duration, cancel: &CancellationToken) -> Result<Vec<Value>, String> {
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
