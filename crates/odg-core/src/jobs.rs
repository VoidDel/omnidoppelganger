//! Commands that keep running in the background: a dev server, a watcher,
//! a long build. `run_command` starts one and returns at once; the model
//! reads what it prints with `command_output` and ends it with
//! `stop_command`. A session's jobs live as long as the app does and are
//! stopped when it exits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
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
    stop: CancellationToken,
    output: Arc<Mutex<Output>>,
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
                    let text = crate::tools::strip_ansi(&String::from_utf8_lossy(&pending[..whole]).replace("\r\n", "\n"));
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
            follow(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)),
            follow(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)),
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

        self.jobs.lock().unwrap().insert(id.clone(), Job { command: line.to_string(), pid, stop, output });
        Ok(id)
    }

    fn report(job: &Job) -> Report {
        let mut output = job.output.lock().unwrap();
        let fresh = output.text[output.read..].to_string();
        output.read = output.text.len();
        Report { command: job.command.clone(), output: fresh, ended: output.ended }
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

    /// Stop the job and report what it printed last. `None` when there is no such job.
    pub async fn stop(&self, id: &str) -> Option<Report> {
        self.jobs.lock().unwrap().get(id)?.stop.cancel();
        // Killing a process tree takes a moment; the report waits for it, within reason.
        for _ in 0..50 {
            if self.jobs.lock().unwrap().get(id)?.output.lock().unwrap().ended.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.jobs.lock().unwrap().get(id).map(Self::report)
    }

    /// The ids and command lines of the jobs still running.
    pub fn running(&self) -> Vec<(String, String)> {
        let jobs = self.jobs.lock().unwrap();
        let mut running: Vec<(String, String)> =
            jobs.iter().filter(|(_, job)| job.output.lock().unwrap().ended.is_none()).map(|(id, job)| (id.clone(), job.command.clone())).collect();
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
                let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
            }
            #[cfg(not(windows))]
            {
                // The negative id names the process group the shell leads.
                let _ = std::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).output();
            }
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.kill_all();
    }
}
