//! The document runtime: a Python of its own, with the libraries that make
//! Word, Excel, PowerPoint and PDF files, kept in the app's home and fetched
//! the first time it is wanted, with the user's leave. Nothing on the system
//! is touched: not its Python, not its `PATH`.
//!
//! What is fetched is fixed: one build of Python from the python-build-standalone
//! project, by its checksum, and the libraries at the versions and checksums in
//! `runtime/requirements.txt`, which pip installs only if every file matches.
//! A mirror may serve either, set by `ODG_PYTHON_MIRROR` (in place of the GitHub
//! release address) and pip's own `PIP_INDEX_URL`; the checksums hold all the same.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// The build of Python, and its release.
pub const PYTHON_VERSION: &str = "3.12.16";
const PYTHON_RELEASE: &str = "20261026";
const PYTHON_ADDRESS: &str = "https://github.com/astral-sh/python-build-standalone/releases/download";

/// Each target's build and the SHA-256 of its archive, from the release's SHA256SUMS.
const BUILDS: &[(&str, &str)] = &[
    ("x86_64-pc-windows-msvc", "1f2ab53880fc67ea323398c6388e05af28addc99a8e389209f08a51fdff4b149"),
    ("aarch64-pc-windows-msvc", "3b2e4da9209cec3e0eaf4db31176977a88df32d598ff94a7899c34cd8304db04"),
    ("aarch64-apple-darwin", "353d5f1b4fd2715e7053f12f068435d5bbad16b91e057006b5d01e0925709a7e"),
    ("x86_64-apple-darwin", "37c38fda76fec1fbf310adcbafb7f0d96af13b5511bf8ea954c3aa8eec185734"),
    ("x86_64-unknown-linux-gnu", "ce4f6d0a6cfdb3bed6e44c2f9affd85943c76a42e25ad3a259002f31d6fb8a20"),
    ("aarch64-unknown-linux-gnu", "bb4b6392670c5d3f93876996fcaa19858a4a413a0c7720144c18b0d6f1f0db2d"),
];

/// The libraries, each pinned to a version and the checksums of its wheels.
pub const REQUIREMENTS: &str = include_str!("../runtime/requirements.txt");

/// What the installed runtime is checked to import, and what it is said to offer.
pub const MODULES: &[(&str, &str)] = &[
    ("docx", "python-docx: Word"),
    ("openpyxl", "openpyxl: Excel"),
    ("xlsxwriter", "XlsxWriter: Excel"),
    ("pptx", "python-pptx: PowerPoint"),
    ("reportlab", "reportlab: PDF"),
    ("pypdf", "pypdf: PDF"),
    ("pandas", "pandas"),
    ("PIL", "Pillow"),
    ("lxml", "lxml"),
    ("matplotlib", "matplotlib: charts"),
    ("qrcode", "qrcode: QR codes"),
    ("resvg_py", "resvg: SVG to PNG"),
];

/// About how much a whole install downloads, said to the user before they agree to it: the most of any system,
/// Linux's. Bringing the libraries up to date downloads only those that changed.
pub const DOWNLOAD_MB: u32 = 90;

/// What `runtime.json` records of an installed runtime: the Python, and which list of libraries is installed in it,
/// by the SHA-256 of [`REQUIREMENTS`]. They are told apart so that a change to the list installs only what changed,
/// in the Python that is there, rather than fetching everything again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The version of Python.
    pub python: String,
    pub target: String,
    /// The SHA-256 of the requirements installed. Runtimes set up before it was kept have none, and so are brought
    /// up to date by installing the libraries again.
    #[serde(default)]
    pub libraries: String,
}

impl Installed {
    /// The record of a runtime as this build sets one up, on `target`.
    pub fn current(target: &str) -> Self {
        Installed { python: PYTHON_VERSION.to_string(), target: target.to_string(), libraries: libraries() }
    }
}

/// Which list of libraries this build installs: the SHA-256 of [`REQUIREMENTS`], line endings aside.
pub fn libraries() -> String {
    Sha256::digest(REQUIREMENTS.replace("\r\n", "\n").as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How the runtime stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// No Python, or another version than this build's: the whole runtime is to be set up. `outdated` when one was there.
    Missing { outdated: bool },
    /// The Python is this build's, and its libraries are another list: only they are to be brought up to date.
    Stale { python: PathBuf, version: String },
    Ready { python: PathBuf, version: String },
}

/// A step of an install, for whoever watches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Progress {
    Downloading { done: u64, total: Option<u64> },
    Unpacking,
    Installing,
    Checking,
    Done,
}

/// The build this machine runs, or `None` where there is none.
pub fn target() -> Option<&'static str> {
    let wanted = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => return None,
    };
    BUILDS.iter().find(|(triple, _)| *triple == wanted).map(|(triple, _)| *triple)
}

/// The interpreter inside a runtime's `python` folder.
fn interpreter(python: &Path) -> PathBuf {
    if cfg!(windows) { python.join("python.exe") } else { python.join("bin").join("python3") }
}

/// How the runtime kept under `root` stands.
pub fn status(root: &Path) -> Status {
    let recorded: Option<Installed> = std::fs::read_to_string(root.join("runtime.json")).ok().and_then(|text| serde_json::from_str(&text).ok());
    let python = interpreter(&root.join("python"));
    match recorded {
        Some(installed) if installed.python == PYTHON_VERSION && python.is_file() => {
            if installed.libraries == libraries() {
                Status::Ready { python, version: installed.python }
            } else {
                Status::Stale { python, version: installed.python }
            }
        }
        Some(_) => Status::Missing { outdated: true },
        None => Status::Missing { outdated: false },
    }
}

/// The interpreter of a ready runtime.
pub fn python(root: &Path) -> Option<PathBuf> {
    match status(root) {
        Status::Ready { python, .. } => Some(python),
        Status::Missing { .. } | Status::Stale { .. } => None,
    }
}

/// Where LibreOffice's command is on this machine, if it is installed: on `PATH`, or where its installer puts it.
pub fn libreoffice() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["soffice.exe", "soffice.com"] } else { &["soffice", "libreoffice"] };
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>());
    let usual: Vec<PathBuf> = if cfg!(windows) {
        ["ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(|base| PathBuf::from(base).join("LibreOffice").join("program")).collect()
    } else if cfg!(target_os = "macos") {
        vec![PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS")]
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin"), PathBuf::from("/opt/libreoffice/program"), PathBuf::from("/snap/bin")]
    };
    on_path.chain(usual).flat_map(|directory| names.iter().map(move |name| directory.join(name))).find(|candidate| candidate.is_file())
}

/// Read an archive from `address` into `into`, checking that its SHA-256 is `sha256`; `watch` is told how far it is.
async fn download(address: &str, sha256: &str, into: &Path, watch: &(dyn Fn(Progress) + Send + Sync), cancel: &CancellationToken) -> Result<(), String> {
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build().map_err(|error| error.to_string())?;
    let response = tokio::select! {
        response = http.get(address).send() => response.map_err(|error| format!("连不上下载地址（{address}）：{error}"))?,
        () = cancel.cancelled() => return Err("已取消。".to_string()),
    };
    if !response.status().is_success() {
        return Err(format!("下载失败：{address} 答 {}", response.status()));
    }
    let total = response.content_length();
    let mut file = tokio::fs::File::create(into).await.map_err(|error| format!("写不了 {}：{error}", into.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut told = 0u64;
    let mut stream = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            chunk = stream.next() => chunk,
            () = cancel.cancelled() => return Err("已取消。".to_string()),
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|error| format!("下载中断：{error}"))?;
        hasher.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await.map_err(|error| format!("写不了 {}：{error}", into.display()))?;
        done += chunk.len() as u64;
        // Told about every half megabyte, not at every chunk.
        if done - told >= 512 * 1024 {
            told = done;
            watch(Progress::Downloading { done, total });
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await.map_err(|error| error.to_string())?;
    watch(Progress::Downloading { done, total });
    let got: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if got != sha256 {
        return Err(format!("下载的文件校验不符，没有安装：应为 {sha256}，实为 {got}。可能是网络或镜像出了问题，可以再试一次。"));
    }
    Ok(())
}

/// Unpack a `.tar.gz` into `into`. Only what stays inside `into` is written: the `tar` crate refuses entries that climb out.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|error| error.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.unpack(into).map_err(|error| format!("解不开下载的 Python：{error}"))
}

/// Run `python` with `arguments`, giving up after `limit` or when cancelled; what went wrong is the end of its output.
async fn run(python: &Path, arguments: &[&str], limit: Duration, cancel: &CancellationToken) -> Result<(), String> {
    let mut command = tokio::process::Command::new(python);
    command.args(arguments).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    // Its own, whatever the user's Python is set up to do.
    command.env("PYTHONNOUSERSITE", "1").env_remove("PYTHONPATH").env_remove("PYTHONHOME").env("PIP_DISABLE_PIP_VERSION_CHECK", "1").env("PIP_NO_INPUT", "1");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = tokio::select! {
        output = tokio::time::timeout(limit, command.output()) => output,
        () = cancel.cancelled() => return Err("已取消。".to_string()),
    };
    match output {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => {
            let said = String::from_utf8_lossy(&output.stderr);
            let tail: Vec<&str> = said.lines().rev().take(12).collect();
            Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!("超过 {} 分钟没有完成，停下了。", limit.as_secs() / 60)),
    }
}

/// Have pip install the pinned libraries into `python`: only what is missing or at another version is fetched.
async fn install_libraries(python: &Path, folder: &Path, watch: &(dyn Fn(Progress) + Send + Sync), cancel: &CancellationToken) -> Result<(), String> {
    watch(Progress::Installing);
    let requirements = folder.join(format!(".requirements-{}.txt", uuid::Uuid::now_v7()));
    std::fs::write(&requirements, REQUIREMENTS).map_err(|error| error.to_string())?;
    let listed = requirements.to_string_lossy().into_owned();
    let installed = run(python, &["-m", "pip", "install", "--require-hashes", "--only-binary", ":all:", "--no-warn-script-location", "-r", &listed], Duration::from_secs(20 * 60), cancel).await;
    let _ = std::fs::remove_file(&requirements);
    installed.map_err(|why| format!("安装文档用的库失败：\n{why}"))?;
    watch(Progress::Checking);
    let imports = format!("import {}", MODULES.iter().map(|(module, _)| *module).collect::<Vec<_>>().join(", "));
    run(python, &["-c", &imports], Duration::from_secs(120), cancel).await.map_err(|why| format!("装好的库导入不了：\n{why}"))
}

/// Bring the runtime under `root` up to date, and return its interpreter. A runtime whose Python is this build's has
/// only its libraries brought up to date, in place: pip fetches what is missing or changed, nothing else; should that
/// fail, the record still says what was there, so it is tried again next time. Otherwise the whole runtime is fetched
/// and set up in a folder of its own, and moved into place only once it imports every library: a runtime half set
/// up is never left where it would be used.
pub async fn install(root: &Path, watch: &(dyn Fn(Progress) + Send + Sync), cancel: &CancellationToken) -> Result<PathBuf, String> {
    // One at a time: the settings page, the app as it starts, and a 分身 may all ask. The later ones find it done.
    static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _held = tokio::select! {
        held = INSTALLING.lock() => held,
        () = cancel.cancelled() => return Err("已取消。".to_string()),
    };
    match status(root) {
        Status::Ready { python, .. } => {
            watch(Progress::Done);
            return Ok(python);
        }
        Status::Stale { python, .. } => {
            install_libraries(&python, root, watch, cancel).await?;
            let triple = target().unwrap_or_default();
            std::fs::write(root.join("runtime.json"), serde_json::to_vec_pretty(&Installed::current(triple)).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            watch(Progress::Done);
            return Ok(python);
        }
        Status::Missing { .. } => {}
    }
    let triple = target().ok_or_else(|| format!("这个系统（{} {}）没有可用的文档运行环境。", std::env::consts::OS, std::env::consts::ARCH))?;
    let sha256 = BUILDS.iter().find(|(name, _)| *name == triple).map(|(_, sha)| *sha).ok_or("no checksum")?;
    std::fs::create_dir_all(root).map_err(|error| format!("建不了 {}：{error}", root.display()))?;
    let staging = root.join(format!(".installing-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let result = async {
        let base = std::env::var("ODG_PYTHON_MIRROR").ok().filter(|mirror| !mirror.trim().is_empty()).unwrap_or_else(|| PYTHON_ADDRESS.to_string());
        let address = format!("{}/{PYTHON_RELEASE}/cpython-{PYTHON_VERSION}+{PYTHON_RELEASE}-{triple}-install_only_stripped.tar.gz", base.trim_end_matches('/'));
        let archive = staging.join("python.tar.gz");
        download(&address, sha256, &archive, watch, cancel).await?;
        watch(Progress::Unpacking);
        let (from, to) = (archive.clone(), staging.clone());
        tokio::task::spawn_blocking(move || unpack(&from, &to)).await.map_err(|error| error.to_string())??;
        let _ = std::fs::remove_file(&archive);
        let python = interpreter(&staging.join("python"));
        if !python.is_file() {
            return Err(format!("下载的 Python 里找不到 {}。", python.display()));
        }
        install_libraries(&python, &staging, watch, cancel).await?;
        std::fs::write(staging.join("runtime.json"), serde_json::to_vec_pretty(&Installed::current(triple)).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        Ok(())
    }
    .await;
    if let Err(why) = result {
        remove_tree(&staging);
        return Err(why);
    }
    // Into place: what was there is taken away first, then the new one moved in.
    let (python, record) = (root.join("python"), root.join("runtime.json"));
    let _ = std::fs::remove_file(&record);
    if python.exists() {
        remove_tree(&python);
    }
    std::fs::rename(staging.join("python"), &python).map_err(|error| format!("放不进 {}：{error}", python.display()))?;
    std::fs::rename(staging.join("runtime.json"), &record).map_err(|error| error.to_string())?;
    remove_tree(&staging);
    watch(Progress::Done);
    Ok(interpreter(&python))
}

/// Take the runtime away.
pub fn remove(root: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(root.join("runtime.json"));
    let python = root.join("python");
    if python.exists() {
        remove_tree(&python);
        if python.exists() {
            return Err(format!("没能删掉 {}：可能有程序正在用它。", python.display()));
        }
    }
    Ok(())
}

/// Remove a folder and all in it, though some files in it are read-only.
fn remove_tree(root: &Path) {
    let mut walk = vec![root.to_path_buf()];
    while let Some(directory) = walk.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().filter_map(Result::ok) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk.push(entry.path());
            } else if let Ok(metadata) = entry.metadata() {
                let mut permissions = metadata.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(entry.path(), permissions);
            }
        }
    }
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_build_and_library_is_pinned_by_its_checksum() {
        assert!(target().is_some(), "this machine has a build");
        assert!(BUILDS.iter().all(|(_, sha)| sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit())));
        for line in REQUIREMENTS.lines().filter(|line| !line.trim().is_empty()) {
            let (pinned, hashes) = line.split_once(' ').unwrap();
            assert!(pinned.contains("=="), "{pinned} is pinned to a version");
            assert!(hashes.split_whitespace().all(|hash| hash.starts_with("--hash=sha256:") && hash.len() == "--hash=sha256:".len() + 64), "{pinned}");
        }
        // What the libraries need is pinned too: pip refuses to install with hashes otherwise.
        for needed in ["lxml", "pillow", "et-xmlfile", "typing-extensions", "charset-normalizer", "numpy", "python-dateutil", "tzdata"] {
            assert!(REQUIREMENTS.lines().any(|line| line.to_ascii_lowercase().starts_with(&format!("{needed}=="))), "{needed}");
        }
    }

    #[test]
    fn tells_a_python_to_fetch_from_libraries_to_bring_up_to_date() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert_eq!(status(root), Status::Missing { outdated: false });
        let python = interpreter(&root.join("python"));
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        let record = |text: &str| std::fs::write(root.join("runtime.json"), text).unwrap();
        // Set up by a build that kept no list of libraries: its Python stays, its libraries are brought up to date.
        record(r#"{"edition": 2, "python": "3.12.16", "target": "t"}"#);
        assert_eq!(status(root), Status::Stale { python: python.clone(), version: "3.12.16".into() });
        // Another list of libraries: the same.
        record(&serde_json::to_string(&Installed { libraries: "0".repeat(64), ..Installed::current("t") }).unwrap());
        assert!(matches!(status(root), Status::Stale { .. }));
        // Another Python: the whole of it is fetched again.
        record(&serde_json::to_string(&Installed { python: "3.11.9".into(), ..Installed::current("t") }).unwrap());
        assert_eq!(status(root), Status::Missing { outdated: true });
        record(&serde_json::to_string(&Installed::current("t")).unwrap());
        assert_eq!(status(root), Status::Ready { python: python.clone(), version: "3.12.16".into() });
        // The list is told by what it says, not by how its lines end.
        assert_eq!(libraries().len(), 64);
        remove(root).unwrap();
        assert_eq!(status(root), Status::Missing { outdated: false });
        assert!(!root.join("python").exists());
    }

    /// Serve `body` once at a local address.
    async fn serve(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}/archive.tar.gz", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
            tokio::io::AsyncWriteExt::write_all(&mut socket, head.as_bytes()).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut socket, &body).await.unwrap();
        });
        address
    }

    #[tokio::test]
    async fn downloads_only_what_matches_its_checksum_and_unpacks_it_inside_where_it_goes() {
        let directory = tempfile::tempdir().unwrap();
        // A small archive with a folder and a file in it.
        let mut packed = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let content = b"print('hello')\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        packed.append_data(&mut header, "python/hello.py", &content[..]).unwrap();
        let archive = packed.into_inner().unwrap().finish().unwrap();
        let sha: String = Sha256::digest(&archive).iter().map(|byte| format!("{byte:02x}")).collect();
        let steps = std::sync::Mutex::new(Vec::new());
        let watch = |step: Progress| steps.lock().unwrap().push(step);
        let into = directory.path().join("got.tar.gz");
        download(&serve(archive.clone()).await, &sha, &into, &watch, &CancellationToken::new()).await.unwrap();
        assert_eq!(steps.lock().unwrap().last(), Some(&Progress::Downloading { done: archive.len() as u64, total: Some(archive.len() as u64) }));
        unpack(&into, directory.path()).unwrap();
        assert_eq!(std::fs::read(directory.path().join("python/hello.py")).unwrap(), content);
        // A file that does not match is refused, whatever it holds.
        let wrong = download(&serve(archive).await, &"0".repeat(64), &into, &watch, &CancellationToken::new()).await.unwrap_err();
        assert!(wrong.contains("校验不符"), "{wrong}");
    }

    #[test]
    fn finds_libreoffice_where_it_is_or_says_it_is_not_there() {
        // Whatever this machine has: the answer is a file that exists, or nothing.
        assert!(libreoffice().is_none_or(|path| path.is_file()));
    }
}
