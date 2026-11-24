//! A client for the Model Context Protocol: tools from servers the user
//! configured, offered to the model beside the built-in ones.
//!
//! Servers are listed in `mcp.json` under the home directory, in the
//! `mcpServers` shape most MCP servers document. One with a `command` is
//! started as a process and spoken to over its standard streams; one with a
//! `url` is spoken to over HTTP. Each server's tools reach the model as
//! `mcp__<server>__<tool>`, and every call asks the user first (see
//! [`crate::policy`]): what such a tool does is the server's business.
//!
//! Tools are used, and resources: what a server offers to be read (files,
//! documents, database schemas), listed and read with `list_mcp_resources`
//! and `read_mcp_resource`, a read asked about as a call is. A server's
//! prompts are not used, and its requests to the client (sampling, roots)
//! are declined.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::fsutil::write_atomically;
use crate::sse::SseParser;

/// The protocol revision this client asks for. A server may answer with
/// another it supports; the tool calls used here are the same in all of them.
const PROTOCOL_VERSION: &str = "2025-06-18";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a server started as a program has to answer its first requests: `npx` and `uvx` download the
/// server the first time it is started, which takes longer than reaching one over HTTP.
pub const START_TIMEOUT: Duration = Duration::from_secs(120);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// A tool result is replayed in every later request, like a file read.
const RESULT_CHARS: usize = 100_000;
/// What tool names may hold, and how long they may be, across the model providers.
const NAME_CHARS: usize = 64;

/// One entry of `mcpServers`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    /// The program to start, for a server spoken to over its standard streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables for the program, added to the app's own.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The address, for a server spoken to over HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Headers sent with every HTTP request, such as `Authorization`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Switched off by the user: kept in the list, and not connected to. The
    /// field other clients write for the same, so their lists read the same here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

/// Read the text of `mcp.json`: `{"mcpServers": {"<name>": {...}}}`. Empty text holds no servers.
pub fn parse_config(text: &str) -> Result<BTreeMap<String, ServerConfig>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default, rename = "mcpServers")]
        servers: BTreeMap<String, ServerConfig>,
    }
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let file: File = serde_json::from_str(text).map_err(|error| format!("not valid JSON for a server list: {error}"))?;
    for (name, server) in &file.servers {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("the server name `{name}` may only hold letters, digits, `-`, and `_`"));
        }
        let command = server.command.as_deref().is_some_and(|command| !command.trim().is_empty());
        let url = server.url.as_deref().is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"));
        if command == url {
            return Err(format!("the server `{name}` needs either a `command` or an http(s) `url`, and not both"));
        }
    }
    Ok(file.servers)
}

/// A tool a server offers.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    /// The name the server knows it by.
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// What a tool call came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallResult {
    pub content: String,
    pub is_error: bool,
    /// `(media_type, base64)` for each image returned by the tool.
    pub images: Vec<(String, String)>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A connection that carries JSON-RPC messages one per line, as a server
/// started as a process does on its standard streams.
struct LineWire {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Pending,
    /// The last of what the process wrote to stderr, for explaining a failure.
    stderr: Arc<Mutex<String>>,
    /// Why the connection ended, once it has: the server closed it, and what it said as it went.
    closed: Arc<Mutex<Option<String>>>,
    /// Kept so the process ends when the connection is dropped.
    _child: Option<tokio::process::Child>,
}

impl LineWire {
    fn new(reader: impl AsyncRead + Send + Unpin + 'static, writer: impl AsyncWrite + Send + Unpin + 'static, child: Option<tokio::process::Child>) -> Self {
        let writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>> = Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
        let pending: Pending = Arc::default();
        let stderr = Arc::new(Mutex::new(String::new()));
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let (waiting, answering, noted, ended) = (pending.clone(), writer.clone(), stderr.clone(), closed.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Anything that is not a JSON-RPC message (a stray log line) is skipped.
                let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                let id = message["id"].as_u64();
                if message.get("method").is_some() {
                    // The server asks the client something. Only a ping is answered; this client offers nothing else.
                    if message.get("id").is_some() {
                        let reply = match message["method"].as_str() {
                            Some("ping") => json!({ "jsonrpc": "2.0", "id": message["id"], "result": {} }),
                            _ => json!({ "jsonrpc": "2.0", "id": message["id"], "error": { "code": -32601, "message": "this client does not offer that" } }),
                        };
                        let mut writer = answering.lock().await;
                        let _ = writer.write_all(format!("{reply}\n").as_bytes()).await;
                        let _ = writer.flush().await;
                    }
                } else if let Some(sender) = id.and_then(|id| waiting.lock().unwrap().remove(&id)) {
                    let _ = sender.send(outcome(&message));
                }
            }
            // The stream ended: nobody will answer what is still waiting.
            let tail = noted.lock().unwrap().trim().to_string();
            let why = if tail.is_empty() { "the server closed the connection".to_string() } else { format!("the server closed the connection: {tail}") };
            // Said first, so a request that arrives after this finds it.
            *ended.lock().unwrap() = Some(why.clone());
            for (_, sender) in waiting.lock().unwrap().drain() {
                let _ = sender.send(Err(why.clone()));
            }
        });
        Self { writer, pending, stderr, closed, _child: child }
    }

    async fn send(&self, message: &Value) -> Result<(), String> {
        let mut writer = self.writer.lock().await;
        writer.write_all(format!("{message}\n").as_bytes()).await.map_err(|error| format!("cannot write to the server: {error}"))?;
        writer.flush().await.map_err(|error| format!("cannot write to the server: {error}"))
    }
}

/// The result of a JSON-RPC response, or its error as text.
fn outcome(message: &Value) -> Result<Value, String> {
    match message.get("error") {
        Some(error) => Err(error["message"].as_str().map(str::to_string).unwrap_or_else(|| error.to_string())),
        None => Ok(message["result"].clone()),
    }
}

/// A connection over HTTP: each message is a POST, answered with JSON or with an event stream.
struct HttpWire {
    http: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    /// The session the server opened at `initialize`, sent back on every later request.
    session: Mutex<Option<String>>,
    /// The protocol revision agreed on, sent as a header once known.
    version: Mutex<Option<String>>,
}

impl HttpWire {
    /// Post one message. A request (with `id`) returns its response; a notification returns `Null`.
    async fn post(&self, message: &Value) -> Result<Value, String> {
        let mut request = self.http.post(&self.url).header("accept", "application/json, text/event-stream").json(message);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(session) = self.session.lock().unwrap().clone() {
            request = request.header("mcp-session-id", session);
        }
        if let Some(version) = self.version.lock().unwrap().clone() {
            request = request.header("mcp-protocol-version", version);
        }
        let mut response = request.send().await.map_err(|error| format!("cannot reach the server: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body: String = response.text().await.unwrap_or_default().chars().take(300).collect();
            return Err(format!("the server answered HTTP {}{}", status.as_u16(), if body.trim().is_empty() { String::new() } else { format!(": {}", body.trim()) }));
        }
        if let Some(session) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) {
            *self.session.lock().unwrap() = Some(session.to_string());
        }
        let Some(id) = message.get("id") else { return Ok(Value::Null) };
        let streamed = response.headers().get("content-type").and_then(|value| value.to_str().ok()).is_some_and(|kind| kind.starts_with("text/event-stream"));
        if !streamed {
            let answer: Value = response.json().await.map_err(|error| format!("the server's answer is not JSON: {error}"))?;
            return outcome(&answer);
        }
        // The stream may carry other messages first; the response is the one with this request's id.
        let mut parser = SseParser::default();
        while let Some(chunk) = response.chunk().await.map_err(|error| format!("the server's answer broke off: {error}"))? {
            for event in parser.feed(&chunk) {
                if let Ok(answer) = serde_json::from_str::<Value>(&event.data)
                    && answer.get("method").is_none()
                    && answer.get("id") == Some(id)
                {
                    return outcome(&answer);
                }
            }
        }
        Err("the server's answer ended without a response".to_string())
    }
}

enum Wire {
    Lines(LineWire),
    Http(HttpWire),
}

/// A connected server and the tools it offers.
pub struct Server {
    wire: Wire,
    next: AtomicU64,
    pub tools: Vec<McpTool>,
    /// It offers resources to be read.
    pub resources: bool,
}

/// What reading a resource gave: its text, and the pictures among it, in base64 with their types.
#[derive(Debug, Default, PartialEq)]
pub struct ReadResource {
    pub text: String,
    pub images: Vec<(String, String)>,
}

impl Server {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let answer = async {
            match &self.wire {
                Wire::Http(wire) => wire.post(&message).await,
                Wire::Lines(wire) => {
                    let (sender, receiver) = oneshot::channel();
                    wire.pending.lock().unwrap().insert(id, sender);
                    if let Err(error) = wire.send(&message).await {
                        wire.pending.lock().unwrap().remove(&id);
                        // A server that has gone cannot be written to. Why it went, and what it said as it
                        // did, explains more than the failed write; its end is noticed a moment after.
                        for _ in 0..20 {
                            if let Some(why) = wire.closed.lock().unwrap().clone() {
                                return Err(why);
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        return Err(error);
                    }
                    receiver.await.unwrap_or_else(|_| Err("the server closed the connection".to_string()))
                }
            }
        };
        match tokio::time::timeout(timeout, answer).await {
            Ok(answer) => answer,
            Err(_) => {
                if let Wire::Lines(wire) = &self.wire {
                    wire.pending.lock().unwrap().remove(&id);
                }
                Err(format!("the server did not answer `{method}` within {} seconds", timeout.as_secs()))
            }
        }
    }

    async fn notify(&self, method: &str) -> Result<(), String> {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        match &self.wire {
            Wire::Http(wire) => wire.post(&message).await.map(|_| ()),
            Wire::Lines(wire) => wire.send(&message).await,
        }
    }

    /// Shake hands and list the server's tools, each request given `timeout` to be answered.
    async fn open(wire: Wire, timeout: Duration) -> Result<Self, String> {
        let mut server = Self { wire, next: AtomicU64::new(0), tools: Vec::new(), resources: false };
        let hello = json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "omnidoppelganger", "version": env!("CARGO_PKG_VERSION") } });
        let welcome = server.request("initialize", hello, timeout).await?;
        if let (Wire::Http(wire), Some(version)) = (&server.wire, welcome["protocolVersion"].as_str()) {
            *wire.version.lock().unwrap() = Some(version.to_string());
        }
        server.notify("notifications/initialized").await?;
        server.resources = welcome["capabilities"].get("resources").is_some();
        // A server without tools says so by not listing the capability.
        if welcome["capabilities"].get("tools").is_none() {
            return Ok(server);
        }
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let page = server.request("tools/list", params, timeout).await?;
            for tool in page["tools"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let Some(name) = tool["name"].as_str() else { continue };
                let schema = if tool["inputSchema"].is_object() { tool["inputSchema"].clone() } else { json!({ "type": "object" }) };
                server.tools.push(McpTool { name: name.to_string(), description: tool["description"].as_str().unwrap_or_default().to_string(), input_schema: schema });
            }
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(server);
            }
        }
    }

    /// Connect to a server over a pair of streams that carry one message per line.
    pub async fn over(reader: impl AsyncRead + Send + Unpin + 'static, writer: impl AsyncWrite + Send + Unpin + 'static) -> Result<Self, String> {
        Self::open(Wire::Lines(LineWire::new(reader, writer, None)), CONNECT_TIMEOUT).await
    }

    /// Start or reach the server `config` describes.
    pub async fn connect(config: &ServerConfig) -> Result<Self, String> {
        if let Some(url) = &config.url {
            let http = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).build().map_err(|error| error.to_string())?;
            let wire = HttpWire { http, url: url.clone(), headers: config.headers.clone(), session: Mutex::new(None), version: Mutex::new(None) };
            return Self::open(Wire::Http(wire), CONNECT_TIMEOUT).await;
        }
        let program = config.command.as_deref().unwrap_or_default();
        let mut command = tokio::process::Command::new(find_program(program));
        command.args(&config.args).envs(&config.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
        let mut child = command.spawn().map_err(|error| format!("cannot start `{program}`: {error}"))?;
        let (stdin, stdout) = (child.stdin.take().expect("stdin is piped"), child.stdout.take().expect("stdout is piped"));
        let errors = child.stderr.take().expect("stderr is piped");
        let wire = LineWire::new(stdout, stdin, Some(child));
        // Servers log to stderr; the last of it explains a server that exits at once.
        let noted = wire.stderr.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(errors).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = noted.lock().unwrap();
                tail.push_str(&line);
                tail.push('\n');
                let excess = tail.chars().count().saturating_sub(600);
                if excess > 0 {
                    *tail = tail.chars().skip(excess).collect();
                }
            }
        });
        Self::open(Wire::Lines(wire), START_TIMEOUT).await
    }

    /// The resources the server offers, every page of them, at most `most`: each as the server described it
    /// (`uri`, `name`, `mimeType`, `description`). Then its templates of addresses, which name resources by a pattern.
    pub async fn list_resources(&self, most: usize) -> Result<(Vec<Value>, Vec<Value>), String> {
        let mut listed = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let page = self.request("resources/list", params, CONNECT_TIMEOUT).await?;
            listed.extend(page["resources"].as_array().cloned().unwrap_or_default());
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() || listed.len() >= most {
                listed.truncate(most);
                break;
            }
        }
        // Not every server has templates; one that does not may answer with an error.
        let templates = self.request("resources/templates/list", json!({}), CONNECT_TIMEOUT).await.ok().and_then(|page| page["resourceTemplates"].as_array().cloned()).unwrap_or_default();
        Ok((listed, templates))
    }

    /// Read a resource: its text parts joined, its pictures kept as pictures, anything else said to be there.
    pub async fn read_resource(&self, uri: &str, cancel: &CancellationToken) -> Result<ReadResource, String> {
        let answer = tokio::select! {
            answer = self.request("resources/read", json!({ "uri": uri }), CALL_TIMEOUT) => answer?,
            () = cancel.cancelled() => return Err("the read was cancelled".to_string()),
        };
        let mut read = ReadResource::default();
        let mut parts = Vec::new();
        for content in answer["contents"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let kind = content["mimeType"].as_str().unwrap_or_default();
            if let Some(text) = content["text"].as_str() {
                parts.push(text.to_string());
            } else if let Some(blob) = content["blob"].as_str() {
                if crate::events::IMAGE_TYPES.contains(&kind) {
                    read.images.push((kind.to_string(), blob.to_string()));
                    parts.push(format!("[a {kind} picture, shown to you here]"));
                } else {
                    parts.push(format!("[{} bytes of {} that are not text: {}]", blob.len() / 4 * 3, if kind.is_empty() { "data" } else { kind }, content["uri"].as_str().unwrap_or(uri)));
                }
            }
        }
        let mut text = parts.join("\n");
        if text.chars().count() > RESULT_CHARS {
            let total = text.chars().count();
            text = format!("{}\n[... the resource is {total} characters; it is cut at {RESULT_CHARS} ...]", text.chars().take(RESULT_CHARS).collect::<String>());
        }
        read.text = if text.is_empty() { "(the resource is empty)".to_string() } else { text };
        Ok(read)
    }

    /// Call a tool, keeping returned images so multimodal models can inspect screenshots.
    pub async fn call(&self, tool: &str, arguments: &Value, cancel: &CancellationToken) -> CallResult {
        let arguments = if arguments.is_object() { arguments.clone() } else { json!({}) };
        let answer = tokio::select! {
            answer = self.request("tools/call", json!({ "name": tool, "arguments": arguments }), CALL_TIMEOUT) => answer,
            () = cancel.cancelled() => Err("the call was cancelled".to_string()),
        };
        let result = match answer {
            Ok(result) => result,
            Err(error) => return CallResult { content: format!("The tool call failed: {error}"), is_error: true, images: Vec::new() },
        };
        let mut images = Vec::new();
        let parts: Vec<String> = result["content"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|part| match part["type"].as_str() {
                Some("text") => part["text"].as_str().unwrap_or_default().to_string(),
                Some("image") => {
                    let kind = part["mimeType"].as_str().unwrap_or_default();
                    if crate::events::IMAGE_TYPES.contains(&kind)
                        && let Some(data) = part["data"].as_str().filter(|data| !data.is_empty())
                    {
                        images.push((kind.to_string(), data.to_string()));
                        format!("[a {kind} picture, shown to you here]")
                    } else {
                        format!("[{} returned by the tool; it is not shown here]", if kind.is_empty() { "image" } else { kind })
                    }
                }
                Some("audio") => format!("[{} returned by the tool; it is not shown here]", part["mimeType"].as_str().unwrap_or("audio")),
                Some("resource") => {
                    let resource = &part["resource"];
                    if let Some(text) = resource["text"].as_str() {
                        text.to_string()
                    } else if let Some(blob) = resource["blob"].as_str().filter(|blob| !blob.is_empty()) {
                        let kind = resource["mimeType"].as_str().unwrap_or_default();
                        if crate::events::IMAGE_TYPES.contains(&kind) {
                            images.push((kind.to_string(), blob.to_string()));
                            format!("[a {kind} picture, shown to you here]")
                        } else {
                            format!("[resource {}]", resource["uri"].as_str().unwrap_or_default())
                        }
                    } else {
                        format!("[resource {}]", resource["uri"].as_str().unwrap_or_default())
                    }
                }
                Some("resource_link") => format!("[link to {}]", part["uri"].as_str().unwrap_or_default()),
                _ => part.to_string(),
            })
            .collect();
        let mut content = parts.join("\n");
        // A tool may answer with structured data alone.
        if content.is_empty() && result.get("structuredContent").is_some() {
            content = result["structuredContent"].to_string();
        }
        if content.chars().count() > RESULT_CHARS {
            let total = content.chars().count();
            content = format!("{}\n[... the result is {total} characters; it is cut at {RESULT_CHARS} ...]", content.chars().take(RESULT_CHARS).collect::<String>());
        }
        if content.is_empty() {
            content = "(the tool returned nothing)".to_string();
        }
        CallResult { content, is_error: result["isError"].as_bool().unwrap_or(false), images }
    }
}

/// On Windows a command such as `npx` is `npx.cmd`, which is only found when
/// its extension is spelled out; this finds the file the name stands for.
fn find_program(name: &str) -> PathBuf {
    let given = Path::new(name);
    if !cfg!(windows) || given.components().count() > 1 || given.extension().is_some() {
        return given.to_path_buf();
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let directories = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect::<Vec<_>>()).unwrap_or_default();
    directories
        .iter()
        .flat_map(|directory| extensions.split(';').filter(|extension| !extension.is_empty()).map(move |extension| directory.join(format!("{name}{extension}"))))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| given.to_path_buf())
}

/// How a configured server stands.
enum Slot {
    Connecting,
    Ready(Arc<Server>),
    Failed(String),
}

/// A server's state, for the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    pub name: String,
    /// `connecting`, `ready`, or `failed`.
    pub state: String,
    /// Why it failed.
    pub error: Option<String>,
    /// The names of the tools it offers.
    pub tools: Vec<String>,
}

/// The name a server's tool goes by among the model's tools: only the
/// characters and the length every provider accepts.
pub fn exposed_name(server: &str, tool: &str) -> String {
    let clean = |text: &str| text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>();
    format!("mcp__{}__{}", clean(server), clean(tool)).chars().take(NAME_CHARS).collect()
}

/// The server a tool name of the model's belongs to, and the rest of the name; `None` for a built-in tool.
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

/// A server in the hub: what it was started from, which attempt at connecting is its own, and how it stands.
struct Entry {
    config: ServerConfig,
    attempt: u64,
    slot: Slot,
}

/// Every configured server, connected in the background and shared by all sessions.
pub struct McpHub {
    path: PathBuf,
    runtime: tokio::runtime::Handle,
    slots: Arc<Mutex<BTreeMap<String, Entry>>>,
    /// Numbers each attempt at connecting, so one that finishes after its server was dropped or started again is let go.
    attempts: Arc<AtomicU64>,
}

impl McpHub {
    /// A hub for the servers listed in the file at `path`. Nothing connects until [`McpHub::reload`].
    pub fn new(path: impl Into<PathBuf>, runtime: tokio::runtime::Handle) -> Self {
        Self { path: path.into(), runtime, slots: Arc::default(), attempts: Arc::default() }
    }

    /// Connect to `name` in the background, in place of whatever stood under that name.
    fn start(&self, name: &str, config: ServerConfig) {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        self.slots.lock().unwrap().insert(name.to_string(), Entry { config: config.clone(), attempt, slot: Slot::Connecting });
        let (slots, name) = (self.slots.clone(), name.to_string());
        self.runtime.spawn(async move {
            let slot = match Server::connect(&config).await {
                Ok(server) => Slot::Ready(Arc::new(server)),
                Err(error) => Slot::Failed(error),
            };
            if let Some(entry) = slots.lock().unwrap().get_mut(&name).filter(|entry| entry.attempt == attempt) {
                entry.slot = slot;
            }
        });
    }

    /// The configuration file's text, or an empty server list when there is none yet.
    pub fn config_text(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_else(|_| "{\n  \"mcpServers\": {}\n}\n".to_string())
    }

    /// Bring the connections in line with what the file now lists: a server whose entry is unchanged is left as it
    /// stands, connected, connecting or failed; one new or changed is connected afresh, and one gone or switched off
    /// is dropped, which ends it if it was started as a process. Returns at once; servers connect in the background.
    pub fn reload(&self) {
        // One switched off stays in the list, and is neither connected to nor shown as standing anywhere.
        let servers: BTreeMap<String, ServerConfig> =
            parse_config(&std::fs::read_to_string(&self.path).unwrap_or_default()).unwrap_or_default().into_iter().filter(|(_, config)| !config.disabled).collect();
        let dropped: Vec<Entry> = {
            let mut slots = self.slots.lock().unwrap();
            let gone: Vec<String> = slots.iter().filter(|(name, entry)| servers.get(*name) != Some(&entry.config)).map(|(name, _)| name.clone()).collect();
            gone.iter().filter_map(|name| slots.remove(name)).collect()
        };
        // Dropped outside the lock: ending a process need not hold up the others.
        drop(dropped);
        let standing: Vec<String> = self.slots.lock().unwrap().keys().cloned().collect();
        for (name, config) in servers {
            if !standing.contains(&name) {
                self.start(&name, config);
            }
        }
    }

    /// Check and save new configuration text, then connect to what it lists.
    pub fn save_config(&self, text: &str) -> Result<(), String> {
        parse_config(text)?;
        if let Some(directory) = self.path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        write_atomically(&self.path, text.as_bytes()).map_err(|error| error.to_string())?;
        self.reload();
        Ok(())
    }

    /// Put one server in the list, under `name` (in place of one already
    /// there only with `replace`), and connect to it alone: the others stay
    /// connected as they are. The rest of the file is kept as it was, though
    /// written out again; a file that cannot be read is not written over.
    pub fn add(&self, name: &str, server: &ServerConfig, replace: bool) -> Result<(), String> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut file: Value = if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text).map_err(|error| format!("the server list in {} is not valid JSON ({error}); the user can mend it on the 法器 page", self.path.display()))?
        };
        if !file.is_object() {
            return Err(format!("the server list in {} is not a JSON object; the user can mend it on the 法器 page", self.path.display()));
        }
        if !file["mcpServers"].is_object() {
            file["mcpServers"] = json!({});
        }
        if file["mcpServers"].get(name).is_some() && !replace {
            return Err(format!("a server named `{name}` is already in the list; pass `replace` to put this one in its place"));
        }
        file["mcpServers"][name] = serde_json::to_value(server).map_err(|error| error.to_string())?;
        let text = format!("{}\n", serde_json::to_string_pretty(&file).map_err(|error| error.to_string())?);
        // The whole list, checked as it will be read: the name, and a command or an address.
        let config = parse_config(&text)?.remove(name).ok_or_else(|| format!("`{name}` is not in the list it was put in"))?;
        if let Some(directory) = self.path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        write_atomically(&self.path, text.as_bytes()).map_err(|error| error.to_string())?;
        if config.disabled {
            self.slots.lock().unwrap().remove(name);
        } else {
            self.start(name, config);
        }
        Ok(())
    }

    /// How the server `name` stands once it is no longer connecting: waited for up to `limit`, or until `cancel`.
    pub async fn settled(&self, name: &str, limit: Duration, cancel: &CancellationToken) -> Option<ServerStatus> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let status = self.statuses().into_iter().find(|status| status.name == name);
            if status.as_ref().is_none_or(|status| status.state != "connecting") || tokio::time::Instant::now() >= deadline || cancel.is_cancelled() {
                return status;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
                () = cancel.cancelled() => {}
            }
        }
    }

    pub fn statuses(&self) -> Vec<ServerStatus> {
        let slots = self.slots.lock().unwrap();
        slots
            .iter()
            .map(|(name, entry)| {
                let (state, error, tools) = match &entry.slot {
                    Slot::Connecting => ("connecting", None, Vec::new()),
                    Slot::Ready(server) => ("ready", None, server.tools.iter().map(|tool| tool.name.clone()).collect()),
                    Slot::Failed(error) => ("failed", Some(error.clone()), Vec::new()),
                };
                ServerStatus { name: name.clone(), state: state.to_string(), error, tools }
            })
            .collect()
    }

    /// The connected servers' tools as the model is told of them.
    pub fn specs(&self) -> Vec<Value> {
        let slots = self.slots.lock().unwrap();
        let mut specs = Vec::new();
        for (name, entry) in slots.iter() {
            let Slot::Ready(server) = &entry.slot else { continue };
            for tool in &server.tools {
                let description = format!("[From the MCP server `{name}`] {}", tool.description);
                specs.push(json!({ "name": exposed_name(name, &tool.name), "description": description, "input_schema": tool.input_schema }));
            }
        }
        specs
    }

    /// The connected servers that offer resources, by name.
    pub fn resource_servers(&self) -> Vec<(String, Arc<Server>)> {
        let slots = self.slots.lock().unwrap();
        slots
            .iter()
            .filter_map(|(name, entry)| match &entry.slot {
                Slot::Ready(server) if server.resources => Some((name.clone(), server.clone())),
                _ => None,
            })
            .collect()
    }

    /// Call the tool the model named. A name no connected server offers comes back as an error result.
    pub async fn call(&self, name: &str, input: &Value, cancel: &CancellationToken) -> CallResult {
        let found = {
            let slots = self.slots.lock().unwrap();
            slots.iter().find_map(|(server_name, entry)| {
                let Slot::Ready(server) = &entry.slot else { return None };
                server.tools.iter().find(|tool| exposed_name(server_name, &tool.name) == name).map(|tool| (server.clone(), tool.name.clone()))
            })
        };
        match found {
            Some((server, tool)) => server.call(&tool, input, cancel).await,
            None => CallResult { content: format!("No connected MCP server offers `{name}`. The server may have stopped or been removed from the settings."), is_error: true, images: Vec::new() },
        }
    }

    /// Put a connected server in place directly, for tests.
    #[cfg(test)]
    pub fn insert(&self, name: &str, server: Server) {
        self.slots.lock().unwrap().insert(name.to_string(), Entry { config: ServerConfig::default(), attempt: 0, slot: Slot::Ready(Arc::new(server)) });
    }

    /// Drop every connection, which ends the servers started as processes.
    pub fn shutdown(&self) {
        let dropped = std::mem::take(&mut *self.slots.lock().unwrap());
        drop(dropped);
    }
}

/// A server on the other end of an in-memory pipe, for tests here and in the agent.
#[cfg(test)]
pub mod testing {
    use super::*;

    /// A server with resources and no tools: two pages of them, a template, and a text, a picture and some bytes to read.
    pub async fn resource_server() -> Server {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let message: Value = serde_json::from_str(&line).unwrap();
                let id = message["id"].clone();
                let result = match message["method"].as_str() {
                    Some("initialize") => json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": { "resources": {} }, "serverInfo": { "name": "notes", "version": "1" } }),
                    Some("resources/list") if message["params"]["cursor"].is_null() => json!({
                        "resources": [{ "uri": "notes://today", "name": "today", "title": "Today's notes", "mimeType": "text/markdown", "description": "What was done today." }],
                        "nextCursor": "page-2",
                    }),
                    Some("resources/list") => json!({ "resources": [{ "uri": "notes://chart", "name": "chart", "mimeType": "image/png" }] }),
                    Some("resources/templates/list") => json!({ "resourceTemplates": [{ "uriTemplate": "notes://day/{date}", "name": "day", "description": "One day's notes." }] }),
                    Some("resources/read") => match message["params"]["uri"].as_str() {
                        Some("notes://today") => json!({ "contents": [{ "uri": "notes://today", "mimeType": "text/markdown", "text": "# Today\nShipped 0.4.7." }] }),
                        Some("notes://chart") => json!({ "contents": [{ "uri": "notes://chart", "mimeType": "image/png", "blob": "aGk=" }] }),
                        _ => json!({ "contents": [{ "uri": "notes://data", "mimeType": "application/octet-stream", "blob": "AAAAAAAA" }] }),
                    },
                    _ => continue,
                };
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                let _ = server_write.write_all(format!("{reply}\n").as_bytes()).await;
            }
        });
        let (read, write) = tokio::io::split(client_side);
        Server::over(read, write).await.unwrap()
    }

    /// A server offering `echo` (returns its `text`) and `fail` (an error result), which also pings the client once.
    pub async fn echo_server() -> Server {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let message: Value = serde_json::from_str(&line).unwrap();
                let id = message["id"].clone();
                let result = match message["method"].as_str() {
                    Some("initialize") => json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": { "tools": {} }, "serverInfo": { "name": "echo", "version": "1" } }),
                    Some("tools/list") if message["params"]["cursor"].is_null() => json!({
                        "tools": [{ "name": "echo", "description": "Says it back.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } }],
                        "nextCursor": "page-2",
                    }),
                    Some("tools/list") => json!({ "tools": [{ "name": "fail now", "description": "Always fails." }] }),
                    Some("tools/call") => match message["params"]["name"].as_str() {
                        Some("echo") => json!({ "content": [{ "type": "text", "text": message["params"]["arguments"]["text"] }, { "type": "image", "mimeType": "image/png", "data": "aGk=" }] }),
                        _ => json!({ "content": [{ "type": "text", "text": "it broke" }], "isError": true }),
                    },
                    // Notifications and the client's answers need no reply.
                    _ => continue,
                };
                // A log line and a request of the server's own come before the answer: the client must cope with both.
                let ping = json!({ "jsonrpc": "2.0", "id": 9000, "method": "ping" });
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                let _ = server_write.write_all(format!("starting up...\n{ping}\n{reply}\n").as_bytes()).await;
            }
        });
        let (read, write) = tokio::io::split(client_side);
        Server::over(read, write).await.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_common_server_list_and_refuses_what_it_cannot_run() {
        let text = r#"{ "mcpServers": {
            "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."], "env": { "DEBUG": "1" } },
            "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer x" } }
        } }"#;
        let servers = parse_config(text).unwrap();
        assert_eq!(servers["files"].args.len(), 3);
        assert_eq!(servers["remote"].headers["Authorization"], "Bearer x");
        assert!(parse_config("").unwrap().is_empty() && parse_config("{}").unwrap().is_empty());
        for bad in [
            r#"{ "mcpServers": { "x": {} } }"#,
            r#"{ "mcpServers": { "x": { "command": "a", "url": "https://b" } } }"#,
            r#"{ "mcpServers": { "x": { "url": "file:///etc" } } }"#,
            r#"{ "mcpServers": { "bad name": { "command": "a" } } }"#,
            "not json",
        ] {
            assert!(parse_config(bad).is_err(), "{bad}");
        }
        assert_eq!(exposed_name("files", "read file/v2"), "mcp__files__read_file_v2");
        assert_eq!(exposed_name("s", &"x".repeat(100)).len(), 64);
        assert_eq!(split_name("mcp__files__read_file"), Some(("files", "read_file")));
        assert_eq!(split_name("read_file"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_a_server_switched_off_in_the_list_without_connecting_to_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp.json");
        std::fs::write(&path, r#"{ "mcpServers": { "on": { "url": "http://127.0.0.1:9/mcp" }, "off": { "command": "odg-no-such-program", "disabled": true } } }"#).unwrap();
        let servers = parse_config(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(servers["off"].disabled && !servers["on"].disabled);
        // Written out again, the flag is kept where it is set and left out where it is not.
        assert_eq!(serde_json::to_value(&servers["off"]).unwrap(), json!({ "command": "odg-no-such-program", "disabled": true }));
        assert_eq!(serde_json::to_value(&servers["on"]).unwrap(), json!({ "url": "http://127.0.0.1:9/mcp" }));
        let hub = McpHub::new(&path, tokio::runtime::Handle::current());
        hub.reload();
        assert_eq!(hub.statuses().iter().map(|status| status.name.as_str()).collect::<Vec<_>>(), ["on"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_the_list_reconnects_only_the_servers_whose_entries_changed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp.json");
        let entry = |args: &str| format!(r#"{{ "command": "odg-no-such-program", "args": ["{args}"] }}"#);
        let list = |servers: &[(&str, String)]| format!(r#"{{ "mcpServers": {{ {} }} }}"#, servers.iter().map(|(name, entry)| format!(r#""{name}": {entry}"#)).collect::<Vec<_>>().join(", "));
        std::fs::write(&path, list(&[("kept", entry("a")), ("changed", entry("a")), ("switched", entry("a")), ("removed", entry("a"))])).unwrap();
        let hub = McpHub::new(&path, tokio::runtime::Handle::current());
        hub.reload();
        for name in ["kept", "changed", "switched", "removed"] {
            hub.settled(name, Duration::from_secs(20), &CancellationToken::new()).await.unwrap();
        }
        let attempt = |name: &str| hub.slots.lock().unwrap().get(name).map(|entry| entry.attempt);
        let before = attempt("kept");
        let switched_off = r#"{ "command": "odg-no-such-program", "args": ["a"], "disabled": true }"#.to_string();
        hub.save_config(&list(&[("kept", entry("a")), ("changed", entry("b")), ("switched", switched_off), ("new", entry("a"))])).unwrap();
        // The one left as it was keeps its connection; the changed and the new are connected afresh.
        assert_eq!(attempt("kept"), before);
        assert!(attempt("changed") > before && attempt("new") > before);
        // The one switched off and the one taken out are let go.
        assert_eq!(hub.statuses().iter().map(|status| status.name.as_str()).collect::<Vec<_>>(), ["changed", "kept", "new"]);
        let status = hub.settled("changed", Duration::from_secs(20), &CancellationToken::new()).await.unwrap();
        assert_eq!(status.state, "failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adds_a_server_to_the_list_and_connects_to_it_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp.json");
        std::fs::write(&path, r#"{ "note": "mine", "mcpServers": { "kept": { "url": "http://127.0.0.1:9/mcp" } } }"#).unwrap();
        let hub = McpHub::new(&path, tokio::runtime::Handle::current());
        hub.reload();
        let missing = ServerConfig { command: Some("odg-no-such-program".into()), args: vec!["--stdio".into()], ..Default::default() };
        hub.add("broken", &missing, false).unwrap();
        // It is connected to at once, and says how it went.
        let status = hub.settled("broken", Duration::from_secs(20), &CancellationToken::new()).await.unwrap();
        assert_eq!(status.state, "failed");
        assert!(status.error.unwrap_or_default().contains("cannot start"));
        // The list keeps what it held, and the server the user had is still there.
        let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file["note"], "mine");
        assert_eq!(file["mcpServers"]["broken"], json!({ "command": "odg-no-such-program", "args": ["--stdio"] }));
        assert!(file["mcpServers"]["kept"].is_object() && hub.statuses().iter().any(|status| status.name == "kept"));
        // Not over one already there, unless asked to; and nothing the list could not hold is written.
        assert!(hub.add("broken", &missing, false).unwrap_err().contains("replace"));
        hub.add("broken", &ServerConfig { url: Some("https://example.com/mcp".into()), ..Default::default() }, true).unwrap();
        assert!(hub.add("bad name", &missing, false).is_err());
        assert!(hub.add("both", &ServerConfig { command: Some("x".into()), url: Some("https://x".into()), ..Default::default() }, false).is_err());
        let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file["mcpServers"].as_object().unwrap().keys().collect::<Vec<_>>(), ["broken", "kept"]);
        assert_eq!(file["mcpServers"]["broken"], json!({ "url": "https://example.com/mcp" }));
        // A list that cannot be read is not written over.
        std::fs::write(&path, "{ not json").unwrap();
        assert!(hub.add("other", &missing, false).unwrap_err().contains("法器"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[tokio::test]
    async fn lists_and_calls_a_servers_tools_over_a_line_connection() {
        let server = testing::echo_server().await;
        // Both pages of the list arrived.
        assert_eq!(server.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(), ["echo", "fail now"]);
        assert_eq!(server.tools[1].input_schema, json!({ "type": "object" }));
        let cancel = CancellationToken::new();
        let said = server.call("echo", &json!({ "text": "分身" }), &cancel).await;
        assert_eq!(said, CallResult { content: "分身\n[a image/png picture, shown to you here]".into(), is_error: false, images: vec![("image/png".into(), "aGk=".into())] });
        assert_eq!(server.call("fail now", &json!({}), &cancel).await, CallResult { content: "it broke".into(), is_error: true, images: Vec::new() });

        let hub = McpHub::new("unused.json", tokio::runtime::Handle::current());
        hub.insert("echo", server);
        let specs = hub.specs();
        assert_eq!(specs[0]["name"], "mcp__echo__echo");
        assert_eq!(specs[1]["name"], "mcp__echo__fail_now");
        assert!(specs[0]["description"].as_str().unwrap().starts_with("[From the MCP server `echo`] Says it back."));
        assert_eq!(hub.call("mcp__echo__fail_now", &json!({}), &cancel).await.content, "it broke");
        assert!(hub.call("mcp__gone__x", &json!({}), &cancel).await.content.starts_with("No connected MCP server offers"));
        assert_eq!(hub.statuses(), [ServerStatus { name: "echo".into(), state: "ready".into(), error: None, tools: vec!["echo".into(), "fail now".into()] }]);
    }

    #[tokio::test]
    async fn speaks_to_a_server_over_http_with_json_and_event_stream_answers() {
        use crate::testing::{event_stream, serve};
        let json_reply = |body: Value, extra: &str| {
            let body = body.to_string();
            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
        };
        let (address, requests) = serve(vec![
            json_reply(json!({ "jsonrpc": "2.0", "id": 1, "result": { "protocolVersion": "2025-03-26", "capabilities": { "tools": {} } } }), "mcp-session-id: s-1\r\n"),
            "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string(),
            json_reply(json!({ "jsonrpc": "2.0", "id": 2, "result": { "tools": [{ "name": "add", "description": "Adds." }] } }), ""),
            // The call is answered as an event stream, a notification first.
            event_stream(&[json!({ "jsonrpc": "2.0", "method": "notifications/progress" }), json!({ "jsonrpc": "2.0", "id": 3, "result": { "content": [{ "type": "text", "text": "3" }] } })], false),
        ])
        .await;
        let config = ServerConfig { url: Some(format!("{address}/mcp")), headers: [("authorization".to_string(), "Bearer t".to_string())].into(), ..Default::default() };
        let server = Server::connect(&config).await.unwrap();
        assert_eq!(server.tools[0].name, "add");
        assert_eq!(server.call("add", &json!({ "a": 1, "b": 2 }), &CancellationToken::new()).await, CallResult { content: "3".into(), is_error: false, images: Vec::new() });

        let requests = requests.await.unwrap();
        let lower: Vec<String> = requests.iter().map(|request| request.to_lowercase()).collect();
        assert!(lower[0].starts_with("post /mcp ") && lower[0].contains("authorization: bearer t") && lower[0].contains("accept: application/json, text/event-stream"));
        // The session and the agreed protocol revision go back on every later request.
        assert!(lower[2].contains("mcp-session-id: s-1") && lower[2].contains("mcp-protocol-version: 2025-03-26"), "{}", lower[2]);
        assert!(requests[3].contains("\"name\":\"add\""));
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_or_answer_says_why() {
        let missing = ServerConfig { command: Some("odg-no-such-program".into()), ..Default::default() };
        assert!(Server::connect(&missing).await.err().unwrap().starts_with("cannot start `odg-no-such-program`"));

        // A process that exits without speaking the protocol: its stderr is the explanation.
        let (program, args): (&str, Vec<String>) = if cfg!(windows) {
            ("cmd", vec!["/C".into(), "echo no such module 1>&2".into()])
        } else {
            ("sh", vec!["-c".into(), "echo no such module >&2".into()])
        };
        let dies = ServerConfig { command: Some(program.into()), args, ..Default::default() };
        let error = Server::connect(&dies).await.err().unwrap();
        assert!(error.starts_with("the server closed the connection"), "{error}");
    }

    #[tokio::test]
    async fn lists_every_page_of_a_servers_resources_and_reads_them() {
        let server = testing::resource_server().await;
        assert!(server.resources && server.tools.is_empty());
        let (resources, templates) = server.list_resources(200).await.unwrap();
        assert_eq!(resources.iter().map(|resource| resource["uri"].as_str().unwrap()).collect::<Vec<_>>(), ["notes://today", "notes://chart"]);
        assert_eq!(templates[0]["uriTemplate"], "notes://day/{date}");
        assert_eq!(server.list_resources(1).await.unwrap().0.len(), 1);
        let cancel = CancellationToken::new();
        assert_eq!(server.read_resource("notes://today", &cancel).await.unwrap(), ReadResource { text: "# Today\nShipped 0.4.7.".into(), images: Vec::new() });
        let chart = server.read_resource("notes://chart", &cancel).await.unwrap();
        assert_eq!((chart.text.as_str(), chart.images), ("[a image/png picture, shown to you here]", vec![("image/png".to_string(), "aGk=".to_string())]));
        let data = server.read_resource("notes://data", &cancel).await.unwrap();
        assert_eq!(data.text, "[6 bytes of application/octet-stream that are not text: notes://data]");
        // A server without the capability is not taken to have resources.
        assert!(!testing::echo_server().await.resources);
    }
}
