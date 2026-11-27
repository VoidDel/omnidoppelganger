//! The desktop shell: Tauri Builder wiring, plugin initialization, 12ms coalesced
//! global session event forwarding, schedule ticker, and deterministic exit hooks.

mod commands;
pub mod error;
mod notify;
mod pty;
mod state;
mod tray;
mod watcher;
mod window_state;

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

pub(crate) fn should_auto_update_runtime(
    settings: &odg_core::settings::Settings,
    status: &odg_core::runtime::Status,
) -> bool {
    settings.auto_update_runtime && matches!(status, odg_core::runtime::Status::Stale { .. })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    odg_core::env::adopt_shell_path();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let home = settings::home();
            let win_state = Arc::new(window_state::WindowStateManager::new(&home));
            app.manage(win_state.clone());

            let start_background = std::env::args().any(|arg| arg == "--background");
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "macos")]
                {
                    let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
                    let _ = window.set_title("");
                }
                win_state.restore(&window);
                if start_background {
                    let _ = window.hide();
                } else {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }

            odg_core::daemon::write_desktop_pid(&home);
            if let Err(err) = tray::setup_tray(app.handle()) {
                eprintln!("system tray unavailable: {err}");
            }
            let pastes_for_prune = home.join("pastes");
            std::thread::spawn(move || {
                odg_core::fsutil::prune_stale_temp_files(Duration::from_secs(86_400));
                commands::workbench::prune_stale_pastes_in(&pastes_for_prune, STALE_PASTE_MAX_AGE);
            });

            let settings = Arc::new(SettingsStore::at_home());
            let key_source = settings.clone();
            let anthropic =
                AnthropicClient::new(anthropic_base_url(), Arc::new(move || key_source.api_key("anthropic")))?;
            let endpoint_source = settings.clone();
            let llm = Arc::new(Router::new(
                anthropic,
                Arc::new(move |p: &str| endpoint_source.endpoint(p)),
            )?);

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
            if should_auto_update_runtime(&settings.load(), &odg_core::runtime::status(&runtime_root)) {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let watch = move |step: odg_core::runtime::Progress| {
                        let _ = handle.emit(commands::runtime_lsp::RUNTIME_EVENT, step);
                    };
                    let _ =
                        odg_core::runtime::install(&runtime_root, &watch, &tokio_util::sync::CancellationToken::new())
                            .await;
                });
            }

            let lsp = Arc::new(odg_core::lsp::LspHub::new(home.join("lsp")));
            let pty = Arc::new(pty::PtyHub::new());
            let watcher = Arc::new(watcher::WorkspaceWatcher::new());
            let core = Arc::new(AppCore::new(
                home,
                settings,
                grants,
                schedules,
                mcp,
                lsp,
                pty,
                watcher,
                llm,
                events,
                runtime_handle,
            ));
            app.manage(App(core));

            let tick_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(SCHEDULE_TICK);
                loop {
                    tick.tick().await;
                    let core = tick_handle.state::<App>().0.clone();
                    let fired_names = tokio::task::spawn_blocking(move || {
                        let due = core.schedules.take_due(&schedule::now()).unwrap_or_default();
                        let mut names = Vec::with_capacity(due.len());
                        for item in &due {
                            let _ = core.schedules.record(&item.id, core.fire(item));
                            names.push(item.name.clone());
                        }
                        names
                    })
                    .await
                    .unwrap_or_default();
                    if !fired_names.is_empty() {
                        let _ = tick_handle.emit(SCHEDULES_EVENT, ());
                        let hidden = tick_handle
                            .get_webview_window("main")
                            .and_then(|w| w.is_visible().ok())
                            .map(|visible| !visible)
                            .unwrap_or(true);
                        if hidden {
                            let summary = fired_names.join("、");
                            notify::notify_desktop("功课已唤醒".to_string(), format!("分身正在后台执行：{summary}"))
                                .await;
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            let handle = window.app_handle();
            match event {
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    if let Some(web_win) = handle.get_webview_window("main")
                        && let Some(ws) = handle.try_state::<Arc<window_state::WindowStateManager>>()
                    {
                        ws.capture(&web_win, false);
                    }
                }
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    if let Some(web_win) = handle.get_webview_window("main")
                        && let Some(ws) = handle.try_state::<Arc<window_state::WindowStateManager>>()
                    {
                        ws.capture(&web_win, true);
                    }
                    if !tray::is_quitting() && handle.state::<App>().0.settings.load().close_to_tray {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                tauri::WindowEvent::Destroyed => {
                    if let Some(ws) = handle.try_state::<Arc<window_state::WindowStateManager>>() {
                        ws.flush();
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(commands::invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } => {
                if !tray::is_quitting() && handle.state::<App>().0.settings.load().close_to_tray {
                    api.prevent_exit();
                }
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows, ..
            } => {
                if !has_visible_windows {
                    tray::show_main_window(handle);
                }
            }
            tauri::RunEvent::Exit => {
                if let Some(ws) = handle.try_state::<Arc<window_state::WindowStateManager>>() {
                    if let Some(web_win) = handle.get_webview_window("main") {
                        ws.capture(&web_win, true);
                    } else {
                        ws.flush();
                    }
                }
                let app = handle.state::<App>();
                odg_core::daemon::clear_desktop_pid(&app.home);
                app.kill_all_jobs();
                app.model_usage.flush_sync();
                app.mcp.shutdown();
                app.lsp.shutdown();
                app.pty.shutdown();
                app.watcher.stop();
            }
            _ => {}
        });
}
