//! Known language servers, locating them on `PATH`, and downloading pinned builds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::LspHub;

/// A build of a language server the app can download where it is not installed: what it is, how it is started, its
/// version, where its release is, and for each target the archive (its path under the release's address), its SHA-256
/// and its size, from the release's published digests. The archive is the program alone, gzipped, or a `.tar.gz`,
/// `.tgz` or `.zip` holding it: then the program is kept with everything beside it in the archive's folder.
#[derive(Debug)]
pub struct Download {
    /// What is downloaded, as the user is told of it.
    pub name: &'static str,
    /// The program downloaded, which need not be the one looked for on the `PATH`, and what it is started with.
    pub program: &'static str,
    pub(super) args: &'static [&'static str],
    pub version: &'static str,
    pub(super) address: &'static str,
    pub(super) builds: &'static [(&'static str, &'static str, &'static str, u64)],
}

impl Download {
    /// This machine's build: the archive's name, its SHA-256, and its size.
    fn build(&self) -> Option<(&'static str, &'static str, u64)> {
        let target = crate::runtime::target()?;
        self.builds
            .iter()
            .find(|(triple, ..)| *triple == target)
            .map(|(_, file, sha256, bytes)| (*file, *sha256, *bytes))
    }

    /// About how many megabytes this machine's build downloads.
    pub fn megabytes(&self) -> Option<u64> {
        self.build().map(|(_, _, bytes)| bytes.div_ceil(1_000_000))
    }
}

/// rust-analyzer's weekly release of 2026-11-16, from <https://github.com/rust-lang/rust-analyzer/releases/tag/2026-11-16>.
pub(super) static RUST_ANALYZER: Download = Download {
    name: "rust-analyzer",
    program: "rust-analyzer",
    args: &[],
    version: "2026-11-16",
    address: "https://github.com/rust-lang/rust-analyzer/releases/download/2026-11-16",
    builds: &[
        (
            "aarch64-apple-darwin",
            "rust-analyzer-aarch64-apple-darwin.gz",
            "608b7c7d6bec1fcd6c964578b1f8234888afe6d6c0e7915284f1e2fd36ef1343",
            14_345_012,
        ),
        (
            "x86_64-apple-darwin",
            "rust-analyzer-x86_64-apple-darwin.gz",
            "6901d956b4f2b091343eaf3570944098488f3ffec018987145285ed8253c07d2",
            15_080_897,
        ),
        (
            "x86_64-unknown-linux-gnu",
            "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            "4a08727a03094e5114db4438e4bd1d6182cc26eff37d6962115ea1ec1a491ca3",
            15_663_964,
        ),
        (
            "aarch64-unknown-linux-gnu",
            "rust-analyzer-aarch64-unknown-linux-gnu.gz",
            "73d8b9b87384b3115a183a8a51db9f7c205ce4c89ea511f3c8c8fea863038a50",
            15_120_173,
        ),
        (
            "x86_64-pc-windows-msvc",
            "rust-analyzer-x86_64-pc-windows-msvc.zip",
            "dbf0c36ec54cf727d34cc40c9994fe71b7d52e3c28a3112c3deca1aba46fbc8f",
            18_054_633,
        ),
        (
            "aarch64-pc-windows-msvc",
            "rust-analyzer-aarch64-pc-windows-msvc.zip",
            "2e8fd876300eeee87afd6de18c79eb17bab425a9b755043c63d5023d58de37b7",
            16_197_678,
        ),
    ],
};

/// TypeScript 7.0.2, from npm: its native compiler for each system, which serves the language protocol with `--lsp`.
/// Digests are the SHA-256 of each package's tarball, checked against the registry's own SHA-512 integrity.
pub(super) static TYPESCRIPT: Download = Download {
    name: "TypeScript",
    program: "tsc",
    args: &["--lsp", "--stdio"],
    version: "7.0.2",
    address: "https://registry.npmjs.org/@typescript",
    builds: &[
        (
            "aarch64-apple-darwin",
            "typescript-darwin-arm64/-/typescript-darwin-arm64-7.0.2.tgz",
            "902e2fe1cf0799198ef902c6b8c310a450fef629a6baba41d45641ef75c04ebd",
            9_270_373,
        ),
        (
            "x86_64-apple-darwin",
            "typescript-darwin-x64/-/typescript-darwin-x64-7.0.2.tgz",
            "eba158cb54050f723d5ff781438f33de5640054440bb4f2bd170cfe9bc2eb551",
            10_057_523,
        ),
        (
            "x86_64-unknown-linux-gnu",
            "typescript-linux-x64/-/typescript-linux-x64-7.0.2.tgz",
            "7ecad6f67377e831856367ab062ef394f21506a611405bf8ac0ff039348637d3",
            9_669_334,
        ),
        (
            "aarch64-unknown-linux-gnu",
            "typescript-linux-arm64/-/typescript-linux-arm64-7.0.2.tgz",
            "c83d931ac9dd7549cde6e71246aa9d6a9812843023df3e277fe3b5dcf41dd0ea",
            8_900_516,
        ),
        (
            "x86_64-pc-windows-msvc",
            "typescript-win32-x64/-/typescript-win32-x64-7.0.2.tgz",
            "61fc4e141d2bc687db580e71bbfa63b9c209f0310645d82ca1b457eb3a24fd19",
            9_776_626,
        ),
        (
            "aarch64-pc-windows-msvc",
            "typescript-win32-arm64/-/typescript-win32-arm64-7.0.2.tgz",
            "0a73534e6ee50cdbb2a29ac48657ca0ad13cf0f424cf63808e4df7baeb87b8be",
            8_900_840,
        ),
    ],
};

/// ty 0.0.107, Astral's type checker for Python, from <https://github.com/astral-sh/ty/releases/tag/0.0.107>.
pub(super) static TY: Download = Download {
    name: "ty",
    program: "ty",
    args: &["server"],
    version: "0.0.107",
    address: "https://github.com/astral-sh/ty/releases/download/0.0.107",
    builds: &[
        (
            "aarch64-apple-darwin",
            "ty-aarch64-apple-darwin.tar.gz",
            "5b514ec3271383263250481b139c2eb59021ca674866b267f37bb988dbea52c9",
            13_896_972,
        ),
        (
            "x86_64-apple-darwin",
            "ty-x86_64-apple-darwin.tar.gz",
            "5bff07e2e802815a303cdb7864b7034347bebb63ad159e22a514b5555d87aae5",
            14_469_364,
        ),
        (
            "x86_64-unknown-linux-gnu",
            "ty-x86_64-unknown-linux-gnu.tar.gz",
            "f93e1f3e45ec81b3ce21473d541eb94470546928499415fc263f2858d65f28af",
            17_385_552,
        ),
        (
            "aarch64-unknown-linux-gnu",
            "ty-aarch64-unknown-linux-gnu.tar.gz",
            "952097983ff6b6b3235b4b5ec8077c1b8bf5c485679fd794ee70ca2007a3cf7f",
            14_291_303,
        ),
        (
            "x86_64-pc-windows-msvc",
            "ty-x86_64-pc-windows-msvc.zip",
            "3ba361f2edda769040daffb52580d4a9a5f54f7c6040d222fd634aa70d2b9145",
            14_846_532,
        ),
        (
            "aarch64-pc-windows-msvc",
            "ty-aarch64-pc-windows-msvc.zip",
            "e09566dd2727d8ec45d39c572040b9b6b4123ea308c1c6200a98e83c65b5c036",
            13_982_334,
        ),
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
    /// Argument passed to verify the program runs (e.g. `--version`, or `version` for `gopls`).
    pub(super) version_arg: &'static str,
    pub(super) args: &'static [&'static str],
    pub(super) extensions: &'static [&'static str],
    /// How to install the server, said when it is missing.
    pub install: &'static str,
    /// The server on the `PATH` runs code from the project, so starting it is asked about first. The one downloaded
    /// never does.
    pub runs_project_code: bool,
    pub(super) env: &'static [(&'static str, &'static str)],
    /// The build the app can download where the server is not installed; `None` where it cannot.
    pub download: Option<&'static Download>,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        key: "rust",
        name: "Rust",
        program: "rust-analyzer",
        version_arg: "--version",
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
        version_arg: "--version",
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
        version_arg: "--version",
        args: &["--stdio"],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        install: "npm install -g typescript-language-server typescript",
        runs_project_code: true,
        // The server fetches type packages for plain JavaScript with npm, unasked: npm is kept offline, so it does not.
        env: &[("npm_config_offline", "true")],
        download: Some(&TYPESCRIPT),
    },
    Language {
        key: "python",
        name: "Python",
        program: "pyright-langserver",
        version_arg: "--version",
        args: &["--stdio"],
        extensions: &["py", "pyi"],
        install: "npm install -g pyright",
        runs_project_code: false,
        env: &[],
        download: Some(&TY),
    },
    Language {
        key: "go",
        name: "Go",
        program: "gopls",
        version_arg: "version",
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
pub(super) fn executable(program: &str) -> String {
    if cfg!(windows) {
        format!("{program}.exe")
    } else {
        program.to_string()
    }
}

/// Where the app keeps its download of a server, under `root` (the app's `lsp` folder), when there is one.
pub fn downloaded(root: &Path, language: &Language) -> Option<PathBuf> {
    let download = language.download?;
    let path = root
        .join(download.program)
        .join(download.version)
        .join(executable(download.program));
    path.is_file().then_some(path)
}

/// Whether a program answers its version check: a stand-in that fails, such as rustup's for a component not installed,
/// does not. Asked once for each program while the app runs.
pub(super) async fn works(program: &Path, version_arg: &'static str) -> bool {
    static ASKED: std::sync::LazyLock<Mutex<HashMap<(PathBuf, &'static str), bool>>> =
        std::sync::LazyLock::new(Mutex::default);
    let key = (program.to_path_buf(), version_arg);
    if let Some(known) = ASKED.lock().unwrap().get(&key) {
        return *known;
    }
    let mut command = tokio::process::Command::new(program);
    command
        .arg(version_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let answered = matches!(tokio::time::timeout(Duration::from_secs(15), command.status()).await, Ok(Ok(status)) if status.success());
    ASKED.lock().unwrap().insert(key, answered);
    answered
}

/// Where a language's server is: on the `PATH` if it runs, else downloaded under `root`, else nowhere.
pub async fn locate(root: Option<&Path>, language: &Language) -> Located {
    if let Some(program) = installed(language.program)
        && works(&program, language.version_arg).await
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
pub(super) async fn install_from(
    root: &Path,
    language: &Language,
    address: &str,
    cancel: &CancellationToken,
) -> Result<PathBuf, String> {
    let download = language.download.ok_or_else(|| {
        format!(
            "{} cannot be downloaded by the app. Ask the user to install it: {}.",
            language.program, language.install
        )
    })?;
    let (file, sha256, _) = download
        .build()
        .ok_or_else(|| format!("There is no build of {} for this machine to download.", download.name))?;
    let folder = root.join(download.program);
    let into = folder.join(download.version);
    std::fs::create_dir_all(&into).map_err(|error| format!("Cannot create {}: {error}", into.display()))?;
    let name = file.rsplit('/').next().unwrap_or(file);
    let archive = folder.join(format!("{name}.part"));
    let fetched = crate::runtime::download(&format!("{address}/{file}"), sha256, &archive, &|_| {}, cancel).await;
    let program = download.program;
    let unpacked = match fetched {
        Ok(()) => {
            let (archive, into) = (archive.clone(), into.clone());
            tokio::task::spawn_blocking(move || unpack_program(&archive, &into, program))
                .await
                .map_err(|error| error.to_string())
                .and_then(|unpacked| unpacked)
        }
        Err(error) => Err(error),
    };
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

/// The program out of its archive (`….part`): a gzipped program on its own, or a `.tar.gz`, `.tgz` or `.zip` holding
/// it, from which the program's folder is kept, all in it. Only plain files are written, and only inside `into`.
/// The program is made runnable.
pub(super) fn unpack_program(archive: &Path, into: &Path, program: &str) -> Result<PathBuf, String> {
    let name = executable(program);
    let failed = |error: String| format!("Cannot unpack {program}: {error}");
    let bytes = std::fs::read(archive).map_err(|error| failed(error.to_string()))?;
    let archive_name = archive
        .file_name()
        .map(|name| name.to_string_lossy().trim_end_matches(".part").to_string())
        .unwrap_or_default();
    // Every file in the archive, by its path in it.
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let inside = |path: &Path| {
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
    };
    if archive_name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|error| failed(error.to_string()))?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|error| failed(error.to_string()))?;
            let Some(path) = entry.enclosed_name().filter(|path| entry.is_file() && inside(path)) else {
                continue;
            };
            let mut read = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut read).map_err(|error| failed(error.to_string()))?;
            files.push((path, read));
        }
    } else if archive_name.ends_with(".tar.gz") || archive_name.ends_with(".tgz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        for entry in tar.entries().map_err(|error| failed(error.to_string()))? {
            let mut entry = entry.map_err(|error| failed(error.to_string()))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().map_err(|error| failed(error.to_string()))?.into_owned();
            if !inside(&path) {
                continue;
            }
            let mut read = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut read).map_err(|error| failed(error.to_string()))?;
            files.push((path, read));
        }
    } else {
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes.as_slice()), &mut read)
            .map_err(|error| failed(error.to_string()))?;
        files.push((PathBuf::from(&name), read));
    }
    // The program nearest the top, and everything in its folder.
    let found = files
        .iter()
        .filter(|(path, _)| path.file_name().is_some_and(|file| file.to_string_lossy() == name))
        .map(|(path, _)| path.clone())
        .min_by_key(|path| path.components().count());
    let found = found.ok_or_else(|| failed(format!("{name} is not in the archive")))?;
    let folder = found.parent().map(Path::to_path_buf).unwrap_or_default();
    for (path, content) in &files {
        let Ok(relative) = path.strip_prefix(&folder) else {
            continue;
        };
        let target = into.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
        }
        crate::fsutil::write_atomically(&target, content).map_err(|error| failed(error.to_string()))?;
    }
    let target = into.join(&name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| failed(error.to_string()))?;
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
    let Some(download) = language.download else {
        return Ok(());
    };
    let folder = root.join(download.program);
    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(|error| format!("Cannot remove {}: {error}", folder.display()))?;
    }
    Ok(())
}

/// What the agent is told when a language's server is nowhere: how it can be had.
pub(super) fn missing(language: &Language) -> String {
    match language
        .download
        .and_then(|download| download.megabytes().map(|megabytes| (download, megabytes)))
    {
        Some((download, megabytes)) => format!(
            "{} is not installed, so {} code cannot be navigated yet. A server can be downloaded for this app: call `language_server` with `language` \"{}\" and `install` true ({} {}, about {megabytes} MB; the user is asked first). Or the user can install {} themselves: {}.",
            language.program,
            language.name,
            language.key,
            download.name,
            download.version,
            language.program,
            language.install
        ),
        None => format!(
            "{} is not installed, so {} code cannot be navigated here. Ask the user to install it ({}), or use `search` instead.",
            language.program, language.name, language.install
        ),
    }
}

/// Answer `language_server`: whether a language's server is here, and with `install`, download it.
pub async fn server_tool(
    hub: &LspHub,
    input: &Value,
    cancel: &CancellationToken,
) -> Result<crate::tools::ToolOutput, String> {
    let key = input["language"].as_str().unwrap_or_default();
    let language = language_by_key(key).ok_or_else(|| {
        format!(
            "`language` must be one of {}.",
            LANGUAGES
                .iter()
                .map(|language| language.key)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let root = hub.root();
    let located = locate(root, language).await;
    let mut meta = json!({ "language": language.key, "server": language.program });
    let content = match (&located, input["install"] == true, root) {
        (Located::System(path), ..) => format!(
            "{} is installed on this machine ({}); `code_nav` uses it.",
            language.program,
            path.display()
        ),
        (Located::Downloaded(path), ..) => format!(
            "{} was downloaded for this app ({}); `code_nav` uses it.",
            language
                .download
                .map(|download| download.name)
                .unwrap_or(language.program),
            path.display()
        ),
        (Located::Missing, true, Some(root)) => {
            let program = install(root, language, cancel).await?;
            meta["installed"] = json!(true);
            let (name, version) = language
                .download
                .map(|download| (download.name, download.version))
                .unwrap_or_default();
            format!(
                "Downloaded {name} {version} for this app ({}). `code_nav` can use it now.",
                program.display()
            )
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
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

/// The LSP language id of a file, by its extension.
pub(super) fn language_id(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
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

/// What a server, by its program, is told it may be set to, as `initializationOptions` and as the answer to
/// `workspace/configuration`.
pub(super) fn settings(program: &str) -> Value {
    match program {
        // Analysis only: nothing of the project is built or run.
        "rust-analyzer" => {
            json!({ "cargo": { "buildScripts": { "enable": false } }, "procMacro": { "enable": false }, "checkOnSave": false })
        }
        // TypeScript 7 runs no outside program a project's settings name; said all the same, as it is off by default.
        "tsc" => json!({ "runExternalCode": false }),
        _ => Value::Null,
    }
}

/// Where a program is on the `PATH`, if it is there.
pub fn installed(program: &str) -> Option<PathBuf> {
    let directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let names: Vec<String> = if cfg!(windows) {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| format!("{program}{extension}"))
            .collect()
    } else {
        vec![program.to_string()]
    };
    directories
        .iter()
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_file())
}
