//! Document Python runtime, LSP server management, background jobs, PTY shell IPC,
//! and sandboxed `!` inline shell execution.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Emitter, State};

use crate::pty;
use crate::state::App;

/// The event that says how far setting up the document runtime has got.
pub const RUNTIME_EVENT: &str = "runtime-progress";

const INLINE_SENSITIVE_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "ZHIPU_API_KEY",
    "GLM_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
    "DASHSCOPE_API_KEY",
    "SILICONFLOW_API_KEY",
];

#[derive(Serialize)]
pub struct JobLogView {
    pub job: odg_core::jobs::JobSnapshot,
    pub output: String,
}

/// What the page shows of the document runtime: how it stands, and what LibreOffice it would use.
#[derive(Serialize)]
pub struct RuntimeView {
    pub status: odg_core::runtime::Status,
    pub python_version: &'static str,
    pub download_mb: u32,
    pub libreoffice: Option<String>,
    /// Whether this system has a build of it at all.
    pub supported: bool,
}

fn runtime_view(runtime_dir: &Path) -> RuntimeView {
    RuntimeView {
        status: odg_core::runtime::status(runtime_dir),
        python_version: odg_core::runtime::PYTHON_VERSION,
        download_mb: odg_core::runtime::DOWNLOAD_MB,
        libreoffice: odg_core::runtime::libreoffice().map(|path| path.display().to_string()),
        supported: odg_core::runtime::target().is_some(),
    }
}

/// A language's server, as the settings page lists it: where it is found, and whether the app can download it.
#[derive(Serialize)]
pub struct LanguageServerView {
    pub key: &'static str,
    pub name: &'static str,
    pub program: &'static str,
    pub state: &'static str,
    pub location: Option<String>,
    pub download: Option<&'static str>,
    pub version: Option<&'static str>,
    pub download_mb: Option<u64>,
    pub install: &'static str,
}

async fn language_server_views(root: Option<&Path>) -> Vec<LanguageServerView> {
    let mut views = Vec::new();
    for language in odg_core::lsp::LANGUAGES {
        let (state, location) = match odg_core::lsp::locate(root, language).await {
            odg_core::lsp::Located::System(path) => ("system", Some(path.to_string_lossy().into_owned())),
            odg_core::lsp::Located::Downloaded(path) => ("downloaded", Some(path.to_string_lossy().into_owned())),
            odg_core::lsp::Located::Missing => ("missing", None),
        };
        views.push(LanguageServerView {
            key: language.key,
            name: language.name,
            program: match (state, language.download) {
                ("downloaded", Some(download)) => download.program,
                _ => language.program,
            },
            state,
            location,
            download: language.download.map(|download| download.name),
            version: language.download.map(|download| download.version),
            download_mb: language.download.and_then(|download| download.megabytes()),
            install: language.install,
        });
    }
    views
}

#[tauri::command]
pub async fn runtime_status(app: State<'_, App>) -> Result<RuntimeView, String> {
    let dir = app.runtime_dir();
    tokio::task::spawn_blocking(move || runtime_view(&dir))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn install_runtime(handle: tauri::AppHandle, app: State<'_, App>) -> Result<RuntimeView, String> {
    let dir = app.runtime_dir();
    let watch = move |step: odg_core::runtime::Progress| {
        let _ = handle.emit(RUNTIME_EVENT, step);
    };
    odg_core::runtime::install(&dir, &watch, &tokio_util::sync::CancellationToken::new()).await?;
    tokio::task::spawn_blocking(move || runtime_view(&dir))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_runtime(app: State<'_, App>) -> Result<RuntimeView, String> {
    let dir = app.runtime_dir();
    tokio::task::spawn_blocking(move || {
        odg_core::runtime::remove(&dir)?;
        Ok(runtime_view(&dir))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn language_servers(app: State<'_, App>) -> Result<Vec<LanguageServerView>, String> {
    Ok(language_server_views(app.lsp.root()).await)
}

#[tauri::command]
pub async fn install_language_server(
    app: State<'_, App>,
    key: String,
) -> Result<Vec<LanguageServerView>, String> {
    let language = odg_core::lsp::language_by_key(&key).ok_or("没有这种语言")?;
    let root = app.lsp.root().ok_or("这里不能下载语言服务")?.to_path_buf();
    odg_core::lsp::install(&root, language, &tokio_util::sync::CancellationToken::new()).await?;
    Ok(language_server_views(Some(&root)).await)
}

#[tauri::command]
pub async fn remove_language_server(
    app: State<'_, App>,
    key: String,
) -> Result<Vec<LanguageServerView>, String> {
    let language = odg_core::lsp::language_by_key(&key).ok_or("没有这种语言")?;
    let root = app.lsp.root().ok_or("这里没有下载的语言服务")?.to_path_buf();
    app.lsp.shutdown_language(language.key).await;
    let remove_root = root.clone();
    tokio::task::spawn_blocking(move || odg_core::lsp::remove(&remove_root, language))
        .await
        .map_err(|e| e.to_string())??;
    Ok(language_server_views(Some(&root)).await)
}

#[tauri::command]
pub async fn list_jobs(app: State<'_, App>, id: String) -> Result<Vec<odg_core::jobs::JobSnapshot>, String> {
    let session = app.get_loaded(&id);
    Ok(session.map(|s| s.jobs().list()).unwrap_or_default())
}

#[tauri::command]
pub async fn read_job_output(
    app: State<'_, App>,
    id: String,
    job_id: String,
    tail_bytes: Option<usize>,
) -> Result<JobLogView, String> {
    let session = app.get_loaded(&id).ok_or_else(|| "会话尚未加载。".to_string())?;
    let cap = tail_bytes.unwrap_or(64_000).clamp(1_024, 400_000);
    let (job, output) = session
        .jobs()
        .peek_output(&job_id, cap)
        .ok_or_else(|| format!("未找到后台任务 {job_id}。"))?;
    Ok(JobLogView { job, output })
}

#[tauri::command]
pub async fn stop_job(
    app: State<'_, App>,
    id: String,
    job_id: String,
) -> Result<odg_core::jobs::JobSnapshot, String> {
    let session = app.get_loaded(&id).ok_or_else(|| "会话尚未加载。".to_string())?;
    session
        .jobs()
        .stop_from_ui(&job_id)
        .await
        .ok_or_else(|| format!("未找到后台任务 {job_id}。"))
}

#[tauri::command]
pub async fn write_job_input(
    app: State<'_, App>,
    id: String,
    job_id: String,
    text: String,
) -> Result<(), String> {
    let session = app.get_loaded(&id).ok_or_else(|| "会话尚未加载。".to_string())?;
    session.jobs().write(&job_id, &text, false).await
}

#[tauri::command]
pub async fn dismiss_job(app: State<'_, App>, id: String, job_id: String) -> Result<bool, String> {
    let session = app.get_loaded(&id);
    Ok(session.is_some_and(|s| s.jobs().remove_ended(&job_id)))
}

#[tauri::command]
pub async fn pty_spawn(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<pty::PtyInfo, String> {
    let workspace = app.0.session_workspace(&id).await?;
    let hub = app.pty.clone();
    let out_handle = handle.clone();
    let exit_handle = handle;
    tokio::task::spawn_blocking(move || {
        hub.spawn(
            id,
            &workspace,
            cols,
            rows,
            move |payload| {
                let _ = out_handle.emit(pty::PTY_OUTPUT_EVENT, payload);
            },
            move |payload| {
                let _ = exit_handle.emit(pty::PTY_EXIT_EVENT, payload);
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn pty_list(app: State<'_, App>, id: String) -> Vec<pty::PtyInfo> {
    app.pty.list(&id)
}

#[tauri::command]
pub fn pty_write(app: State<'_, App>, pty_id: String, data: String) -> Result<(), String> {
    app.pty.write(&pty_id, &data)
}

#[tauri::command]
pub fn pty_resize(app: State<'_, App>, pty_id: String, cols: u16, rows: u16) -> Result<(), String> {
    app.pty.resize(&pty_id, cols, rows)
}

#[tauri::command]
pub fn pty_clear(app: State<'_, App>, pty_id: String) {
    app.pty.clear_backlog(&pty_id);
}

#[tauri::command]
pub fn pty_close(app: State<'_, App>, pty_id: String) {
    app.pty.close(&pty_id);
}

#[cfg(target_os = "macos")]
fn enforce_macos_inline_workspace_denies(
    cmd: tokio::process::Command,
    cwd: &Path,
    git_dir: Option<&Path>,
    common_git_dir: Option<&Path>,
) -> tokio::process::Command {
    let std_cmd = cmd.as_std();
    if std_cmd.get_program() != std::ffi::OsStr::new("/usr/bin/sandbox-exec") {
        return cmd;
    }
    let args: Vec<std::ffi::OsString> = std_cmd.get_args().map(|a| a.to_os_string()).collect();
    let Some(p_idx) = args.iter().position(|a| a == "-p") else {
        return cmd;
    };
    let Some(profile_os) = args.get(p_idx + 1) else {
        return cmd;
    };
    let Some(profile) = profile_os.to_str() else {
        return cmd;
    };
    if profile.contains("DENY_") || profile.contains("appleevent-send") {
        return cmd;
    }

    let ws_canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut denies: Vec<(bool, PathBuf)> = Vec::new();
    let push_deny = |denies: &mut Vec<(bool, PathBuf)>, is_subpath: bool, path: PathBuf| {
        let resolved = path.canonicalize().unwrap_or(path);
        if !denies.iter().any(|(sub, p)| *sub == is_subpath && *p == resolved) {
            denies.push((is_subpath, resolved));
        }
    };

    push_deny(&mut denies, true, ws_canon.join(".odg"));
    let dot_git = ws_canon.join(".git");
    if dot_git.is_file() {
        push_deny(&mut denies, false, dot_git.clone());
        push_deny(&mut denies, false, ws_canon.join(".git.lock"));
    }

    let mut git_dirs = Vec::new();
    if !dot_git.is_file() {
        git_dirs.push(dot_git);
    }
    if let Some(g) = git_dir {
        git_dirs.push(g.to_path_buf());
    }
    if let Some(c) = common_git_dir {
        git_dirs.push(c.to_path_buf());
    }
    for gdir in git_dirs {
        let base = gdir.canonicalize().unwrap_or(gdir);
        push_deny(&mut denies, true, base.join("hooks"));
        push_deny(&mut denies, true, base.join("info"));
        push_deny(&mut denies, false, base.join("config"));
        push_deny(&mut denies, false, base.join("config.lock"));
        push_deny(&mut denies, false, base.join("config.worktree"));
        push_deny(&mut denies, false, base.join("config.worktree.lock"));
    }

    let mut extra_rules = String::from("(deny appleevent-send)\n");
    if !denies.is_empty() {
        extra_rules.push_str("(deny file-write*");
        for (idx, (is_subpath, _)) in denies.iter().enumerate() {
            if *is_subpath {
                extra_rules.push_str(&format!(" (subpath (param \"INLINE_DENY_{idx}\"))"));
            } else {
                extra_rules.push_str(&format!(" (literal (param \"INLINE_DENY_{idx}\"))"));
            }
        }
        extra_rules.push_str(")\n");
    }

    let new_profile = format!("{profile}{extra_rules}");
    let mut rebuilt = tokio::process::Command::new("/usr/bin/sandbox-exec");
    let program_idx = args.len().saturating_sub(1);
    for (idx, arg) in args.iter().enumerate() {
        if idx == p_idx + 1 {
            rebuilt.arg(&new_profile);
        } else if idx == program_idx {
            for (d_idx, (_, path)) in denies.iter().enumerate() {
                let mut param = std::ffi::OsString::from(format!("INLINE_DENY_{d_idx}="));
                param.push(path);
                rebuilt.arg("-D").arg(param);
            }
            rebuilt.arg(arg);
        } else {
            rebuilt.arg(arg);
        }
    }
    rebuilt
}

pub fn strip_inline_ansi(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    if !raw.contains('\u{1b}') {
        return raw;
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                let _ = chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                let _ = chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{07}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                        break;
                    }
                }
            }
            Some(_) => {
                let _ = chars.next();
            }
            None => {}
        }
    }
    out
}

async fn capture_inline_pipe(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    const CAP: usize = 1024 * 1024;
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let room = CAP.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    kept
}

pub async fn run_inline_shell_with_sandbox(
    command_line: &str,
    cwd: &Path,
    sandbox_extra: Option<Vec<PathBuf>>,
    timeout_secs: u64,
    max_chars: usize,
) -> Result<odg_core::tools::InlineShellOutput, String> {
    let Some(mut extra) = sandbox_extra.filter(|_| odg_core::sandbox::support().is_ok()) else {
        return odg_core::tools::run_inline_shell(command_line, cwd, timeout_secs, max_chars).await;
    };
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return Err("命令不能为空".to_string());
    }

    let cwd_owned = cwd.to_path_buf();
    let (git_writable, git_dir, common_git_dir) =
        tokio::task::spawn_blocking(move || odg_core::git::sandbox_git_paths(&cwd_owned))
            .await
            .map_err(|e| e.to_string())?;
    extra.extend(git_writable);
    let sandbox = odg_core::sandbox::Sandbox {
        writable: odg_core::sandbox::roots(cwd, &extra),
    };
    let (program, shell_args, _) = odg_core::tools::shell();
    let cmd = odg_core::sandbox::command(program, &sandbox)
        .map_err(|e| format!("无法在沙箱中启动终端命令：{e}"))?;
    #[cfg(target_os = "macos")]
    let mut cmd = enforce_macos_inline_workspace_denies(
        cmd,
        cwd,
        git_dir.as_deref(),
        common_git_dir.as_deref(),
    );
    #[cfg(not(target_os = "macos"))]
    let mut cmd = {
        let _ = (&git_dir, &common_git_dir);
        cmd
    };

    cmd.args(shell_args);
    if cfg!(windows) {
        cmd.arg(format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; if ($PSStyle) {{ $PSStyle.OutputRendering = 'PlainText' }}; {trimmed}"
        ));
    } else {
        cmd.arg(trimmed);
    }
    cmd.env("NO_COLOR", "1");
    for var in INLINE_SENSITIVE_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(|e| format!("无法执行命令 `{trimmed}`：{e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(pipe) => capture_inline_pipe(pipe).await,
            None => Vec::new(),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(pipe) => capture_inline_pipe(pipe).await,
            None => Vec::new(),
        }
    });

    let mut timed_out = false;
    let status = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs.max(1)), child.wait()).await {
        Ok(res) => res.ok(),
        Err(_) => {
            timed_out = true;
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
            }
            let _ = child.kill().await;
            None
        }
    };

    let drain = |task| async {
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let out_bytes = drain(stdout_task).await;
    let err_bytes = drain(stderr_task).await;
    let out_str = strip_inline_ansi(&out_bytes);
    let err_str = strip_inline_ansi(&err_bytes);

    let mut combined = out_str.trim_end().to_string();
    if !err_str.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&format!("[stderr]\n{}", err_str.trim_end()));
    }

    let total = combined.chars().count();
    let truncated = total > max_chars;
    if truncated {
        let half = max_chars / 2;
        let head: String = combined.chars().take(half).collect();
        let tail: String = combined.chars().skip(total - half).collect();
        combined = format!("{head}\n[... 省略 {} 个字符 ...]\n{tail}", total - max_chars);
    }

    Ok(odg_core::tools::InlineShellOutput {
        command: trimmed.to_string(),
        output: combined,
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        truncated,
    })
}

#[tauri::command]
pub async fn run_inline_command(
    app: State<'_, App>,
    id: String,
    command: String,
) -> Result<odg_core::tools::InlineShellOutput, String> {
    let core = app.0.clone();
    let id_lookup = id.clone();
    let (workspace, sandbox_extra) = tokio::task::spawn_blocking(move || {
        let ws = core
            .all_live()
            .get(&id_lookup)
            .map(|s| s.summary().workspace)
            .or_else(|| core.store.peek_idle(&id_lookup).ok().map(|s| s.workspace))
            .ok_or_else(|| "找不到当前会话的洞府目录".to_string())?;
        Ok::<_, String>((ws, core.settings.sandbox()))
    })
    .await
    .map_err(|e| e.to_string())??;

    let cwd = PathBuf::from(&workspace);
    if !cwd.is_dir() {
        return Err(format!("洞府目录不存在：{}", cwd.display()));
    }
    run_inline_shell_with_sandbox(&command, &cwd, sandbox_extra, 15, 32_000).await
}
