use std::time::{Duration, Instant};

use crate::events::{QuestionKind, Reply};

use super::*;

fn context() -> (tempfile::TempDir, ToolContext) {
    let directory = tempfile::tempdir().unwrap();
    let context = ToolContext { workspace: directory.path().to_path_buf(), cancel: CancellationToken::new(), snapshots: None, jobs: None, mcp: None, skills: Vec::new(), search: None, sandbox: None, runtime: None, painter: None, vision: Some(true), lsp: None };
    (directory, context)
}

#[tokio::test]
async fn paints_a_picture_into_the_workspace_as_the_kind_it_is_and_never_over_a_file() {
    let (directory, context) = context();
    let unset = execute("generate_image", &json!({ "prompt": "一只白鹤", "path": "crane.png" }), &context).await;
    assert!(unset.is_error && unset.content.contains("API key"), "{}", unset.content);

    let answer = format!(r#"{{"data":[{{"b64_json":"{}"}}]}}"#, crate::events::base64(crate::imagegen::tests::PNG));
    let (address, _sent) = crate::imagegen::tests::serve(vec![(200, "application/json", answer.into_bytes())]);
    let painter = crate::imagegen::painter("gpt-image-2").unwrap();
    let context = ToolContext { painter: Some(crate::imagegen::ImageConfig { painter, url: format!("{address}/v1/images/generations"), key: "sk".into() }), ..context };
    // Asked for as a JPEG, it comes back a PNG and is saved as one, in a folder made for it.
    let painted = execute("generate_image", &json!({ "prompt": "一只白鹤", "path": "images/crane.jpg", "shape": "landscape" }), &context).await;
    assert!(!painted.is_error, "{}", painted.content);
    let saved = directory.path().join("images").join("crane.png");
    assert_eq!(std::fs::read(&saved).unwrap(), crate::imagegen::tests::PNG);
    assert!(painted.content.contains("images/crane.png") && painted.content.contains("1536x1024") && painted.content.contains("saved as"), "{}", painted.content);
    assert_eq!((painted.meta["file"].as_str(), painted.meta["model"].as_str()), (Some(saved.to_string_lossy().as_ref()), Some("gpt-image-2")));

    // A file that is there is not painted over, and nothing is asked for; nor is a picture put outside.
    let taken = execute("generate_image", &json!({ "prompt": "又一只", "path": "images/crane.png" }), &context).await;
    assert!(taken.is_error && taken.content.contains("crane-v2.png"), "{}", taken.content);
    let outside = execute("generate_image", &json!({ "prompt": "x", "path": "../crane.png" }), &context).await;
    assert!(outside.is_error && outside.content.contains("outside the workspace"), "{}", outside.content);
    std::fs::create_dir_all(directory.path().join(".git/hooks")).unwrap();
    let into_git = execute("generate_image", &json!({ "prompt": "x", "path": ".git/hooks/crane.png" }), &context).await;
    assert!(into_git.is_error && into_git.content.contains(".git directory"), "{}", into_git.content);
    let shaped = execute("generate_image", &json!({ "prompt": "x", "path": "a.png", "shape": "round" }), &context).await;
    assert!(shaped.is_error && shaped.content.contains("square"), "{}", shaped.content);
}

#[tokio::test]
async fn says_where_the_document_runtime_is_or_how_to_set_it_up_and_hands_over_files() {
    let (directory, context) = context();
    // Not offered here: a Python the user has is to be used, with leave.
    assert!(execute("document_runtime", &json!({}), &context).await.is_error);
    let root = directory.path().join("runtime");
    let mut context = ToolContext { runtime: Some(root.clone()), ..context };
    let missing = execute("document_runtime", &json!({}), &context).await;
    assert!(!missing.is_error && missing.content.contains("`install` true") && missing.meta["state"] == "missing", "{}", missing.content);
    // Set up as an install leaves it: the interpreter is named, quoted, to run scripts with.
    let python = if cfg!(windows) { root.join("python").join("python.exe") } else { root.join("python").join("bin").join("python3") };
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "").unwrap();
    std::fs::write(root.join("runtime.json"), serde_json::to_vec(&crate::runtime::Installed::current("t")).unwrap()).unwrap();
    let ready = execute("document_runtime", &json!({}), &context).await;
    let call = if cfg!(windows) { format!("& \"{}\" make_report.py", python.display()) } else { format!("\"{}\" make_report.py", python.display()) };
    assert!(ready.content.contains(&call) && ready.meta["python"] == python.to_string_lossy().as_ref(), "{}", ready.content);
    assert!(ready.content.contains("LibreOffice:"));

    std::fs::write(directory.path().join("report.docx"), "v1").unwrap();
    let snaps = tempfile::tempdir().unwrap();
    context.snapshots = Some(snaps.path().to_path_buf());
    let report_path = directory.path().join("report.docx").to_string_lossy().into_owned();
    let handed = execute("deliver_files", &json!({ "paths": ["report.docx"] }), &context).await;
    assert_eq!(handed.meta["files"], json!([report_path]));
    let snap1 = handed.meta["snapshots"][&report_path].as_str().unwrap().to_string();
    assert_eq!(handed.meta["sizes"][&report_path].as_u64(), Some(2));
    assert_eq!(std::fs::read(snaps.path().join(&snap1)).unwrap(), b"v1");
    // Revising and re-delivering preserves both versions in the snapshot directory.
    std::fs::write(directory.path().join("report.docx"), "v2 revised").unwrap();
    let handed2 = execute("deliver_files", &json!({ "paths": ["report.docx"] }), &context).await;
    let snap2 = handed2.meta["snapshots"][&report_path].as_str().unwrap().to_string();
    assert_ne!(snap1, snap2);
    assert_eq!(std::fs::read(snaps.path().join(&snap1)).unwrap(), b"v1");
    assert_eq!(std::fs::read(snaps.path().join(&snap2)).unwrap(), b"v2 revised");
    let refused = execute("deliver_files", &json!({ "paths": ["report.docx", "missing.pdf"] }), &context).await;
    assert!(refused.is_error && refused.content.contains("missing.pdf"));
    // Files outside the workspace (whether present or not, or reached through a symlink) are never delivered.
    let elsewhere = tempfile::tempdir().unwrap();
    let secret = elsewhere.path().join("secret.txt");
    std::fs::write(&secret, "top secret").unwrap();
    let outside = execute("deliver_files", &json!({ "paths": [secret.to_string_lossy()] }), &context).await;
    assert!(outside.is_error && outside.content.starts_with("Outside the workspace:"), "{}", outside.content);
    let outside_missing = execute("deliver_files", &json!({ "paths": ["../does-not-exist.txt"] }), &context).await;
    assert!(outside_missing.is_error && outside_missing.content.starts_with("Outside the workspace:"), "{}", outside_missing.content);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&secret, directory.path().join("escape.txt")).unwrap();
        let escaped = execute("deliver_files", &json!({ "paths": ["escape.txt"] }), &context).await;
        assert!(escaped.is_error && escaped.content.starts_with("Outside the workspace:"), "{}", escaped.content);
    }
    assert!(execute("deliver_files", &json!({ "paths": [] }), &context).await.is_error);
}

#[tokio::test]
async fn installs_a_skill_from_a_folder_or_a_repository() {
    let (directory, mut context) = context();
    let elsewhere = tempfile::tempdir().unwrap();
    let user = elsewhere.path().join("user-skills");
    context.skills = vec![user.clone(), directory.path().join(".odg/skills")];
    // A skill in a folder, for this project.
    let made = elsewhere.path().join("made/release");
    std::fs::create_dir_all(&made).unwrap();
    std::fs::write(made.join("SKILL.md"), "---\nname: release\ndescription: How to cut a release.\n---\nTag it.\n").unwrap();
    let done = execute("install_skill", &json!({ "source": made.to_string_lossy() }), &context).await;
    assert!(!done.is_error && done.content.starts_with("Installed the skill `release` (How to cut a release.)"), "{}", done.content);
    assert_eq!((done.meta["scope"].as_str(), done.meta["directory"].as_str()), (Some("project"), Some(".odg/skills/release")));
    assert!(crate::skills::load(&context.skills, "release").unwrap().starts_with("Tag it."));

    // From a Git repository, a folder in it, for every session.
    let repository = elsewhere.path().join("skills.git");
    std::fs::create_dir_all(repository.join("skills/pdf")).unwrap();
    std::fs::write(repository.join("skills/pdf/SKILL.md"), "---\nname: pdf\ndescription: Work with PDFs.\n---\nSplit them.\n").unwrap();
    for args in [&["init", "--quiet"][..], &["add", "-A"], &["-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "--quiet", "-m", "skills"]] {
        assert!(std::process::Command::new("git").arg("-C").arg(&repository).args(args).status().unwrap().success());
    }
    let cloned = execute("install_skill", &json!({ "source": repository.to_string_lossy(), "path": "skills/pdf", "scope": "user" }), &context).await;
    assert!(!cloned.is_error, "{}", cloned.content);
    assert!(user.join("pdf/SKILL.md").is_file() && !user.join("pdf/.git").exists());
    // Not naming the folder, the repository's skills are named; and nothing is left of the copies fetched.
    let missed = execute("install_skill", &json!({ "source": repository.to_string_lossy() }), &context).await;
    assert!(missed.is_error && missed.content.contains("skills/pdf") && missed.content.contains("`path`"), "{}", missed.content);
    let left = std::fs::read_dir(std::env::temp_dir()).unwrap().filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().starts_with("odg-skill-")).count();
    assert_eq!(left, 0, "a fetched repository was left in the temporary directory");
    // Again, not over what is there.
    assert!(execute("install_skill", &json!({ "source": made.to_string_lossy() }), &context).await.content.contains("replace"));
}

#[tokio::test(flavor = "multi_thread")]
async fn adds_an_mcp_server_and_says_what_it_offers() {
    let (directory, mut context) = context();
    let hub = Arc::new(crate::mcp::McpHub::new(directory.path().join("home/mcp.json"), tokio::runtime::Handle::current()));
    context.mcp = Some(hub.clone());
    // Neither a command nor an address: nothing is added.
    let empty = execute("add_mcp_server", &json!({ "name": "notes" }), &context).await;
    assert!(empty.is_error && empty.content.starts_with("The server was not added"), "{}", empty.content);
    // One that cannot start is added and says why it did not connect.
    let broken = execute("add_mcp_server", &json!({ "name": "broken", "command": "odg-no-such-program" }), &context).await;
    assert!(broken.is_error && broken.content.contains("did not connect") && broken.content.contains("`replace`"), "{}", broken.content);
    assert_eq!(broken.meta["state"], "failed");
    // One that starts is connected, and its tools are named. The stand-in server needs only Node.
    let node = std::process::Command::new("node").arg("--version").output().is_ok_and(|output| output.status.success());
    if node {
        let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/mcp-server.mjs");
        let added = execute("add_mcp_server", &json!({ "name": "notes", "command": "node", "args": [server.to_string_lossy()], "env": { "LEVEL": 2 } }), &context).await;
        assert!(!added.is_error && added.content.contains("mcp__notes__<tool>") && added.content.ends_with(": shout."), "{}", added.content);
        assert_eq!(added.meta["tools"], json!(["shout"]));
        let listed: Value = serde_json::from_str(&std::fs::read_to_string(directory.path().join("home/mcp.json")).unwrap()).unwrap();
        assert_eq!(listed["mcpServers"]["notes"]["env"], json!({ "LEVEL": "2" }));
        assert!(hub.specs().iter().any(|spec| spec["name"] == "mcp__notes__shout"));
    }
    context.mcp = None;
    assert!(execute("add_mcp_server", &json!({ "name": "x", "url": "https://x" }), &context).await.content.contains("cannot be added here"));
}

#[tokio::test]
async fn reads_numbered_lines_with_offset_and_limit() {
    let (directory, context) = context();
    std::fs::write(directory.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let all = execute("read_file", &json!({ "path": "a.txt" }), &context).await;
    assert!(!all.is_error);
    assert_eq!(all.content, "     1\tone\n     2\ttwo\n     3\tthree\n     4\tfour");
    assert_eq!(all.meta, json!({ "path": "a.txt", "lines": 4, "shown": 4 }));

    let part = execute("read_file", &json!({ "path": "a.txt", "offset": 2, "limit": 2 }), &context).await;
    assert_eq!(part.content, "     2\ttwo\n     3\tthree\n[showing lines 2-3 of 4; pass offset to read more]");

    assert!(execute("read_file", &json!({ "path": "missing.txt" }), &context).await.is_error);
    assert!(execute("read_file", &json!({}), &context).await.is_error);
    std::fs::write(directory.path().join("b.bin"), [1u8, 0, 2]).unwrap();
    assert!(execute("read_file", &json!({ "path": "b.bin" }), &context).await.content.contains("binary"));
}

#[tokio::test]
async fn reads_the_text_of_a_document_in_numbered_lines() {
    let (directory, context) = context();
    // Named as anything: a PDF is known by its first bytes.
    std::fs::write(directory.path().join("sheet.dat"), crate::pdf::tests::sample(&[&["Pin 1 is VIN", "Pin 2 is GND"], &["Absolute maximum 42 V"]])).unwrap();
    let all = execute("read_file", &json!({ "path": "sheet.dat" }), &context).await;
    assert!(!all.is_error, "{}", all.content);
    assert!(all.content.starts_with("[sheet.dat is a PDF of 2 pages. This is its text"), "{}", all.content);
    assert!(all.content.contains("     1\t[page 1 of 2]\n     2\tPin 1 is VIN") && all.content.contains("\t[page 2 of 2]\n"), "{}", all.content);
    assert_eq!((all.meta["document"].as_str(), all.meta["parts"].as_u64()), (Some("pdf"), Some(2)));
    // Read on from a line, as a text file is: here from where the second page starts.
    let second = all.content.lines().find(|line| line.ends_with("\t[page 2 of 2]")).and_then(|line| line.trim().split('\t').next()?.parse::<u64>().ok()).unwrap();
    let part = execute("read_file", &json!({ "path": "sheet.dat", "offset": second, "limit": 2 }), &context).await;
    assert!(part.content.starts_with("[sheet.dat is a PDF of 2 pages."), "{}", part.content);
    assert!(part.content.ends_with(&format!("{second:>6}\t[page 2 of 2]\n{:>6}\tAbsolute maximum 42 V", second + 1)), "{}", part.content);
    // A PDF without words says so, rather than that it is a binary file.
    std::fs::write(directory.path().join("scan.pdf"), crate::pdf::tests::sample(&[&[]])).unwrap();
    let scan = execute("read_file", &json!({ "path": "scan.pdf" }), &context).await;
    assert!(scan.is_error && scan.content.contains("no text in it"), "{}", scan.content);
    // A Word document, and an old one that is not read, with what would make it readable.
    use crate::documents::tests::{paragraph, word_document};
    std::fs::write(directory.path().join("说明.docx"), word_document(&[paragraph(Some("1"), "安装"), paragraph(None, "先装驱动。")].concat())).unwrap();
    let word = execute("read_file", &json!({ "path": "说明.docx" }), &context).await;
    assert_eq!(word.content, "[说明.docx is a Word document. This is its text: paragraphs in order, headings marked with #, list items with -, and tables as rows of cells split by |. Its pictures, charts, and formatting are not kept.]\n     1\t# 安装\n     2\t先装驱动。");
    std::fs::write(directory.path().join("old.doc"), [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).unwrap();
    let old = execute("read_file", &json!({ "path": "old.doc" }), &context).await;
    assert!(old.is_error && old.content.contains("saved as .docx or .pptx"), "{}", old.content);
}

#[tokio::test]
async fn stops_a_read_of_long_lines_at_the_output_limit() {
    let (directory, context) = context();
    // 2000 lines of 1500 characters: within the line and file limits, but 3 MB of text.
    let line = "x".repeat(1500);
    std::fs::write(directory.path().join("wide.txt"), format!("{line}\n").repeat(2000)).unwrap();
    let read = execute("read_file", &json!({ "path": "wide.txt" }), &context).await;
    assert!(!read.is_error);
    assert!(read.content.chars().count() < READ_OUTPUT_CHARS + 200);
    let shown = read.meta["shown"].as_u64().unwrap();
    assert!(shown > 0 && shown < 2000);
    assert!(read.content.ends_with(&format!("pass offset {} to read on]", shown + 1)), "{}", &read.content[read.content.len() - 200..]);

    let next = execute("read_file", &json!({ "path": "wide.txt", "offset": shown + 1 }), &context).await;
    assert!(next.content.starts_with(&format!("{:>6}\t", shown + 1)));
}

#[tokio::test]
async fn edits_exactly_one_occurrence_and_keeps_crlf() {
    let (directory, context) = context();
    let file = directory.path().join("src").join("lib.rs");
    let created = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "", "new_string": "fn a() {}\nfn b() {}\n" }), &context).await;
    assert!(!created.is_error, "{}", created.content);
    assert_eq!(created.meta["created"], true);

    std::fs::write(&file, "fn a() {}\r\nfn b() {}\r\nfn a() {}\r\n").unwrap();
    let ambiguous = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn a() {}", "new_string": "fn c() {}" }), &context).await;
    assert!(ambiguous.is_error && ambiguous.content.contains("2 times"));
    let missing = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn z()", "new_string": "x" }), &context).await;
    assert!(missing.is_error && missing.content.contains("not found"));

    let edited = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn b() {}\nfn a() {}", "new_string": "fn b() {\n    1;\n}" }), &context).await;
    assert!(!edited.is_error, "{}", edited.content);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn a() {}\r\nfn b() {\r\n    1;\r\n}\r\n");
    assert_eq!((edited.meta["added"].as_u64(), edited.meta["removed"].as_u64()), (Some(3), Some(2)));
    let kinds: String = edited.meta["hunk"]["lines"].as_array().unwrap().iter().map(|line| line["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, " --+++");

    let all = execute("edit_file", &json!({ "path": "src/lib.rs", "old_string": "fn", "new_string": "pub fn", "replace_all": true }), &context).await;
    assert!(!all.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap().matches("pub fn").count(), 2);
}

#[tokio::test]
async fn searches_text_files_and_honours_gitignore_and_glob() {
    let (directory, context) = context();
    let root = directory.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(".gitignore"), "target\n").unwrap();
    std::fs::write(root.join(".git/config"), "read_events\n").unwrap();
    std::fs::write(root.join(".github/workflows/ci.yml"), "name: READ_EVENTS\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    read_events();\n}\n").unwrap();
    std::fs::write(root.join("notes.md"), "call read_events here\n").unwrap();
    std::fs::write(root.join("target/out.rs"), "read_events\n").unwrap();

    let found = execute("search", &json!({ "pattern": r"read_events\b" }), &context).await;
    assert!(!found.is_error);
    let mut lines: Vec<&str> = found.content.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, vec!["notes.md:1: call read_events here", "src/main.rs:2:     read_events();"]);
    assert_eq!(found.meta["files"], 2);

    // Dot folders such as `.github` are searched (while `.git` stays skipped), and `case_insensitive` / `files_only` work.
    let ci = execute("search", &json!({ "pattern": "read_events", "case_insensitive": true, "files_only": true }), &context).await;
    assert!(!ci.is_error, "{}", ci.content);
    assert_eq!(ci.content, ".github/workflows/ci.yml\nnotes.md\nsrc/main.rs");
    assert_eq!((ci.meta["files"].as_u64(), ci.meta["files_only"].as_bool()), (Some(3), Some(true)));

    // Context lines are marked with `-`, overlapping windows merge, and separate hunks split on `--`.
    std::fs::write(root.join("src/ctx.rs"), "l1\nhit a\nl3\nhit b\nl5\nl6\nl7\nhit c\nl9\n").unwrap();
    let with_ctx = execute("search", &json!({ "pattern": "hit", "path": "src/ctx.rs", "context": 1 }), &context).await;
    assert_eq!(
        with_ctx.content,
        "src/ctx.rs:1- l1\nsrc/ctx.rs:2: hit a\nsrc/ctx.rs:3- l3\nsrc/ctx.rs:4: hit b\nsrc/ctx.rs:5- l5\n--\nsrc/ctx.rs:7- l7\nsrc/ctx.rs:8: hit c\nsrc/ctx.rs:9- l9"
    );
    assert_eq!((with_ctx.meta["matches"].as_u64(), with_ctx.meta["files"].as_u64()), (Some(3), Some(1)));

    let rust_only = execute("search", &json!({ "pattern": "read_events", "glob": "**/*.rs" }), &context).await;
    assert_eq!(rust_only.content, "src/main.rs:2:     read_events();");
    assert_eq!(execute("search", &json!({ "pattern": "nothing_like_this" }), &context).await.content, "No matches for `nothing_like_this`.");
    assert!(execute("search", &json!({ "pattern": "(" }), &context).await.is_error);
}

#[tokio::test]
async fn runs_commands_and_reports_exit_code_timeout_and_cancel() {
    let (directory, context) = context();
    std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
    let listed = execute("run_command", &json!({ "command": if cfg!(windows) { "Get-ChildItem -Name" } else { "ls" } }), &context).await;
    assert!(!listed.is_error, "{}", listed.content);
    assert!(listed.content.contains("marker.txt"));
    assert_eq!(listed.meta["exit_code"], 0);

    let unicode = execute("run_command", &json!({ "command": "echo 分身" }), &context).await;
    assert_eq!(unicode.content.trim(), "分身");

    let failed = execute("run_command", &json!({ "command": "exit 3" }), &context).await;
    assert!(failed.is_error);
    assert!(failed.content.ends_with("[exit code 3]"), "{}", failed.content);

    let sleep = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
    let started = Instant::now();
    let timed_out = execute("run_command", &json!({ "command": sleep, "timeout_seconds": 1 }), &context).await;
    assert!(timed_out.is_error && timed_out.content.contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(15));

    let cancel = context.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let cancelled = execute("run_command", &json!({ "command": sleep }), &context).await;
    assert!(cancelled.content.contains("[stopped: cancelled]"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_sandboxed_command_is_told_why_a_write_was_refused_and_can_ask_to_leave() {
    if let Err(why) = crate::sandbox::support() {
        eprintln!("not run: {why}");
        return;
    }
    let (directory, context) = context();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("out.txt");
    // Only the workspace: the test's other directory is under the system's temporary one, which a session would allow.
    let context = ToolContext { sandbox: Some(crate::sandbox::Sandbox { writable: vec![directory.path().to_path_buf()] }), jobs: Some(Arc::new(Jobs::default())), ..context };

    let inside = execute("run_command", &json!({ "command": "echo in > in.txt && cat in.txt" }), &context).await;
    assert!(!inside.is_error && inside.content == "in" && inside.meta["sandboxed"] == true, "{}", inside.content);

    let refused = execute("run_command", &json!({ "command": format!("echo out > {}", target.display()) }), &context).await;
    assert!(refused.is_error && !target.exists(), "{}", refused.content);
    assert!(refused.content.contains("ran in the sandbox") && refused.content.contains("`unsandboxed`") && refused.content.contains(&directory.path().display().to_string()), "{}", refused.content);
    // A command that fails for a reason of its own is not told about the sandbox.
    let failed = execute("run_command", &json!({ "command": "exit 3" }), &context).await;
    assert!(failed.is_error && !failed.content.contains("sandbox"), "{}", failed.content);

    // One left running is confined too.
    let started = execute("run_command", &json!({ "command": format!("echo out > {}; echo done", target.display()), "background": true }), &context).await;
    assert_eq!(started.meta["sandboxed"], true);
    let printed = execute("command_output", &json!({ "job": started.meta["job"], "wait_seconds": 20 }), &context).await;
    assert!(printed.content.contains("done") && !target.exists(), "{}", printed.content);

    // Asked to run unconfined, it does: whether it may is the policy's to decide before this.
    let free = execute("run_command", &json!({ "command": format!("echo out > {}", target.display()), "unsandboxed": true }), &context).await;
    assert!(!free.is_error && free.meta["sandboxed"] == false && target.is_file(), "{}", free.content);
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_a_command_running_reads_what_it_prints_and_stops_it() {
    let (_directory, mut context) = context();
    assert!(execute("run_command", &json!({ "command": "echo x", "background": true }), &context).await.is_error);
    let jobs = Arc::new(Jobs::default());
    context.jobs = Some(jobs.clone());

    // A command that prints, then stays: a server.
    let serve = if cfg!(windows) { "'ready'; Start-Sleep -Seconds 60; 'never'" } else { "echo ready; sleep 60; echo never" };
    let started = execute("run_command", &json!({ "command": serve, "background": true }), &context).await;
    assert!(!started.is_error && started.meta["job"] == "b1" && started.meta["background"] == true, "{}", started.content);
    let read = execute("command_output", &json!({ "job": "b1", "wait_seconds": 30 }), &context).await;
    assert_eq!(read.content, "ready\n[job b1: still running]");
    assert_eq!((read.meta["running"].as_bool(), read.is_error), (Some(true), false));
    // Nothing new: the read returns at once and says so.
    assert_eq!(execute("command_output", &json!({ "job": "b1" }), &context).await.content, "(nothing new)\n[job b1: still running]");
    assert_eq!(jobs.running(), [("b1".to_string(), serve.to_string())]);

    let begun = Instant::now();
    let stopped = execute("stop_command", &json!({ "job": "b1" }), &context).await;
    assert!(stopped.content.ends_with("[job b1: stopped]") && !stopped.content.contains("never"), "{}", stopped.content);
    assert!(begun.elapsed() < Duration::from_secs(20));
    assert!(jobs.running().is_empty());

    // Stopping takes what the shell started with it, not only the shell.
    #[cfg(unix)]
    {
        // The shell starts a child of its own and prints the child's process id.
        let child = execute("run_command", &json!({ "command": "sleep 300 & echo $!; wait", "background": true }), &context).await;
        let id = child.meta["job"].as_str().unwrap().to_string();
        let printed = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
        let pid = printed.content.lines().next().unwrap().trim().to_string();
        // A process that is gone has no state; one that was killed and not yet collected shows Z.
        let alive = || {
            let state = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid]).output().unwrap().stdout;
            !state.is_empty() && !state.starts_with(b"Z")
        };
        assert!(alive(), "{}", printed.content);
        execute("stop_command", &json!({ "job": id }), &context).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!alive(), "the command's child outlived it");
    }

    // A command that ends by itself reports its exit code, and a failure is an error result.
    let fail = execute("run_command", &json!({ "command": "echo 分身; exit 3", "background": true }), &context).await;
    let id = fail.meta["job"].as_str().unwrap().to_string();
    let ended = execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await;
    // The exit can arrive a moment after the output; a second read settles it.
    let ended = if ended.meta["running"] == true { execute("command_output", &json!({ "job": id, "wait_seconds": 30 }), &context).await } else { ended };
    assert!(ended.is_error && ended.content.ends_with(&format!("[job {id}: exited with code 3]")), "{}", ended.content);
    assert_eq!(ended.meta["exit_code"], 3);

    assert!(execute("command_output", &json!({ "job": "b9" }), &context).await.content.starts_with("There is no background command `b9`."));
    assert!(execute("stop_command", &json!({ "job": "b9" }), &context).await.is_error);
}

#[tokio::test]
async fn command_output_reaches_the_model_without_terminal_colors() {
    let (directory, context) = context();
    std::fs::write(directory.path().join("marker.txt"), "x").unwrap();
    let commands: &[&str] = if cfg!(windows) {
        &[
            // PowerShell 7 colors a table's header.
            "Get-ChildItem | Select-Object Name, Length",
            // ...and a native program's stderr redirected into the pipeline.
            "cmd /c \"echo oops 1>&2\" 2>&1",
            // A program that colors its output regardless.
            "Write-Output \"$([char]27)[31mred$([char]27)[0m $([char]27)]8;;https://example.com$([char]7)link$([char]27)]8;;$([char]7)\"",
        ]
    } else {
        &["printf '\\033[31mred\\033[0m \\033]8;;https://example.com\\007link\\033]8;;\\007\\n'"]
    };
    for command in commands {
        let output = execute("run_command", &json!({ "command": command }), &context).await;
        assert!(!output.content.contains('\u{1b}'), "{command}: {:?}", output.content);
        assert!(!output.meta["output"].as_str().unwrap().contains('\u{1b}'), "{command}");
    }
    let colored = execute("run_command", &json!({ "command": commands.last().unwrap() }), &context).await;
    assert_eq!(colored.content.trim(), "red link");
}

#[test]
fn strips_terminal_escape_sequences() {
    assert_eq!(strip_ansi("\u{1b}[32;1mMode\u{1b}[0m plain"), "Mode plain");
    assert_eq!(strip_ansi("\u{1b}]8;;https://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
    assert_eq!(strip_ansi("a\u{1b}(Bb\u{1b}[?25lc"), "abc");
    assert_eq!(strip_ansi("no escapes, 分身"), "no escapes, 分身");
}

#[tokio::test]
async fn finds_files_by_listing_a_directory_or_by_glob() {
    let (directory, context) = context();
    let root = directory.path();
    for file in ["src/main.rs", "src/lexer/mod.rs", "src/lexer/notes.txt", ".github/workflows/ci.yml", ".git/config", "target/debug/app.rs", "README.md"] {
        std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
        std::fs::write(root.join(file), "x").unwrap();
    }
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

    // A listing is one level, as it is on disk, directories marked.
    let listed = execute("find_files", &json!({}), &context).await;
    assert_eq!(listed.content, ".git/\n.github/\n.gitignore\nREADME.md\nsrc/\ntarget/");
    assert_eq!(execute("find_files", &json!({ "path": "src/lexer" }), &context).await.content, "mod.rs\nnotes.txt");

    // A glob walks below, skipping what Git ignores and Git's own directory, but not other dot directories.
    let rust = execute("find_files", &json!({ "glob": "**/*.rs" }), &context).await;
    assert_eq!(rust.content, "src/lexer/mod.rs\nsrc/main.rs");
    assert_eq!(rust.meta["entries"], 2);
    assert_eq!(execute("find_files", &json!({ "glob": "**/*.yml" }), &context).await.content, ".github/workflows/ci.yml");
    assert_eq!(execute("find_files", &json!({ "glob": "*config*" }), &context).await.content, "No file below . matches `*config*`.");
    assert_eq!(execute("find_files", &json!({ "path": "src", "glob": "**/mod.rs" }), &context).await.content, "src/lexer/mod.rs");

    assert!(execute("find_files", &json!({ "path": "README.md" }), &context).await.is_error);
    assert!(execute("find_files", &json!({ "glob": "[" }), &context).await.is_error);
}

#[tokio::test]
async fn writes_a_whole_file_and_applies_several_edits_or_none() {
    let (directory, mut context) = context();
    let snapshots = directory.path().join("snapshots");
    context.snapshots = Some(snapshots.clone());
    let file = directory.path().join("notes/a.txt");

    let created = execute("write_file", &json!({ "path": "notes/a.txt", "content": "one\ntwo\nthree\n" }), &context).await;
    assert!(!created.is_error && created.meta["created"] == true && created.meta["before"].is_null(), "{}", created.content);
    // Overwriting saves what was there, like an edit does; unchanged lines between edits are not counted as changed.
    let rewritten = execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await;
    assert_eq!((rewritten.meta["added"].as_u64(), rewritten.meta["removed"].as_u64()), (Some(2), Some(2)));
    let saved = snapshots.join(rewritten.meta["before"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "one\ntwo\nthree\n");
    assert!(execute("write_file", &json!({ "path": "notes/a.txt", "content": "alpha\ntwo\nomega\n" }), &context).await.is_error);
    assert!(execute("write_file", &json!({ "path": "notes", "content": "x" }), &context).await.is_error);

    // Distant edits in one call produce separate hunks with accurate line counts instead of swallowing the unchanged middle.
    let long: String = (1..=510).map(|n| format!("line {n}\n")).collect();
    std::fs::write(directory.path().join("notes/long.txt"), &long).unwrap();
    let multi = execute(
        "edit_file",
        &json!({ "path": "notes/long.txt", "edits": [
            { "old_string": "line 5\n", "new_string": "line 5 updated\nline 5b\n" },
            { "old_string": "line 505\n", "new_string": "line 505 updated\n" },
        ] }),
        &context,
    )
    .await;
    assert!(!multi.is_error, "{}", multi.content);
    assert_eq!((multi.meta["added"].as_u64(), multi.meta["removed"].as_u64()), (Some(3), Some(2)));
    let hunks = multi.meta["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 2);
    assert_eq!((hunks[0]["old_start"].as_u64(), hunks[0]["new_start"].as_u64()), (Some(3), Some(3)));
    assert_eq!((hunks[1]["old_start"].as_u64(), hunks[1]["new_start"].as_u64()), (Some(503), Some(504)));

    // Several edits apply in order, each to the result of the one before.
    let edits = json!({ "path": "notes/a.txt", "edits": [
        { "old_string": "alpha", "new_string": "first" },
        { "old_string": "first\ntwo", "new_string": "first\nsecond" },
    ] });
    let edited = execute("edit_file", &edits, &context).await;
    assert!(!edited.is_error, "{}", edited.content);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");

    // One that does not apply stops all of them: the file stays as it was.
    let failing = json!({ "path": "notes/a.txt", "edits": [
        { "old_string": "omega", "new_string": "last" },
        { "old_string": "missing", "new_string": "x" },
    ] });
    let failed = execute("edit_file", &failing, &context).await;
    assert!(failed.is_error && failed.content.starts_with("Edit 2 of 2: `old_string` was not found") && failed.content.ends_with("Nothing was written."), "{}", failed.content);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\nsecond\nomega\n");
    assert!(execute("edit_file", &json!({ "path": "notes/a.txt", "edits": [] }), &context).await.is_error);
}

#[tokio::test]
async fn keeps_a_checked_todo_list() {
    let (_directory, context) = context();
    let list = json!({ "todos": [{ "text": " read the parser ", "status": "done" }, { "text": "fix the lexer", "status": "in_progress" }, { "text": "run tests", "status": "pending" }] });
    let kept = execute("update_todos", &list, &context).await;
    assert_eq!(kept.content, "The list has 3 items, 1 done.");
    assert_eq!(kept.meta["todos"][0], json!({ "text": "read the parser", "status": "done" }));
    // Once no pending items remain, the result reminds the model to close out the final item before replying.
    let final_step = json!({ "todos": [{ "text": "read the parser", "status": "done" }, { "text": "write summary", "status": "in_progress" }] });
    assert!(execute("update_todos", &final_step, &context).await.content.contains("Before your final reply"));
    let finished = json!({ "todos": [{ "text": "read the parser", "status": "done" }, { "text": "write summary", "status": "done" }] });
    assert_eq!(execute("update_todos", &finished, &context).await.content, "The list has 2 items, 2 done.");
    for bad in [json!({}), json!({ "todos": [{ "text": "x", "status": "started" }] }), json!({ "todos": [{ "text": "  ", "status": "done" }] })] {
        assert!(execute("update_todos", &bad, &context).await.is_error, "{bad}");
    }
    assert!(execute("update_todos", &json!({ "todos": [] }), &context).await.content.starts_with("The list has 0 items"));
}

#[test]
fn reads_the_checked_out_branch_from_gits_files() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("project");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    assert_eq!(git_branch(directory.path()), None);
    std::fs::write(workspace.join(".git/HEAD"), "ref: refs/heads/feat/providers\n").unwrap();
    assert_eq!(git_branch(&workspace).as_deref(), Some("feat/providers"));
    // No branch checked out: the commit, shortened.
    std::fs::write(workspace.join(".git/HEAD"), "7cbbed4a96c5740b9387e85f1f58fabcdef01234\n").unwrap();
    assert_eq!(git_branch(&workspace).as_deref(), Some("7cbbed4"));
    // A linked worktree keeps a file that points at its real directory.
    let worktree = directory.path().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(workspace.join(".git/worktrees/w")).unwrap();
    std::fs::write(workspace.join(".git/worktrees/w/HEAD"), "ref: refs/heads/side\n").unwrap();
    std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", workspace.join(".git/worktrees/w").display())).unwrap();
    assert_eq!(git_branch(&worktree).as_deref(), Some("side"));
}

#[test]
fn resolves_paths_and_detects_what_is_outside_the_workspace() {
    let (directory, context) = context();
    let workspace = &context.workspace;
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    assert_eq!(resolve(workspace, "src/../a.txt"), workspace.join("a.txt"));
    assert!(inside_workspace(workspace, &resolve(workspace, "src/new/file.rs")));
    assert!(inside_workspace(workspace, workspace));
    assert!(!inside_workspace(workspace, &resolve(workspace, "../elsewhere.txt")));
    assert!(!inside_workspace(workspace, &std::env::temp_dir()));

    std::fs::create_dir_all(directory.path().join(".git/hooks")).unwrap();
    assert!(in_git_directory(workspace, &resolve(workspace, ".git/hooks/pre-commit")));
    assert!(in_git_directory(workspace, &resolve(workspace, "vendor/lib/.GIT/config")));
    assert!(!in_git_directory(workspace, &resolve(workspace, "src/.gitignore")));
    assert!(!in_git_directory(workspace, &resolve(workspace, "src/new/file.rs")));
}

#[test]
fn questions_are_checked_and_tidied_before_the_user_sees_them() {
    let asked = questions(&json!({ "questions": [
        { "header": "  一个很长很长很长的标签名字啊  ", "question": " 用哪个？ ", "options": [{ "label": " 方案  一 ", "description": " 改得少 " }, { "label": "方案二" }] },
        { "question": "哪些要测？", "kind": "multiple", "options": [{ "label": "解析" }, { "label": "渲染" }, { "label": "导出" }] },
        { "question": "还有什么？" }
    ] }))
    .unwrap();
    // A header cut to fit its tab, or made up; the kind from the options when it is not given.
    assert_eq!(asked.iter().map(|question| (question.header.as_str(), question.kind)).collect::<Vec<_>>(), [("一个很长很长很长的标签名", QuestionKind::Single), ("问题 2", QuestionKind::Multiple), ("问题 3", QuestionKind::Text)]);
    assert_eq!((asked[0].question.as_str(), asked[0].options[0].label.as_str(), asked[0].options[0].description.as_str()), ("用哪个？", "方案 一", "改得少"));
    // A text question takes no options, even if some were given.
    let text = questions(&json!({ "questions": [{ "question": "说说？", "kind": "text", "options": [{ "label": "a" }, { "label": "b" }] }] })).unwrap();
    assert!(text[0].options.is_empty());

    let refused = |input: Value| questions(&input).unwrap_err();
    assert!(refused(json!({})).contains("`questions` is required"));
    assert!(refused(json!({ "questions": [] })).contains("`questions` is required"));
    assert!(refused(json!({ "questions": vec![json!({ "question": "?" }); 5] })).contains("5 questions are too many"));
    assert!(refused(json!({ "questions": [{ "question": " " }] })).contains("Question 1 has no `question`"));
    assert!(refused(json!({ "questions": [{ "question": "?", "kind": "rank" }] })).contains("kind `rank`"));
    assert!(refused(json!({ "questions": [{ "question": "?", "kind": "single" }] })).contains("offers 0 options"));
    assert!(refused(json!({ "questions": [{ "question": "?", "options": vec![json!({ "label": "x" }); 7] }] })).contains("offers 7 options"));
    assert!(refused(json!({ "questions": [{ "question": "?", "options": [{ "label": "a" }, { "label": "" }] }] })).contains("has no `label`"));
    assert!(refused(json!({ "questions": [{ "question": "?", "options": [{ "label": "a" }, { "label": " a " }] }] })).contains("offers `a` twice"));
}

#[test]
fn the_agent_is_told_each_answer_under_its_question() {
    let asked = questions(&json!({ "questions": [
        { "question": "用哪个？", "options": [{ "label": "一" }, { "label": "二" }] },
        { "question": "哪些要测？", "kind": "multiple", "options": [{ "label": "解析" }, { "label": "渲染" }] },
        { "question": "还有什么？" },
        { "question": "叫什么？", "options": [{ "label": "甲" }, { "label": "乙" }] }
    ] }))
    .unwrap();
    let reply = |chosen: &[&str], text: &str| Reply { chosen: chosen.iter().map(|label| label.to_string()).collect(), text: text.to_string() };
    let told = answered(&asked, &[reply(&["一"], ""), reply(&["解析", "渲染"], "先测解析"), reply(&[], "别动接口"), reply(&[], "")], false, None);
    assert_eq!(
        told,
        "The user answered:\n1. 用哪个？\n   → 一\n2. 哪些要测？\n   → 解析; 渲染. And in their own words: 先测解析\n3. 还有什么？\n   → 别动接口\n4. 叫什么？\n   → (left unanswered: decide this one yourself, and say what you chose)"
    );
    // Words in place of an option.
    assert!(answered(&asked[..1], &[reply(&[], "都不要")], false, None).ends_with("→ None of the options. In their own words: 都不要"));
    // Nothing answered, left to the agent, or nobody there in time.
    assert!(answered(&asked, &[], true, None).starts_with("The user chose not to answer"));
    assert!(answered(&asked, &[reply(&[], " ")], false, None).starts_with("The user chose not to answer"));
    assert!(answered(&asked, &[], false, Some(30)).starts_with("Nobody answered within 30 seconds"));
}

/// The smallest headers of each kind that say what a picture is and how big.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    bytes
}

#[test]
fn tells_a_picture_and_its_size_by_its_first_bytes() {
    assert_eq!(picture(&png(640, 480)), Some(("image/png", Some((640, 480)))));
    assert_eq!(picture(b"GIF89a\x20\x03\x58\x02rest"), Some(("image/gif", Some((800, 600)))));
    // A JPEG's size is in its first frame header, after the segments before it.
    let jpeg = [&[0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0][..], &[0xff, 0xc0, 0, 11, 8, 0x01, 0xe0, 0x02, 0x80, 3][..]].concat();
    assert_eq!(picture(&jpeg), Some(("image/jpeg", Some((640, 480)))));
    assert_eq!(picture(&[0xff, 0xd8, 0xff, 0xd9]), Some(("image/jpeg", None)));
    let webp = |chunk: &[u8], header: &[u8]| [b"RIFF\0\0\0\0WEBP".as_slice(), chunk, &[0; 4], header].concat();
    // VP8X keeps width and height less one in three bytes each, from the 24th byte.
    assert_eq!(picture(&webp(b"VP8X", &[0, 0, 0, 0, 0x7f, 0x02, 0, 0xdf, 0x01, 0])), Some(("image/webp", Some((640, 480)))));
    // VP8L packs them less one in fourteen bits each, from the 21st byte.
    let (width, height) = (639u32, 479u32);
    let bits = width | height << 14;
    assert_eq!(picture(&webp(b"VP8L", &[0x2f, bits as u8, (bits >> 8) as u8, (bits >> 16) as u8, (bits >> 24) as u8])), Some(("image/webp", Some((640, 480)))));
    assert_eq!(picture(b"plain text"), None);
}

#[tokio::test]
async fn shows_a_picture_to_a_model_that_can_see_and_says_why_not_otherwise() {
    let (directory, context) = context();
    std::fs::write(directory.path().join("shot.png"), png(2, 3)).unwrap();
    let read = execute("read_file", &json!({ "path": "shot.png" }), &context).await;
    assert!(!read.is_error && read.content == "shot.png is a PNG picture, 2×3, 1 KB. It is shown to you here.", "{}", read.content);
    assert_eq!(read.images, [Image { media_type: "image/png".into(), data: crate::events::base64(&png(2, 3)) }]);
    assert_eq!((read.meta["picture"].as_str(), read.meta["width"].as_u64(), read.meta["path"].as_str()), (Some("image/png"), Some(2), Some("shot.png")));
    // What it is, by its bytes: a picture under another name is still one; a text file named as a picture is not.
    std::fs::write(directory.path().join("shot.bin"), png(2, 3)).unwrap();
    assert_eq!(execute("read_file", &json!({ "path": "shot.bin" }), &context).await.images.len(), 1);
    std::fs::write(directory.path().join("fake.jpg"), "not a picture").unwrap();
    let fake = execute("read_file", &json!({ "path": "fake.jpg" }), &context).await;
    assert!(fake.is_error && fake.content.contains("is not a PNG, JPEG, GIF, or WebP one"), "{}", fake.content);
    // A model that cannot see is told what the picture is, and what to do instead.
    let blind = ToolContext { vision: Some(false), workspace: context.workspace.clone(), cancel: CancellationToken::new(), ..context };
    let refused = execute("read_file", &json!({ "path": "shot.png" }), &blind).await;
    assert!(refused.is_error && refused.images.is_empty() && refused.content.contains("2×3") && refused.content.contains("cannot see pictures"), "{}", refused.content);
    // One too large to send is refused by its size, before it is read.
    let mut large = png(4000, 4000);
    large.resize(crate::events::IMAGE_FILE_BYTES as usize + 1, 0);
    std::fs::write(directory.path().join("large.png"), large).unwrap();
    let large = execute("read_file", &json!({ "path": "large.png" }), &ToolContext { vision: Some(true), ..blind }).await;
    assert!(large.is_error && large.content.contains("too large to be shown"), "{}", large.content);
}

#[tokio::test]
async fn deletes_and_moves_one_file_keeping_a_copy_to_undo_it() {
    let (directory, mut context) = context();
    let workspace = directory.path().join("work");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    context.workspace = workspace.clone();
    let snapshots = directory.path().join("snapshots");
    context.snapshots = Some(snapshots.clone());
    std::fs::write(workspace.join("old.txt"), "one\ntwo\n").unwrap();
    let deleted = execute("delete_file", &json!({ "path": "old.txt" }), &context).await;
    assert!(!deleted.is_error && deleted.content == "Deleted old.txt (2 lines).", "{}", deleted.content);
    assert!(!workspace.join("old.txt").exists());
    // What went is shown as lines taken out, and kept for undoing; the file is not there to open.
    assert_eq!((deleted.meta["deleted"].as_bool(), deleted.meta["removed"].as_u64(), deleted.meta["file"].is_null()), (Some(true), Some(2), true));
    let change = &deleted.meta["changes"][0];
    assert_eq!((change["path"].as_str(), change["after"].is_null()), (Some("old.txt"), true));
    assert_eq!(std::fs::read(snapshots.join(change["before"].as_str().unwrap())).unwrap(), b"one\ntwo\n");
    // A file that is not text is deleted all the same, by its bytes.
    std::fs::write(workspace.join("blob.bin"), [0u8, 1, 2]).unwrap();
    assert_eq!(execute("delete_file", &json!({ "path": "blob.bin" }), &context).await.content, "Deleted blob.bin (3 bytes).");

    std::fs::write(workspace.join("a.txt"), "moved\n").unwrap();
    let moved = execute("move_file", &json!({ "path": "a.txt", "to": "lib/deep/b.txt" }), &context).await;
    assert!(!moved.is_error && moved.content == "Moved a.txt to lib/deep/b.txt.", "{}", moved.content);
    assert!(!workspace.join("a.txt").exists());
    assert_eq!(std::fs::read_to_string(workspace.join("lib/deep/b.txt")).unwrap(), "moved\n");
    // For undoing: the file that came, and the file that went.
    let changes: Vec<(&str, bool, bool)> = moved.meta["changes"].as_array().unwrap().iter().map(|change| (change["path"].as_str().unwrap(), change["before"].is_null(), change["after"].is_null())).collect();
    assert_eq!(changes, [("lib/deep/b.txt", true, false), ("a.txt", false, true)]);
    assert_eq!(moved.meta["to"], "lib/deep/b.txt");

    // What it will not do.
    std::fs::write(workspace.join("c.txt"), "c").unwrap();
    let refused = |input: Value| {
        let context = &context;
        async move { execute(input["tool"].as_str().unwrap(), &input, context).await }
    };
    let taken = refused(json!({ "tool": "move_file", "path": "c.txt", "to": "lib/deep/b.txt" })).await;
    assert!(taken.is_error && taken.content.contains("lib/deep/b.txt already exists"), "{}", taken.content);
    assert!(workspace.join("c.txt").exists());
    let folder = refused(json!({ "tool": "delete_file", "path": "src" })).await;
    assert!(folder.is_error && folder.content.contains("is a folder"), "{}", folder.content);
    let missing = refused(json!({ "tool": "delete_file", "path": "gone.txt" })).await;
    assert!(missing.is_error && missing.content.contains("does not exist"), "{}", missing.content);
    let same = refused(json!({ "tool": "move_file", "path": "c.txt", "to": "./c.txt" })).await;
    assert!(same.is_error && same.content.contains("already there"), "{}", same.content);

    // Changing only letter case works on a case-insensitive filesystem, while on a case-sensitive one two
    // distinct files whose names differ only by case are never overwritten.
    std::fs::write(workspace.join("case.txt"), "lower").unwrap();
    if std::fs::symlink_metadata(workspace.join("CASE.txt")).is_ok() {
        let renamed = execute("move_file", &json!({ "path": "case.txt", "to": "CASE.txt" }), &context).await;
        assert!(!renamed.is_error, "{}", renamed.content);
        assert_eq!(std::fs::read_to_string(workspace.join("CASE.txt")).unwrap(), "lower");
    } else {
        std::fs::write(workspace.join("CASE.txt"), "upper").unwrap();
        let clash = execute("move_file", &json!({ "path": "case.txt", "to": "CASE.txt" }), &context).await;
        assert!(clash.is_error && clash.content.contains("CASE.txt already exists"), "{}", clash.content);
        assert_eq!(std::fs::read_to_string(workspace.join("CASE.txt")).unwrap(), "upper");
    }
}

/// A background program that answers each line it reads, and says goodbye at the end of its input.
fn echoing() -> &'static str {
    if cfg!(windows) {
        "while (($line = [Console]::In.ReadLine()) -ne $null) { \"got $line\" }; 'bye'"
    } else {
        "while read line; do echo \"got $line\"; done; echo bye"
    }
}

#[tokio::test]
async fn types_into_a_background_command_and_reads_what_it_answers() {
    let (_directory, mut context) = context();
    context.jobs = Some(Arc::new(Jobs::default()));
    let started = execute("run_command", &json!({ "command": echoing(), "background": true }), &context).await;
    let job = started.meta["job"].as_str().unwrap().to_string();
    assert!(started.content.contains("`command_input`"), "{}", started.content);

    // A line, ended by Enter unless told otherwise; what the program says back comes with it.
    let answered = execute("command_input", &json!({ "job": job, "text": "hello", "wait_seconds": 10 }), &context).await;
    assert!(!answered.is_error && answered.content.starts_with("Sent 6 characters. What it printed since:\ngot hello"), "{}", answered.content);
    assert_eq!((answered.meta["sent"].as_str(), answered.meta["running"].as_bool()), (Some("hello\n"), Some(true)));

    // Closed, its input ends, and so does the program.
    let closed = execute("command_input", &json!({ "job": job, "text": "last", "close": true, "wait_seconds": 10 }), &context).await;
    assert!(closed.content.starts_with("Sent 5 characters and closed its input."), "{}", closed.content);
    let mut ended = closed;
    for _ in 0..20 {
        if ended.meta["running"] == false {
            break;
        }
        ended = execute("command_output", &json!({ "job": job, "wait_seconds": 1 }), &context).await;
    }
    assert_eq!((ended.meta["running"].as_bool(), ended.meta["exit_code"].as_i64()), (Some(false), Some(0)), "{}", ended.content);

    // What it will not do.
    let late = execute("command_input", &json!({ "job": job, "text": "more" }), &context).await;
    assert!(late.is_error && late.content.contains("has ended"), "{}", late.content);
    let nothing = execute("command_input", &json!({ "job": job }), &context).await;
    assert!(nothing.is_error && nothing.content.contains("Give `text`"), "{}", nothing.content);
    let unknown = execute("command_input", &json!({ "job": "b99", "text": "x" }), &context).await;
    assert!(unknown.is_error && unknown.content.contains("b99"), "{}", unknown.content);
}

#[tokio::test]
async fn a_command_in_the_foreground_reads_no_input_and_does_not_wait_for_any() {
    let (_directory, context) = context();
    let reading = if cfg!(windows) { "$line = [Console]::In.ReadLine(); if ($line -eq $null) { 'none' } else { $line }" } else { "read line || echo none" };
    let ran = execute("run_command", &json!({ "command": reading, "timeout_seconds": 20 }), &context).await;
    assert!(ran.content.contains("none") && ran.meta["stopped"].is_null(), "{}", ran.content);
}

#[tokio::test]
async fn lists_and_reads_the_resources_of_the_users_mcp_servers() {
    let (directory, mut context) = context();
    let hub = Arc::new(crate::mcp::McpHub::new(directory.path().join("mcp.json"), tokio::runtime::Handle::current()));
    hub.insert("notes", crate::mcp::testing::resource_server().await);
    hub.insert("echo", crate::mcp::testing::echo_server().await);
    context.mcp = Some(hub);
    // Only the server with resources is named, and listed.
    assert!(mcp_resource_specs(&["notes".to_string()])[0]["description"].as_str().unwrap().contains("Servers with resources: `notes`."));
    let listed = execute("list_mcp_resources", &json!({}), &context).await;
    assert_eq!(
        listed.content,
        "Resources of `notes` (2):\n- notes://today — Today's notes (text/markdown): What was done today.\n- notes://chart — chart (image/png)\nTemplates of addresses:\n- notes://day/{date} — day: One day's notes."
    );
    assert_eq!(listed.meta["results"], 2);
    let read = execute("read_mcp_resource", &json!({ "server": "notes", "uri": "notes://today" }), &context).await;
    assert!(!read.is_error && read.content == "# Today\nShipped 0.4.7.");
    // A picture goes to a model that can see, and only its kind to one that cannot.
    let chart = execute("read_mcp_resource", &json!({ "server": "notes", "uri": "notes://chart" }), &context).await;
    assert_eq!(chart.images, [Image { media_type: "image/png".into(), data: "aGk=".into() }]);
    context.vision = Some(false);
    assert!(execute("read_mcp_resource", &json!({ "server": "notes", "uri": "notes://chart" }), &context).await.images.is_empty());
    // What it will not do.
    let unknown = execute("read_mcp_resource", &json!({ "server": "echo", "uri": "x" }), &context).await;
    assert!(unknown.is_error && unknown.content.contains("No connected MCP server `echo` offers resources"), "{}", unknown.content);
    let none = execute("list_mcp_resources", &json!({ "server": "gone" }), &context).await;
    assert!(none.is_error && none.content.contains("`gone`"), "{}", none.content);
}

#[tokio::test]
async fn mcp_tool_call_forwards_images_only_when_model_supports_vision() {
    let (directory, mut context) = context();
    let hub = Arc::new(crate::mcp::McpHub::new(directory.path().join("mcp.json"), tokio::runtime::Handle::current()));
    hub.insert("echo", crate::mcp::testing::echo_server().await);
    context.mcp = Some(hub);

    let shot = execute("mcp__echo__echo", &json!({ "text": "screenshot" }), &context).await;
    assert!(!shot.is_error);
    assert_eq!(shot.images, [Image { media_type: "image/png".into(), data: "aGk=".into() }]);
    assert_eq!(shot.meta, json!({ "mcp": true, "pictures": 1 }));

    context.vision = Some(false);
    let blind = execute("mcp__echo__echo", &json!({ "text": "screenshot" }), &context).await;
    assert!(blind.images.is_empty());
}

#[tokio::test]
async fn run_command_respects_cwd_and_rejects_missing_or_non_directory_cwd() {
    let (directory, context) = context();
    let sub = directory.path().join("packages/app");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("marker.txt"), "found-in-sub\n").unwrap();
    std::fs::write(directory.path().join("plain.txt"), "not a dir\n").unwrap();

    let read_marker = if cfg!(windows) { "Get-Content marker.txt" } else { "cat marker.txt" };
    let ran = execute("run_command", &json!({ "command": read_marker, "cwd": "packages/app" }), &context).await;
    assert!(!ran.is_error && ran.content.trim() == "found-in-sub", "{}", ran.content);
    assert_eq!(ran.meta["cwd"].as_str(), Some("packages/app"));

    // Default cwd (`.` or omitted) leaves `meta["cwd"]` unset.
    let echo = execute("run_command", &json!({ "command": "echo hi", "cwd": "." }), &context).await;
    assert!(!echo.is_error && echo.meta["cwd"].is_null(), "{:?}", echo.meta);

    // Missing or file cwd returns a clear error before spawning.
    let missing = execute("run_command", &json!({ "command": "echo hi", "cwd": "no/such/dir" }), &context).await;
    assert!(missing.is_error && missing.content.contains("does not exist"), "{}", missing.content);
    let not_dir = execute("run_command", &json!({ "command": "echo hi", "cwd": "plain.txt" }), &context).await;
    assert!(not_dir.is_error && not_dir.content.contains("is not a directory"), "{}", not_dir.content);
}

#[test]
fn delegate_tasks_parses_read_only_flag() {
    let parsed = delegate_tasks(&json!({ "tasks": [
        { "name": "audit", "task": "Inspect auth.", "read_only": true },
        { "name": "fix", "task": "Patch parser." },
    ] }))
    .unwrap();
    assert_eq!(
        parsed,
        vec![
            DelegateTask { name: "audit".into(), task: "Inspect auth.".into(), read_only: true },
            DelegateTask { name: "fix".into(), task: "Patch parser.".into(), read_only: false },
        ]
    );
}
