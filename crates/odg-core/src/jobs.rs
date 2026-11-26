//! Commands that keep running in the background: a dev server, a watcher,
//! a long build. `run_command` starts one and returns at once; the model
//! reads what it prints with `command_output`, types into it with
//! `command_input`, and ends it with `stop_command`. A session's jobs live as
//! long as the app does and are stopped when it exits.
//!
//! A job's standard input is a pipe, not a terminal: a program that reads
//! lines takes what is written, but one that wants a terminal (a menu moved
//! with arrow keys, a password prompt) does not.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// How much of a job's output is kept; the oldest goes first.
const KEPT_BYTES: usize = 400_000;

/// How a job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// On its own, with this exit code when the system gave one.
    Exited(Option<i32>),
    /// Because it was told to.
    Stopped,
}

#[derive(Default)]
struct Output {
    text: String,
    /// How many bytes of `text` have been handed out by [`Jobs::read`].
    read: usize,
    ended: Option<Ended>,
}

struct Job {
    command: String,
    pid: Option<u32>,
    started_at: u64,
    stop: CancellationToken,
    output: Arc<Mutex<Output>>,
    /// Its standard input, while it is open: `None` once closed, or for a job started without one.
    input: Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdin>>>,
}

/// Serializable status snapshot of one background command for the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JobSnapshot {
    pub id: String,
    pub command: String,
    pub pid: Option<u32>,
    pub started_at: u64,
    pub running: bool,
    pub stopped: bool,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
}

/// What [`Jobs::read`] and [`Jobs::stop`] report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub command: String,
    /// What the job printed since the last report.
    pub output: String,
    /// `None` while it is still running.
    pub ended: Option<Ended>,
}

/// One session's background commands.
#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    jobs: Mutex<HashMap<String, Job>>,
}

impl Jobs {
    /// Start `command`, already set up with its program, arguments, and
    /// working directory, and return the job's id. `line` is the command
    /// line as the model wrote it, for reports.
    pub fn start(&self, line: &str, mut command: tokio::process::Command) -> std::io::Result<String> {
        let mut child = command.spawn()?;
        let input = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
        let id = format!("b{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        let output = Arc::new(Mutex::new(Output::default()));
        let stop = CancellationToken::new();
        let pid = child.id();

        // Both streams go into one text, in the order they arrive.
        let follow = |pipe: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>| {
            let output = output.clone();
            tokio::spawn(async move {
                let Some(mut pipe) = pipe else { return };
                let mut buffer = [0u8; 8192];
                // Bytes of a character split across two reads wait here for the rest.
                let mut pending: Vec<u8> = Vec::new();
                while let Ok(read) = pipe.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buffer[..read]);
                    let whole = match std::str::from_utf8(&pending) {
                        Ok(_) => pending.len(),
                        Err(error) if error.error_len().is_none() => error.valid_up_to(),
                        // Not UTF-8 at all: take it as it is, lossily.
                        Err(_) => pending.len(),
                    };
                    let text =
                        crate::tools::strip_ansi(&String::from_utf8_lossy(&pending[..whole]).replace("\r\n", "\n"));
                    pending.drain(..whole);
                    let mut output = output.lock().unwrap();
                    output.text.push_str(&text);
                    if output.text.len() > KEPT_BYTES {
                        let mut cut = output.text.len() - KEPT_BYTES;
                        while !output.text.is_char_boundary(cut) {
                            cut += 1;
                        }
                        output.text.drain(..cut);
                        output.read = output.read.saturating_sub(cut);
                    }
                }
            })
        };
        let streams = [
            follow(
                child
                    .stdout
                    .take()
                    .map(|pipe| Box::new(pipe) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            ),
            follow(
                child
                    .stderr
                    .take()
                    .map(|pipe| Box::new(pipe) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            ),
        ];

        let (watched, stopping) = (output.clone(), stop.clone());
        tokio::spawn(async move {
            let ended = tokio::select! {
                status = child.wait() => Ended::Exited(status.ok().and_then(|status| status.code())),
                () = stopping.cancelled() => {
                    crate::tools::kill_tree(&mut child).await;
                    Ended::Stopped
                }
            };
            // What the job printed last is read before it is reported as ended. A
            // grandchild can hold the pipes open, so the wait has a limit.
            for stream in streams {
                let _ = tokio::time::timeout(Duration::from_secs(2), stream).await;
            }
            watched.lock().unwrap().ended = Some(ended);
        });

        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.jobs.lock().unwrap().insert(
            id.clone(),
            Job {
                command: line.to_string(),
                pid,
                started_at,
                stop,
                output,
                input,
            },
        );
        Ok(id)
    }

    fn snapshot_of(id: &str, job: &Job) -> JobSnapshot {
        let output = job.output.lock().unwrap();
        let (running, stopped, exit_code) = match output.ended {
            None => (true, false, None),
            Some(Ended::Stopped) => (false, true, None),
            Some(Ended::Exited(code)) => (false, false, code),
        };
        JobSnapshot {
            id: id.to_string(),
            command: job.command.clone(),
            pid: job.pid,
            started_at: job.started_at,
            running,
            stopped,
            exit_code,
            output_bytes: output.text.len(),
        }
    }

    /// List all background commands in this session (running first, then newest first).
    pub fn list(&self) -> Vec<JobSnapshot> {
        let jobs = self.jobs.lock().unwrap();
        let mut items: Vec<JobSnapshot> = jobs.iter().map(|(id, job)| Self::snapshot_of(id, job)).collect();
        items.sort_by(|a, b| {
            b.running
                .cmp(&a.running)
                .then_with(|| b.started_at.cmp(&a.started_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        items
    }

    /// Read the latest `tail_bytes` of a job's output without advancing the agent's read cursor.
    pub fn peek_output(&self, id: &str, tail_bytes: usize) -> Option<(JobSnapshot, String)> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id)?;
        let snapshot = Self::snapshot_of(id, job);
        let output = job.output.lock().unwrap();
        let slice = if output.text.len() <= tail_bytes {
            output.text.as_str()
        } else {
            let mut cut = output.text.len() - tail_bytes;
            while !output.text.is_char_boundary(cut) {
                cut += 1;
            }
            &output.text[cut..]
        };
        Some((snapshot, slice.to_string()))
    }

    /// Stop a background command from the UI without advancing the agent's read cursor.
    pub async fn stop_from_ui(&self, id: &str) -> Option<JobSnapshot> {
        self.jobs.lock().unwrap().get(id)?.stop.cancel();
        for _ in 0..50 {
            if self
                .jobs
                .lock()
                .unwrap()
                .get(id)?
                .output
                .lock()
                .unwrap()
                .ended
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let jobs = self.jobs.lock().unwrap();
        jobs.get(id).map(|job| Self::snapshot_of(id, job))
    }

    /// Remove an already-ended job from the panel; refuses if it is still running.
    pub fn remove_ended(&self, id: &str) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let ended = jobs
            .get(id)
            .is_some_and(|job| job.output.lock().unwrap().ended.is_some());
        if ended {
            jobs.remove(id);
            true
        } else {
            false
        }
    }

    fn report(job: &Job) -> Report {
        let mut output = job.output.lock().unwrap();
        let fresh = output.text[output.read..].to_string();
        output.read = output.text.len();
        Report {
            command: job.command.clone(),
            output: fresh,
            ended: output.ended,
        }
    }

    /// What the job printed since it was last read. Waits up to `wait` for it
    /// to print something or end, so the model need not poll in a tight loop.
    /// `None` when there is no such job.
    pub async fn read(&self, id: &str, wait: Duration, cancel: &CancellationToken) -> Option<Report> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            {
                let jobs = self.jobs.lock().unwrap();
                let job = jobs.get(id)?;
                let quiet = {
                    let output = job.output.lock().unwrap();
                    output.read == output.text.len() && output.ended.is_none()
                };
                if !quiet || tokio::time::Instant::now() >= deadline || cancel.is_cancelled() {
                    return Some(Self::report(job));
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The command line of a job, as the model wrote it; `None` when there is no such job.
    pub fn command(&self, id: &str) -> Option<String> {
        self.jobs.lock().unwrap().get(id).map(|job| job.command.clone())
    }

    /// Write `text` to the job's standard input, then with `close` close it, which a program reading to the end of
    /// its input takes as the end. What went wrong is said: no such job, it has ended, or its input is closed.
    pub async fn write(&self, id: &str, text: &str, close: bool) -> Result<(), String> {
        let (input, ended) = {
            let jobs = self.jobs.lock().unwrap();
            let job = jobs
                .get(id)
                .ok_or_else(|| format!("There is no background command `{id}`."))?;
            let ended = job.output.lock().unwrap().ended.is_some();
            (job.input.clone(), ended)
        };
        if ended {
            return Err(format!("Job {id} has ended: nothing reads its input any more."));
        }
        let mut input = input.lock().await;
        let pipe = input.as_mut().ok_or_else(|| format!("Job {id}'s input was closed."))?;
        if !text.is_empty() {
            pipe.write_all(text.as_bytes())
                .await
                .map_err(|error| format!("Job {id} took no input: {error}"))?;
            pipe.flush()
                .await
                .map_err(|error| format!("Job {id} took no input: {error}"))?;
        }
        if close {
            // Dropped, the pipe closes, and the program reads the end of its input.
            input.take();
        }
        Ok(())
    }

    /// Stop the job and report what it printed last. `None` when there is no such job.
    pub async fn stop(&self, id: &str) -> Option<Report> {
        self.jobs.lock().unwrap().get(id)?.stop.cancel();
        // Killing a process tree takes a moment; the report waits for it, within reason.
        for _ in 0..50 {
            if self
                .jobs
                .lock()
                .unwrap()
                .get(id)?
                .output
                .lock()
                .unwrap()
                .ended
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.jobs.lock().unwrap().get(id).map(Self::report)
    }

    /// The ids and command lines of the jobs still running.
    pub fn running(&self) -> Vec<(String, String)> {
        let jobs = self.jobs.lock().unwrap();
        let mut running: Vec<(String, String)> = jobs
            .iter()
            .filter(|(_, job)| job.output.lock().unwrap().ended.is_none())
            .map(|(id, job)| (id.clone(), job.command.clone()))
            .collect();
        running.sort();
        running
    }

    /// End every running job now, without waiting on the async runtime: for
    /// when the app exits and nothing will run the tasks that would do it.
    pub fn kill_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.stop.cancel();
            if job.output.lock().unwrap().ended.is_some() {
                continue;
            }
            let Some(pid) = job.pid else { continue };
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // /T takes the processes the shell started with it.
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(0x0800_0000)
                    .output();
            }
            #[cfg(not(windows))]
            {
                // The negative id names the process group the shell leads.
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .output();
            }
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.kill_all();
    }
}
