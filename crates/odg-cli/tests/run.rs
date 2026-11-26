//! Runs the built `odg` against a stand-in for the Anthropic API, in a home
//! directory of its own.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;

use serde_json::{Value, json};

/// One scripted reply: the text the model says, then the tool it calls, if any.
struct Reply {
    text: &'static str,
    tool: Option<(&'static str, Value)>,
}

/// The reply as the API streams it.
fn stream(reply: &Reply, index: usize) -> String {
    let mut events = vec![
        json!({ "type": "message_start", "message": { "id": format!("msg_{index}"), "model": "claude-opus-5-5", "usage": { "input_tokens": 10, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": reply.text } }),
        json!({ "type": "content_block_stop", "index": 0 }),
    ];
    if let Some((name, input)) = &reply.tool {
        events.push(json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": format!("toolu_{index}"), "name": name, "input": {} } }));
        events.push(json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": input.to_string() } }));
        events.push(json!({ "type": "content_block_stop", "index": 1 }));
    }
    let stop_reason = if reply.tool.is_some() { "tool_use" } else { "end_turn" };
    events.push(
        json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason }, "usage": { "output_tokens": 5 } }),
    );
    events.push(json!({ "type": "message_stop" }));
    let body: String = events
        .iter()
        .map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap()))
        .collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

/// A request the API refuses outright, which the client does not send again.
const REFUSED: &str = "refused";

/// Answer one request per reply, in order. Resolves to the request bodies received.
fn serve(replies: Vec<Reply>) -> (String, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for (index, reply) in replies.iter().enumerate() {
            let (mut socket, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 8192];
            // Read the headers, then as much body as they announce.
            let body = loop {
                let read = socket.read(&mut buffer).unwrap();
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if received.len() >= end + 4 + length || read == 0 {
                        break received[end + 4..].to_vec();
                    }
                }
            };
            bodies.push(serde_json::from_slice(&body).unwrap());
            let response = if reply.text == REFUSED {
                let body = json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "the model is not available" } }).to_string();
                format!(
                    "HTTP/1.1 400 Bad Request
content-type: application/json
content-length: {}
connection: close

{body}",
                    body.len()
                )
            } else {
                stream(reply, index)
            };
            socket.write_all(response.as_bytes()).unwrap();
        }
        bodies
    });
    (address, server)
}

struct Run {
    output: Output,
    requests: Vec<Value>,
    home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
}

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).replace("\r\n", "\n")
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).replace("\r\n", "\n")
    }
}

/// Run `odg` with these arguments against a server that gives these replies.
fn odg(arguments: &[&str], replies: Vec<Reply>) -> Run {
    let (home, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (address, server) = serve(replies);
    let output = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &workspace.path().to_string_lossy()])
        .args(arguments)
        // A home of its own: no settings, keys, or sessions of the user's are read or written.
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &address)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Run {
        output,
        requests: server.join().unwrap(),
        home,
        _workspace: workspace,
    }
}

/// The tool results the model was sent back in a request's last message.
fn results(request: &Value) -> Vec<(String, bool)> {
    let last = request["messages"].as_array().unwrap().last().unwrap();
    let blocks = last["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "tool_result");
    blocks
        .map(|block| {
            (
                block["content"].as_str().unwrap().to_string(),
                block["is_error"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[test]
fn prints_what_the_model_says_and_keeps_the_session() {
    let run = odg(
        &["say", "hello"],
        vec![Reply {
            text: "Hello from the model.",
            tool: None,
        }],
    );
    assert!(run.output.status.success(), "{}", run.stderr());
    assert_eq!(run.stdout(), "Hello from the model.\n");
    assert_eq!(run.requests[0]["messages"][0]["content"][0]["text"], "say hello");
    assert_eq!(run.requests[0]["model"], "claude-opus-5-5");
    // The session is in the home directory, where the desktop app lists it.
    let sessions: Vec<_> = std::fs::read_dir(run.home.path().join("sessions")).unwrap().collect();
    assert_eq!(sessions.len(), 1);
    assert!(
        run.stderr().starts_with("odg: anthropic claude-opus-5-5, in "),
        "{}",
        run.stderr()
    );
}

#[test]
fn approves_or_declines_what_the_agent_asks_as_told() {
    let command = || {
        vec![
            Reply {
                text: "Running it.",
                tool: Some(("run_command", json!({ "command": "echo cli-ok" }))),
            },
            Reply {
                text: "Done.",
                tool: None,
            },
        ]
    };

    let approved = odg(&["--yes", "run", "it"], command());
    assert!(approved.output.status.success(), "{}", approved.stderr());
    assert_eq!(approved.stdout(), "Running it.\nDone.\n");
    let ran = results(&approved.requests[1]);
    assert!(ran[0].0.trim() == "cli-ok" && !ran[0].1, "{ran:?}");
    assert!(
        approved
            .stderr()
            .contains("> run_command  echo cli-ok\n  approved (--yes)\n  ✓ cli-ok"),
        "{}",
        approved.stderr()
    );

    // Without a terminal to ask on, the default is to decline; the model is told and goes on.
    let declined = odg(&["run", "it"], command());
    assert!(declined.output.status.success(), "{}", declined.stderr());
    assert_eq!(
        results(&declined.requests[1]),
        [("The user declined this action.".to_string(), true)]
    );
    assert!(declined.stderr().contains("declined (--no)"), "{}", declined.stderr());
}

#[test]
fn leaves_the_agent_s_questions_to_it_when_nobody_can_answer() {
    let questions = json!({ "questions": [{ "header": "方案", "question": "用哪个方案？", "options": [{ "label": "一" }, { "label": "二" }] }] });
    let replies = vec![
        Reply {
            text: "Asking.",
            tool: Some(("ask_user", questions)),
        },
        Reply {
            text: "I chose one.",
            tool: None,
        },
    ];
    let run = odg(&["pick", "one"], replies);
    assert!(run.output.status.success(), "{}", run.stderr());
    assert!(
        run.stderr()
            .contains("> ask_user  用哪个方案？\n  nobody to answer (--no); the agent decides\n"),
        "{}",
        run.stderr()
    );
    let told = results(&run.requests[1]);
    assert!(
        told[0].0.starts_with("The user chose not to answer") && !told[0].1,
        "{told:?}"
    );
}

#[test]
fn prints_a_plan_presented_in_plan_mode_as_its_answer() {
    let plan = json!({ "title": "Faster parser", "plan": "1. Cache the tokens.\n2. Run the tests." });
    // One reply only: the plan ends the turn, and nothing more is asked of the model.
    let run = odg(
        &["--mode", "plan", "plan", "it"],
        vec![Reply {
            text: "Here is the plan.",
            tool: Some(("present_plan", plan)),
        }],
    );
    assert!(run.output.status.success(), "{}", run.stderr());
    assert_eq!(
        run.stdout(),
        "Here is the plan.\n1. Cache the tokens.\n2. Run the tests.\n"
    );
    assert_eq!(run.requests.len(), 1);
}

#[test]
fn runs_a_subagent_and_answers_what_it_asks() {
    let replies = vec![
        Reply {
            text: "Splitting it.",
            tool: Some((
                "delegate",
                json!({ "tasks": [{ "name": "runner", "task": "Run the command." }] }),
            )),
        },
        // The subagent's two requests come while the agent waits for it.
        Reply {
            text: "Running it.",
            tool: Some(("run_command", json!({ "command": "echo sub-ok" }))),
        },
        Reply {
            text: "It printed sub-ok.",
            tool: None,
        },
        Reply {
            text: "All done.",
            tool: None,
        },
    ];
    let run = odg(&["--yes", "split", "it"], replies);
    assert!(run.output.status.success(), "{}", run.stderr());
    // Only the agent's own words are the output; the subagent's work shows on standard error, under its name.
    assert_eq!(run.stdout(), "Splitting it.\nAll done.\n");
    assert!(run.stderr().contains("  [runner] set off\n"), "{}", run.stderr());
    assert!(
        run.stderr().contains("  [runner] > run_command  echo sub-ok\n"),
        "{}",
        run.stderr()
    );
    assert_eq!(run.requests[1]["messages"][0]["content"][0]["text"], "Run the command.");
    assert!(results(&run.requests[2])[0].0.trim() == "sub-ok");
    assert_eq!(
        results(&run.requests[3]),
        [(
            "<subagent name=\"runner\">\nIt printed sub-ok.\n</subagent>".to_string(),
            false
        )]
    );
    assert_eq!(std::fs::read_dir(run.home.path().join("sessions")).unwrap().count(), 2);
}

#[test]
fn says_what_is_wrong_with_its_arguments_or_the_run() {
    let unused_home = tempfile::tempdir().unwrap();
    let run = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_odg"))
            .args(arguments)
            .env("ODG_HOME", unused_home.path())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let none = run(&[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&none.stderr).contains("give the task"));
    let help = run(&["--help"]);
    assert!(help.status.success() && String::from_utf8_lossy(&help.stdout).starts_with("odg: run tasks"));
    for flag in ["--version", "-V"] {
        let ver = run(&[flag]);
        assert!(
            ver.status.success(),
            "{flag} failed: {}",
            String::from_utf8_lossy(&ver.stderr)
        );
        let text = String::from_utf8_lossy(&ver.stdout).replace("\r\n", "\n");
        assert_eq!(text, format!("odg {}\n", env!("CARGO_PKG_VERSION")));
        let parts: Vec<&str> = text
            .trim()
            .strip_prefix("odg ")
            .expect("starts with odg ")
            .split('.')
            .collect();
        assert!(
            parts.len() >= 3
                && parts[..3]
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "unexpected version output: {text}"
        );
    }
    let unknown = run(&["--provider", "nobody", "hello"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("there is no provider `nobody`"));

    // A turn that fails ends with status 1 and the reason on standard error.
    let failed = odg(
        &["hello"],
        vec![Reply {
            text: REFUSED,
            tool: None,
        }],
    );
    assert_eq!(failed.output.status.code(), Some(1));
    assert!(
        failed
            .stderr()
            .contains("! the turn ended: invalid_request_error: the model is not available"),
        "{}",
        failed.stderr()
    );
    assert_eq!(failed.stdout(), "");
}

#[test]
fn continues_resumes_streams_json_and_exports_markdown() {
    let (home, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // 1. First turn in workspace
    let (addr1, srv1) = serve(vec![Reply {
        text: "First reply.",
        tool: None,
    }]);
    let out1 = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &workspace.path().to_string_lossy(), "first", "question"])
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &addr1)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out1.status.success());
    let _ = srv1.join().unwrap();

    // 2. Continue latest session in workspace with `--continue` and `--json`
    let (addr2, srv2) = serve(vec![Reply {
        text: "Second reply.",
        tool: None,
    }]);
    let out2 = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args([
            "-C",
            &workspace.path().to_string_lossy(),
            "-c",
            "--json",
            "second",
            "question",
        ])
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &addr2)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
    let reqs2 = srv2.join().unwrap();
    let msgs2 = reqs2[0]["messages"].as_array().unwrap();
    assert_eq!(msgs2.len(), 3, "expected user1 + assistant1 + user2 in resumed history");
    assert_eq!(msgs2[0]["content"][0]["text"], "first question");
    assert_eq!(msgs2[2]["content"][0]["text"], "second question");

    let lines: Vec<Value> = String::from_utf8_lossy(&out2.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();
    assert_eq!(lines[0]["kind"], "session");
    assert_eq!(lines[0]["resumed"], true);
    let session_id = lines[0]["session_id"].as_str().unwrap().to_string();
    assert!(lines.iter().any(|l| l["kind"] == "stream"));
    assert!(lines.iter().any(|l| l["kind"] == "log"));

    // 3. Resume by prefix and export Markdown
    let prefix = &session_id[..8];
    let exported = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["--export", prefix])
        .env("ODG_HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(exported.status.success());
    let md = String::from_utf8_lossy(&exported.stdout);
    assert!(md.contains("first question") && md.contains("Second reply."), "{md}");
}

#[test]
fn validates_bypass_mode_and_promotes_external_cli_with_yes() {
    let (home, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // 1. `--mode bypass` without `-w` fails with exit code 2.
    let no_wt = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &workspace.path().to_string_lossy(), "--mode", "bypass", "do", "x"])
        .env("ODG_HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(no_wt.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_wt.stderr).contains("--worktree"));

    // 2. Resuming a non-worktree session with `--mode bypass` also fails with exit code 2.
    let (addr1, srv1) = serve(vec![Reply {
        text: "Initial.",
        tool: None,
    }]);
    let first = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &workspace.path().to_string_lossy(), "--json", "init"])
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &addr1)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(first.status.success());
    let _ = srv1.join().unwrap();
    let first_line: Value =
        serde_json::from_str(String::from_utf8_lossy(&first.stdout).lines().next().unwrap()).unwrap();
    let non_wt_id = first_line["session_id"].as_str().unwrap();

    let resume_bypass = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args([
            "-C",
            &workspace.path().to_string_lossy(),
            "-r",
            non_wt_id,
            "--mode",
            "bypass",
            "next",
        ])
        .env("ODG_HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(resume_bypass.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&resume_bypass.stderr).contains("--worktree"));

    // 3. `-w --mode bypass` inside a Git repository succeeds.
    let repo = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init"])
            .current_dir(repo.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init"
            ])
            .current_dir(repo.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );

    let (addr2, srv2) = serve(vec![Reply {
        text: "Bypass ok.",
        tool: None,
    }]);
    let wt_bypass = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &repo.path().to_string_lossy(), "-w", "--mode", "bypass", "ship"])
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &addr2)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        wt_bypass.status.success(),
        "{}",
        String::from_utf8_lossy(&wt_bypass.stderr)
    );
    let _ = srv2.join().unwrap();

    // 4. `--yes` without `--mode` promotes external CLI providers to Auto (no worktree) and Bypass (with `-w`).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let args_log = home.path().join("codex-args.log");
        let fake_codex = home.path().join("fake-codex.sh");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> \"{}\"\necho '{{\"type\":\"thread.started\",\"thread_id\":\"t1\"}}'\necho '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"m1\",\"type\":\"agent_message\",\"text\":\"codex done\"}}}}'\necho '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":2}}}}'\n",
            args_log.display()
        );
        std::fs::write(&fake_codex, script).unwrap();
        std::fs::set_permissions(&fake_codex, std::fs::Permissions::from_mode(0o755)).unwrap();

        let auto_out = Command::new(env!("CARGO_BIN_EXE_odg"))
            .args([
                "-C",
                &workspace.path().to_string_lossy(),
                "--provider",
                "codex",
                "--model",
                "gpt-5.4",
                "--yes",
                "run",
                "tests",
            ])
            .env("ODG_HOME", home.path())
            .env("ODG_CODEX_BIN", &fake_codex)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            auto_out.status.success(),
            "{}",
            String::from_utf8_lossy(&auto_out.stderr)
        );

        let bypass_out = Command::new(env!("CARGO_BIN_EXE_odg"))
            .args([
                "-C",
                &repo.path().to_string_lossy(),
                "-w",
                "--provider",
                "codex",
                "--model",
                "gpt-5.4",
                "--yes",
                "run",
                "tests",
            ])
            .env("ODG_HOME", home.path())
            .env("ODG_CODEX_BIN", &fake_codex)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            bypass_out.status.success(),
            "{}",
            String::from_utf8_lossy(&bypass_out.stderr)
        );

        let recorded = std::fs::read_to_string(&args_log).unwrap();
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(lines.len(), 2, "{recorded}");
        assert!(
            lines[0].contains("--approve-for-me"),
            "expected Auto mode for --yes without -w: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("--dangerously-bypass-approvals-and-sandbox"),
            "expected Bypass mode for -w --yes: {}",
            lines[1]
        );
    }
}

#[cfg(unix)]
#[test]
fn odg_cli_sigint_cancels_turn_kills_child_group_and_closes_session_log() {
    use std::time::{Duration, Instant};

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let (home, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pid_file = home.path().join("grandchild.pid");
    let cmd_str = format!("sleep 60 & echo $! > \"{}\"; wait", pid_file.display());
    let (address, _server) = serve(vec![Reply {
        text: "Starting long task.",
        tool: Some(("run_command", json!({ "command": cmd_str }))),
    }]);

    let child = Command::new(env!("CARGO_BIN_EXE_odg"))
        .args(["-C", &workspace.path().to_string_lossy(), "--yes", "sleep", "now"])
        .env("ODG_HOME", home.path())
        .env("ANTHROPIC_BASE_URL", &address)
        .env("ANTHROPIC_API_KEY", "test-key")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Wait until the grandchild `sleep 60` process is spawned and writes its PID.
    let wait_start = Instant::now();
    let grandchild_pid: i32 = loop {
        if let Ok(raw) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = raw.trim().parse::<i32>()
            && pid > 0
        {
            break pid;
        }
        assert!(
            wait_start.elapsed() < Duration::from_secs(10),
            "timed out waiting for grandchild pid file"
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    // Send SIGINT to `odg` and assert it exits with code 130 within 3 seconds.
    let sig_start = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-s", "INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let output = child.wait_with_output().unwrap();
    assert!(
        sig_start.elapsed() < Duration::from_secs(3),
        "odg took {:?} to exit after SIGINT: {}",
        sig_start.elapsed(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(130),
        "expected exit code 130 on SIGINT, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("! stopping the turn..."));

    // Assert grandchild process is completely dead (`kill(grandchild_pid, 0) == -1` with ESRCH).
    for _ in 0..20 {
        if unsafe { kill(grandchild_pid, 0) } == -1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let rc = unsafe { kill(grandchild_pid, 0) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(
        rc, -1,
        "grandchild process {grandchild_pid} should be dead after SIGINT"
    );
    assert_eq!(
        errno,
        Some(3),
        "expected ESRCH (3) for dead grandchild {grandchild_pid}, got {errno:?}"
    );

    // Assert `session.v1.jsonl` closed cleanly with `turn/end` (`reason == "cancelled"`).
    let session_entry = std::fs::read_dir(home.path().join("sessions"))
        .unwrap()
        .next()
        .expect("session directory created")
        .unwrap();
    let log_path = session_entry.path().join("session.v1.jsonl");
    let raw_log = std::fs::read_to_string(&log_path).expect("session.v1.jsonl readable");
    let events: Vec<Value> = raw_log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid jsonl event"))
        .collect();
    let last = events.last().expect("non-empty session log");
    assert_eq!(
        last["type"], "turn/end",
        "expected final event to be turn/end, got: {last}"
    );
    assert_eq!(
        last["data"]["reason"], "cancelled",
        "expected cancelled reason, got: {last}"
    );
}
