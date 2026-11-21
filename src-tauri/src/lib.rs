//! The desktop shell: Tauri commands over `odg-core`, and one event channel
//! that carries every session's log, stream, and status events to the window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use odg_core::agent::{Services, Session, Status, Tuning, UiEvent};
use odg_core::anthropic::{AnthropicClient, DEFAULT_BASE_URL};
use odg_core::events::{Decision, Envelope, Event, FILES_PER_MESSAGE, Image, Picked, Reply, attach, check_images, pick};
use odg_core::grants::{GrantStore, Grants};
use odg_core::mcp::{McpHub, ServerStatus};
use odg_core::policy::Mode;
use odg_core::providers::{Instance, PROVIDERS, Provider, Router, vision};
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
    /// The language servers `code_nav` started, shared by every session.
    lsp: Arc<odg_core::lsp::LspHub>,
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
    /// Every provider there is to choose from: the first of each kind, and those the user added.
    providers: Vec<Instance>,
    /// The kinds of provider, which one is added as.
    kinds: &'static [Provider],
    efforts: &'static [&'static str],
    home: String,
    /// The command approvals given for good, by workspace.
    grants: Grants,
    /// The engines `web_search` can ask, `(id, what it is)`, and whether a GLM key is saved for it to work.
    search_engines: &'static [(&'static str, &'static str)],
    search_ready: bool,
    /// The image models `generate_image` can ask, `(id, name, provider)`, and the one in force, if any provider has a key.
    painters: Vec<(&'static str, &'static str, &'static str)>,
    painter_ready: Option<&'static str>,
    /// The user's own skills, and the directory they are kept in.
    skills: Vec<odg_core::events::SkillInfo>,
    skills_directory: String,
    /// The folder kept for conversations (闲谈), which a session there works in.
    chat_folder: String,
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
        // The window and the limit follow the provider and model the session is on when the request is made.
        let tuning = move |provider: &str, model: &str| {
            let settings = store.load();
            let window = store.context_window(&settings, provider, model).tokens;
            let (guardian, approval_wait) = (settings.guardian_model(provider, model), settings.approval_wait());
            Tuning { context_limit: settings.context_limit_within(window), context_window: window, effort: settings.effort, guardian, approval_wait }
        };
        let searching = self.settings.clone();
        let painting = self.settings.clone();
        let confining = self.settings.clone();
        let services = Services {
            grants: self.grants.clone(),
            mcp: Some(self.mcp.clone()),
            skills: Some(settings::home().join("skills")),
            search: Some(Arc::new(move || searching.search())),
            sandbox: Some(Arc::new(move || confining.sandbox())),
            chat: Some(settings::chat_folder()),
            runtime: Some(settings::home().join("runtime")),
            painter: Some(Arc::new(move || painting.painter())),
            lsp: Some(self.lsp.clone()),
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
            let name = settings.instances().into_iter().find(|known| known.id == settings.provider).map_or_else(|| settings.provider.clone(), |known| known.name);
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
        let settings = self.settings.load();
        SettingsView {
            providers: settings.instances(),
            settings,
            keys: self.settings.key_statuses(),
            kinds: PROVIDERS,
            efforts: EFFORTS,
            home: settings::home().display().to_string(),
            grants: self.grants.all(),
            search_engines: odg_core::web::SEARCH_ENGINES,
            search_ready: self.settings.search().is_some(),
            painters: odg_core::imagegen::PAINTERS.iter().map(|painter| (painter.id, painter.name, painter.provider)).collect(),
            painter_ready: self.settings.painter().map(|config| config.painter.id),
            skills: odg_core::skills::listing(&odg_core::skills::discover(&[settings::home().join("skills")])),
            skills_directory: settings::home().join("skills").display().to_string(),
            chat_folder: settings::chat_folder().display().to_string(),
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

/// The 秘籍 page: the skills that come with the app, on or off, and the user's own, in the user's skills directory.
#[derive(Serialize)]
struct SkillsView {
    skills: Vec<odg_core::skills::Listed>,
    folder: String,
}

fn skills_view() -> SkillsView {
    let folder = settings::home().join("skills");
    SkillsView { skills: odg_core::skills::catalog(&folder), folder: folder.display().to_string() }
}

#[tauri::command]
async fn list_skills() -> Result<SkillsView, String> {
    Ok(skills_view())
}

/// Switch one of the app's skills on or off. Off fails with `edited` for one the user changed, unless `force`.
#[tauri::command]
async fn switch_skill(name: String, on: bool, force: Option<bool>) -> Result<SkillsView, String> {
    odg_core::skills::switch(&settings::home().join("skills"), &name, on, force == Some(true))?;
    Ok(skills_view())
}

/// Put a skill in the user's skills directory from a folder on this computer or a Git address; `path` is the folder in it that holds the skill.
#[tauri::command]
async fn install_skill(source: String, path: Option<String>, replace: Option<bool>) -> Result<SkillsView, String> {
    let source = source.trim().to_string();
    let within = path.map(|path| path.trim().trim_matches('/').to_string()).filter(|path| !path.is_empty());
    let local = std::path::PathBuf::from(&source);
    let cancel = tokio_util::sync::CancellationToken::new();
    odg_core::skills::fetch(&source, local, within.as_deref(), &settings::home().join("skills"), replace == Some(true), &cancel).await?;
    Ok(skills_view())
}

/// Write a skill of the user's own, from its name, description and instructions.
#[tauri::command]
async fn write_skill(name: String, description: String, body: String, replace: Option<bool>) -> Result<SkillsView, String> {
    odg_core::skills::write(&settings::home().join("skills"), name.trim(), &description, &body, replace == Some(true))?;
    Ok(skills_view())
}

/// Take one of the user's own skills out of their skills directory.
#[tauri::command]
async fn remove_skill(name: String) -> Result<SkillsView, String> {
    odg_core::skills::remove(&settings::home().join("skills"), &name)?;
    Ok(skills_view())
}

/// Start a conversation (闲谈): a session in the folder kept for them, made the first time it is wanted.
#[tauri::command]
async fn start_chat(app: State<'_, App>) -> Result<SessionView, String> {
    let folder = settings::chat_folder();
    std::fs::create_dir_all(&folder).map_err(|error| format!("Cannot make {}: {error}", folder.display()))?;
    let session = app.start(folder, false, None)?;
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

/// Kinds of file that are programs, or that run one when opened: pressed under a message, they are shown in their
/// folder rather than opened, so that a slip of the hand runs nothing.
const RUNNABLE: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "psd1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "msi", "msp", "msc", "scr", "cpl", "lnk",
    "url", "reg", "jar", "appref-ms", "sh", "bash", "zsh", "command", "app", "run", "bin",
];

/// Whether `path` is a file this session has to do with: attached to one of its messages, or written, moved, handed
/// over or left in its workspace by its 分身, or a picture it looked at.
/// Only such a file is opened or shown from the conversation: the page cannot name any path at all.
fn session_file(session: &Session<Router>, path: &str) -> bool {
    session.snapshot().iter().any(|envelope| match &envelope.event {
        Event::UserMessage { files, .. } => files.iter().any(|file| file.path == path),
        Event::ToolResult { tool, is_error: false, meta, .. } => match tool.as_str() {
            // Written, painted, or moved there; or a picture it looked at, which the conversation shows as it saw it.
            "edit_file" | "write_file" | "generate_image" | "move_file" => meta["file"].as_str() == Some(path),
            "read_file" => meta["picture"].is_string() && meta["file"].as_str() == Some(path),
            // What the 分身 handed over, having made it some other way, such as with a script.
            "deliver_files" => meta["files"].as_array().is_some_and(|files| files.iter().any(|file| file.as_str() == Some(path))),
            _ => false,
        },
        // What a turn left in the workspace without a tool telling of it, found when the turn ended.
        Event::FilesMade { files, .. } => files.iter().any(|file| file == path),
        _ => false,
    })
}

/// Open a file attached to a message in this session, or written by its 分身, with the system's own program for
/// it, or with `reveal`, show it in its folder. A program is only shown in its folder. Answers `opened` or `revealed`.
#[tauri::command]
async fn open_attachment(app: State<'_, App>, id: String, path: String, reveal: bool) -> Result<String, String> {
    let session = app.live(&id)?;
    if !session_file(&session, &path) {
        return Err("这个文件不是这段对话里附过或写过的，不能从这里打开。".to_string());
    }
    let file = std::path::PathBuf::from(&path);
    if !file.exists() {
        return Err(format!("找不到这个文件了，可能已经移走或删掉：{path}"));
    }
    // The end-to-end test asks what would be done, and has nothing opened on the machine it runs on.
    let dry = std::env::var_os("ODG_E2E_DRY_OPEN").is_some();
    if reveal || runnable(&file) || file.is_dir() {
        if !dry {
            tauri_plugin_opener::reveal_item_in_dir(&file).map_err(|error| format!("没能在文件夹里显示它：{error}"))?;
        }
        return Ok("revealed".to_string());
    }
    if !dry {
        tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|error| format!("没能打开它：{error}"))?;
    }
    Ok("opened".to_string())
}

/// What the page shows of the document runtime: how it stands, and what LibreOffice it would use.
#[derive(Serialize)]
struct RuntimeView {
    status: odg_core::runtime::Status,
    python_version: &'static str,
    download_mb: u32,
    libreoffice: Option<String>,
    /// Whether this system has a build of it at all.
    supported: bool,
}

fn runtime_view() -> RuntimeView {
    RuntimeView {
        status: odg_core::runtime::status(&settings::home().join("runtime")),
        python_version: odg_core::runtime::PYTHON_VERSION,
        download_mb: odg_core::runtime::DOWNLOAD_MB,
        libreoffice: odg_core::runtime::libreoffice().map(|path| path.display().to_string()),
        supported: odg_core::runtime::target().is_some(),
    }
}

/// The event that says how far setting up the document runtime has got.
const RUNTIME_EVENT: &str = "runtime-progress";

#[tauri::command]
async fn runtime_status() -> Result<RuntimeView, String> {
    Ok(runtime_view())
}

/// Download and set up the document runtime, telling the page how far it has got.
#[tauri::command]
async fn install_runtime(handle: tauri::AppHandle) -> Result<RuntimeView, String> {
    let watch = move |step: odg_core::runtime::Progress| {
        let _ = handle.emit(RUNTIME_EVENT, step);
    };
    odg_core::runtime::install(&settings::home().join("runtime"), &watch, &tokio_util::sync::CancellationToken::new()).await?;
    Ok(runtime_view())
}

/// A language's server, as the settings page lists it: where it is found, and whether the app can download it.
#[derive(Serialize)]
struct LanguageServerView {
    key: &'static str,
    name: &'static str,
    /// The program that serves: the one downloaded, when that is what is used.
    program: &'static str,
    /// `system` (on the `PATH`), `downloaded` (for the app), or `missing`.
    state: &'static str,
    location: Option<String>,
    /// What the app downloads, its version, and about how many megabytes; absent where it cannot.
    download: Option<&'static str>,
    version: Option<&'static str>,
    download_mb: Option<u64>,
    /// How the user installs it themselves.
    install: &'static str,
}

async fn language_server_views(root: Option<&std::path::Path>) -> Vec<LanguageServerView> {
    let mut views = Vec::new();
    for language in odg_core::lsp::LANGUAGES {
        let (state, location) = match odg_core::lsp::locate(root, language).await {
            odg_core::lsp::Located::System(path) => ("system", Some(path.to_string_lossy().into_owned())),
            odg_core::lsp::Located::Downloaded(path) => ("downloaded", Some(path.to_string_lossy().into_owned())),
            odg_core::lsp::Located::Missing => ("missing", None),
        };
        views.push(LanguageServerView {
            key: language.key,
            name: language.name,
            program: match (state, language.download) {
                ("downloaded", Some(download)) => download.program,
                _ => language.program,
            },
            state,
            location,
            download: language.download.map(|download| download.name),
            version: language.download.map(|download| download.version),
            download_mb: language.download.and_then(|download| download.megabytes()),
            install: language.install,
        });
    }
    views
}

/// The language servers `code_nav` uses, and where each is.
#[tauri::command]
async fn language_servers(app: State<'_, App>) -> Result<Vec<LanguageServerView>, String> {
    Ok(language_server_views(app.lsp.root()).await)
}

/// Download a language's server for the app, checked against its pinned checksum.
#[tauri::command]
async fn install_language_server(app: State<'_, App>, key: String) -> Result<Vec<LanguageServerView>, String> {
    let language = odg_core::lsp::language_by_key(&key).ok_or("没有这种语言")?;
    let root = app.lsp.root().ok_or("这里不能下载语言服务")?.to_path_buf();
    odg_core::lsp::install(&root, language, &tokio_util::sync::CancellationToken::new()).await?;
    Ok(language_server_views(Some(&root)).await)
}

/// Take away the app's download of a language's server. The servers running are ended first, so none holds it open.
#[tauri::command]
async fn remove_language_server(app: State<'_, App>, key: String) -> Result<Vec<LanguageServerView>, String> {
    let language = odg_core::lsp::language_by_key(&key).ok_or("没有这种语言")?;
    let root = app.lsp.root().ok_or("这里没有下载的语言服务")?.to_path_buf();
    app.lsp.shutdown();
    odg_core::lsp::remove(&root, language)?;
    Ok(language_server_views(Some(&root)).await)
}

#[tauri::command]
async fn remove_runtime() -> Result<RuntimeView, String> {
    odg_core::runtime::remove(&settings::home().join("runtime"))?;
    Ok(runtime_view())
}

/// Kinds of picture the conversation shows from what the 分身 wrote, and the media type each is shown as.
const PICTURES: &[(&str, &str)] = &[("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp"), ("svg", "image/svg+xml"), ("bmp", "image/bmp"), ("ico", "image/x-icon")];

/// The largest picture shown in the conversation.
const PICTURE_BYTES: u64 = 20 * 1024 * 1024;

/// A picture this session's 分身 wrote, as it is now on disk, to be shown under the act that wrote it. It is shown as
/// an image, so an SVG's scripts never run.
#[tauri::command]
async fn picture(app: State<'_, App>, id: String, path: String) -> Result<Image, String> {
    let session = app.live(&id)?;
    if !session_file(&session, &path) {
        return Err("这个文件不是这段对话里写过的。".to_string());
    }
    let file = std::path::PathBuf::from(&path);
    let extension = file.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let media_type = PICTURES.iter().find(|(kind, _)| *kind == extension).map(|(_, media)| media.to_string()).ok_or("这不是能显示的图片。")?;
    let size = std::fs::metadata(&file).map_err(|_| format!("找不到这个文件了，可能已经移走或删掉：{path}"))?.len();
    if size > PICTURE_BYTES {
        return Err(format!("这张图有 {} MB，太大，不在这里显示。", size / 1024 / 1024));
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("读不了这张图：{error}"))?;
    Ok(Image { media_type, data: odg_core::events::base64(&bytes) })
}

/// Whether a file is a program, or runs one when opened; see [`RUNNABLE`].
fn runnable(file: &std::path::Path) -> bool {
    file.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| RUNNABLE.contains(&extension.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::runnable;
    use std::path::Path;

    #[test]
    fn a_program_is_known_by_its_kind_whatever_its_case() {
        for program in ["setup.exe", "RUN.BAT", "build.cmd", "tool.ps1", "install.msi", "shortcut.lnk", "site.url", "script.sh", "a.Exe"] {
            assert!(runnable(Path::new(program)), "{program}");
        }
        for document in ["notes.md", "report.pdf", "sheet.xlsx", "build.log", "photo.png", "README", "archive.tar.gz", ".exe-notes.txt"] {
            assert!(!runnable(Path::new(document)), "{document}");
        }
    }
}

/// Returns true when the message was queued behind a running turn.
#[tauri::command]
async fn send_message(app: State<'_, App>, id: String, text: String, images: Vec<Image>, files: Vec<String>) -> Result<bool, String> {
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() && files.is_empty() {
        return Err("the message is empty".to_string());
    }
    check_images(&images)?;
    if files.len() > FILES_PER_MESSAGE {
        return Err(format!("a message takes at most {FILES_PER_MESSAGE} files, not {}", files.len()));
    }
    // Read now, as they are when the message is sent: what the model is shown is then in the log, whatever becomes of the files.
    let files = tokio::task::spawn_blocking(move || {
        files.iter().map(|path| attach(std::path::Path::new(path)).map_err(|why| format!("{path} cannot be attached: {}", why.replace('_', " ")))).collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|error| error.to_string())??;
    let session = app.live(&id)?;
    let summary = session.summary();
    if !images.is_empty() && vision(&summary.provider, &summary.model) == Some(false) {
        return Err(format!("{} takes text only: remove the images, or start a session on a model that takes them", summary.model));
    }
    Ok(session.send_with(text, images, files))
}

/// What each path the user picked to attach is: a file, a picture, or neither and why. Nothing is sent yet.
#[tauri::command]
async fn attach_files(paths: Vec<String>) -> Result<Vec<Picked>, String> {
    tokio::task::spawn_blocking(move || paths.iter().map(|path| pick(std::path::Path::new(path))).collect()).await.map_err(|error| error.to_string())
}

#[tauri::command]
async fn resolve_approval(app: State<'_, App>, id: String, tool_use_id: String, decision: Decision) -> Result<bool, String> {
    Ok(app.live(&id)?.resolve_approval(&tool_use_id, decision))
}

/// Answer the questions an `ask_user` call is waiting on: a reply to each, or none to leave it to the 分身.
#[tauri::command]
async fn answer_questions(app: State<'_, App>, id: String, tool_use_id: String, answers: Option<Vec<Reply>>) -> Result<bool, String> {
    Ok(app.live(&id)?.answer_questions(&tool_use_id, answers))
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

/// Move the session to another model, of its provider or another. Fails while its agent is working.
#[tauri::command]
async fn set_model(app: State<'_, App>, id: String, provider: String, model: String) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.set_model(&provider, &model)?;
    Ok(app.item(session.summary(), session.status()))
}

/// Set how hard the model works for this session, in place of the settings' default.
#[tauri::command]
async fn set_effort(app: State<'_, App>, id: String, effort: String) -> Result<SessionItem, String> {
    let session = app.live(&id)?;
    session.set_effort(&effort)?;
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

/// A provider put in the list, and the settings as they now are.
#[derive(Serialize)]
struct AddedProvider {
    id: String,
    view: SettingsView,
}

/// Put a provider of `kind` in the list under `name`: the first of the kind while that is not set up, else another beside it.
#[tauri::command]
fn add_provider(app: State<'_, App>, kind: String, name: String) -> Result<AddedProvider, String> {
    let id = app.settings.add_provider(&kind, &name).map_err(|error| error.to_string())?;
    Ok(AddedProvider { id, view: app.settings_view() })
}

/// Take a provider out of the list, with its key, address and name.
#[tauri::command]
fn remove_provider(app: State<'_, App>, id: String) -> Result<SettingsView, String> {
    app.settings.remove_provider(&id).map_err(|error| error.to_string())?;
    Ok(app.settings_view())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Before anything else: a `.app` is started with a `PATH` that leaves out where the programs it starts live.
    odg_core::env::adopt_shell_path();
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

            // A document runtime whose libraries are behind this build's list has what is missing installed now, in
            // the background, without fetching its Python again; the settings page is told how it goes.
            if matches!(odg_core::runtime::status(&home.join("runtime")), odg_core::runtime::Status::Stale { .. }) {
                let (handle, root) = (app.handle().clone(), home.join("runtime"));
                tauri::async_runtime::spawn(async move {
                    let watch = move |step: odg_core::runtime::Progress| {
                        let _ = handle.emit(RUNTIME_EVENT, step);
                    };
                    // What did not go is tried again by the next start, or by the first 分身 that asks for it.
                    let _ = odg_core::runtime::install(&root, &watch, &tokio_util::sync::CancellationToken::new()).await;
                });
            }
            let lsp = Arc::new(odg_core::lsp::LspHub::new(home.join("lsp")));
            app.manage(App { store: SessionStore::new(home.join("sessions")), settings, grants, schedules, mcp, lsp, llm, sessions: Mutex::new(HashMap::new()), events });

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
            attach_files,
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
            answer_questions,
            language_servers,
            install_language_server,
            remove_language_server,
            set_mode,
            set_model,
            set_effort,
            start_chat,
            list_skills,
            switch_skill,
            remove_skill,
            open_attachment,
            picture,
            runtime_status,
            install_runtime,
            remove_runtime,
            add_provider,
            remove_provider,
            install_skill,
            write_skill,
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
                app.lsp.shutdown();
            }
        });
}
