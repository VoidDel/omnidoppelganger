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
//! that fails when the component is not installed. Where it is not there, rust-analyzer can be downloaded for the app
//! with the user's leave, as the document runtime is: one build, by its checksum, kept in the app's home.

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

/// A build of a language server the app can download where it is not installed: its version, where its release is,
/// and for each target the archive's name, its SHA-256 and its size, from the release's published digests.
#[derive(Debug)]
pub struct Download {
    pub version: &'static str,
    address: &'static str,
    builds: &'static [(&'static str, &'static str, &'static str, u64)],
}

impl Download {
    /// This machine's build: the archive's name, its SHA-256, and its size.
    fn build(&self) -> Option<(&'static str, &'static str, u64)> {
        let target = crate::runtime::target()?;
        self.builds.iter().find(|(triple, ..)| *triple == target).map(|(_, file, sha256, bytes)| (*file, *sha256, *bytes))
    }

    /// About how many megabytes this machine's build downloads.
    pub fn megabytes(&self) -> Option<u64> {
        self.build().map(|(_, _, bytes)| bytes.div_ceil(1_000_000))
    }
}

/// rust-analyzer's weekly release of 2026-11-16, from <https://github.com/rust-lang/rust-analyzer/releases/tag/2026-11-16>.
static RUST_ANALYZER: Download = Download {
    version: "2026-11-16",
    address: "https://github.com/rust-lang/rust-analyzer/releases/download/2026-11-16",
    builds: &[
        ("aarch64-apple-darwin", "rust-analyzer-aarch64-apple-darwin.gz", "608b7c7d6bec1fcd6c964578b1f8234888afe6d6c0e7915284f1e2fd36ef1343", 14_345_012),
        ("x86_64-apple-darwin", "rust-analyzer-x86_64-apple-darwin.gz", "6901d956b4f2b091343eaf3570944098488f3ffec018987145285ed8253c07d2", 15_080_897),
        ("x86_64-unknown-linux-gnu", "rust-analyzer-x86_64-unknown-linux-gnu.gz", "4a08727a03094e5114db4438e4bd1d6182cc26eff37d6962115ea1ec1a491ca3", 15_663_964),
        ("aarch64-unknown-linux-gnu", "rust-analyzer-aarch64-unknown-linux-gnu.gz", "73d8b9b87384b3115a183a8a51db9f7c205ce4c89ea511f3c8c8fea863038a50", 15_120_173),
        ("x86_64-pc-windows-msvc", "rust-analyzer-x86_64-pc-windows-msvc.zip", "dbf0c36ec54cf727d34cc40c9994fe71b7d52e3c28a3112c3deca1aba46fbc8f", 18_054_633),
        ("aarch64-pc-windows-msvc", "rust-analyzer-aarch64-pc-windows-msvc.zip", "2e8fd876300eeee87afd6de18c79eb17bab425a9b755043c63d5023d58de37b7", 16_197_678),
    ],
};

/// A language a server is known for, and how that server is started.
#[derive(Debug)]
pub struct Language {
    /// How `language_server` names it.
    pub key: &'static str,
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
    /// The build the app can download where the server is not installed; `None` where it cannot.
    pub download: Option<&'static Download>,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        key: "rust",
        name: "Rust",
        program: "rust-analyzer",
        args: &[],
        extensions: &["rs"],
        install: "rustup component add rust-analyzer",
        runs_project_code: false,
        env: &[],
        download: Some(&RUST_ANALYZER),
    },
    Language {
        key: "c",
        name: "C/C++",
        program: "clangd",
        args: &["--log=error"],
        extensions: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm"],
        install: "install clangd (it comes with LLVM, and with Xcode's command line tools on macOS)",
        runs_project_code: false,
        env: &[],
        download: None,
    },
    Language {
        key: "typescript",
        name: "TypeScript/JavaScript",
        program: "typescript-language-server",
        args: &["--stdio"],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        install: "npm install -g typescript-language-server typescript",
        runs_project_code: true,
        env: &[],
        download: None,
    },
    Language {
        key: "python",
        name: "Python",
        program: "pyright-langserver",
        args: &["--stdio"],
        extensions: &["py", "pyi"],
        install: "npm install -g pyright",
        runs_project_code: false,
        env: &[],
        download: None,
    },
    Language {
        key: "go",
        name: "Go",
        program: "gopls",
        args: &[],
        extensions: &["go"],
        install: "go install golang.org/x/tools/gopls@latest",
        runs_project_code: false,
        // No module is fetched, and no other Go is downloaded to run.
        env: &[("GOPROXY", "off"), ("GOTOOLCHAIN", "local")],
        download: None,
    },
];

/// The language `language_server` names by its key.
pub fn language_by_key(key: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| language.key == key)
}

/// Where a language's server is found: on the `PATH`, downloaded for the app, or nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    System(PathBuf),
    Downloaded(PathBuf),
    Missing,
}

/// The program's file name on this system.
fn executable(program: &str) -> String {
    if cfg!(windows) { format!("{program}.exe") } else { program.to_string() }
}

/// Where the app keeps its download of a server, under `root` (the app's `lsp` folder), when there is one.
pub fn downloaded(root: &Path, language: &Language) -> Option<PathBuf> {
    let download = language.download?;
    let path = root.join(language.program).join(download.version).join(executable(language.program));
    path.is_file().then_some(path)
}

/// Whether a program answers `--version`: a stand-in that fails, such as rustup's for a component not installed, does
/// not. Asked once for each program while the app runs.
async fn works(program: &Path) -> bool {
    static ASKED: std::sync::LazyLock<Mutex<HashMap<PathBuf, bool>>> = std::sync::LazyLock::new(Mutex::default);
    if let Some(known) = ASKED.lock().unwrap().get(program) {
        return *known;
    }
    let mut command = tokio::process::Command::new(program);
    command.arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let answered = matches!(tokio::time::timeout(Duration::from_secs(15), command.status()).await, Ok(Ok(status)) if status.success());
    ASKED.lock().unwrap().insert(program.to_path_buf(), answered);
    answered
}

/// Where a language's server is: on the `PATH` if it runs, else downloaded under `root`, else nowhere.
pub async fn locate(root: Option<&Path>, language: &Language) -> Located {
    if let Some(program) = installed(language.program)
        && works(&program).await
    {
        return Located::System(program);
    }
    match root.and_then(|root| downloaded(root, language)) {
        Some(program) => Located::Downloaded(program),
        None => Located::Missing,
    }
}

/// Download this machine's build of a language's server into `root`, checked against its SHA-256, and unpack it.
/// `address` stands in for the release's own, for tests. Returns where the program is.
async fn install_from(root: &Path, language: &Language, address: &str, cancel: &CancellationToken) -> Result<PathBuf, String> {
    let download = language.download.ok_or_else(|| format!("{} cannot be downloaded by the app. Ask the user to install it: {}.", language.program, language.install))?;
    let (file, sha256, _) = download.build().ok_or_else(|| format!("There is no build of {} for this machine to download.", language.program))?;
    let folder = root.join(language.program);
    let into = folder.join(download.version);
    std::fs::create_dir_all(&into).map_err(|error| format!("Cannot create {}: {error}", into.display()))?;
    let archive = folder.join(format!("{file}.part"));
    let fetched = crate::runtime::download(&format!("{address}/{file}"), sha256, &archive, &|_| {}, cancel).await;
    let unpacked = fetched.and_then(|()| unpack_program(&archive, &into, language.program));
    let _ = std::fs::remove_file(&archive);
    let program = match unpacked {
        Ok(program) => program,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&into);
            return Err(error);
        }
    };
    // Earlier versions are taken away: only the one in use is kept.
    for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
        if entry.path() != into && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(program)
}

/// The program out of its archive: a gzipped program on its own, or a zip holding it. Made runnable.
fn unpack_program(archive: &Path, into: &Path, program: &str) -> Result<PathBuf, String> {
    let name = executable(program);
    let target = into.join(&name);
    let failed = |error: String| format!("Cannot unpack {program}: {error}");
    let bytes = std::fs::read(archive).map_err(|error| failed(error.to_string()))?;
    let unpacked: Vec<u8> = if archive.to_string_lossy().ends_with(".zip.part") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|error| failed(error.to_string()))?;
        let mut entry = zip.by_name(&name).map_err(|_| failed(format!("{name} is not in the archive")))?;
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut read).map_err(|error| failed(error.to_string()))?;
        read
    } else {
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes.as_slice()), &mut read).map_err(|error| failed(error.to_string()))?;
        read
    };
    crate::fsutil::write_atomically(&target, &unpacked).map_err(|error| failed(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).map_err(|error| failed(error.to_string()))?;
    }
    Ok(target)
}

/// Download a language's server for the app; see [`install_from`].
pub async fn install(root: &Path, language: &Language, cancel: &CancellationToken) -> Result<PathBuf, String> {
    let address = language.download.map(|download| download.address).unwrap_or_default();
    install_from(root, language, address, cancel).await
}

/// Take away the app's download of a language's server.
pub fn remove(root: &Path, language: &Language) -> Result<(), String> {
    let folder = root.join(language.program);
    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(|error| format!("Cannot remove {}: {error}", folder.display()))?;
    }
    Ok(())
}

/// What the agent is told when a language's server is nowhere: how it can be had.
fn missing(language: &Language) -> String {
    match language.download.and_then(|download| download.megabytes().map(|megabytes| (download, megabytes))) {
        Some((download, megabytes)) => format!(
            "{} is not installed, so {} code cannot be navigated yet. It can be downloaded for this app: call `language_server` with `language` \"{}\" and `install` true (rust-analyzer {}, about {megabytes} MB; the user is asked first). Or the user can install it: {}.",
            language.program, language.name, language.key, download.version, language.install
        ),
        None => format!("{} is not installed, so {} code cannot be navigated here. Ask the user to install it ({}), or use `search` instead.", language.program, language.name, language.install),
    }
}

/// Answer `language_server`: whether a language's server is here, and with `install`, download it.
pub async fn server_tool(hub: &LspHub, input: &Value, cancel: &CancellationToken) -> Result<crate::tools::ToolOutput, String> {
    let key = input["language"].as_str().unwrap_or_default();
    let language = language_by_key(key).ok_or_else(|| format!("`language` must be one of {}.", LANGUAGES.iter().map(|language| language.key).collect::<Vec<_>>().join(", ")))?;
    let root = hub.root.as_deref();
    let located = locate(root, language).await;
    let mut meta = json!({ "language": language.key, "server": language.program });
    let content = match (&located, input["install"] == true, root) {
        (Located::System(path), ..) => format!("{} is installed on this machine ({}); `code_nav` uses it.", language.program, path.display()),
        (Located::Downloaded(path), ..) => format!("{} was downloaded for this app ({}); `code_nav` uses it.", language.program, path.display()),
        (Located::Missing, true, Some(root)) => {
            let program = install(root, language, cancel).await?;
            meta["installed"] = json!(true);
            format!("Downloaded {} {} for this app ({}). `code_nav` can use it now.", language.program, language.download.map(|download| download.version).unwrap_or_default(), program.display())
        }
        (Located::Missing, ..) => missing(language),
    };
    meta["state"] = json!(match located {
        Located::System(_) => "system",
        Located::Downloaded(_) => "downloaded",
        Located::Missing if meta["installed"] == true => "downloaded",
        Located::Missing => "missing",
    });
    Ok(crate::tools::ToolOutput::ok(content, meta))
}

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
    let mut encoded = String::from(if text.starts_with("//") { "file:" } else if text.starts_with('/') { "file://" } else { "file:///" });
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
        if raw[at] == b'%' && at + 3 <= raw.len() && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&raw[at + 1..at + 3]).unwrap_or(""), 16) {
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
    let drive = decoded.len() > 3 && decoded.as_bytes()[0] == b'/' && decoded.as_bytes()[1].is_ascii_alphabetic() && decoded.as_bytes()[2] == b':';
    Some(if drive {
        format!("{}{}", decoded[1..2].to_ascii_uppercase(), decoded[2..].replace('/', "\\"))
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
    /// Where servers downloaded for the app are kept; `None` where nothing is downloaded.
    root: Option<PathBuf>,
}

impl LspHub {
    /// A hub whose downloaded servers are kept under `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { clients: tokio::sync::Mutex::default(), root: Some(root) }
    }

    /// Where servers downloaded for the app are kept.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The server for `language` in `root`: the one running, or a new one, started now.
    pub async fn client(&self, root: &Path, language: &'static Language) -> Result<Arc<Client>, String> {
        let mut clients = self.clients.lock().await;
        let key = (root.to_path_buf(), language.program);
        if let Some(client) = clients.get(&key).filter(|client| !client.closed()) {
            return Ok(client.clone());
        }
        let program = match locate(self.root.as_deref(), language).await {
            Located::System(program) | Located::Downloaded(program) => program,
            Located::Missing => return Err(missing(language)),
        };
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
                return Err(format!("{error}.{said} {}", missing(language)));
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
    // Paths come back from the server as it writes them, which is plainly: the workspace is compared with them so.
    let workspace = &plain(workspace);
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
    fn writes_windows_paths_as_servers_do_and_reads_them_back_alike() {
        // A verbatim path, as canonicalizing gives on Windows, is written as the path it names.
        assert_eq!(uri_of(r"\\?\C:\Users\me\a b\x.c"), "file:///C:/Users/me/a%20b/x.c");
        assert_eq!(uri_of(r"C:\Users\me\x.c"), "file:///C:/Users/me/x.c");
        assert_eq!(uri_of(r"\\?\UNC\server\share\x.c"), "file://server/share/x.c");
        assert_eq!(plain_text(r"\\?\C:\w"), r"C:\w");
        // Read back on Windows with backslashes and a capital drive, however the server wrote the drive.
        assert_eq!(path_text("file:///C:/Users/me/a%20b/x.c", true).unwrap(), r"C:\Users\me\a b\x.c");
        assert_eq!(path_text("file:///c%3A/Users/me/x.c", true).unwrap(), r"C:\Users\me\x.c");
        assert_eq!(path_text("file://server/share/x.c", true).unwrap(), r"\\server\share\x.c");
        // Elsewhere, as it is.
        assert_eq!(path_text("file:///home/me/x.c", false).unwrap(), "/home/me/x.c");
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
    async fn a_stand_in_that_fails_is_not_taken_for_a_server_and_one_that_will_not_serve_says_so() {
        // rustup's stand-in for rust-analyzer, when the component is not installed, fails at `--version`: `false` does
        // the same, and is not taken for an installed server.
        static FAILING: Language = Language { key: "test", name: "Test", program: "false", args: &[], extensions: &["test"], install: "install it somehow", runs_project_code: false, env: &[], download: None };
        // `true` answers `--version`, then exits rather than serve.
        static SILENT: Language = Language { key: "test", name: "Test", program: "true", args: &[], extensions: &["test"], install: "install it somehow", runs_project_code: false, env: &[], download: None };
        if installed("false").is_none() || installed("true").is_none() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(locate(None, &FAILING).await, Located::Missing);
        let missing = LspHub::default().client(directory.path(), &FAILING).await.err().unwrap();
        assert!(missing.starts_with("false is not installed") && missing.contains("install it somehow"), "{missing}");
        let silent = LspHub::default().client(directory.path(), &SILENT).await.err().unwrap();
        assert!(silent.starts_with("true would not start") && silent.contains("install it somehow"), "{silent}");
    }

    /// A server's archive as its release would have it on this system: a zip holding the program on Windows, the
    /// program gzipped elsewhere. With its name and SHA-256.
    fn archive(program: &str, bytes: &[u8]) -> (String, Vec<u8>, String) {
        use sha2::Digest;
        let (name, packed) = if cfg!(windows) {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file(executable(program), zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, bytes).unwrap();
            (format!("{program}.zip"), zip.finish().unwrap().into_inner())
        } else {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut gz, bytes).unwrap();
            (format!("{program}.gz"), gz.finish().unwrap())
        };
        let sha256 = sha2::Sha256::digest(&packed).iter().map(|byte| format!("{byte:02x}")).collect();
        (name, packed, sha256)
    }

    /// A language whose server is downloaded from `archive`, pinned to `sha256`, for this machine.
    fn downloadable(program: &'static str, file: String, sha256: String) -> &'static Language {
        let target = crate::runtime::target().expect("a target with builds");
        let builds: &'static [(&'static str, &'static str, &'static str, u64)] = Box::leak(vec![(target, &*Box::leak(file.into_boxed_str()), &*Box::leak(sha256.into_boxed_str()), 1_000)].into_boxed_slice());
        let download: &'static Download = Box::leak(Box::new(Download { version: "1.0", address: "", builds }));
        Box::leak(Box::new(Language { key: "fake", name: "Fake", program, args: &[], extensions: &["fake"], install: "install it somehow", runs_project_code: false, env: &[], download: Some(download) }))
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        [format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).into_bytes(), body.to_vec()].concat()
    }

    #[tokio::test]
    async fn downloads_a_server_checked_against_its_checksum_and_keeps_only_that_version() {
        if crate::runtime::target().is_none() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (file, packed, sha256) = archive("odg-fake-server", b"#!/bin/sh\necho fake\n");
        let language = downloadable("odg-fake-server", file.clone(), sha256);
        // A version from before, to be taken away.
        std::fs::create_dir_all(root.join("odg-fake-server/0.9")).unwrap();
        let (address, requests) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
        let program = install_from(root, language, &address, &CancellationToken::new()).await.unwrap();
        assert_eq!(program, root.join("odg-fake-server/1.0").join(executable("odg-fake-server")));
        assert_eq!(std::fs::read(&program).unwrap(), b"#!/bin/sh\necho fake\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&program).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert!(requests.await.unwrap()[0].starts_with(&format!("GET /{file} ")));
        assert!(!root.join("odg-fake-server/0.9").exists() && !root.join(format!("odg-fake-server/{file}.part")).exists());
        // Not on the PATH, it is found where it was downloaded; and taken away, it is nowhere.
        assert_eq!(locate(Some(root), language).await, Located::Downloaded(program));
        remove(root, language).unwrap();
        assert_eq!(locate(Some(root), language).await, Located::Missing);
    }

    #[tokio::test]
    async fn refuses_a_download_whose_checksum_does_not_match_and_leaves_nothing() {
        if crate::runtime::target().is_none() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (file, packed, _) = archive("odg-other-server", b"anything");
        let language = downloadable("odg-other-server", file, "0".repeat(64));
        let (address, _) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
        let error = install_from(root, language, &address, &CancellationToken::new()).await.unwrap_err();
        assert!(error.contains("校验不符"), "{error}");
        assert!(!root.join("odg-other-server/1.0").exists());
        assert_eq!(std::fs::read_dir(root.join("odg-other-server")).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn says_how_a_missing_server_can_be_had() {
        let rust = language_by_key("rust").unwrap();
        // Downloadable, with this machine's size, unless rust-analyzer is installed here.
        if !matches!(locate(None, rust).await, Located::System(_)) && RUST_ANALYZER.megabytes().is_some() {
            let directory = tempfile::tempdir().unwrap();
            let told = server_tool(&LspHub::new(directory.path().to_path_buf()), &json!({ "language": "rust" }), &CancellationToken::new()).await.unwrap();
            assert!(told.content.contains("call `language_server` with `language` \"rust\" and `install` true") && told.content.contains("about 1"), "{}", told.content);
            assert_eq!(told.meta["state"], "missing");
        }
        // One the app cannot download: how the user installs it.
        assert!(missing(language_by_key("python").unwrap()).contains("npm install -g pyright"));
        let wrong = server_tool(&LspHub::default(), &json!({ "language": "cobol" }), &CancellationToken::new()).await.unwrap_err();
        assert!(wrong.contains("`language` must be one of rust, c, typescript, python, go"), "{wrong}");
    }
}
