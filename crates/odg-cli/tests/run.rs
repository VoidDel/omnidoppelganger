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
    events.push(json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason }, "usage": { "output_tokens": 5 } }));
    events.push(json!({ "type": "message_stop" }));
    let body: String = events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect();
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
                    let length: usize = text[..end].lines().find_map(|line| line.to_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse().unwrap())).unwrap_or(0);
                    if received.len() >= end + 4 + length || read == 0 {
                        break received[end + 4..].to_vec();
                    }
                }
            };
            bodies.push(serde_json::from_slice(&body).unwrap());
            let response = if reply.text == REFUSED {
                let body = json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "the model is not available" } }).to_string();
                format!("HTTP/1.1 400 Bad Request
content-type: application/json
content-length: {}
connection: close

{body}", body.len())
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
    Run { output, requests: server.join().unwrap(), home, _workspace: workspace }
}

/// The tool results the model was sent back in a request's last message.
fn results(request: &Value) -> Vec<(String, bool)> {
    let last = request["messages"].as_array().unwrap().last().unwrap();
    let blocks = last["content"].as_array().unwrap().iter().filter(|block| block["type"] == "tool_result");
    blocks.map(|block| (block["content"].as_str().unwrap().to_string(), block["is_error"].as_bool().unwrap())).collect()
}

#[test]
fn prints_what_the_model_says_and_keeps_the_session() {
    let run = odg(&["say", "hello"], vec![Reply { text: "Hello from the model.", tool: None }]);
    assert!(run.output.status.success(), "{}", run.stderr());
    assert_eq!(run.stdout(), "Hello from the model.\n");
    assert_eq!(run.requests[0]["messages"][0]["content"][0]["text"], "say hello");
    assert_eq!(run.requests[0]["model"], "claude-opus-5-5");
    // The session is in the home directory, where the desktop app lists it.
    let sessions: Vec<_> = std::fs::read_dir(run.home.path().join("sessions")).unwrap().collect();
    assert_eq!(sessions.len(), 1);
    assert!(run.stderr().starts_with("odg: anthropic claude-opus-5-5, in "), "{}", run.stderr());
}

#[test]
fn approves_or_declines_what_the_agent_asks_as_told() {
    let command = || vec![Reply { text: "Running it.", tool: Some(("run_command", json!({ "command": "echo cli-ok" }))) }, Reply { text: "Done.", tool: None }];

    let approved = odg(&["--yes", "run", "it"], command());
    assert!(approved.output.status.success(), "{}", approved.stderr());
    assert_eq!(approved.stdout(), "Running it.\nDone.\n");
    let ran = results(&approved.requests[1]);
    assert!(ran[0].0.trim() == "cli-ok" && !ran[0].1, "{ran:?}");
    assert!(approved.stderr().contains("> run_command  echo cli-ok\n  approved (--yes)\n  ✓ cli-ok"), "{}", approved.stderr());

    // Without a terminal to ask on, the default is to decline; the model is told and goes on.
    let declined = odg(&["run", "it"], command());
    assert!(declined.output.status.success(), "{}", declined.stderr());
    assert_eq!(results(&declined.requests[1]), [("The user declined this action.".to_string(), true)]);
    assert!(declined.stderr().contains("declined (--no)"), "{}", declined.stderr());
}

#[test]
fn leaves_the_agent_s_questions_to_it_when_nobody_can_answer() {
    let questions = json!({ "questions": [{ "header": "方案", "question": "用哪个方案？", "options": [{ "label": "一" }, { "label": "二" }] }] });
    let replies = vec![Reply { text: "Asking.", tool: Some(("ask_user", questions)) }, Reply { text: "I chose one.", tool: None }];
    let run = odg(&["pick", "one"], replies);
    assert!(run.output.status.success(), "{}", run.stderr());
    assert!(run.stderr().contains("> ask_user  用哪个方案？\n  nobody to answer (--no); the agent decides\n"), "{}", run.stderr());
    let told = results(&run.requests[1]);
    assert!(told[0].0.starts_with("The user chose not to answer") && !told[0].1, "{told:?}");
}

#[test]
fn runs_a_subagent_and_answers_what_it_asks() {
    let replies = vec![
        Reply { text: "Splitting it.", tool: Some(("delegate", json!({ "tasks": [{ "name": "runner", "task": "Run the command." }] }))) },
        // The subagent's two requests come while the agent waits for it.
        Reply { text: "Running it.", tool: Some(("run_command", json!({ "command": "echo sub-ok" }))) },
        Reply { text: "It printed sub-ok.", tool: None },
        Reply { text: "All done.", tool: None },
    ];
    let run = odg(&["--yes", "split", "it"], replies);
    assert!(run.output.status.success(), "{}", run.stderr());
    // Only the agent's own words are the output; the subagent's work shows on standard error, under its name.
    assert_eq!(run.stdout(), "Splitting it.\nAll done.\n");
    assert!(run.stderr().contains("  [runner] set off\n"), "{}", run.stderr());
    assert!(run.stderr().contains("  [runner] > run_command  echo sub-ok\n"), "{}", run.stderr());
    assert_eq!(run.requests[1]["messages"][0]["content"][0]["text"], "Run the command.");
    assert!(results(&run.requests[2])[0].0.trim() == "sub-ok");
    assert_eq!(results(&run.requests[3]), [("<subagent name=\"runner\">\nIt printed sub-ok.\n</subagent>".to_string(), false)]);
    assert_eq!(std::fs::read_dir(run.home.path().join("sessions")).unwrap().count(), 2);
}

#[test]
fn says_what_is_wrong_with_its_arguments_or_the_run() {
    let run = |arguments: &[&str]| Command::new(env!("CARGO_BIN_EXE_odg")).args(arguments).env("ODG_HOME", std::env::temp_dir().join("odg-cli-unused-home")).stdin(Stdio::null()).output().unwrap();
    let none = run(&[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&none.stderr).contains("give the task"));
    let help = run(&["--help"]);
    assert!(help.status.success() && String::from_utf8_lossy(&help.stdout).starts_with("odg: run one task"));
    let unknown = run(&["--provider", "nobody", "hello"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("there is no provider `nobody`"));

    // A turn that fails ends with status 1 and the reason on standard error.
    let failed = odg(&["hello"], vec![Reply { text: REFUSED, tool: None }]);
    assert_eq!(failed.output.status.code(), Some(1));
    assert!(failed.stderr().contains("! the turn ended: invalid_request_error: the model is not available"), "{}", failed.stderr());
    assert_eq!(failed.stdout(), "");
}
