//! Foreground and background shell command execution tools.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::jobs::{Ended, Jobs, Report};

use super::{ToolContext, ToolOutput, command_cwd, display, string_arg};

const COMMAND_OUTPUT_CHARS: usize = 30_000;
const COMMAND_CAPTURE_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT_SECONDS: u64 = 120;
const COMMAND_TIMEOUT_LIMIT_SECONDS: u64 = 600;
/// The longest `command_output` waits for a background command to print something.
const OUTPUT_WAIT_LIMIT_SECONDS: u64 = 60;

/// What a failed command printed when it was not let to write somewhere, as the systems that confine say it.
const DENIED: &[&str] = &["permission denied", "operation not permitted", "read-only file system", "eacces", "eperm"];

/// Cloud provider API keys stripped from child shell processes so untrusted scripts or
/// prompt-injected commands cannot exfiltrate host credentials via environment inspection.
pub const SENSITIVE_ENV_VARS: &[&str] = &[
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

/// The shell `run_command` uses on this machine: `(program, arguments before the command, name for the model)`.
pub fn shell() -> (&'static str, &'static [&'static str], &'static str) {
    if cfg!(windows) {
        let has_pwsh = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).any(|directory| directory.join("pwsh.exe").is_file()))
            .unwrap_or(false);
        if has_pwsh {
            ("pwsh.exe", &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"], "PowerShell 7")
        } else {
            (
                "powershell.exe",
                &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
                "Windows PowerShell 5.1 (chain commands with `;`, not `&&`)",
            )
        }
    } else {
        ("sh", &["-c"], "sh")
    }
}

/// Read a pipe to its end, keeping at most `COMMAND_CAPTURE_BYTES`.
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    while let Ok(read) = pipe.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let room = COMMAND_CAPTURE_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
    kept
}

/// Remove terminal escape sequences: colors and cursor moves (CSI), links and
/// titles (OSC), and the two-character escapes. They mean nothing to the
/// model, cost tokens in every later request, and clutter the transcript.
pub(crate) fn strip_ansi(text: &str) -> String {
    static ESCAPES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[ -/]*[0-~])").expect("valid pattern"));
    if text.contains('\u{1b}') { ESCAPES.replace_all(text, "").into_owned() } else { text.to_string() }
}

/// Stop a command and the processes it started.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        // `Child::kill` ends only the shell on Windows; taskkill /T takes its children too.
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status()
            .await;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The shell leads its own process group (see `shell_command`), so this
        // reaches what it started as well: a server it launched does not live on.
        let _ = tokio::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }
    let _ = child.kill().await;
}

/// The shell set up to run `command_line` in `cwd`, its output piped;
/// confined to `sandbox` when there is one. Fails when it cannot be confined.
fn shell_command(command_line: &str, cwd: &Path, sandbox: Option<&crate::sandbox::Sandbox>) -> std::io::Result<tokio::process::Command> {
    let (program, arguments, _) = shell();
    let mut command = match sandbox {
        Some(sandbox) => crate::sandbox::command(program, sandbox)?,
        None => tokio::process::Command::new(program),
    };
    command.args(arguments);
    if cfg!(windows) {
        // PowerShell writes pipes in the console code page unless told otherwise,
        // and PowerShell 7 colors tables and redirected stderr even into a pipe.
        command.arg(format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; if ($PSStyle) {{ $PSStyle.OutputRendering = 'PlainText' }}; {command_line}"
        ));
    } else {
        command.arg(command_line);
    }
    // Ask the programs the command runs not to color either; whatever still does is stripped from the output.
    command.env("NO_COLOR", "1");
    for var in SENSITIVE_ENV_VARS {
        command.env_remove(var);
    }
    command.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    // Its own process group, so stopping the command can stop everything it started.
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}

fn job_arg<'a>(input: &'a Value, context: &'a ToolContext) -> Result<(&'a str, &'a Arc<Jobs>), ToolOutput> {
    let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
    Ok((string_arg(input, "job")?, jobs))
}

/// What a background command printed, cut like a foreground command's output, with how it stands.
fn job_report(id: &str, report: Report) -> ToolOutput {
    let mut output = report.output.trim_end().to_string();
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        // The newest output says the most about where a running command stands.
        let tail: String = output.chars().skip(total - COMMAND_OUTPUT_CHARS).collect();
        output = format!("[... {} earlier characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let state = match report.ended {
        None => "still running".to_string(),
        Some(Ended::Stopped) => "stopped".to_string(),
        Some(Ended::Exited(Some(code))) => format!("exited with code {code}"),
        Some(Ended::Exited(None)) => "ended without an exit code".to_string(),
    };
    let content = format!("{}\n[job {id}: {state}]", if output.is_empty() { "(nothing new)" } else { output.as_str() });
    let exit_code = match report.ended {
        Some(Ended::Exited(code)) => code,
        _ => None,
    };
    let failed = matches!(report.ended, Some(Ended::Exited(code)) if code != Some(0));
    ToolOutput {
        content,
        is_error: failed,
        meta: json!({ "job": id, "command": report.command, "running": report.ended.is_none(), "stopped": matches!(report.ended, Some(Ended::Stopped)).then_some("stopped"), "exit_code": exit_code, "output": output }),
        images: Vec::new(),
    }
}

/// A job id that names nothing: jobs do not outlive the app.
fn no_job(id: &str) -> ToolOutput {
    ToolOutput::error(format!("There is no background command `{id}`. Background commands end when the app closes; start it again if you need it."))
}

pub(super) async fn command_output(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let wait = Duration::from_secs(input["wait_seconds"].as_u64().unwrap_or(0).min(OUTPUT_WAIT_LIMIT_SECONDS));
    let report = jobs.read(id, wait, &context.cancel).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

pub(super) async fn command_input(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let close = input["close"].as_bool().unwrap_or(false);
    let mut text = input["text"].as_str().unwrap_or_default().to_string();
    if text.is_empty() && !close {
        return Err(ToolOutput::error("Give `text` to type, or `close` to end its input."));
    }
    if !text.is_empty() && input["newline"].as_bool().unwrap_or(true) && !text.ends_with('\n') {
        text.push('\n');
    }
    jobs.write(id, &text, close).await.map_err(ToolOutput::error)?;
    let wait = Duration::from_secs(input["wait_seconds"].as_u64().unwrap_or(2).min(OUTPUT_WAIT_LIMIT_SECONDS));
    let report = jobs.read(id, wait, &context.cancel).await.ok_or_else(|| no_job(id))?;
    let mut output = job_report(id, report);
    let sent = if close && text.is_empty() { "Closed its input.".to_string() } else { format!("Sent {} characters{}.", text.chars().count(), if close { " and closed its input" } else { "" }) };
    output.content = format!("{sent} What it printed since:\n{}", output.content);
    output.meta["sent"] = json!(text);
    output.meta["closed"] = json!(close);
    Ok(output)
}

pub(super) async fn stop_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let (id, jobs) = job_arg(input, context)?;
    let report = jobs.stop(id).await.ok_or_else(|| no_job(id))?;
    Ok(job_report(id, report))
}

pub(super) async fn run_command(input: &Value, context: &ToolContext) -> Result<ToolOutput, ToolOutput> {
    let command_line = string_arg(input, "command")?.to_string();
    let timeout = Duration::from_secs(input["timeout_seconds"].as_u64().unwrap_or(COMMAND_TIMEOUT_SECONDS).clamp(1, COMMAND_TIMEOUT_LIMIT_SECONDS));
    let cwd = match command_cwd(input, &context.workspace) {
        Some(dir) => {
            if !dir.exists() {
                return Err(ToolOutput::error(format!("Working directory {} does not exist.", dir.display())));
            }
            if !dir.is_dir() {
                return Err(ToolOutput::error(format!("Working directory {} is not a directory.", dir.display())));
            }
            dir
        }
        None => context.workspace.clone(),
    };
    let shown_cwd = Some(display(&context.workspace, &cwd)).filter(|shown| !shown.is_empty());
    let (program, _, _) = shell();
    // The model may ask for a command to run unconfined; the user has then been asked (see `policy::check_in`).
    let sandbox = context.sandbox.as_ref().filter(|_| input["unsandboxed"] != true);
    let mut command = shell_command(&command_line, &cwd, sandbox).map_err(|error| ToolOutput::error(format!("Cannot confine the command to the sandbox: {error}")))?;
    if input["background"].as_bool().unwrap_or(false) {
        let jobs = context.jobs.as_ref().ok_or_else(|| ToolOutput::error("Background commands are not available here."))?;
        // Left open, for `command_input`; a command in the foreground gets none, and a prompt fails at once there.
        command.stdin(Stdio::piped());
        let id = jobs.start(&command_line, command).map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
        let content = format!("Started in the background as job {id}. Read what it prints with `command_output`, type into it with `command_input`, and end it with `stop_command` when it is no longer needed.");
        let mut meta = json!({ "command": command_line, "job": id, "background": true, "sandboxed": sandbox.is_some() });
        if let Some(shown) = &shown_cwd {
            meta["cwd"] = json!(shown);
        }
        return Ok(ToolOutput::ok(content, meta));
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| ToolOutput::error(format!("Cannot start {program}: {error}")))?;
    let stdout = tokio::spawn(capture(child.stdout.take().expect("stdout is piped")));
    let stderr = tokio::spawn(capture(child.stderr.take().expect("stderr is piped")));

    let (status, stopped) = tokio::select! {
        status = child.wait() => (status.ok(), None),
        () = tokio::time::sleep(timeout) => (None, Some("timed out")),
        () = context.cancel.cancelled() => (None, Some("cancelled")),
    };
    if stopped.is_some() {
        kill_tree(&mut child).await;
    }
    // A grandchild can keep the pipes open after the kill, so reading gets a deadline.
    let read = |task| async { tokio::time::timeout(Duration::from_secs(2), task).await.ok().and_then(Result::ok).unwrap_or_default() };
    let (stdout, stderr): (Vec<u8>, Vec<u8>) = (read(stdout).await, read(stderr).await);

    let mut output = strip_ansi(&String::from_utf8_lossy(&stdout).replace("\r\n", "\n")).trim_end().to_string();
    let stderr = strip_ansi(&String::from_utf8_lossy(&stderr).replace("\r\n", "\n"));
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("[stderr]\n{}", stderr.trim_end()));
    }
    let total = output.chars().count();
    if total > COMMAND_OUTPUT_CHARS {
        let half = COMMAND_OUTPUT_CHARS / 2;
        let head: String = output.chars().take(half).collect();
        let tail: String = output.chars().skip(total - half).collect();
        output = format!("{head}\n[... {} characters omitted ...]\n{tail}", total - COMMAND_OUTPUT_CHARS);
    }
    let exit_code = status.and_then(|status| status.code());
    let mut content = if output.is_empty() { "(no output)".to_string() } else { output.clone() };
    match (stopped, exit_code) {
        (Some("timed out"), _) => content.push_str(&format!("\n[stopped after {} seconds: timed out]", timeout.as_secs())),
        (Some(reason), _) => content.push_str(&format!("\n[stopped: {reason}]")),
        (None, Some(0)) => {}
        (None, Some(code)) => content.push_str(&format!("\n[exit code {code}]")),
        (None, None) => content.push_str("\n[ended without an exit code]"),
    }
    // A confined command that was refused a write is told why, and the way out.
    if let Some(sandbox) = sandbox
        && exit_code != Some(0)
        && DENIED.iter().any(|said| output.to_lowercase().contains(said))
    {
        let roots: Vec<String> = sandbox.writable.iter().map(|root| root.display().to_string()).collect();
        content.push_str(&format!(
            "\n[The command ran in the sandbox the user switched on: it could write only under {}. If that is why it failed and it must write elsewhere, run it again with `unsandboxed` set; the user is asked first.]",
            roots.join(", ")
        ));
    }
    let mut meta = json!({
        "command": command_line,
        "exit_code": exit_code,
        "stopped": stopped,
        "duration_ms": started.elapsed().as_millis() as u64,
        "output": output,
        "sandboxed": sandbox.is_some(),
    });
    if let Some(shown) = &shown_cwd {
        meta["cwd"] = json!(shown);
    }
    Ok(ToolOutput {
        content,
        is_error: stopped.is_some() || exit_code != Some(0),
        meta,
        images: Vec::new(),
    })
}

/// Output captured from a user-initiated `!command` inline execution in the composer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InlineShellOutput {
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

/// Run a shell command directly on behalf of the user (`!cmd` in the composer),
/// capturing merged stdout/stderr with ANSI sequences stripped and bounded by `timeout_secs` and `max_chars`.
pub async fn run_inline_shell(
    command_line: &str,
    cwd: &Path,
    timeout_secs: u64,
    max_chars: usize,
) -> Result<InlineShellOutput, String> {
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return Err("命令不能为空".to_string());
    }
    let mut command = shell_command(trimmed, cwd, None).map_err(|e| format!("无法启动终端命令：{e}"))?;
    let mut child = command.spawn().map_err(|e| format!("无法执行命令 `{trimmed}`：{e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(pipe) => capture(pipe).await,
            None => Vec::new(),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            Some(pipe) => capture(pipe).await,
            None => Vec::new(),
        }
    });

    let mut timed_out = false;
    let status = match tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), child.wait()).await {
        Ok(res) => res.ok(),
        Err(_) => {
            timed_out = true;
            kill_tree(&mut child).await;
            None
        }
    };

    let out_bytes = stdout_task.await.unwrap_or_default();
    let err_bytes = stderr_task.await.unwrap_or_default();
    let out_str = strip_ansi(&String::from_utf8_lossy(&out_bytes));
    let err_str = strip_ansi(&String::from_utf8_lossy(&err_bytes));

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

    Ok(InlineShellOutput {
        command: trimmed.to_string(),
        output: combined,
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inline_shell_captures_stdout_stderr_and_strips_ansi() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "odg-inline\n").unwrap();
        let cmd = if cfg!(windows) {
            "Write-Output 'ok-line'; [Console]::Error.WriteLine('err-line')"
        } else {
            "printf '\\033[32mok-line\\033[0m\\n'; printf 'err-line\\n' >&2"
        };
        let out = run_inline_shell(cmd, dir.path(), 5, 4096).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(out.output.contains("ok-line"));
        assert!(!out.output.contains('\u{1b}'));
        assert!(out.output.contains("[stderr]\nerr-line"));
    }
}
