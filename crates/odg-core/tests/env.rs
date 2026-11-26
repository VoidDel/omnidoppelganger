//! The `PATH` an app started from a `.app` is given, and what the app does about it. A process of its own, since it
//! changes its own environment.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn adds_the_folders_programs_are_installed_in_before_the_ones_it_was_given_and_only_once() {
    let _guard = ENV_LOCK.lock().unwrap();
    // As macOS starts an app from the Dock or Finder.
    let given = "/usr/bin:/bin:/usr/sbin:/sbin";
    // SAFETY: this is a test binary of its own, and nothing in it reads the environment while the test runs.
    unsafe { std::env::set_var("PATH", given) };
    odg_core::env::adopt_shell_path();
    let folders: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();

    // What it was given is kept, in its own order, after what was added.
    let given: Vec<PathBuf> = std::env::split_paths(given).collect();
    assert_eq!(folders[folders.len() - given.len()..], given[..], "{folders:?}");
    let place = |path: &str| folders.iter().position(|folder| folder == Path::new(path));
    // Homebrew's folder is now there, before the system's own, where this machine has it.
    if Path::new("/opt/homebrew/bin").is_dir() {
        let (homebrew, usr) = (
            place("/opt/homebrew/bin").expect("Homebrew's folder was not added"),
            place("/usr/bin").unwrap(),
        );
        assert!(homebrew < usr, "{folders:?}");
        // And so a program there is found, where before it was not.
        let found = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|folder| folder.join("npx"))
            .find(|path| path.is_file());
        assert!(found.is_some(), "{folders:?}");
    }
    // Nothing twice, and the like of it done again changes nothing.
    let mut once = folders.clone();
    once.sort();
    once.dedup();
    assert_eq!(once.len(), folders.len(), "{folders:?}");
    odg_core::env::adopt_shell_path();
    assert_eq!(
        std::env::var_os("PATH").unwrap(),
        std::env::join_paths(&folders).unwrap()
    );
}

#[test]
fn shell_command_strips_provider_api_keys_from_child_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();

    // Inject fake provider credentials alongside a benign environment variable.
    // SAFETY: guarded by `ENV_LOCK` in this dedicated integration test binary.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-test-secret-openai");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-secret");
        std::env::set_var("GEMINI_API_KEY", "ai-gemini-test-secret");
        std::env::set_var("ODG_BENIGN_TEST_VAR", "keep-this-benign-value");
    }

    let print_env = if cfg!(windows) {
        "Get-ChildItem env: | ForEach-Object { \"$($_.Name)=$($_.Value)\" }"
    } else {
        "env"
    };

    let context = odg_core::tools::ToolContext {
        workspace: dir.path().to_path_buf(),
        cancel: tokio_util::sync::CancellationToken::new(),
        snapshots: None,
        jobs: None,
        mcp: None,
        skills: Vec::new(),
        search: None,
        sandbox: None,
        runtime: None,
        painter: None,
        vision: None,
        lsp: None,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (ran, inline) = rt.block_on(async {
        let ran = odg_core::tools::execute("run_command", &serde_json::json!({ "command": print_env }), &context).await;
        let inline = odg_core::tools::run_inline_shell(print_env, dir.path(), 10, 32_000)
            .await
            .unwrap();
        (ran, inline)
    });

    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("ODG_BENIGN_TEST_VAR");
    }

    assert!(!ran.is_error, "{}", ran.content);
    for secret in ["sk-test-secret-openai", "sk-ant-test-secret", "ai-gemini-test-secret"] {
        assert!(!ran.content.contains(secret), "run_command leaked {secret}");
        assert!(!inline.output.contains(secret), "run_inline_shell leaked {secret}");
    }
    assert!(
        ran.content.contains("keep-this-benign-value"),
        "benign env var missing: {}",
        ran.content
    );
    assert!(
        inline.output.contains("keep-this-benign-value"),
        "benign env var missing in inline shell: {}",
        inline.output
    );
    assert!(odg_core::tools::SENSITIVE_ENV_VARS.contains(&"OPENAI_API_KEY"));
    assert!(odg_core::tools::SENSITIVE_ENV_VARS.contains(&"SILICONFLOW_API_KEY"));
}
