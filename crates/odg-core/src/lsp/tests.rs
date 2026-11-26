use tokio::io::AsyncRead;

use super::*;

#[test]
fn turns_paths_into_uris_and_back() {
    let path = Path::new("/work/my project/src/日志.rs");
    let document = uri(path);
    assert_eq!(document, "file:///work/my%20project/src/%E6%97%A5%E5%BF%97.rs");
    assert_eq!(path_of(&document).unwrap(), path);
    assert_eq!(path_of("untitled:Untitled-1"), None);
    assert_eq!(
        language_for(Path::new("a/B.TSX")).map(|language| language.program),
        Some("typescript-language-server")
    );
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
    assert_eq!(
        path_text("file:///C:/Users/me/a%20b/x.c", true).unwrap(),
        r"C:\Users\me\a b\x.c"
    );
    assert_eq!(
        path_text("file:///c%3A/Users/me/x.c", true).unwrap(),
        r"C:\Users\me\x.c"
    );
    assert_eq!(
        path_text("file://server/share/x.c", true).unwrap(),
        r"\\server\share\x.c"
    );
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
    assert_eq!(
        position(text, &json!({ "line": 2, "symbol": "id" }), "a.rs"),
        Ok((1, 14))
    );
    assert_eq!(
        position(text, &json!({ "line": 2, "symbol": "width" }), "a.rs"),
        Ok((1, 8))
    );
    assert_eq!(position(text, &json!({ "line": 1, "column": 4 }), "a.rs"), Ok((0, 3)));
    assert!(
        position(text, &json!({ "line": 2, "symbol": "height" }), "a.rs")
            .unwrap_err()
            .contains("which reads: let w = width(id);")
    );
    assert!(
        position(text, &json!({ "line": 9, "symbol": "w" }), "a.rs")
            .unwrap_err()
            .contains("has no line 9")
    );
    assert!(
        position(text, &json!({ "symbol": "w" }), "a.rs")
            .unwrap_err()
            .contains("`line` is required")
    );
}

#[test]
fn reads_every_shape_an_answer_comes_in() {
    let link = json!({ "targetUri": "file:///w/a.rs", "targetRange": {}, "targetSelectionRange": { "start": { "line": 3, "character": 1 } } });
    let location = json!({ "uri": "file:///w/b.rs", "range": { "start": { "line": 0, "character": 0 } } });
    assert_eq!(locations(&json!([link, location])).len(), 2);
    assert_eq!(locations(&location)[0].0, "file:///w/b.rs");
    assert!(locations(&Value::Null).is_empty());
    assert_eq!(
        hover_text(&json!({ "kind": "markdown", "value": "```rust\nfn f()\n```" })),
        "```rust\nfn f()\n```"
    );
    assert_eq!(
        hover_text(&json!([{ "language": "rust", "value": "fn f()" }, "docs"])),
        "```rust\nfn f()\n```\n\ndocs"
    );
    let mut lines = Vec::new();
    outline(
        &[
            json!({ "name": "S", "kind": 23, "selectionRange": { "start": { "line": 0 } }, "children": [{ "name": "f", "kind": 6, "detail": "fn()", "selectionRange": { "start": { "line": 1 } } }] }),
        ],
        0,
        &mut lines,
    );
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
                Some("initialize") => send(
                    &writer,
                    &reply(json!({ "capabilities": { "definitionProvider": true } })),
                )
                .await
                .unwrap(),
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
    Client::start(
        "rust-analyzer",
        root,
        Box::new(client_read),
        Box::new(client_write),
        None,
    )
    .await
    .unwrap()
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
    let defined = navigate(
        &hub,
        root,
        &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(defined.content, "Defined at (1):\na.rs:1:4  fn f() {}");
    assert_eq!(
        (defined.meta["results"].as_u64(), defined.meta["line"].as_u64()),
        (Some(1), Some(2))
    );
    assert!(!client.busy());
    let found = navigate(
        &hub,
        root,
        &json!({ "operation": "diagnostics", "path": "a.rs" }),
        &cancel,
    )
    .await
    .unwrap();
    assert!(
        found
            .content
            .starts_with("1 problems in a.rs:\nerror a.rs:2:5 [E0425]: cannot find value `y`\n    fn g() { y; f(); }"),
        "{}",
        found.content
    );
    assert_eq!(found.meta["errors"], 1);
    // What it asked for its settings was answered with them: no build scripts, no proc macros, no `cargo check`.
    assert_eq!(configured.lock().unwrap()[0]["procMacro"]["enable"], false);
    assert_eq!(configured.lock().unwrap()[0]["checkOnSave"], false);
    let refused = navigate(&hub, root, &json!({ "operation": "rename", "path": "a.rs" }), &cancel)
        .await
        .unwrap_err();
    assert!(refused.contains("`operation` must be one of"), "{refused}");
    let other = navigate(
        &hub,
        root,
        &json!({ "operation": "symbols", "path": "notes.md" }),
        &cancel,
    )
    .await
    .unwrap_err();
    assert!(other.contains("is not of a language navigated here"), "{other}");
}

/// Whether a language server can be run here: on the `PATH`, and answering (rustup's stand-in for a component
/// not installed is on the `PATH` but fails).
fn runs(program: &str) -> bool {
    installed(program).is_some()
        && std::process::Command::new(program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
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
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .unwrap();
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
    assert!(
        defined
            .content
            .starts_with("Defined at (1):\nsrc/lib.rs:8:8  pub fn helper(counter: &Counter) -> u32 {"),
        "{}",
        defined.content
    );
    let used = ask(json!({ "operation": "references", "path": "src/lib.rs", "line": 8, "symbol": "helper" })).await;
    assert_eq!(used.meta["results"], 3, "{}", used.content);
    let hover = ask(json!({ "operation": "hover", "path": "src/lib.rs", "line": 14, "symbol": "helper" })).await;
    assert!(
        hover.content.contains("fn helper(counter: &Counter) -> u32") && hover.content.contains("Adds one."),
        "{}",
        hover.content
    );
    let outline = ask(json!({ "operation": "symbols", "path": "src/lib.rs" })).await;
    assert!(
        outline.content.contains("struct Counter  (line 3)") && outline.content.contains("function helper"),
        "{}",
        outline.content
    );
    let found = ask(json!({ "operation": "find_symbol", "path": "src/lib.rs", "query": "Counter" })).await;
    assert!(
        found.content.contains("struct Counter") && found.content.contains("src/lib.rs:3:"),
        "{}",
        found.content
    );
    let broken = ask(json!({ "operation": "diagnostics", "path": "src/broken.rs" })).await;
    assert!(
        broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error src/broken.rs:1:"),
        "{}",
        broken.content
    );
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
    assert!(
        defined
            .content
            .starts_with("Defined at (1):\ncounter.c:6:5  int helper(struct counter *c) {"),
        "{}",
        defined.content
    );
    let used = ask(json!({ "operation": "references", "path": "counter.c", "line": 6, "symbol": "helper" })).await;
    assert_eq!(used.meta["results"], 3, "{}", used.content);
    let hover = ask(json!({ "operation": "hover", "path": "counter.c", "line": 12, "symbol": "helper" })).await;
    assert!(
        hover.content.contains("int helper(struct counter *c)") && hover.content.contains("Adds one."),
        "{}",
        hover.content
    );
    let outline = ask(json!({ "operation": "symbols", "path": "counter.c" })).await;
    // clangd calls a C struct a class, with `struct` as its detail.
    assert!(
        outline.content.contains("counter  struct  (line 1)")
            && outline.content.contains("  field count  int  (line 2)")
            && outline.content.contains("function helper"),
        "{}",
        outline.content
    );
    let broken = ask(json!({ "operation": "diagnostics", "path": "broken.c" })).await;
    assert!(
        broken.meta["errors"].as_u64().unwrap() >= 1 && broken.content.contains("error broken.c:1:"),
        "{}",
        broken.content
    );
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
    static FAILING: Language = Language {
        key: "test",
        name: "Test",
        program: "false",
        version_arg: "--version",
        args: &[],
        extensions: &["test"],
        install: "install it somehow",
        runs_project_code: false,
        env: &[],
        download: None,
    };
    // `true` answers `--version`, then exits rather than serve.
    static SILENT: Language = Language {
        key: "test",
        name: "Test",
        program: "true",
        version_arg: "--version",
        args: &[],
        extensions: &["test"],
        install: "install it somehow",
        runs_project_code: false,
        env: &[],
        download: None,
    };
    if installed("false").is_none() || installed("true").is_none() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(locate(None, &FAILING).await, Located::Missing);
    let missing = LspHub::default()
        .client(directory.path(), &FAILING)
        .await
        .err()
        .unwrap();
    assert!(
        missing.starts_with("false is not installed") && missing.contains("install it somehow"),
        "{missing}"
    );
    let silent = LspHub::default().client(directory.path(), &SILENT).await.err().unwrap();
    assert!(
        silent.starts_with("true would not start") && silent.contains("install it somehow"),
        "{silent}"
    );
}

/// A server's archive as its release would have it on this system: a zip holding the program on Windows, the
/// program gzipped elsewhere. With its name and SHA-256.
fn archive(program: &str, bytes: &[u8]) -> (String, Vec<u8>, String) {
    use sha2::Digest;
    let (name, packed) = if cfg!(windows) {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(executable(program), zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, bytes).unwrap();
        (format!("{program}.zip"), zip.finish().unwrap().into_inner())
    } else {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, bytes).unwrap();
        (format!("{program}.gz"), gz.finish().unwrap())
    };
    let sha256 = sha2::Sha256::digest(&packed)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (name, packed, sha256)
}

/// A language whose server is downloaded from `archive`, pinned to `sha256`, for this machine.
fn downloadable(program: &'static str, file: String, sha256: String) -> &'static Language {
    downloadable_in_place(program, program, file, sha256)
}

/// A language whose server on the `PATH` is `program`, and in whose place `downloaded` is downloaded.
fn downloadable_in_place(
    program: &'static str,
    downloaded: &'static str,
    file: String,
    sha256: String,
) -> &'static Language {
    let target = crate::runtime::target().expect("a target with builds");
    let builds: &'static [(&'static str, &'static str, &'static str, u64)] = Box::leak(
        vec![(
            target,
            &*Box::leak(file.into_boxed_str()),
            &*Box::leak(sha256.into_boxed_str()),
            1_000,
        )]
        .into_boxed_slice(),
    );
    let download: &'static Download = Box::leak(Box::new(Download {
        name: "Fake",
        program: downloaded,
        args: &["--serve"],
        version: "1.0",
        address: "",
        builds,
    }));
    Box::leak(Box::new(Language {
        key: "fake",
        name: "Fake",
        program,
        version_arg: "--version",
        args: &[],
        extensions: &["fake"],
        install: "install it somehow",
        runs_project_code: false,
        env: &[],
        download: Some(download),
    }))
}

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn every_download_is_pinned_for_each_system_the_app_runs_on() {
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ];
    let downloads: Vec<&Download> = LANGUAGES.iter().filter_map(|language| language.download).collect();
    assert_eq!(
        downloads.iter().map(|download| download.program).collect::<Vec<_>>(),
        ["rust-analyzer", "tsc", "ty"]
    );
    for download in downloads {
        assert_eq!(
            download.builds.iter().map(|(target, ..)| *target).collect::<Vec<_>>(),
            targets,
            "{}",
            download.name
        );
        for (target, file, sha256, bytes) in download.builds {
            assert!(
                sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{target} {file}"
            );
            assert!((1_000_000..40_000_000).contains(bytes), "{target} {file}");
            assert!(
                [".gz", ".tgz", ".tar.gz", ".zip"].iter().any(|end| file.ends_with(end)),
                "{file}"
            );
            assert!(download.address.starts_with("https://"));
        }
    }
    // TypeScript and Python are served by programs of their own when downloaded, started as servers.
    assert_eq!(
        (TYPESCRIPT.program, TYPESCRIPT.args),
        ("tsc", &["--lsp", "--stdio"][..])
    );
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
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
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
    let language = downloadable_in_place(
        "odg-fake-tsls",
        "odg-fake-tsc",
        "pkg/-/odg-fake-tsc-1.0.tgz".to_string(),
        sha256_of(&packed),
    );
    let (address, requests) = crate::testing::serve_bytes(vec![ok(&packed)]).await;
    let got = install_from(&root, language, &address, &CancellationToken::new())
        .await
        .unwrap();
    assert!(requests.await.unwrap()[0].starts_with("GET /pkg/-/odg-fake-tsc-1.0.tgz "));
    let into = root.join("odg-fake-tsc/1.0");
    assert_eq!(got, into.join(&program));
    assert_eq!(std::fs::read(&got).unwrap(), b"the program");
    assert_eq!(std::fs::read(into.join("lib.d.ts")).unwrap(), b"declare var x: number;");
    assert_eq!(std::fs::read(into.join("sub/more.d.ts")).unwrap(), b"more");
    assert!(
        !into.join("README.md").exists()
            && !directory.path().join("escaped").exists()
            && !root.join("escaped").exists()
    );
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
        (
            "typescript",
            "a.ts",
            "export function greet(name: string): string {\n    return name;\n}\n",
            "b.ts",
            "import { greet } from \"./a\";\ngreet(\"x\");\nconst n: number = \"oops\";\n",
        ),
        (
            "python",
            "a.py",
            "def greet(name: str) -> str:\n    return name\n",
            "b.py",
            "from a import greet\ngreet(\"x\")\nn: int = \"oops\"\n",
        ),
    ];
    for (key, defined_in, defining, used_in, using) in projects {
        let language = language_by_key(key).unwrap();
        if matches!(locate(None, language).await, Located::System(_)) {
            continue;
        }
        let installed = server_tool(&hub, &json!({ "language": key, "install": true }), &cancel)
            .await
            .unwrap();
        assert_eq!(installed.meta["state"], "downloaded", "{}", installed.content);
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::write(root.join(defined_in), defining).unwrap();
        std::fs::write(root.join(used_in), using).unwrap();
        if key == "typescript" {
            std::fs::write(
                root.join("tsconfig.json"),
                r#"{ "compilerOptions": { "strict": true } }"#,
            )
            .unwrap();
        }
        let defined = navigate(
            &hub,
            root,
            &json!({ "operation": "definition", "path": used_in, "line": 2, "symbol": "greet" }),
            &cancel,
        )
        .await
        .unwrap();
        assert!(
            defined.content.contains(&format!("{defined_in}:1:")),
            "{key}: {}",
            defined.content
        );
        let hover = navigate(
            &hub,
            root,
            &json!({ "operation": "hover", "path": used_in, "line": 2, "symbol": "greet" }),
            &cancel,
        )
        .await
        .unwrap();
        assert!(hover.content.contains("str"), "{key}: {}", hover.content);
        let outline = navigate(
            &hub,
            root,
            &json!({ "operation": "symbols", "path": defined_in }),
            &cancel,
        )
        .await
        .unwrap();
        assert!(outline.content.contains("greet"), "{key}: {}", outline.content);
        let found = navigate(
            &hub,
            root,
            &json!({ "operation": "diagnostics", "path": used_in }),
            &cancel,
        )
        .await
        .unwrap();
        assert!(
            found.meta["errors"].as_u64().unwrap_or(0) >= 1,
            "{key}: {}",
            found.content
        );
        let referenced = navigate(
            &hub,
            root,
            &json!({ "operation": "references", "path": defined_in, "line": 1, "symbol": "greet" }),
            &cancel,
        )
        .await
        .unwrap();
        assert!(referenced.content.contains(used_in), "{key}: {}", referenced.content);
    }
    hub.shutdown();
}

#[test]
fn keeps_the_program_from_a_zip_and_nothing_that_would_climb_out() {
    let directory = tempfile::tempdir().unwrap();
    let program = executable("odg-zipped");
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [
        (program.as_str(), &b"zipped"[..]),
        ("../outside", b"out"),
        ("notes.txt", b"notes"),
    ] {
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
    [
        format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes(),
        body.to_vec(),
    ]
    .concat()
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
    let program = install_from(root, language, &address, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        program,
        root.join("odg-fake-server/1.0").join(executable("odg-fake-server"))
    );
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
    let error = install_from(root, language, &address, &CancellationToken::new())
        .await
        .unwrap_err();
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
        let told = server_tool(
            &LspHub::new(directory.path().to_path_buf()),
            &json!({ "language": "rust" }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            told.content
                .contains("call `language_server` with `language` \"rust\" and `install` true")
                && told.content.contains("about 1"),
            "{}",
            told.content
        );
        assert_eq!(told.meta["state"], "missing");
    }
    // Python: ty can be downloaded in pyright's place, or the user installs pyright.
    let python = missing(language_by_key("python").unwrap());
    assert!(
        python.contains("`language` \"python\" and `install` true (ty 0.0.107")
            && python.contains("npm install -g pyright"),
        "{python}"
    );
    let typescript = missing(language_by_key("typescript").unwrap());
    assert!(typescript.contains("(TypeScript 7.0.2"), "{typescript}");
    // One the app cannot download: how the user installs it.
    assert!(missing(language_by_key("go").unwrap()).contains("go install golang.org/x/tools/gopls@latest"));
    let wrong = server_tool(
        &LspHub::default(),
        &json!({ "language": "cobol" }),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        wrong.contains("`language` must be one of rust, c, typescript, python, go"),
        "{wrong}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn checks_gopls_with_version_subcommand_instead_of_flag() {
    use std::os::unix::fs::PermissionsExt;
    let go = language_by_key("go").unwrap();
    assert_eq!(go.version_arg, "version");
    let directory = tempfile::tempdir().unwrap();
    let fake_gopls = directory.path().join("gopls");
    std::fs::write(
        &fake_gopls,
        "#!/bin/sh\nif [ \"$1\" = \"version\" ]; then echo 'gopls v0.24.0'; exit 0; fi\nexit 2\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_gopls, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(servers::works(&fake_gopls, go.version_arg).await);
    assert!(!servers::works(&fake_gopls, "--version").await);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn singleflight_slots_do_not_block_other_languages_and_shutdown_kills_inflight_server() {
    use std::os::unix::fs::PermissionsExt;

    if crate::runtime::target().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lsp-root");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    // Language A: spawns a grandchild `sleep 60`, records both PIDs, and hangs during `initialize`.
    let pid_file = dir.path().join("slow-pids.txt");
    let slow_script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'slow 1.0'; exit 0; fi\nsleep 60 &\ngpid=$!\nprintf '%s %s\\n' \"$$\" \"$gpid\" > \"{}\"\nwait\n",
        pid_file.display()
    );
    let slow_dir = root.join("odg-slow-lsp/1.0");
    std::fs::create_dir_all(&slow_dir).unwrap();
    let slow_bin = slow_dir.join(executable("odg-slow-lsp"));
    std::fs::write(&slow_bin, slow_script).unwrap();
    std::fs::set_permissions(&slow_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Language B: downloaded binary does not exist -> immediately returns `missing` without blocking on Language A!
    static SLOW_DOWNLOAD: Download = Download {
        name: "Slow",
        program: "odg-slow-lsp",
        args: &["--serve"],
        version: "1.0",
        address: "https://example.invalid",
        builds: &[
            (
                "aarch64-apple-darwin",
                "slow.gz",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
            (
                "x86_64-apple-darwin",
                "slow.gz",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
            (
                "x86_64-unknown-linux-gnu",
                "slow.gz",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
            (
                "aarch64-unknown-linux-gnu",
                "slow.gz",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
            (
                "x86_64-pc-windows-msvc",
                "slow.zip",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
            (
                "aarch64-pc-windows-msvc",
                "slow.zip",
                "0000000000000000000000000000000000000000000000000000000000000000",
                1_000_000,
            ),
        ],
    };
    static SLOW_LANG: Language = Language {
        key: "slow_a",
        name: "SlowA",
        program: "odg-nonexistent-slow-lsp",
        version_arg: "--version",
        args: &[],
        extensions: &["slowa"],
        install: "install slow",
        runs_project_code: false,
        env: &[],
        download: Some(&SLOW_DOWNLOAD),
    };
    static FAST_LANG: Language = Language {
        key: "fast_b",
        name: "FastB",
        program: "odg-nonexistent-fast-lsp",
        version_arg: "--version",
        args: &[],
        extensions: &["fastb"],
        install: "install fast",
        runs_project_code: false,
        env: &[],
        download: None,
    };

    let hub = Arc::new(LspHub::new(root));
    // Also insert a live scripted client under ` FAST_LANG ` in another workspace to verify `shutdown_language`.
    let other_ws = dir.path().join("ws2");
    std::fs::create_dir_all(&other_ws).unwrap();
    let live_b = scripted(&other_ws, Arc::new(Mutex::new(Value::Null))).await;
    hub.insert(&other_ws, &FAST_LANG, live_b.clone()).await;

    let hub_a = hub.clone();
    let ws_a = workspace.clone();
    let slow_task = tokio::spawn(async move { hub_a.client(&ws_a, &SLOW_LANG).await });

    // Wait until Language A has spawned its process group and is hanging inside `initialize`.
    for _ in 0..100 {
        if pid_file.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let raw_pids = std::fs::read_to_string(&pid_file).expect("slow LSP should have started and written PIDs");
    let pids: Vec<u32> = raw_pids
        .split_whitespace()
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    assert_eq!(pids.len(), 2, "expected child and grandchild PIDs: {raw_pids}");

    // Language B in the same workspace is NOT blocked by Language A's hanging `initialize`!
    let t0 = tokio::time::Instant::now();
    let fast_res = hub.client(&workspace, &FAST_LANG).await;
    assert!(fast_res.is_err());
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "Language B blocked for {:?}",
        t0.elapsed()
    );

    // Targeted `shutdown_language("fast_b")` closes `live_b` without disturbing `slow_a`.
    assert!(!live_b.closed());
    hub.shutdown_language("fast_b").await;
    assert!(live_b.closed());
    assert!(!slow_task.is_finished());

    // `hub.shutdown()` never fails with `try_lock` and immediately kills Language A's entire process group!
    let shutdown_t0 = tokio::time::Instant::now();
    hub.shutdown();
    let slow_err = tokio::time::timeout(Duration::from_secs(2), slow_task)
        .await
        .expect("slow_task must abort promptly after shutdown")
        .unwrap()
        .err()
        .expect("expected startup error");
    assert!(
        shutdown_t0.elapsed() < Duration::from_secs(2),
        "shutdown took {:?}",
        shutdown_t0.elapsed()
    );
    assert!(!slow_err.is_empty());

    for pid in pids {
        for _ in 0..30 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let still_alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            !still_alive,
            "process {pid} should have been killed by LspHub::shutdown"
        );
    }
}

#[tokio::test]
async fn cancelled_first_navigate_preserves_first_settle_budget_and_aborts_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.rs"), "fn f() {}\nfn g() { f(); }\n").unwrap();

    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let writer: Writer = Arc::new(tokio::sync::Mutex::new(Box::new(server_write)));
    let hang_rpc = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hang_rpc_server = hang_rpc.clone();

    tokio::spawn(async move {
        let mut reader = BufReader::new(Box::new(server_read) as Box<dyn AsyncRead + Send + Unpin>);
        while let Some(message) = receive(&mut reader).await {
            let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
            match message["method"].as_str() {
                Some("initialize") => {
                    send(
                        &writer,
                        &reply(json!({ "capabilities": { "definitionProvider": true } })),
                    )
                    .await
                    .unwrap();
                }
                Some("initialized") => {
                    send(&writer, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "idx", "value": { "kind": "begin" } } })).await.unwrap();
                    let writer_bg = writer.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let _ = send(&writer_bg, &json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "idx", "value": { "kind": "end" } } })).await;
                        let _ = send(&writer_bg, &json!({ "jsonrpc": "2.0", "method": "experimental/serverStatus", "params": { "quiescent": true } })).await;
                    });
                }
                Some("textDocument/definition") => {
                    if hang_rpc_server.load(Ordering::Acquire) {
                        // Do not answer: exercise `client.request(..., cancel)` immediate cancellation.
                        continue;
                    }
                    let doc = message["params"]["textDocument"]["uri"].clone();
                    send(&writer, &reply(json!([{ "uri": doc, "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 4 } } }]))).await.unwrap();
                }
                _ => {}
            }
        }
    });

    let client = Client::start(
        "rust-analyzer",
        root,
        Box::new(client_read),
        Box::new(client_write),
        None,
    )
    .await
    .unwrap();
    let hub = LspHub::default();
    hub.insert(root, &LANGUAGES[0], client.clone()).await;

    // 1) Cancel the first `navigate` after 40ms while indexing is still busy: `settled` must remain `false`!
    let cancel1 = CancellationToken::new();
    let c1_trigger = cancel1.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        c1_trigger.cancel();
    });
    let err1 = navigate(
        &hub,
        root,
        &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }),
        &cancel1,
    )
    .await
    .unwrap_err();
    assert_eq!(err1, "Cancelled.");
    assert!(
        !client.settled.load(Ordering::Acquire),
        "settled must not be set when first settle was cancelled"
    );

    // 2) Second `navigate` still gets `FIRST_SETTLE`, finishes indexing cleanly, and sets `settled = true`.
    let cancel2 = CancellationToken::new();
    let ok2 = navigate(
        &hub,
        root,
        &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }),
        &cancel2,
    )
    .await
    .unwrap();
    assert!(ok2.content.starts_with("Defined at (1):"), "{}", ok2.content);
    assert!(client.settled.load(Ordering::Acquire));

    // 3) Hang RPC and cancel mid-flight: `client.request` aborts within milliseconds instead of waiting 60s.
    hang_rpc.store(true, Ordering::Release);
    let cancel3 = CancellationToken::new();
    let c3_trigger = cancel3.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        c3_trigger.cancel();
    });
    let t_rpc = tokio::time::Instant::now();
    let err3 = navigate(
        &hub,
        root,
        &json!({ "operation": "definition", "path": "a.rs", "line": 2, "symbol": "f" }),
        &cancel3,
    )
    .await
    .unwrap_err();
    assert_eq!(err3, "Cancelled.");
    assert!(t_rpc.elapsed() < Duration::from_secs(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_large_server_requests_and_client_sync_never_deadlock_and_bound_lru_and_frames() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Deliberately tiny 1 KB duplex pipe buffer: would immediately deadlock if reader task wrote to `stdin` directly!
    let (client_side, server_side) = tokio::io::duplex(1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let writer: Writer = Arc::new(tokio::sync::Mutex::new(Box::new(server_write)));
    let did_close_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let did_close_seen = did_close_count.clone();

    tokio::spawn(async move {
        let mut reader = BufReader::new(Box::new(server_read) as Box<dyn AsyncRead + Send + Unpin>);
        while let Some(message) = receive(&mut reader).await {
            let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
            match message["method"].as_str() {
                Some("initialize") => {
                    send(
                        &writer,
                        &reply(json!({ "capabilities": { "definitionProvider": true } })),
                    )
                    .await
                    .unwrap();
                }
                Some("initialized") => {
                    let _ = send(&writer, &json!({ "jsonrpc": "2.0", "method": "experimental/serverStatus", "params": { "quiescent": true } })).await;
                }
                Some("textDocument/didOpen") => {
                    // Flood large server->client requests concurrently while client is sending `didOpen` frames!
                    let pad = "x".repeat(8 * 1024);
                    let writer_req = writer.clone();
                    tokio::spawn(async move {
                        let _ = send(
                            &writer_req,
                            &json!({
                                "jsonrpc": "2.0",
                                "id": 5000,
                                "method": "workspace/configuration",
                                "params": { "items": [{ "section": "rust-analyzer", "pad": pad }] }
                            }),
                        )
                        .await;
                    });
                }
                Some("textDocument/didClose") => {
                    did_close_seen.fetch_add(1, Ordering::Release);
                }
                Some("textDocument/definition") => {
                    let doc = message["params"]["textDocument"]["uri"].clone();
                    let _ = send(&writer, &reply(json!([{ "uri": doc, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 2 } } }]))).await;
                }
                _ => {}
            }
        }
    });

    let client = Client::start(
        "rust-analyzer",
        root,
        Box::new(client_read),
        Box::new(client_write),
        None,
    )
    .await
    .unwrap();

    // Concurrently sync 70 files (~4 KB each) > MAX_OPEN_DOCUMENTS (64), verifying zero deadlock and LRU eviction.
    let mut handles = Vec::new();
    for idx in 0..70usize {
        let path = root.join(format!("mod_{idx}.rs"));
        std::fs::write(&path, format!("pub fn item_{idx}() {{ /* {} */ }}\n", "a".repeat(4096))).unwrap();
        let c = client.clone();
        handles.push(tokio::spawn(async move { c.sync(&path).await.unwrap() }));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        for h in handles {
            h.await.unwrap();
        }
    })
    .await
    .expect("concurrent sync + server requests must not deadlock");

    assert_eq!(client.open_document_count().await, 64);
    // Roundtrip a FIFO request over `outbound_tx` so all preceding `didOpen` / `didClose` frames
    // and concurrent 8 KB `workspace/configuration` replies have drained across the 1 KB duplex pipe.
    let def = client
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri(&root.join("mod_69.rs")) },
                "position": { "line": 0, "character": 0 }
            }),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .expect("FIFO definition request after flood must succeed without deadlocking");
    assert!(def.is_array());
    assert_eq!(did_close_count.load(Ordering::Acquire), 6);

    // Oversized > 10 MB file is rejected by `Client::sync`.
    let huge_file = root.join("huge.rs");
    {
        let f = std::fs::File::create(&huge_file).unwrap();
        f.set_len(10 * 1024 * 1024 + 1).unwrap();
    }
    let sync_err = client.sync(&huge_file).await.unwrap_err();
    assert!(sync_err.contains("sync limit"), "{sync_err}");

    // Oversized > 64 MB `Content-Length` frame is rejected without allocating the body.
    let bomb = b"Content-Length: 99999999\r\n\r\n{}";
    let mut bomb_reader = BufReader::new(Box::new(&bomb[..]) as Box<dyn AsyncRead + Send + Unpin>);
    let frame_err = receive_checked(&mut bomb_reader).await.unwrap_err();
    assert!(frame_err.contains("exceeds"), "{frame_err}");
}
