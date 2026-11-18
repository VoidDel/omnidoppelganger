//! The desktop shell: Tauri commands over `odg-core`, and one event channel
//! that carries every session's log, stream, and status events to the window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use odg_core::agent::{Session, Status, UiEvent};
use odg_core::anthropic::{AnthropicClient, DEFAULT_BASE_URL};
use odg_core::events::{Decision, Envelope};
use odg_core::policy::Mode;
use odg_core::providers::{PROVIDERS, Provider, Router, provider};
use odg_core::session::{SessionStore, SessionSummary};
use odg_core::settings::{self, EFFORTS, KeyStatus, Settings, SettingsStore};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc::UnboundedSender;

/// Name of the event the window listens to.
const SESSION_EVENT: &str = "session-event";

struct App {
    store: SessionStore,
    settings: Arc<SettingsStore>,
    llm: Arc<Router>,
    /// Sessions opened since the app started. A session stays here so its turn keeps running while another is shown.
    sessions: Mutex<HashMap<String, Session<Router>>>,
    events: UnboundedSender<UiEvent>,
}

#[derive(Serialize)]
struct SessionItem {
    #[serde(flatten)]
    summary: SessionSummary,
    status: Status,
}

#[derive(Serialize)]
struct SessionView {
    #[serde(flatten)]
    item: SessionItem,
    events: Vec<Envelope>,
}

#[derive(Serialize)]
struct SettingsView {
    settings: Settings,
    /// Each provider's key status, by provider id.
    keys: BTreeMap<String, KeyStatus>,
    providers: &'static [Provider],
    efforts: &'static [&'static str],
    home: String,
}

impl App {
    fn wrap(&self, log: odg_core::session::SessionLog) -> Result<Session<Router>, String> {
        let settings = self.settings.clone();
        Session::new(log, self.llm.clone(), Arc::new(move || settings.load().effort), self.events.clone(), tokio::runtime::Handle::current())
            .map_err(|error| error.to_string())
    }

    /// The live session, opening it from disk on first use. The map stays
    /// locked while the log is opened, so a log never gets two writers.
    fn live(&self, id: &str) -> Result<Session<Router>, String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
        let session = self.wrap(self.store.open(id).map_err(|error| error.to_string())?)?;
        sessions.insert(id.to_string(), session.clone());
        Ok(session)
    }

    fn settings_view(&self) -> SettingsView {
        SettingsView {
            settings: self.settings.load(),
            keys: self.settings.key_statuses(),
            providers: PROVIDERS,
            efforts: EFFORTS,
            home: settings::home().display().to_string(),
        }
    }
}

fn view(session: &Session<Router>) -> SessionView {
    SessionView { item: SessionItem { summary: session.summary(), status: session.status() }, events: session.snapshot() }
}

#[tauri::command]
async fn list_sessions(app: State<'_, App>) -> Result<Vec<SessionItem>, String> {
    let live = app.sessions.lock().unwrap().clone();
    // Live sessions are summarized from memory: their logs may be mid-append.
    let mut items: Vec<SessionItem> = app
        .store
        .ids()
        .into_iter()
        .filter_map(|id| match live.get(&id) {
            Some(session) => Some(SessionItem { summary: session.summary(), status: session.status() }),
            // A stored session has no running turn: one left open by a crash is closed when it is opened.
            None => app.store.peek(&id).ok().map(|summary| SessionItem {
                summary: SessionSummary { turn_open: false, awaiting_approval: false, ..summary },
                status: Status::Idle,
            }),
        })
        .collect();
    items.sort_by(|a, b| b.summary.updated_at.cmp(&a.summary.updated_at));
    Ok(items)
}

#[tauri::command]
async fn create_session(app: State<'_, App>, workspace: String) -> Result<SessionView, String> {
    let workspace = PathBuf::from(workspace);
    if !workspace.is_dir() {
        return Err(format!("{} is not a directory", workspace.display()));
    }
    let settings = app.settings.load();
    if settings.model.is_empty() {
        let name = provider(&settings.provider).map_or(settings.provider.as_str(), |known| known.name);
        return Err(format!("{name} has no model chosen: enter a model id in settings"));
    }
    let log = app.store.create(&workspace, &settings.provider, &settings.model).map_err(|error| error.to_string())?;
    let session = app.wrap(log)?;
    session.set_mode(settings.mode);
    app.sessions.lock().unwrap().insert(session.id().to_string(), session.clone());
    Ok(view(&session))
}

#[tauri::command]
async fn open_session(app: State<'_, App>, id: String) -> Result<SessionView, String> {
    Ok(view(&app.live(&id)?))
}

/// Returns true when the message was queued behind a running turn.
#[tauri::command]
async fn send_message(app: State<'_, App>, id: String, text: String) -> Result<bool, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("the message is empty".to_string());
    }
    Ok(app.live(&id)?.send(text))
}

#[tauri::command]
async fn resolve_approval(app: State<'_, App>, id: String, tool_use_id: String, decision: Decision) -> Result<bool, String> {
    Ok(app.live(&id)?.resolve_approval(&tool_use_id, decision))
}

/// Switch what the session does without asking; the next tool call follows the new mode.
#[tauri::command]
async fn set_mode(app: State<'_, App>, id: String, mode: Mode) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.set_mode(mode);
    Ok(SessionItem { summary: session.summary(), status: session.status() })
}

#[tauri::command]
async fn cancel_turn(app: State<'_, App>, id: String) -> Result<(), String> {
    app.live(&id)?.cancel();
    Ok(())
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> SettingsView {
    app.settings_view()
}

#[tauri::command]
fn save_settings(app: State<'_, App>, settings: Settings) -> Result<SettingsView, String> {
    app.settings.save(settings).map_err(|error| error.to_string())?;
    Ok(app.settings_view())
}

#[tauri::command]
fn save_api_key(app: State<'_, App>, provider: String, key: String) -> Result<SettingsView, String> {
    app.settings.save_api_key(&provider, &key).map_err(|error| error.to_string())?;
    Ok(app.settings_view())
}

#[tauri::command]
fn delete_api_key(app: State<'_, App>, provider: String) -> Result<SettingsView, String> {
    app.settings.delete_api_key(&provider).map_err(|error| error.to_string())?;
    Ok(app.settings_view())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let home = settings::home();
            let settings = Arc::new(SettingsStore::new(&home));
            let key_source = settings.clone();
            let base_url = std::env::var("ANTHROPIC_BASE_URL").ok().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
            let anthropic = AnthropicClient::new(base_url, Arc::new(move || key_source.api_key("anthropic")))?;
            let endpoint_source = settings.clone();
            let llm = Arc::new(Router::new(anthropic, Arc::new(move |provider: &str| endpoint_source.endpoint(provider)))?);

            let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = receiver.recv().await {
                    // A closed window has nobody to tell; the session log already holds the durable part.
                    let _ = handle.emit(SESSION_EVENT, event);
                }
            });

            app.manage(App { store: SessionStore::new(home.join("sessions")), settings, llm, sessions: Mutex::new(HashMap::new()), events });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            create_session,
            open_session,
            send_message,
            resolve_approval,
            set_mode,
            cancel_turn,
            get_settings,
            save_settings,
            save_api_key,
            delete_api_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
