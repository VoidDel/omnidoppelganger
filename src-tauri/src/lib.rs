//! The desktop shell: Tauri commands over `odg-core`, and one event channel
//! that carries every session's log, stream, and status events to the window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use odg_core::agent::{Services, Session, Status, Tuning, UiEvent};
use odg_core::anthropic::{AnthropicClient, DEFAULT_BASE_URL};
use odg_core::events::{Decision, Envelope, Image, check_images};
use odg_core::grants::{GrantStore, Grants};
use odg_core::mcp::{McpHub, ServerStatus};
use odg_core::policy::Mode;
use odg_core::providers::{PROVIDERS, Provider, Router, provider, vision};
use odg_core::schedule::{self, Run, Schedule, ScheduleStore};
use odg_core::session::{NewSession, SessionStore, SessionSummary};
use odg_core::worktree::{self, Landed};
use odg_core::settings::{self, EFFORTS, KeyStatus, Settings, SettingsStore};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc::UnboundedSender;

/// Name of the event the window listens to.
const SESSION_EVENT: &str = "session-event";
/// Name of the event that tells the window the list of schedules changed by itself: one came due.
const SCHEDULES_EVENT: &str = "schedules-changed";
/// How often the clock is checked against the schedules.
const SCHEDULE_TICK: std::time::Duration = std::time::Duration::from_secs(5);

struct App {
    store: SessionStore,
    settings: Arc<SettingsStore>,
    /// Command approvals given for good, by workspace.
    grants: Arc<GrantStore>,
    /// The tasks set to start by themselves.
    schedules: ScheduleStore,
    /// The MCP servers the user configured, shared by every session.
    mcp: Arc<McpHub>,
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
    /// The most that limit can be for the session's model: four fifths of its context window.
    context_ceiling: u64,
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
    /// The engines `web_search` can ask, `(id, what it is)`, and whether a GLM key is saved for it to work.
    search_engines: &'static [(&'static str, &'static str)],
    search_ready: bool,
    /// The user's own skills, and the directory they are kept in.
    skills: Vec<odg_core::events::SkillInfo>,
    skills_directory: String,
    /// The context window of the model new sessions use, and how it is known.
    context_window: settings::Window,
    /// What confines commands on this system when the user switches that on, or why nothing can.
    sandbox_support: Result<&'static str, String>,
    /// The text of `mcp.json`, and how each server it lists stands.
    mcp_config: String,
    mcp_servers: Vec<ServerStatus>,
}

impl App {
    fn wrap(&self, log: odg_core::session::SessionLog) -> Result<Session<Router>, String> {
        let store = self.settings.clone();
        let (_, provider, _) = log.header();
        // The window and the limit follow the model the session is on when the request is made.
        let tuning = move |model: &str| {
            let settings = store.load();
            let window = store.context_window(&settings, &provider, model).tokens;
            let (guardian, approval_wait) = (settings.guardian_model(&provider, model), settings.approval_wait());
            Tuning { context_limit: settings.context_limit_within(window), context_window: window, effort: settings.effort, guardian, approval_wait }
        };
        let searching = self.settings.clone();
        let confining = self.settings.clone();
        let services = Services {
            grants: self.grants.clone(),
            mcp: Some(self.mcp.clone()),
            skills: Some(settings::home().join("skills")),
            search: Some(Arc::new(move || searching.search())),
            sandbox: Some(Arc::new(move || confining.sandbox())),
        };
        Session::new(log, self.llm.clone(), Arc::new(tuning), services, self.events.clone(), tokio::runtime::Handle::current()).map_err(|error| error.to_string())
    }

    fn item(&self, summary: SessionSummary, status: Status) -> SessionItem {
        let settings = self.settings.load();
        let window = self.settings.context_window(&settings, &summary.provider, &summary.model).tokens;
        let context_limit = settings.context_limit_within(window);
        let branch = odg_core::tools::git_branch(std::path::Path::new(&summary.workspace));
        let vision = vision(&summary.provider, &summary.model);
        let context_ceiling = window / 5 * 4;
        SessionItem { summary, status, context_limit, context_ceiling, branch, vision }
    }

    /// Start a session in `workspace` on the provider and model of the
    /// settings, in the settings' mode. With `isolated`, in a Git worktree
    /// made for it. `schedule` names the schedule that starts it, if one does.
    fn start(&self, workspace: PathBuf, isolated: bool, schedule: Option<&str>) -> Result<Session<Router>, String> {
        if !workspace.is_dir() {
            return Err(format!("{} is not a directory", workspace.display()));
        }
        let settings = self.settings.load();
        if settings.model.is_empty() {
            let name = provider(&settings.provider).map_or(settings.provider.as_str(), |known| known.name);
            return Err(format!("{name} has no model chosen: enter a model id in settings"));
        }
        let (workspace, made) = match isolated {
            true => {
                let (workspace, made) = worktree::create(&workspace, &settings::home().join("worktrees"))?;
                (workspace, Some(made))
            }
            false => (workspace, None),
        };
        let new = NewSession { workspace: &workspace, provider: &settings.provider, model: &settings.model, parent: None, worktree: made.clone(), schedule };
        let log = self.store.create_with(new).map_err(|error| {
            // A worktree without its session would never be found again.
            if let Some(made) = &made {
                let _ = worktree::remove(made);
            }
            error.to_string()
        })?;
        let session = self.wrap(log)?;
        session.set_mode(settings.mode);
        self.sessions.lock().unwrap().insert(session.id().to_string(), session.clone());
        Ok(session)
    }

    /// Start what a schedule asks for: a session in its folder, in its mode,
    /// named after it and given its task. Returns how that went, to be kept
    /// with the schedule. While the session of its last start is still
    /// working, none is started: the same task would be at work twice.
    fn fire(&self, schedule: &Schedule) -> Run {
        let at = schedule::stamp(&schedule::now().to_utc());
        let last = schedule.last.as_ref().and_then(|run| run.session.clone());
        if let Some(busy) = last.filter(|id| self.all_live().get(id).is_some_and(|session| session.status() != Status::Idle)) {
            return Run { at, session: Some(busy), error: Some("the session it started last is still working, so this time was passed over".to_string()) };
        }
        match self.start(PathBuf::from(&schedule.workspace), schedule.isolated, Some(&schedule.name)) {
            Ok(session) => {
                session.set_mode(schedule.mode);
                // The name is the title; a session that cannot take it is titled after the task instead.
                let _ = session.rename(&schedule.name);
                session.send(schedule.prompt.clone());
                Run { at, session: Some(session.id().to_string()), error: None }
            }
            Err(error) => Run { at, session: None, error: Some(error) },
        }
    }

    /// The live session, opening it from disk on first use. The map stays
    /// locked while the log is opened, so a log never gets two writers.
    fn live(&self, id: &str) -> Result<Session<Router>, String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
        // A subagent set off since the app started is live with the agent that set it off.
        let session = match sessions.values().find_map(|session| session.child(id)) {
            Some(child) => child,
            None => self.wrap(self.store.open(id).map_err(|error| error.to_string())?)?,
        };
        sessions.insert(id.to_string(), session.clone());
        Ok(session)
    }

    /// Every live session by id: those opened here, and the subagents their agents have set off.
    fn all_live(&self) -> HashMap<String, Session<Router>> {
        let mut live = self.sessions.lock().unwrap().clone();
        for child in live.values().flat_map(Session::children).collect::<Vec<_>>() {
            live.entry(child.id().to_string()).or_insert(child);
        }
        live
    }

    fn settings_view(&self) -> SettingsView {
        SettingsView {
            settings: self.settings.load(),
            keys: self.settings.key_statuses(),
            providers: PROVIDERS,
            efforts: EFFORTS,
            home: settings::home().display().to_string(),
            grants: self.grants.all(),
            search_engines: odg_core::web::SEARCH_ENGINES,
            search_ready: self.settings.search().is_some(),
            skills: odg_core::skills::listing(&odg_core::skills::discover(&[settings::home().join("skills")])),
            skills_directory: settings::home().join("skills").display().to_string(),
            context_window: {
                let settings = self.settings.load();
                self.settings.context_window(&settings, &settings.provider, &settings.model)
            },
            sandbox_support: odg_core::sandbox::support(),
            mcp_config: self.mcp.config_text(),
            mcp_servers: self.mcp.statuses(),
        }
    }
}

fn view(app: &App, session: &Session<Router>) -> SessionView {
    SessionView { item: app.item(session.summary(), session.status()), events: session.snapshot() }
}

#[tauri::command]
async fn list_sessions(app: State<'_, App>) -> Result<Vec<SessionItem>, String> {
    let live = app.all_live();
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

/// Start a session in `workspace`. With `isolated`, in a Git worktree made for
/// it from that folder's repository, so that it shares no files with sessions
/// already at work there; its work comes back with [`land_session`].
#[tauri::command]
async fn create_session(app: State<'_, App>, workspace: String, isolated: Option<bool>) -> Result<SessionView, String> {
    let session = app.start(PathBuf::from(workspace), isolated == Some(true), None)?;
    Ok(view(&app, &session))
}

/// Start a session that goes on from a point in another's conversation: its
/// log begins as a copy of that session's up to where turn `before` starts,
/// or of all of it when `before` is none, less a turn still running. Only the
/// conversation is copied: the fork works on the same files, as they are now.
#[tauri::command]
async fn fork_session(app: State<'_, App>, id: String, before: Option<u64>) -> Result<SessionView, String> {
    let source = app.live(&id)?;
    let log = app.store.fork(&source.snapshot(), before).map_err(|error| error.to_string())?;
    let session = app.wrap(log)?;
    app.sessions.lock().unwrap().insert(session.id().to_string(), session.clone());
    Ok(view(&app, &session))
}

/// The tasks set to start by themselves, in the order they were added.
#[tauri::command]
fn list_schedules(app: State<'_, App>) -> Vec<Schedule> {
    app.schedules.all()
}

/// Add a schedule, or change the one with its id. Returns the list.
#[tauri::command]
fn save_schedule(app: State<'_, App>, schedule: Schedule) -> Result<Vec<Schedule>, String> {
    app.schedules.save(schedule, &schedule::now())?;
    Ok(app.schedules.all())
}

/// Remove a schedule. The sessions it started stay. Returns the list.
#[tauri::command]
fn delete_schedule(app: State<'_, App>, id: String) -> Result<Vec<Schedule>, String> {
    app.schedules.remove(&id)?;
    Ok(app.schedules.all())
}

/// Start a schedule's task now, whatever its time; when it is next due stays
/// as it was. Returns the list, which says how the start went.
#[tauri::command]
async fn run_schedule(app: State<'_, App>, id: String) -> Result<Vec<Schedule>, String> {
    let schedule = app.schedules.all().into_iter().find(|kept| kept.id == id).ok_or("this schedule is no longer there")?;
    app.schedules.record(&id, app.fire(&schedule))?;
    Ok(app.schedules.all())
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

/// Bring back the work of a session that has a worktree of its own: commit
/// what its checkout holds, and merge its branch into what the repository has
/// checked out. A merge that conflicts is undone and reported. Fails while
/// the session's agent, or a subagent of it, is working.
#[tauri::command]
async fn land_session(app: State<'_, App>, id: String) -> Result<Landed, String> {
    let session = app.live(&id)?;
    let summary = session.summary();
    let made = summary.worktree.ok_or("this session works in the folder itself, so there is nothing to bring back")?;
    if session.status() != Status::Idle || session.children().iter().any(|child| child.status() != Status::Idle) {
        return Err("the agent is working; stop it or wait before bringing its work back".to_string());
    }
    let message = format!("odg: {}", if summary.title.is_empty() { "work of a session" } else { summary.title.as_str() });
    tokio::task::spawn_blocking(move || worktree::land(&made, &message)).await.map_err(|error| error.to_string())?
}

/// Remove a session and its file snapshots for good, and with it the sessions
/// of the subagents its agent set off, and the worktree made for it: what its
/// checkout holds uncommitted goes too, while commits that were never brought
/// back stay in the repository under the worktree's branch. Fails while any of
/// their agents is working.
#[tauri::command]
async fn delete_session(app: State<'_, App>, id: String) -> Result<(), String> {
    let live = app.all_live();
    let summary_of = |id: &str| match live.get(id) {
        Some(session) => Some(session.summary()),
        None => app.store.peek(id).ok(),
    };
    // The worktree is the session's own to remove unless it is a subagent, which works in its agent's;
    // or another session of the user's works in it too: a fork of this one, or the one this was forked from.
    let made = summary_of(&id).filter(|summary| summary.parent.is_none()).and_then(|summary| summary.worktree).filter(|made| {
        !app.store.ids().iter().filter(|other| **other != id).filter_map(|other| summary_of(other)).any(|other| other.parent.is_none() && other.worktree.as_ref() == Some(made))
    });
    let subagents: Vec<String> = app
        .store
        .ids()
        .into_iter()
        .filter(|other| match live.get(other) {
            Some(session) => session.parent() == Some(id.as_str()),
            None => app.store.peek(other).is_ok_and(|summary| summary.parent.as_deref() == Some(id.as_str())),
        })
        .collect();
    drop(live);
    // The map stays locked so the session cannot be opened again halfway through.
    let mut sessions = app.sessions.lock().unwrap();
    let busy = |session: &Session<Router>| session.status() != Status::Idle || session.children().iter().any(|child| child.status() != Status::Idle);
    if std::iter::once(&id).chain(&subagents).filter_map(|id| sessions.get(id)).any(busy) {
        return Err("the agent of this session, or one of its subagents, is working; stop it before deleting the session".to_string());
    }
    // Dropping a live session closes its handle on the log, which Windows needs before the file can go.
    // The session itself goes first: it holds its subagents' sessions open.
    for id in std::iter::once(&id).chain(&subagents) {
        if let Some(session) = sessions.remove(id) {
            session.kill_jobs();
        }
        app.store.delete(id).map_err(|error| error.to_string())?;
    }
    drop(sessions);
    match made {
        Some(made) => worktree::remove(&made).map_err(|error| format!("the session is deleted, but its worktree at {} could not be removed: {error}", made.checkout)),
        None => Ok(()),
    }
}

/// Ids of the sessions whose title, workspace, or conversation holds `query`.
#[tauri::command]
async fn search_sessions(app: State<'_, App>, query: String) -> Result<Vec<String>, String> {
    let live = app.all_live();
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

/// Move the session to another model of its provider. Fails while its agent is working.
#[tauri::command]
async fn set_model(app: State<'_, App>, id: String, model: String) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.set_model(&model)?;
    Ok(app.item(session.summary(), session.status()))
}

/// The models a provider serves with the saved key, as `(id, display name)`, asked from the provider itself.
#[tauri::command]
async fn list_models(app: State<'_, App>, provider: String) -> Result<Vec<(String, String)>, String> {
    let endpoint = app.settings.endpoint(&provider);
    // Anthropic's address comes from the environment, like its client's.
    let base_url = if provider == "anthropic" { anthropic_base_url() } else { endpoint.base_url };
    let models = odg_core::providers::list_models(&provider, &base_url, endpoint.key.as_deref()).await?;
    // What it said of its models' context windows is kept; a list that cannot be kept is still a list.
    let _ = app.settings.record_models(&provider, &models);
    Ok(models.into_iter().map(|model| (model.id, model.name)).collect())
}

/// Where Anthropic's API is: `ANTHROPIC_BASE_URL`, or the official address.
fn anthropic_base_url() -> String {
    std::env::var("ANTHROPIC_BASE_URL").ok().filter(|url| !url.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// Switch what the session does without asking; the next tool call follows the new mode.
#[tauri::command]
async fn set_mode(app: State<'_, App>, id: String, mode: Mode) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    if mode == Mode::Bypass && session.summary().worktree.is_none() {
        return Err("only a session in a worktree of its own can be left to ask about nothing; start one with the branch button beside the folder".to_string());
    }
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

/// Have the model summarize the session's conversation now, between turns.
/// Returns how many tokens the conversation held before.
#[tauri::command]
async fn compact_session(app: State<'_, App>, id: String) -> Result<u64, String> {
    let session = app.live(&id)?;
    session.compact_now().await
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

/// Check and save the MCP server list, then connect to what it lists.
#[tauri::command]
fn save_mcp_config(app: State<'_, App>, config: String) -> Result<SettingsView, String> {
    app.mcp.save_config(&config)?;
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
            let settings = Arc::new(SettingsStore::at_home());
            let key_source = settings.clone();
            let anthropic = AnthropicClient::new(anthropic_base_url(), Arc::new(move || key_source.api_key("anthropic")))?;
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
            let mcp = Arc::new(McpHub::new(home.join("mcp.json"), tauri::async_runtime::handle().inner().clone()));
            // Servers connect in the background; a session uses whichever are ready when it asks the model.
            mcp.reload();
            let schedules = ScheduleStore::new(home.join("schedules.json"));
            app.manage(App { store: SessionStore::new(home.join("sessions")), settings, grants, schedules, mcp, llm, sessions: Mutex::new(HashMap::new()), events });

            // The clock: whatever has come due is started, with or without a window to show it in.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(SCHEDULE_TICK);
                loop {
                    tick.tick().await;
                    let app = handle.state::<App>();
                    // A list that cannot be read or written is left for the next tick.
                    let due = app.schedules.take_due(&schedule::now()).unwrap_or_default();
                    for schedule in &due {
                        let _ = app.schedules.record(&schedule.id, app.fire(schedule));
                    }
                    if !due.is_empty() {
                        let _ = handle.emit(SCHEDULES_EVENT, ());
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            create_session,
            preset_workspace,
            fork_session,
            list_schedules,
            save_schedule,
            delete_schedule,
            run_schedule,
            open_session,
            land_session,
            rename_session,
            delete_session,
            search_sessions,
            send_message,
            resolve_approval,
            set_mode,
            set_model,
            list_models,
            undo_turn,
            cancel_turn,
            compact_session,
            get_settings,
            save_settings,
            save_mcp_config,
            revoke_grant,
            save_api_key,
            delete_api_key,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // Commands left running in the background must not outlive the app.
            if let tauri::RunEvent::Exit = event {
                let app = handle.state::<App>();
                for session in app.sessions.lock().unwrap().values() {
                    session.kill_jobs();
                }
                // Ends the servers started as processes.
                app.mcp.shutdown();
            }
        });
}
