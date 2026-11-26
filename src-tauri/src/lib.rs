//! The desktop shell: Tauri Builder wiring, plugin initialization, 12ms coalesced
//! global session event forwarding, schedule ticker, and deterministic exit hooks.

mod commands;
mod notify;
mod pty;
mod state;
mod watcher;

use std::sync::Arc;
use std::time::Duration;

use odg_core::agent::UiEvent;
use odg_core::anthropic::AnthropicClient;
use odg_core::grants::GrantStore;
use odg_core::mcp::McpHub;
use odg_core::providers::Router;
use odg_core::schedule::{self, ScheduleStore};
use odg_core::settings::{self, SettingsStore};
pub(crate) use state::forward_session_events;
use state::{App, AppCore, anthropic_base_url};
use tauri::{Emitter, Manager};

const SESSION_EVENT: &str = "session-event";
const SCHEDULES_EVENT: &str = "schedules-changed";
const SCHEDULE_TICK: Duration = Duration::from_secs(5);
const STALE_PASTE_MAX_AGE: Duration = Duration::from_secs(7 * 86_400);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    odg_core::env::adopt_shell_path();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
                let _ = window.set_title("");
            }

            let home = settings::home();
            let pastes_for_prune = home.join("pastes");
            std::thread::spawn(move || {
                odg_core::fsutil::prune_stale_temp_files(Duration::from_secs(86_400));
                commands::workbench::prune_stale_pastes_in(&pastes_for_prune, STALE_PASTE_MAX_AGE);
            });

            let settings = Arc::new(SettingsStore::at_home());
            let key_source = settings.clone();
            let anthropic = AnthropicClient::new(anthropic_base_url(), Arc::new(move || key_source.api_key("anthropic")))?;
            let endpoint_source = settings.clone();
            let llm = Arc::new(Router::new(anthropic, Arc::new(move |p: &str| endpoint_source.endpoint(p)))?);

            let (events, receiver) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
            let emit_handle = app.handle().clone();
            tauri::async_runtime::spawn(forward_session_events(receiver, move |event| {
                let _ = emit_handle.emit(SESSION_EVENT, event);
            }));

            let runtime_handle = tauri::async_runtime::handle().inner().clone();
            let grants = Arc::new(GrantStore::new(home.join("grants.json")));
            let mcp = Arc::new(McpHub::new(home.join("mcp.json"), runtime_handle.clone()));
            mcp.reload();
            let schedules = ScheduleStore::new(home.join("schedules.json"));

            let runtime_root = home.join("runtime");
            if matches!(odg_core::runtime::status(&runtime_root), odg_core::runtime::Status::Stale { .. }) {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let watch = move |step: odg_core::runtime::Progress| {
                        let _ = handle.emit(commands::runtime_lsp::RUNTIME_EVENT, step);
                    };
                    let _ = odg_core::runtime::install(&runtime_root, &watch, &tokio_util::sync::CancellationToken::new()).await;
                });
            }

            let lsp = Arc::new(odg_core::lsp::LspHub::new(home.join("lsp")));
            let pty = Arc::new(pty::PtyHub::new());
            let watcher = Arc::new(watcher::WorkspaceWatcher::new());
            let core = Arc::new(AppCore::new(
                home, settings, grants, schedules, mcp, lsp, pty, watcher, llm, events, runtime_handle,
            ));
            app.manage(App(core));

            let tick_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(SCHEDULE_TICK);
                loop {
                    tick.tick().await;
                    let core = tick_handle.state::<App>().0.clone();
                    let fired = tokio::task::spawn_blocking(move || {
                        let due = core.schedules.take_due(&schedule::now()).unwrap_or_default();
                        for item in &due {
                            let _ = core.schedules.record(&item.id, core.fire(item));
                        }
                        !due.is_empty()
                    })
                    .await
                    .unwrap_or(false);
                    if fired {
                        let _ = tick_handle.emit(SCHEDULES_EVENT, ());
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(commands::invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                let app = handle.state::<App>();
                app.kill_all_jobs();
                app.model_usage.flush_sync();
                app.mcp.shutdown();
                app.lsp.shutdown();
                app.pty.shutdown();
                app.watcher.stop();
            }
        });
}
