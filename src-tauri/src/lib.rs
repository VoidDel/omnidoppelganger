//! The desktop shell: Tauri commands over `odg-core`, and one event channel
//! that carries every session's log, stream, and status events to the window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use odg_core::agent::{Session, Status, Tuning, UiEvent};
use odg_core::anthropic::{AnthropicClient, DEFAULT_BASE_URL};
use odg_core::events::{Decision, Envelope, Image, check_images};
use odg_core::grants::{GrantStore, Grants};
use odg_core::policy::Mode;
use odg_core::providers::{PROVIDERS, Provider, Router, context_window, provider, vision};
use odg_core::session::{SessionStore, SessionSummary};
use odg_core::settings::{self, EFFORTS, KeyStatus, Settings, SettingsStore, SystemVault};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc::UnboundedSender;

/// Name of the event the window listens to.
const SESSION_EVENT: &str = "session-event";

struct App {
    store: SessionStore,
    settings: Arc<SettingsStore>,
    /// Command approvals given for good, by workspace.
    grants: Arc<GrantStore>,
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
    /// The number of tokens at which this session's conversation is compacted.
    context_limit: u64,
    /// The Git branch checked out in the workspace, when it is a repository.
    branch: Option<String>,
    /// Whether the session's model takes pictures; `None` when its provider's documentation does not say.
    vision: Option<bool>,
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
    /// The command approvals given for good, by workspace.
    grants: Grants,
}

impl App {
    fn wrap(&self, log: odg_core::session::SessionLog) -> Result<Session<Router>, String> {
        let settings = self.settings.clone();
        let (_, provider, model) = log.header();
        let tuning = move || {
            let settings = settings.load();
            Tuning { context_limit: settings.context_limit_for(&provider, &model), context_window: context_window(&provider, &model), effort: settings.effort }
        };
        Session::new(log, self.llm.clone(), Arc::new(tuning), self.grants.clone(), self.events.clone(), tokio::runtime::Handle::current()).map_err(|error| error.to_string())
    }

    fn item(&self, summary: SessionSummary, status: Status) -> SessionItem {
        let context_limit = self.settings.load().context_limit_for(&summary.provider, &summary.model);
        let branch = odg_core::tools::git_branch(std::path::Path::new(&summary.workspace));
        let vision = vision(&summary.provider, &summary.model);
        SessionItem { summary, status, context_limit, branch, vision }
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
            grants: self.grants.all(),
        }
    }
}

fn view(app: &App, session: &Session<Router>) -> SessionView {
    SessionView { item: app.item(session.summary(), session.status()), events: session.snapshot() }
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
            Some(session) => Some(app.item(session.summary(), session.status())),
            // A stored session has no running turn: one left open by a crash is closed when it is opened.
            None => app.store.peek(&id).ok().map(|summary| app.item(SessionSummary { turn_open: false, awaiting_approval: false, ..summary }, Status::Idle)),
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
    Ok(view(&app, &session))
}

/// The folder a new session opens in without asking, when `ODG_E2E_WORKSPACE`
/// names one. It is there for the end-to-end test, which can drive the
/// window but not the system's folder dialog.
#[tauri::command]
fn preset_workspace() -> Option<String> {
    std::env::var("ODG_E2E_WORKSPACE").ok().filter(|path| !path.trim().is_empty())
}

#[tauri::command]
async fn open_session(app: State<'_, App>, id: String) -> Result<SessionView, String> {
    Ok(view(&app, &app.live(&id)?))
}

/// Returns true when the message was queued behind a running turn.
#[tauri::command]
async fn send_message(app: State<'_, App>, id: String, text: String, images: Vec<Image>) -> Result<bool, String> {
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() {
        return Err("the message is empty".to_string());
    }
    check_images(&images)?;
    let session = app.live(&id)?;
    let summary = session.summary();
    if !images.is_empty() && vision(&summary.provider, &summary.model) == Some(false) {
        return Err(format!("{} takes text only: remove the images, or start a session on a model that takes them", summary.model));
    }
    Ok(session.send_with(text, images))
}

#[tauri::command]
async fn resolve_approval(app: State<'_, App>, id: String, tool_use_id: String, decision: Decision) -> Result<bool, String> {
    Ok(app.live(&id)?.resolve_approval(&tool_use_id, decision))
}

#[tauri::command]
async fn rename_session(app: State<'_, App>, id: String, title: String) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.rename(&title)?;
    Ok(app.item(session.summary(), session.status()))
}

/// Remove a session and its file snapshots for good. Fails while its agent is working.
#[tauri::command]
async fn delete_session(app: State<'_, App>, id: String) -> Result<(), String> {
    // The map stays locked so the session cannot be opened again halfway through.
    let mut sessions = app.sessions.lock().unwrap();
    if sessions.get(&id).is_some_and(|session| session.status() != Status::Idle) {
        return Err("the agent of this session is working; stop it before deleting the session".to_string());
    }
    // Dropping the live session closes its handle on the log, which Windows needs before the file can go.
    if let Some(session) = sessions.remove(&id) {
        session.kill_jobs();
    }
    app.store.delete(&id).map_err(|error| error.to_string())
}

/// Ids of the sessions whose title, workspace, or conversation holds `query`.
#[tauri::command]
async fn search_sessions(app: State<'_, App>, query: String) -> Result<Vec<String>, String> {
    let live = app.sessions.lock().unwrap().clone();
    Ok(app
        .store
        .ids()
        .into_iter()
        .filter(|id| match live.get(id) {
            Some(session) => odg_core::session::matches(&session.snapshot(), &query),
            None => app.store.search(id, &query),
        })
        .collect())
}

/// Switch what the session does without asking; the next tool call follows the new mode.
#[tauri::command]
async fn set_mode(app: State<'_, App>, id: String, mode: Mode) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.set_mode(mode);
    Ok(app.item(session.summary(), session.status()))
}

/// What an undo did, for the window to report.
#[derive(Serialize)]
struct Undone {
    restored: Vec<String>,
    skipped: Vec<String>,
}

/// Undo the agent's file edits from `turn` on. Fails while a turn is running.
#[tauri::command]
async fn undo_turn(app: State<'_, App>, id: String, turn: u64) -> Result<Undone, String> {
    let outcome = app.live(&id)?.undo(turn)?;
    Ok(Undone { restored: outcome.restored, skipped: outcome.skipped })
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

/// Take back a command approval that was given for good.
#[tauri::command]
fn revoke_grant(app: State<'_, App>, workspace: String, grant: String) -> Result<SettingsView, String> {
    app.grants.remove(&workspace, &grant).map_err(|error| error.to_string())?;
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
            // A home named by ODG_HOME (a test's, a portable one) keeps its keys in its own
            // file, so it never reads or changes the keys of the user's everyday home.
            let custom_home = std::env::var_os("ODG_HOME").is_some_and(|value| !value.is_empty());
            let settings = Arc::new(if custom_home { SettingsStore::new(&home) } else { SettingsStore::with_vault(&home, Arc::new(SystemVault)) });
            settings.migrate_keys();
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

            let grants = Arc::new(GrantStore::new(home.join("grants.json")));
            app.manage(App { store: SessionStore::new(home.join("sessions")), settings, grants, llm, sessions: Mutex::new(HashMap::new()), events });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            create_session,
            preset_workspace,
            open_session,
            rename_session,
            delete_session,
            search_sessions,
            send_message,
            resolve_approval,
            set_mode,
            undo_turn,
            cancel_turn,
            get_settings,
            save_settings,
            revoke_grant,
            save_api_key,
            delete_api_key,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // Commands left running in the background must not outlive the app.
            if let tauri::RunEvent::Exit = event {
                for session in handle.state::<App>().sessions.lock().unwrap().values() {
                    session.kill_jobs();
                }
            }
        });
}
