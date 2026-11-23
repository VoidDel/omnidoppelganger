use tokio::io::AsyncRead;

use super::*;

#[test]
fn turns_paths_into_uris_and_back() {
    let path = Path::new("/work/my project/src/日志.rs");
    let document = uri(path);
    assert_eq!(document, "file:///work/my%20project/src/%E6%97%A5%E5%BF%97.rs");
    assert_eq!(path_of(&document).unwrap(), path);
    assert_eq!(path_of("untitled:Untitled-1"), None);
    assert_eq!(language_for(Path::new("a/B.TSX")).map(|language| language.program), Some("typescript-language-server"));
    assert!(language_for(Path::new("README.md")).is_none());
}

#[test]
fn writes_windows_paths_as_servers_do_and_reads_them_back_alike() {
    // A verbatim path, as canonicalizing gives on Windows, is written as the path it names.
    assert_eq!(uri_of(r"\\?\C:\Users\me\a b\x.c"), "file:///C:/Users/me/a%20b/x.c");
    assert_eq!(uri_of(r"C:\Users\me\x.c"), "file:///C:/Users/me/x.c");
    assert_eq!(uri_of(r"\\?\UNC\server\share\x.c"), "file://server/share/x.c");
    assert_eq!(plain_text(r"\\?\C:\w"), r"C:\w");
    // Read back on Windows with backslashes and a capital drive, however the server wrote the drive.
    assert_eq!(path_text("file:///C:/Users/me/a%20b/x.c", true).unwrap(), r"C:\Users\me\a b\x.c");
    assert_eq!(path_text("file:///c%3A/Users/me/x.c", true).unwrap(), r"C:\Users\me\x.c");
    assert_eq!(path_text("file://server/share/x.c", true).unwrap(), r"\\server\share\x.c");
    // Elsewhere, as it is.
    assert_eq!(path_text("file:///home/me/x.c", false).unwrap(), "/home/me/x.c");
}

#[test]
fn counts_columns_in_utf16_as_lsp_does() {
    // An emoji is two UTF-16 units, a Chinese character one.
    let line = "let 名 = \"😀\"; x";
    let x = line.chars().count() - 1;
    assert_eq!(to_utf16(line, x), x + 1);
    assert_eq!(from_utf16(line, x + 1), x);
    assert_eq!(from_utf16(line, 999), line.chars().count());
}

#[test]
fn finds_the_place_by_the_symbol_named_on_the_line() {
    let text = "fn width() {}\nlet w = width(id);\n";
    // The whole word: `id` in `width` is passed over for the `id` after it.
    assert_eq!(position(text, &json!({ "line": 2, "symbol": "id" }), "a.rs"), Ok((1, 14)));
    assert_eq!(position(text, &json!({ "line": 2, "symbol": "width" }), "a.rs"), Ok((1, 8)));
    assert_eq!(position(text, &json!({ "line": 1, "column": 4 }), "a.rs"), Ok((0, 3)));
    assert!(position(text, &json!({ "line": 2, "symbol": "height" }), "a.rs").unwrap_err().contains("which reads: let w = width(id);"));
    assert!(position(text, &json!({ "line": 9, "symbol": "w" }), "a.rs").unwrap_err().contains("has no line 9"));
    assert!(position(text, &json!({ "symbol": "w" }), "a.rs").unwrap_err().contains("`line` is required"));
}

#[test]
fn reads_every_shape_an_answer_comes_in() {
    let link = json!({ "targetUri": "file:///w/a.rs", "targetRange": {}, "targetSelectionRange": { "start": { "line": 3, "character": 1 } } });
    let location = json!({ "uri": "file:///w/b.rs", "range": { "start": { "line": 0, "character": 0 } } });
    assert_eq!(locations(&json!([link, location])).len(), 2);
    assert_eq!(locations(&location)[0].0, "file:///w/b.rs");
    assert!(locations(&Value::Null).is_empty());
    assert_eq!(hover_text(&json!({ "kind": "markdown", "value": "```rust\nfn f()\n```" })), "```rust\nfn f()\n```");
    assert_eq!(hover_text(&json!([{ "language": "rust", "value": "fn f()" }, "docs"])), "```rust\nfn f()\n```\n\ndocs");
    let mut lines = Vec::new();
    outline(&[json!({ "name": "S", "kind": 23, "selectionRange": { "start": { "line": 0 } }, "children": [{ "name": "f", "kind": 6, "detail": "fn()", "selectionRange": { "start": { "line": 1 } } }] })], 0, &mut lines);
    assert_eq!(lines, ["struct S  (line 1)", "  method f  fn()  (line 2)"]);
}

/// A server on the other end of an in-memory pipe: it says it is indexing, then done, publishes a diagnostic for
/// the file opened, asks its client one thing, and answers `definition` with the file's first line.
async fn scripted(root: &Path, configured: Arc<Mutex<Value>>) -> Arc<Client> {
    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let writer: Writer = Arc::new(tokio::sync::Mutex::new(Box::new(server_write)));
    tokio::spawn(async move {
        let mut reader = BufReader::new(Box::new(server_read) as Box<dyn AsyncRead + Send + Unpin>);
        while let Some(message) = receive(&mut reader).await {
            let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
            match message["method"].as_str() {
                Some("initialize") => send(&writer, &reply(json!({ "capabilities": { "definitionProvider": true } }))).await.unwrap(),
                Some("initialized") => {
                    send(&writer, &json!({ "jsonrpc": "2.0", "id": 900, "method": "workspace/configuration", "params": { "items": [{ "section": "rust-analyzer" }] } })).await.unwrap();
                    send(&writer, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "index", "value": { "kind": "begin", "title": "Indexing" } } })).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    send(&writer, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "index", "value": { "kind": "end" } } })).await.unwrap();
                    send(&writer, &json!({ "jsonrpc": "2.0", "method": "experimental/serverStatus", "params": { "quiescent": true, "health": "ok" } })).await.unwrap();
                }
                Some("textDocument/didOpen") => {
                    let document = message["params"]["textDocument"]["uri"].clone();
                    let diagnostic = json!({ "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 5 } }, "severity": 1, "code": "E0425", "message": "cannot find value `y`" });
                    send(&writer, &json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": document, "diagnostics": [diagnostic] } })).await.unwrap();
                }
                Some("textDocument/definition") => {
                    let document = message["params"]["textDocument"]["uri"].clone();
                    send(&writer, &reply(json!([{ "uri": document, "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 4 } } }]))).await.unwrap();
                }
                // The answer to the server's own question, kept for the test to see.
                None if message["id"] == 900 => *configured.lock().unwrap() = message["result"].clone(),
                _ => {}
            }
        }
    });
    Client::start("rust-analyzer", root, Box::new(client_read), Box::new(client_write), None).await.unwrap()
}

#[tokio::test]
async fn waits_for_indexing_then_answers_with_the_line_it_points_at() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(root.join("a.rs"), "fn f() {}\nfn g() { y; f(); }\n").unwrap();
    let hub = LspHub::default();
    let configured = Arc::new(Mutex::new(Value::Null));
    let client = scripted(root, configured.clone()).await;
    hub.insert(root, &LANGUAGES[0], client.clone()).await;
    let cancel = CancellationToken::new();
    // rust-analyzer's own word counts: until it says it is done, it is at work.
    assert!(client.busy());
    let defined = navigate(&hub, root, &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }), &cancel).await.unwrap();
    assert_eq!(defined.content, "Defined at (1):\na.rs:1:4  fn f() {}");
    assert_eq!((defined.meta["results"].as_u64(), defined.meta["line"].as_u64()), (Some(1), Some(2)));
    assert!(!client.busy());
    let found = navigate(&hub, root, &json!({ "operation": "diagnostics", "path": "a.rs" }), &cancel).await.unwrap();
    assert!(found.content.starts_with("1 problems in a.rs:\nerror a.rs:2:5 [E0425]: cannot find value `y`\n    fn g() { y; f(); }"), "{}", found.content);
    assert_eq!(found.meta["errors"], 1);
    // What it asked for its settings was answered with them: no build scripts, no proc macros, no `cargo check`.
    assert_eq!(configured.lock().unwrap()[0]["procMacro"]["enable"], false);
    assert_eq!(configured.lock().unwrap()[0]["checkOnSave"], false);
    let refused = navigate(&hub, root, &json!({ "operation": "rename", "path": "a.rs" }), &cancel).await.unwrap_err();
    assert!(refused.contains("`operation` must be one of"), "{refused}");
    let other = navigate(&hub, root, &json!({ "operation": "symbols", "path": "notes.md" }), &cancel).await.unwrap_err();
    assert!(other.contains("is not of a language navigated here"), "{other}");
}

/// Whether a language server can be run here: on the `PATH`, and answering (rustup's stand-in for a component
/// not installed is on the `PATH` but fails).
fn runs(program: &str) -> bool {
    installed(program).is_some() && std::process::Command::new(program).arg("--version").output().is_ok_and(|output| output.status.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn navigates_a_real_rust_project_with_rust_analyzer() {
    if !runs("rust-analyzer") {
        eprintln!("rust-analyzer is not installed here: skipped");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n").unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "mod broken;\n\npub struct Counter {\n    pub count: u32,\n}\n\n/// Adds one.\npub fn helper(counter: &Counter) -> u32 {\n    counter.count + 1\n}\n\npub fn caller() -> u32 {\n    let counter = Counter { count: 1 };\n    helper(&counter) + helper(&counter)\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/broken.rs"), "pub fn oops( {\n}\n").unwrap();
    let hub = LspHub::default();
    let cancel = CancellationToken::new();
    let ask = |input: Value| {
        let (hub, root, cancel) = (&hub, &root, &cancel);
        async move { navigate(hub, root, &input, cancel).await.unwrap() }
    };

    let defined = ask(json!({ "operation": "definition", "path": "src/lib.rs", "line": 14, "symbol": "helper" })).await;
    assert!(defined.content.starts_with("Defined at (1):\nsrc/lib.rs:8:8  pub fn helper(counter: &Counter) -> u32 {"), "{}", defined.content);
    let used = ask(json!({ "operation": "references", "path": "src/lib.rs", "line": 8, "symbol": "helper" })).await;
    assert_eq!(used.meta["results"], 3, "{}", used.content);
    let hover = ask(json!({ "operation": "hover", "path": "src/lib.rs", "line": 14, "symbol": "helper" })).await;
    assert!(hover.content.contains("fn helper(counter: &Counter) -> u32") && hover.content.contains("Adds one."), "{}", hover.content);
    let outline = ask(json!({ "operation": "symbols", "path": "src/lib.rs" })).await;
    assert!(outline.content.contains("struct Counter  (line 3)") && outline.content.contains("function helper"), "{}", outline.content);
    let found = ask(json!({ "operation": "find_symbol", "path": "src/lib.rs", "query": "Counter" })).await;
    assert!(found.content.contains("struct Counter") && found.content.contains("src/lib.rs:3:"), "{}", found.content);
    let broken = ask(json!({ "operation": "diagnostics", "path": "src/broken.rs" })).await;
    assert!(broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error src/broken.rs:1:"), "{}", broken.content);
    // A file changed on disk is given to the server again before it is asked.
    std::fs::write(root.join("src/broken.rs"), "pub fn oops() {}\n").unwrap();
    let mended = ask(json!({ "operation": "diagnostics", "path": "src/broken.rs" })).await;
    assert_eq!(mended.meta["errors"], 0, "{}", mended.content);
    hub.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn navigates_a_real_c_project_with_clangd() {
    if !runs("clangd") {
        eprintln!("clangd is not installed here: skipped");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(
        root.join("counter.c"),
        "struct counter {\n    int count;\n};\n\n/* Adds one. */\nint helper(struct counter *c) {\n    return c->count + 1;\n}\n\nint caller(void) {\n    struct counter c = { 1 };\n    return helper(&c) + helper(&c);\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("broken.c"), "int oops( {\n}\n").unwrap();
    let hub = LspHub::default();
    let cancel = CancellationToken::new();
    let ask = |input: Value| {
        let (hub, root, cancel) = (&hub, &root, &cancel);
        async move { navigate(hub, root, &input, cancel).await.unwrap() }
    };
    let defined = ask(json!({ "operation": "definition", "path": "counter.c", "line": 12, "symbol": "helper" })).await;
    assert!(defined.content.starts_with("Defined at (1):\ncounter.c:6:5  int helper(struct counter *c) {"), "{}", defined.content);
    let used = ask(json!({ "operation": "references", "path": "counter.c", "line": 6, "symbol": "helper" })).await;
    assert_eq!(used.meta["results"], 3, "{}", used.content);
    let hover = ask(json!({ "operation": "hover", "path": "counter.c", "line": 12, "symbol": "helper" })).await;
    assert!(hover.content.contains("int helper(struct counter *c)") && hover.content.contains("Adds one."), "{}", hover.content);
    let outline = ask(json!({ "operation": "symbols", "path": "counter.c" })).await;
    // clangd calls a C struct a class, with `struct` as its detail.
    assert!(outline.content.contains("counter  struct  (line 1)") && outline.content.contains("  field count  int  (line 2)") && outline.content.contains("function helper"), "{}", outline.content);
    let broken = ask(json!({ "operation": "diagnostics", "path": "broken.c" })).await;
    assert!(broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error broken.c:1:"), "{}", broken.content);
    // A file changed on disk is given to the server again before it is asked.
    std::fs::write(root.join("broken.c"), "int oops(void) {\n    return 0;\n}\n").unwrap();
    let mended = ask(json!({ "operation": "diagnostics", "path": "broken.c" })).await;
    assert_eq!(mended.meta["errors"], 0, "{}", mended.content);
    hub.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stand_in_that_fails_is_not_taken_for_a_server_and_one_that_will_not_serve_says_so() {
    // rustup's stand-in for rust-analyzer, when the component is not installed, fails at `--version`: `false` does
    // the same, and is not taken for an installed server.
    static FAILING: Language = Language { key: "test", name: "Test", program: "false", version_arg: "--version", args: &[], extensions: &["test"], install: "install it somehow", runs_project_code: false, env: &[], download: None };
    // `true` answers `--version`, then exits rather than serve.
    static SILENT: Language = Language { key: "test", name: "Test", program: "true", version_arg: "--version", args: &[], extensions: &["test"], install: "install it somehow", runs_project_code: false, env: &[], download: None };
    if installed("false").is_none() || installed("true").is_none() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(locate(None, &FAILING).await, Located::Missing);
    let missing = LspHub::default().client(directory.path(), &FAILING).await.err().unwrap();
    assert!(missing.starts_with("false is not installed") && missing.contains("install it somehow"), "{missing}");
    let silent = LspHub::default().client(directory.path(), &SILENT).await.err().unwrap();
    assert!(silent.starts_with("true would not start") && silent.contains("install it somehow"), "{silent}");
}

/// A server's archive as its release would have it on this system: a zip holding the program on Windows, the
/// program gzipped elsewhere. With its name and SHA-256.
fn archive(program: &str, bytes: &[u8]) -> (String, Vec<u8>, String) {
    use sha2::Digest;
    let (name, packed) = if cfg!(windows) {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(executable(program), zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, bytes).unwrap();
        (format!("{program}.zip"), zip.finish().unwrap().into_inner())
    } else {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        (format!("{program}.gz"), gz.finish().unwrap())
    };
    let sha256 = sha2::Sha256::digest(&packed).iter().map(|byte| format!("{byte:02x}")).collect();
    (name, packed, sha256)
}

/// A language whose server is downloaded from `archive`, pinned to `sha256`, for this machine.
fn downloadable(program: &'static str, file: String, sha256: String) -> &'static Language {
    downloadable_in_place(program, program, file, sha256)
}

/// A language whose server on the `PATH` is `program`, and in whose place `downloaded` is downloaded.
fn downloadable_in_place(program: &'static str, downloaded: &'static str, file: String, sha256: String) -> &'static Language {
    let target = crate::runtime::target().expect("a target with builds");
    let builds: &'static [(&'static str, &'static str, &'static str, u64)] = Box::leak(vec![(target, &*Box::leak(file.into_boxed_str()), &*Box::leak(sha256.into_boxed_str()), 1_000)].into_boxed_slice());
    let download: &'static Download = Box::leak(Box::new(Download { name: "Fake", program: downloaded, args: &["--serve"], version: "1.0", address: "", builds }));
    Box::leak(Box::new(Language { key: "fake", name: "Fake", program, version_arg: "--version", args: &[], extensions: &["fake"], install: "install it somehow", runs_project_code: false, env: &[], download: Some(download) }))
}

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn every_download_is_pinned_for_each_system_the_app_runs_on() {
    let targets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];
    let downloads: Vec<&Download> = LANGUAGES.iter().filter_map(|language| language.download).collect();
    assert_eq!(downloads.iter().map(|download| download.program).collect::<Vec<_>>(), ["rust-analyzer", "tsc", "ty"]);
    for download in downloads {
        assert_eq!(download.builds.iter().map(|(target, ..)| *target).collect::<Vec<_>>(), targets, "{}", download.name);
        for (target, file, sha256, bytes) in download.builds {
            assert!(sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{target} {file}");
            assert!((1_000_000..40_000_000).contains(bytes), "{target} {file}");
            assert!([".gz", ".tgz", ".tar.gz", ".zip"].iter().any(|end| file.ends_with(end)), "{file}");
            assert!(download.address.starts_with("https://"));
        }
    }
    // TypeScript and Python are served by programs of their own when downloaded, started as servers.
    assert_eq!((TYPESCRIPT.program, TYPESCRIPT.args), ("tsc", &["--lsp", "--stdio"][..]));
    assert_eq!((TY.program, TY.args), ("ty", &["server"][..]));
    assert_eq!(settings("tsc")["runExternalCode"], false);
}

#[tokio::test]
async fn keeps_the_program_with_its_folder_from_a_tarball_and_nothing_outside_it() {
    if crate::runtime::target().is_none() {
        return;
    }
    // As an npm package has it: the program in `package/lib`, beside the files it reads; other files above.
    let program = executable("odg-fake-tsc");
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
    let mut add = |path: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        tar.append(&header, bytes).unwrap();
    };
    add("package/README.md", b"readme");
    add(&format!("package/lib/{program}"), b"the program");
    add("package/lib/lib.d.ts", b"declare var x: number;");
    add("package/lib/sub/more.d.ts", b"more");
    add("package/../../escaped", b"out");
    let packed = tar.into_inner().unwrap().finish().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("lsp");
    let language = downloadable_in_place("odg-fake-tsls", "odg-fake-tsc", "pkg/-/odg-fake-tsc-1.0.tgz".to_string(), sha256_of(&packed));
    let (address, requests) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
    let got = install_from(&root, language, &address, &CancellationToken::new()).await.unwrap();
    assert!(requests.await.unwrap()[0].starts_with("GET /pkg/-/odg-fake-tsc-1.0.tgz "));
    let into = root.join("odg-fake-tsc/1.0");
    assert_eq!(got, into.join(&program));
    assert_eq!(std::fs::read(&got).unwrap(), b"the program");
    assert_eq!(std::fs::read(into.join("lib.d.ts")).unwrap(), b"declare var x: number;");
    assert_eq!(std::fs::read(into.join("sub/more.d.ts")).unwrap(), b"more");
    assert!(!into.join("README.md").exists() && !directory.path().join("escaped").exists() && !root.join("escaped").exists());
    assert!(!root.join("odg-fake-tsc/odg-fake-tsc-1.0.tgz.part").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&got).unwrap().permissions().mode() & 0o777, 0o755);
    }
    // Found by the program downloaded, though the language's own program is another; and taken away by it.
    assert_eq!(locate(Some(&root), language).await, Located::Downloaded(got));
    assert_eq!(downloaded(&root, language_by_key("rust").unwrap()), None);
    remove(&root, language).unwrap();
    assert!(!root.join("odg-fake-tsc").exists());
}

/// Downloads the real TypeScript and ty, as the app does, and asks each about a small project. It fetches about
/// 25 MB and runs what it fetched, so it runs only when `ODG_TEST_DOWNLOADS` is set.
#[tokio::test(flavor = "multi_thread")]
async fn navigates_with_the_servers_it_downloads() {
    if std::env::var_os("ODG_TEST_DOWNLOADS").is_none() || crate::runtime::target().is_none() {
        return;
    }
    let downloads = tempfile::tempdir().unwrap();
    let hub = LspHub::new(downloads.path().to_path_buf());
    let cancel = CancellationToken::new();
    let projects = [
        ("typescript", "a.ts", "export function greet(name: string): string {\n    return name;\n}\n", "b.ts", "import { greet } from \"./a\";\ngreet(\"x\");\nconst n: number = \"oops\";\n"),
        ("python", "a.py", "def greet(name: str) -> str:\n    return name\n", "b.py", "from a import greet\ngreet(\"x\")\nn: int = \"oops\"\n"),
    ];
    for (key, defined_in, defining, used_in, using) in projects {
        let language = language_by_key(key).unwrap();
        if matches!(locate(None, language).await, Located::System(_)) {
            continue;
        }
        let installed = server_tool(&hub, &json!({ "language": key, "install": true }), &cancel).await.unwrap();
        assert_eq!(installed.meta["state"], "downloaded", "{}", installed.content);
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::write(root.join(defined_in), defining).unwrap();
        std::fs::write(root.join(used_in), using).unwrap();
        if key == "typescript" {
            std::fs::write(root.join("tsconfig.json"), r#"{ "compilerOptions": { "strict": true } }"#).unwrap();
        }
        let defined = navigate(&hub, root, &json!({ "operation": "definition", "path": used_in, "line": 2, "symbol": "greet" }), &cancel).await.unwrap();
        assert!(defined.content.contains(&format!("{defined_in}:1:")), "{key}: {}", defined.content);
        let hover = navigate(&hub, root, &json!({ "operation": "hover", "path": used_in, "line": 2, "symbol": "greet" }), &cancel).await.unwrap();
        assert!(hover.content.contains("str"), "{key}: {}", hover.content);
        let outline = navigate(&hub, root, &json!({ "operation": "symbols", "path": defined_in }), &cancel).await.unwrap();
        assert!(outline.content.contains("greet"), "{key}: {}", outline.content);
        let found = navigate(&hub, root, &json!({ "operation": "diagnostics", "path": used_in }), &cancel).await.unwrap();
        assert!(found.meta["errors"].as_u64().unwrap_or(0) >= 1, "{key}: {}", found.content);
        let referenced = navigate(&hub, root, &json!({ "operation": "references", "path": defined_in, "line": 1, "symbol": "greet" }), &cancel).await.unwrap();
        assert!(referenced.content.contains(used_in), "{key}: {}", referenced.content);
    }
    hub.shutdown();
}

#[test]
fn keeps_the_program_from_a_zip_and_nothing_that_would_climb_out() {
    let directory = tempfile::tempdir().unwrap();
    let program = executable("odg-zipped");
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [(program.as_str(), &b"zipped"[..]), ("../outside", b"out"), ("notes.txt", b"notes")] {
        zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, bytes).unwrap();
    }
    let archive = directory.path().join("odg-zipped.zip.part");
    std::fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
    let into = directory.path().join("into");
    let got = unpack_program(&archive, &into, "odg-zipped").unwrap();
    assert_eq!(std::fs::read(&got).unwrap(), b"zipped");
    assert_eq!(std::fs::read(into.join("notes.txt")).unwrap(), b"notes");
    assert!(!directory.path().join("outside").exists());
    // An archive without the program is refused.
    let wrong = unpack_program(&archive, &into, "odg-elsewhere").unwrap_err();
    assert!(wrong.contains("is not in the archive"), "{wrong}");
}

fn ok(body: &[u8]) -> Vec<u8> {
    [format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).into_bytes(), body.to_vec()].concat()
}

#[tokio::test]
async fn downloads_a_server_checked_against_its_checksum_and_keeps_only_that_version() {
    if crate::runtime::target().is_none() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (file, packed, sha256) = archive("odg-fake-server", b"#!/bin/sh\necho fake\n");
    let language = downloadable("odg-fake-server", file.clone(), sha256);
    // A version from before, to be taken away.
    std::fs::create_dir_all(root.join("odg-fake-server/0.9")).unwrap();
    let (address, requests) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
    let program = install_from(root, language, &address, &CancellationToken::new()).await.unwrap();
    assert_eq!(program, root.join("odg-fake-server/1.0").join(executable("odg-fake-server")));
    assert_eq!(std::fs::read(&program).unwrap(), b"#!/bin/sh\necho fake\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&program).unwrap().permissions().mode() & 0o777, 0o755);
    }
    assert!(requests.await.unwrap()[0].starts_with(&format!("GET /{file} ")));
    assert!(!root.join("odg-fake-server/0.9").exists() && !root.join(format!("odg-fake-server/{file}.part")).exists());
    // Not on the PATH, it is found where it was downloaded; and taken away, it is nowhere.
    assert_eq!(locate(Some(root), language).await, Located::Downloaded(program));
    remove(root, language).unwrap();
    assert_eq!(locate(Some(root), language).await, Located::Missing);
}

#[tokio::test]
async fn refuses_a_download_whose_checksum_does_not_match_and_leaves_nothing() {
    if crate::runtime::target().is_none() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (file, packed, _) = archive("odg-other-server", b"anything");
    let language = downloadable("odg-other-server", file, "0".repeat(64));
    let (address, _) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
    let error = install_from(root, language, &address, &CancellationToken::new()).await.unwrap_err();
    assert!(error.contains("校验不符"), "{error}");
    assert!(!root.join("odg-other-server/1.0").exists());
    assert_eq!(std::fs::read_dir(root.join("odg-other-server")).unwrap().count(), 0);
}

#[tokio::test]
async fn says_how_a_missing_server_can_be_had() {
    let rust = language_by_key("rust").unwrap();
    // Downloadable, with this machine's size, unless rust-analyzer is installed here.
    if !matches!(locate(None, rust).await, Located::System(_)) && RUST_ANALYZER.megabytes().is_some() {
        let directory = tempfile::tempdir().unwrap();
        let told = server_tool(&LspHub::new(directory.path().to_path_buf()), &json!({ "language": "rust" }), &CancellationToken::new()).await.unwrap();
        assert!(told.content.contains("call `language_server` with `language` \"rust\" and `install` true") && told.content.contains("about 1"), "{}", told.content);
        assert_eq!(told.meta["state"], "missing");
    }
    // Python: ty can be downloaded in pyright's place, or the user installs pyright.
    let python = missing(language_by_key("python").unwrap());
    assert!(python.contains("`language` \"python\" and `install` true (ty 0.0.107") && python.contains("npm install -g pyright"), "{python}");
    let typescript = missing(language_by_key("typescript").unwrap());
    assert!(typescript.contains("(TypeScript 7.0.2"), "{typescript}");
    // One the app cannot download: how the user installs it.
    assert!(missing(language_by_key("go").unwrap()).contains("go install golang.org/x/tools/gopls@latest"));
    let wrong = server_tool(&LspHub::default(), &json!({ "language": "cobol" }), &CancellationToken::new()).await.unwrap_err();
    assert!(wrong.contains("`language` must be one of rust, c, typescript, python, go"), "{wrong}");
}

#[cfg(unix)]
#[tokio::test]
async fn checks_gopls_with_version_subcommand_instead_of_flag() {
    use std::os::unix::fs::PermissionsExt;
    let go = language_by_key("go").unwrap();
    assert_eq!(go.version_arg, "version");
    let directory = tempfile::tempdir().unwrap();
    let fake_gopls = directory.path().join("gopls");
    std::fs::write(&fake_gopls, "#!/bin/sh\nif [ \"$1\" = \"version\" ]; then echo 'gopls v0.24.0'; exit 0; fi\nexit 2\n").unwrap();
    std::fs::set_permissions(&fake_gopls, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(servers::works(&fake_gopls, go.version_arg).await);
    assert!(!servers::works(&fake_gopls, "--version").await);
}
