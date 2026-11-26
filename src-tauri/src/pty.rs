//! Interactive pseudo-terminal (PTY) management for the side terminal pane.
//!
//! Each session can open one or more local shell tabs rooted at its workspace
//! (including an isolated Git worktree when applicable). Output is decoded on
//! UTF-8 character boundaries, tracked with a monotonic byte offset so UI tab
//! re-mounts never drop or duplicate chunks, and underlying OS PTY descriptors
//! are released immediately when the child shell exits.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;

pub const PTY_OUTPUT_EVENT: &str = "pty-output";
pub const PTY_EXIT_EVENT: &str = "pty-exit";
const MAX_BACKLOG_BYTES: usize = 128 * 1024;

static NEXT_PTY_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct PtyBuffer {
    text: String,
    total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PtyOutputPayload {
    pub id: String,
    pub session_id: String,
    pub data: String,
    pub start_offset: u64,
    pub end_offset: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PtyExitPayload {
    pub id: String,
    pub session_id: String,
    pub exit_code: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PtyInfo {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub shell: String,
    pub cwd: String,
    pub exited: Option<u32>,
    pub backlog: String,
    pub backlog_end_offset: u64,
}

struct PtyHandle {
    info_title: String,
    shell_name: String,
    session_id: String,
    cwd: String,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    backlog: Arc<Mutex<PtyBuffer>>,
    exited: Arc<Mutex<Option<u32>>>,
}

pub struct PtyHub {
    instances: Arc<Mutex<HashMap<String, Arc<PtyHandle>>>>,
}

impl Default for PtyHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the user's preferred interactive shell on the current OS.
fn detect_shell() -> (String, String, Vec<&'static str>) {
    #[cfg(windows)]
    {
        let on_path = |prog: &str| {
            std::env::var_os("PATH")
                .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file()))
        };
        for candidate in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
            if on_path(candidate) {
                let label = candidate.trim_end_matches(".exe").to_string();
                let args = if candidate == "cmd.exe" { Vec::new() } else { vec!["-NoLogo"] };
                return (candidate.to_string(), label, args);
            }
        }
        ("powershell.exe".to_string(), "powershell".to_string(), vec!["-NoLogo"])
    }
    #[cfg(not(windows))]
    {
        let program = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty() && Path::new(s).exists())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") && Path::new("/bin/zsh").exists() {
                    "/bin/zsh".to_string()
                } else if Path::new("/bin/bash").exists() {
                    "/bin/bash".to_string()
                } else {
                    "/bin/sh".to_string()
                }
            });
        let name = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sh".to_string());
        let args = if matches!(name.as_str(), "zsh" | "bash" | "fish") {
            vec!["-l"]
        } else {
            Vec::new()
        };
        (program, name, args)
    }
}

/// Decode incoming byte chunks into valid UTF-8 strings while carrying incomplete
/// trailing multi-byte sequences (1..=3 bytes) across read boundaries.
pub fn decode_utf8_stream(carry: &mut Vec<u8>, incoming: &[u8]) -> String {
    let mut combined: Vec<u8> = Vec::with_capacity(carry.len() + incoming.len());
    combined.append(carry);
    combined.extend_from_slice(incoming);

    let mut out = String::new();
    let mut slice: &[u8] = &combined;
    loop {
        match std::str::from_utf8(slice) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(err) => {
                let valid_len = err.valid_up_to();
                if valid_len > 0 {
                    out.push_str(&String::from_utf8_lossy(&slice[..valid_len]));
                }
                match err.error_len() {
                    None => {
                        carry.extend_from_slice(&slice[valid_len..]);
                        break;
                    }
                    Some(bad_len) => {
                        out.push('\u{FFFD}');
                        slice = &slice[valid_len + bad_len..];
                    }
                }
            }
        }
    }
    out
}

fn append_pty_buffer(buf: &mut PtyBuffer, chunk: &str) -> (u64, u64) {
    let start = buf.total_bytes;
    buf.total_bytes = buf.total_bytes.saturating_add(chunk.len() as u64);
    let end = buf.total_bytes;
    buf.text.push_str(chunk);
    if buf.text.len() > MAX_BACKLOG_BYTES {
        let excess = buf.text.len() - (MAX_BACKLOG_BYTES * 3 / 4);
        let mut cut = excess;
        while cut < buf.text.len() && !buf.text.is_char_boundary(cut) {
            cut += 1;
        }
        buf.text.drain(..cut);
    }
    (start, end)
}

impl PtyHandle {
    fn release_os_handles(&self) {
        let _ = self.writer.lock().unwrap().take();
        let _ = self.master.lock().unwrap().take();
    }

    fn kill_and_release(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
        }
        self.release_os_handles();
    }
}

impl PtyHub {
    pub fn new() -> Self {
        Self { instances: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn spawn<FOut, FExit>(
        &self,
        session_id: String,
        workspace: &Path,
        cols: u16,
        rows: u16,
        on_output: FOut,
        on_exit: FExit,
    ) -> Result<PtyInfo, String>
    where
        FOut: Fn(PtyOutputPayload) + Send + Sync + 'static,
        FExit: Fn(PtyExitPayload) + Send + Sync + 'static,
    {
        let cwd: PathBuf = if workspace.is_dir() {
            workspace.to_path_buf()
        } else {
            std::env::current_dir().map_err(|e| format!("无法定位终端目录：{e}"))?
        };
        let cols = cols.clamp(20, 500);
        let rows = rows.clamp(5, 200);

        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|error| format!("无法开启终端伪设备：{error}"))?;

        let (shell_prog, shell_name, shell_args) = detect_shell();
        let mut cmd = CommandBuilder::new(&shell_prog);
        for arg in shell_args {
            cmd.arg(arg);
        }
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        #[cfg(not(windows))]
        if std::env::var_os("LANG").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|error| format!("无法启动终端 {shell_prog}：{error}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("无法读取终端输出：{error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("无法连接终端输入：{error}"))?;

        let seq = NEXT_PTY_ID.fetch_add(1, Ordering::Relaxed);
        let id = format!("pty-{seq}");

        let index_in_session = {
            let map = self.instances.lock().unwrap();
            map.values().filter(|h| h.session_id == session_id).count() + 1
        };
        let title = format!("{shell_name} {index_in_session}");
        let cwd_str = cwd.to_string_lossy().into_owned();

        let backlog = Arc::new(Mutex::new(PtyBuffer::default()));
        let exited = Arc::new(Mutex::new(None));

        let handle = Arc::new(PtyHandle {
            info_title: title.clone(),
            shell_name: shell_name.clone(),
            session_id: session_id.clone(),
            cwd: cwd_str.clone(),
            master: Mutex::new(Some(pair.master)),
            writer: Mutex::new(Some(writer)),
            child: Mutex::new(Some(child)),
            backlog: backlog.clone(),
            exited: exited.clone(),
        });

        self.instances.lock().unwrap().insert(id.clone(), handle.clone());

        let reader_id = id.clone();
        let reader_session = session_id.clone();
        let reader_backlog = backlog.clone();
        let reader_exited = exited.clone();
        let reader_handle = handle.clone();

        std::thread::Builder::new()
            .name(format!("odg-pty-{seq}"))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut carry = Vec::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = decode_utf8_stream(&mut carry, &buf[..n]);
                            if !text.is_empty() {
                                let (start_offset, end_offset) = {
                                    let mut log = reader_backlog.lock().unwrap();
                                    append_pty_buffer(&mut log, &text)
                                };
                                on_output(PtyOutputPayload {
                                    id: reader_id.clone(),
                                    session_id: reader_session.clone(),
                                    data: text,
                                    start_offset,
                                    end_offset,
                                });
                            }
                        }
                        Err(_) => break,
                    }
                }
                drop(reader);
                let code = {
                    let mut child_guard = reader_handle.child.lock().unwrap();
                    let exit = child_guard
                        .as_mut()
                        .and_then(|c| c.wait().ok())
                        .map(|status| status.exit_code());
                    *child_guard = None;
                    exit
                };
                reader_handle.release_os_handles();
                *reader_exited.lock().unwrap() = Some(code.unwrap_or(0));
                on_exit(PtyExitPayload {
                    id: reader_id,
                    session_id: reader_session,
                    exit_code: code,
                });
            })
            .map_err(|error| format!("无法启动终端监听线程：{error}"))?;

        let (initial_backlog, initial_offset) = {
            let snap = backlog.lock().unwrap();
            (snap.text.clone(), snap.total_bytes)
        };

        Ok(PtyInfo {
            id,
            session_id,
            title,
            shell: shell_name,
            cwd: cwd_str,
            exited: None,
            backlog: initial_backlog,
            backlog_end_offset: initial_offset,
        })
    }

    pub fn list(&self, session_id: &str) -> Vec<PtyInfo> {
        let map = self.instances.lock().unwrap();
        let mut out: Vec<PtyInfo> = map
            .iter()
            .filter(|(_, h)| h.session_id == session_id)
            .map(|(id, h)| {
                let exited = *h.exited.lock().unwrap();
                let (backlog, backlog_end_offset) = {
                    let snap = h.backlog.lock().unwrap();
                    (snap.text.clone(), snap.total_bytes)
                };
                PtyInfo {
                    id: id.clone(),
                    session_id: h.session_id.clone(),
                    title: h.info_title.clone(),
                    shell: h.shell_name.clone(),
                    cwd: h.cwd.clone(),
                    exited,
                    backlog,
                    backlog_end_offset,
                }
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        let handle = self
            .instances
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or("这个终端已经关闭了。")?;
        let mut guard = handle.writer.lock().unwrap();
        let writer = guard.as_mut().ok_or("这个终端已经结束运行了。")?;
        writer.write_all(data.as_bytes()).map_err(|e| format!("写入终端失败：{e}"))?;
        writer.flush().map_err(|e| format!("刷新终端输入失败：{e}"))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let Some(handle) = self.instances.lock().unwrap().get(id).cloned() else {
            return Ok(());
        };
        let cols = cols.clamp(20, 500);
        let rows = rows.clamp(5, 200);
        let guard = handle.master.lock().unwrap();
        let Some(master) = guard.as_ref() else {
            return Ok(());
        };
        master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("调整终端尺寸失败：{e}"))
    }

    pub fn clear_backlog(&self, id: &str) {
        if let Some(handle) = self.instances.lock().unwrap().get(id) {
            handle.backlog.lock().unwrap().text.clear();
        }
    }

    pub fn close(&self, id: &str) {
        if let Some(handle) = self.instances.lock().unwrap().remove(id) {
            handle.kill_and_release();
        }
    }

    pub fn close_session(&self, session_id: &str) {
        let ids: Vec<String> = self
            .instances
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, h)| h.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.close(&id);
        }
    }

    pub fn shutdown(&self) {
        let drained: Vec<Arc<PtyHandle>> = self.instances.lock().unwrap().drain().map(|(_, h)| h).collect();
        for handle in drained {
            handle.kill_and_release();
        }
    }

    #[cfg(test)]
    pub fn handles_released(&self, id: &str) -> Option<bool> {
        let handle = self.instances.lock().unwrap().get(id).cloned()?;
        let master_free = handle.master.lock().unwrap().is_none();
        let writer_free = handle.writer.lock().unwrap().is_none();
        let child_free = handle.child.lock().unwrap().is_none();
        Some(master_free && writer_free && child_free)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_multibyte_utf8_split_across_chunk_boundaries() {
        let text = "洞府分身终端测试 🚀";
        let bytes = text.as_bytes();
        let mut carry = Vec::new();
        let mut rebuilt = String::new();
        for chunk in bytes.chunks(2) {
            rebuilt.push_str(&decode_utf8_stream(&mut carry, chunk));
        }
        assert!(carry.is_empty());
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn pty_buffer_offsets_stay_strictly_monotonic_across_truncation_and_clear() {
        let mut buf = PtyBuffer::default();
        let (s1, e1) = append_pty_buffer(&mut buf, "修真终端");
        assert_eq!((s1, e1), (0, 12));
        assert_eq!(buf.total_bytes, 12);

        // Ring truncation caps `buf.text` while keeping `total_bytes` strictly cumulative.
        let big = "x".repeat(MAX_BACKLOG_BYTES + 1024);
        let (s2, e2) = append_pty_buffer(&mut buf, &big);
        assert_eq!(s2, 12);
        assert_eq!(e2, 12 + big.len() as u64);
        assert!(buf.text.len() <= MAX_BACKLOG_BYTES);

        buf.text.clear();
        let (s3, e3) = append_pty_buffer(&mut buf, "after");
        assert_eq!(s3, e2);
        assert_eq!(e3, e2 + 5);
    }

    #[test]
    fn spawns_writes_tracks_monotonic_offsets_and_releases_handles_on_exit() {
        let hub = Arc::new(PtyHub::new());
        let temp = tempfile::tempdir().unwrap();
        let payloads: Arc<Mutex<Vec<PtyOutputPayload>>> = Arc::new(Mutex::new(Vec::new()));
        let out_sink = payloads.clone();
        let exited_flag = Arc::new(Mutex::new(false));
        let exit_sink = exited_flag.clone();
        let responder_hub = hub.clone();

        let info = hub
            .spawn(
                "sess-1".to_string(),
                temp.path(),
                80,
                24,
                move |payload| {
                    if payload.data.contains("\x1b[6n") {
                        let _ = responder_hub.write(&payload.id, "\x1b[1;1R");
                    }
                    out_sink.lock().unwrap().push(payload);
                },
                move |_| {
                    *exit_sink.lock().unwrap() = true;
                },
            )
            .unwrap();

        assert_eq!(hub.list("sess-1").len(), 1);
        hub.resize(&info.id, 100, 30).unwrap();

        let has_marker = || {
            let list = payloads.lock().unwrap();
            let raw: String = list.iter().map(|p| p.data.as_str()).collect();
            if raw.contains("odg_pty_ok") {
                return true;
            }
            let mut plain = String::with_capacity(raw.len());
            let mut chars = raw.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' && chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                } else {
                    plain.push(c);
                }
            }
            plain.contains("odg_pty_ok")
        };

        for _ in 0..40 {
            if !payloads.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        hub.write(&info.id, "echo odg_pty_ok\r\n").unwrap();

        for tick in 0..150 {
            if has_marker() {
                break;
            }
            if tick == 40 || tick == 90 {
                let _ = hub.write(&info.id, "echo odg_pty_ok\r\n");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(has_marker(), "PTY did not echo marker");

        // Verify contiguous monotonic offsets across all emitted chunks.
        {
            let chunks = payloads.lock().unwrap();
            assert!(!chunks.is_empty());
            let mut expected = 0u64;
            for chunk in chunks.iter() {
                assert_eq!(chunk.start_offset, expected);
                assert_eq!(chunk.end_offset, expected + chunk.data.len() as u64);
                expected = chunk.end_offset;
            }
            let snap = &hub.list("sess-1")[0];
            assert_eq!(snap.backlog_end_offset, expected);
        }

        // Tell the shell to exit and assert OS handles are released while backlog stays readable.
        let _ = hub.write(&info.id, "exit\r\n");
        for _ in 0..80 {
            if *exited_flag.lock().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(*exited_flag.lock().unwrap(), "expected shell exit callback");
        assert_eq!(hub.handles_released(&info.id), Some(true), "master/writer/child must be released on exit");
        let after_exit = &hub.list("sess-1")[0];
        assert!(after_exit.exited.is_some());
        assert!(after_exit.backlog_end_offset > 0);
        assert!(!after_exit.backlog.is_empty());

        hub.close_session("sess-1");
        assert!(hub.list("sess-1").is_empty());
    }
}
