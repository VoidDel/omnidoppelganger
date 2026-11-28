//! Headless-testable desktop application state (`AppCore` / `App`), per-session
//! non-blocking `OnceCell` loading slots, idle LRU session eviction, and shared DTOs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use odg_core::agent::{Services, Session, Status, Tuning, UiEvent};
use odg_core::anthropic::DEFAULT_BASE_URL;
use odg_core::events::Envelope;
use odg_core::grants::{GrantStore, Grants, PROJECT_TRUST_GRANT};
use odg_core::llm::Llm;
use odg_core::mcp::{McpHub, ServerStatus};
use odg_core::model_usage::ModelUsageStore;
use odg_core::policy::Mode;
use odg_core::providers::{Instance, PROVIDERS, Provider, Router, vision};
use odg_core::schedule::{self, Run, Schedule, ScheduleStore};
use odg_core::session::{NewSession, SessionLog, SessionStore, SessionSummary};
use odg_core::settings::{self, EFFORTS, KeyStatus, Settings, SettingsStore};
use odg_core::sync_ext::MutexExt;
use odg_core::worktree;
use serde::Serialize;
use tokio::sync::OnceCell;
use tokio::sync::mpsc::UnboundedSender;

use crate::{pty, watcher};

/// Maximum number of idle sessions kept resident in memory before evicting the least-recently accessed ones.
pub const MAX_IDLE_SESSIONS: usize = 24;

pub enum UsageStoreRef {
    Global(&'static ModelUsageStore),
    #[cfg(test)]
    Local(ModelUsageStore),
}

impl Deref for UsageStoreRef {
    type Target = ModelUsageStore;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Global(s) => s,
            #[cfg(test)]
            Self::Local(s) => s,
        }
    }
}

#[derive(Clone)]
struct SessionSlot<L: Llm> {
    cell: Arc<OnceCell<Session<L>>>,
    last_access: Arc<AtomicU64>,
}

/// Core desktop bridge state, decoupled from `tauri::State` and WebView windows so all
/// business workflows can be exercised directly in headless unit/integration tests.
pub struct AppCore<L: Llm + 'static = Router> {
    pub home: PathBuf,
    pub store: SessionStore,
    pub settings: Arc<SettingsStore>,
    pub grants: Arc<GrantStore>,
    pub schedules: ScheduleStore,
    pub mcp: Arc<McpHub>,
    pub lsp: Arc<odg_core::lsp::LspHub>,
    pub pty: Arc<pty::PtyHub>,
    pub watcher: Arc<watcher::WorkspaceWatcher>,
    pub model_usage: UsageStoreRef,
    pub llm: Arc<L>,
    pub events: UnboundedSender<UiEvent>,
    pub runtime: tokio::runtime::Handle,
    sessions: Arc<Mutex<HashMap<String, SessionSlot<L>>>>,
    active_id: Mutex<Option<String>>,
    access_seq: AtomicU64,
}

#[derive(Clone)]
pub struct App(pub Arc<AppCore<Router>>);

impl Deref for App {
    type Target = AppCore<Router>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionItem {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub status: Status,
    /// The number of tokens at which this session's conversation is compacted.
    pub context_limit: u64,
    /// The most that limit can be for the session's model: four fifths of its context window.
    pub context_ceiling: u64,
    /// The Git branch checked out in the workspace, when it is a repository.
    pub branch: Option<String>,
    /// Whether the session's model takes pictures; `None` when its provider's documentation does not say.
    pub vision: Option<bool>,
    /// Whether this workspace has been trusted to load project-local `AGENTS.md` / `.odg/skills/`.
    pub project_trusted: bool,
    /// Project-local configuration files withheld while `project_trusted` is false.
    pub untrusted_resources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub item: SessionItem,
    pub events: Vec<Envelope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandoned_brief: Option<String>,
}

#[derive(Serialize)]
pub struct SettingsView {
    pub settings: Settings,
    /// Each provider's key status, by provider id.
    pub keys: BTreeMap<String, KeyStatus>,
    /// Every provider there is to choose from: the first of each kind, and those the user added.
    pub providers: Vec<Instance>,
    /// The kinds of provider, which one is added as.
    pub kinds: &'static [Provider],
    pub efforts: &'static [&'static str],
    pub home: String,
    /// The command approvals given for good, by workspace.
    pub grants: Grants,
    /// The engines `web_search` can ask, `(id, what it is)`, and whether a GLM key is saved for it to work.
    pub search_engines: &'static [(&'static str, &'static str)],
    pub search_ready: bool,
    /// The image models `generate_image` can ask, `(id, name, provider)`, and the one in force, if any provider has a key.
    pub painters: Vec<(&'static str, &'static str, &'static str)>,
    pub painter_ready: Option<&'static str>,
    /// The user's own skills, and the directory they are kept in.
    pub skills: Vec<odg_core::events::SkillInfo>,
    pub skills_directory: String,
    /// The folder kept for conversations (闲谈), which a session there works in.
    pub chat_folder: String,
    /// The context window of the model new sessions use, and how it is known.
    pub context_window: settings::Window,
    /// What confines commands on this system when the user switches that on, or why nothing can.
    pub sandbox_support: Result<&'static str, String>,
    /// The text of `mcp.json`, and how each server it lists stands.
    pub mcp_config: String,
    pub mcp_servers: Vec<ServerStatus>,
    /// Static build version (`CARGO_PKG_VERSION`) so Settings always displays it even offline.
    pub version: &'static str,
}

/// Where Anthropic's API is: `ANTHROPIC_BASE_URL`, or the official address.
pub fn anthropic_base_url() -> String {
    std::env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

pub fn safe_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

struct WorkspaceItemCache {
    branches: HashMap<String, Option<String>>,
    untrusted: HashMap<String, Vec<String>>,
}

impl WorkspaceItemCache {
    fn new() -> Self {
        Self {
            branches: HashMap::new(),
            untrusted: HashMap::new(),
        }
    }

    fn branch(&mut self, workspace: &str) -> Option<String> {
        self.branches
            .entry(workspace.to_string())
            .or_insert_with(|| odg_core::tools::git_branch(Path::new(workspace)))
            .clone()
    }

    fn untrusted_resources(&mut self, workspace: &str) -> Vec<String> {
        self.untrusted
            .entry(workspace.to_string())
            .or_insert_with(|| odg_core::agent::untrusted_project_resources(Path::new(workspace)))
            .clone()
    }
}

impl AppCore<Router> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        home: PathBuf,
        settings: Arc<SettingsStore>,
        grants: Arc<GrantStore>,
        schedules: ScheduleStore,
        mcp: Arc<McpHub>,
        lsp: Arc<odg_core::lsp::LspHub>,
        pty: Arc<pty::PtyHub>,
        watcher: Arc<watcher::WorkspaceWatcher>,
        llm: Arc<Router>,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            store: SessionStore::new(home.join("sessions")),
            home,
            settings,
            grants,
            schedules,
            mcp,
            lsp,
            pty,
            watcher,
            model_usage: UsageStoreRef::Global(ModelUsageStore::global()),
            llm,
            events,
            runtime,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            active_id: Mutex::new(None),
            access_seq: AtomicU64::new(1),
        }
    }
}

impl<L: Llm + 'static> AppCore<L> {
    #[cfg(test)]
    pub fn new_for_test(
        home: PathBuf,
        llm: Arc<L>,
        events: UnboundedSender<UiEvent>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&home);
        let settings = Arc::new(SettingsStore::new(&home));
        let grants = Arc::new(GrantStore::new(home.join("grants.json")));
        let schedules = ScheduleStore::new(home.join("schedules.json"));
        let mcp = Arc::new(McpHub::new(home.join("mcp.json"), runtime.clone()));
        let lsp = Arc::new(odg_core::lsp::LspHub::new(home.join("lsp")));
        let pty = Arc::new(pty::PtyHub::new());
        let watcher = Arc::new(watcher::WorkspaceWatcher::new());
        let model_usage = UsageStoreRef::Local(ModelUsageStore::at(home.join("model-usage.json")));
        Arc::new(Self {
            store: SessionStore::new(home.join("sessions")),
            home,
            settings,
            grants,
            schedules,
            mcp,
            lsp,
            pty,
            watcher,
            model_usage,
            llm,
            events,
            runtime,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            active_id: Mutex::new(None),
            access_seq: AtomicU64::new(1),
        })
    }

    pub fn chat_folder(&self) -> PathBuf {
        self.home.join("闲谈")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.home.join("skills")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.home.join("runtime")
    }

    pub fn pastes_dir(&self) -> PathBuf {
        self.home.join("pastes")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.home.join("tmp").join("versions")
    }

    fn next_access(&self) -> u64 {
        self.access_seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn set_active_session(&self, id: Option<&str>) {
        *self.active_id.lock_unpoisoned() = id.map(str::to_string);
    }

    fn insert_loaded(&self, session: Session<L>) {
        let id = session.id().to_string();
        let stamp = self.next_access();
        self.sessions.lock_unpoisoned().insert(
            id,
            SessionSlot {
                cell: Arc::new(OnceCell::new_with(Some(session))),
                last_access: Arc::new(AtomicU64::new(stamp)),
            },
        );
        self.evict_idle_lru();
    }

    /// Evict least-recently accessed idle sessions when resident idle sessions exceed [`MAX_IDLE_SESSIONS`].
    /// Never evicts the currently active session, sessions with running turns/subagents, or sessions with running background jobs.
    pub fn evict_idle_lru(&self) {
        let active = self.active_id.lock_unpoisoned().clone();
        let mut sessions = self.sessions.lock_unpoisoned();
        let mut total_idle = 0usize;
        let mut candidates: Vec<(u64, String)> = Vec::new();

        for (id, slot) in sessions.iter() {
            let Some(session) = slot.cell.get() else { continue };
            let is_idle = session.status() == Status::Idle
                && session.children().iter().all(|c| c.status() == Status::Idle)
                && !session.jobs().list().iter().any(|j| j.running);
            if !is_idle {
                continue;
            }
            total_idle += 1;
            if active.as_deref() != Some(id.as_str()) {
                candidates.push((slot.last_access.load(Ordering::Relaxed), id.clone()));
            }
        }

        if total_idle <= MAX_IDLE_SESSIONS {
            return;
        }
        candidates.sort_by_key(|(ts, _)| *ts);
        let to_evict = (total_idle - MAX_IDLE_SESSIONS).min(candidates.len());
        for (_, id) in candidates.into_iter().take(to_evict) {
            sessions.remove(&id);
        }
    }

    #[cfg(test)]
    pub fn loaded_session_ids(&self) -> HashSet<String> {
        self.sessions
            .lock_unpoisoned()
            .iter()
            .filter_map(|(id, slot)| slot.cell.get().map(|_| id.clone()))
            .collect()
    }

    pub fn get_loaded(&self, id: &str) -> Option<Session<L>> {
        let sessions = self.sessions.lock_unpoisoned();
        let slot = sessions.get(id)?;
        let session = slot.cell.get()?.clone();
        slot.last_access.store(self.next_access(), Ordering::Relaxed);
        Some(session)
    }

    pub fn wrap(&self, log: SessionLog) -> Result<Session<L>, String> {
        let store = self.settings.clone();
        let tuning = move |provider: &str, model: &str| {
            let settings = store.load();
            let window = store.context_window(&settings, provider, model).tokens;
            let (guardian, approval_wait) = (settings.guardian_model(provider, model), settings.approval_wait());
            Tuning {
                context_limit: settings.context_limit_within(window),
                context_window: window,
                effort: settings.effort,
                guardian,
                approval_wait,
            }
        };
        let searching = self.settings.clone();
        let painting = self.settings.clone();
        let confining = self.settings.clone();
        let services = Services {
            grants: self.grants.clone(),
            mcp: Some(self.mcp.clone()),
            skills: Some(self.skills_dir()),
            search: Some(Arc::new(move || searching.search_mode())),
            sandbox: Some(Arc::new(move || confining.sandbox())),
            chat: Some(self.chat_folder()),
            runtime: Some(self.runtime_dir()),
            painter: Some(Arc::new(move || painting.painter())),
            lsp: Some(self.lsp.clone()),
        };
        let session = Session::new(
            log,
            self.llm.clone(),
            Arc::new(tuning),
            services,
            self.events.clone(),
            self.runtime.clone(),
        )
        .map_err(|error| error.to_string())?;

        let mem_home = self.home.clone();
        session.set_memory_source(Arc::new(move || Some(odg_core::memory::memory_dir(&mem_home))));

        if let Some(main) = session.aside().map(str::to_string) {
            let (sessions, store) = (self.sessions.clone(), self.store.clone());
            session.set_brief_source(Arc::new(move || {
                let live = sessions
                    .lock_unpoisoned()
                    .get(&main)
                    .and_then(|slot| slot.cell.get().cloned());
                let (title, status, events) = match live {
                    Some(main) => (main.summary().title, main.status(), main.snapshot()),
                    None => {
                        let events = store.read(&main).ok()?;
                        (odg_core::session::summarize(&events).title, Status::Idle, events)
                    }
                };
                Some(odg_core::aside::brief(&title, status, &events))
            }));
        }
        Ok(session)
    }

    /// Open or retrieve session `id` using a per-id `OnceCell` slot + `spawn_blocking` so reading/repairing
    /// a multi-megabyte JSONL log never blocks other sessions or Tokio worker threads.
    pub async fn live(self: &Arc<Self>, id: &str) -> Result<Session<L>, String> {
        let cell = {
            let mut sessions = self.sessions.lock_unpoisoned();
            if let Some(slot) = sessions.get(id)
                && let Some(existing) = slot.cell.get()
            {
                slot.last_access.store(self.next_access(), Ordering::Relaxed);
                return Ok(existing.clone());
            }
            if let Some(child) = sessions
                .values()
                .filter_map(|slot| slot.cell.get())
                .find_map(|session| session.child(id))
            {
                let stamp = self.next_access();
                sessions.insert(
                    id.to_string(),
                    SessionSlot {
                        cell: Arc::new(OnceCell::new_with(Some(child.clone()))),
                        last_access: Arc::new(AtomicU64::new(stamp)),
                    },
                );
                drop(sessions);
                self.evict_idle_lru();
                return Ok(child);
            }
            let stamp = self.next_access();
            let entry = sessions.entry(id.to_string()).or_insert_with(|| SessionSlot {
                cell: Arc::new(OnceCell::new()),
                last_access: Arc::new(AtomicU64::new(stamp)),
            });
            entry.last_access.store(stamp, Ordering::Relaxed);
            entry.cell.clone()
        };

        let id_owned = id.to_string();
        let core = self.clone();
        let result = cell
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || {
                    let log = core.store.open(&id_owned).map_err(|error| error.to_string())?;
                    core.wrap(log)
                })
                .await
                .map_err(|error| error.to_string())?
            })
            .await;

        match result {
            Ok(session) => {
                let out = session.clone();
                self.evict_idle_lru();
                Ok(out)
            }
            Err(error) => {
                let mut sessions = self.sessions.lock_unpoisoned();
                if sessions.get(id).is_some_and(|slot| Arc::ptr_eq(&slot.cell, &cell)) {
                    sessions.remove(id);
                }
                Err(error)
            }
        }
    }

    /// Clone all currently loaded `Session` handles in a short critical section and immediately release the table lock.
    pub fn loaded_sessions(&self) -> Vec<Session<L>> {
        self.sessions
            .lock_unpoisoned()
            .values()
            .filter_map(|slot| slot.cell.get().cloned())
            .collect()
    }

    /// Clone a single currently loaded `Session` handle by id, if present.
    pub fn loaded_session(&self, id: &str) -> Option<Session<L>> {
        self.sessions
            .lock_unpoisoned()
            .get(id)
            .and_then(|slot| slot.cell.get().cloned())
    }

    /// Every loaded session by id: those opened here, and the subagents their agents have set off.
    pub fn all_live(&self) -> HashMap<String, Session<L>> {
        let mut live: HashMap<String, Session<L>> = self
            .sessions
            .lock_unpoisoned()
            .iter()
            .filter_map(|(id, slot)| slot.cell.get().cloned().map(|s| (id.clone(), s)))
            .collect();
        for child in live.values().flat_map(Session::children).collect::<Vec<_>>() {
            live.entry(child.id().to_string()).or_insert(child);
        }
        live
    }

    pub fn kill_all_jobs(&self) {
        for session in self.loaded_sessions() {
            session.kill_jobs();
        }
    }

    pub fn find_aside(&self, main: &str) -> Option<String> {
        let live = self.all_live();
        if let Some(id) = live
            .iter()
            .find(|(_, session)| session.aside() == Some(main))
            .map(|(id, _)| id.clone())
        {
            return Some(id);
        }
        self.store
            .ids()
            .into_iter()
            .filter(|id| !live.contains_key(id))
            .find(|id| {
                self.store
                    .peek_idle(id)
                    .is_ok_and(|summary| summary.aside.as_deref() == Some(main))
            })
    }

    pub fn is_aside(&self, id: &str, live: &HashMap<String, Session<L>>) -> bool {
        match live.get(id) {
            Some(session) => session.aside().is_some(),
            None => self.store.peek_idle(id).is_ok_and(|summary| summary.aside.is_some()),
        }
    }

    pub fn provider_usable(&self, settings: &Settings, provider: &str) -> bool {
        if !settings.instances().iter().any(|p| p.id == provider) {
            return false;
        }
        if settings.disabled_providers.iter().any(|d| d == provider) {
            return false;
        }
        if self.settings.key_status(provider).source != "none" {
            return true;
        }
        let kind = odg_core::providers::kind_id(provider);
        matches!(kind, "magpie" | "cpa" | "ollama")
            && (settings.provider == provider
                || settings.base_urls.contains_key(provider)
                || settings.provider_names.contains_key(provider)
                || provider != kind)
    }

    pub fn ensure_valid_model(&self, session: &Session<L>) {
        if session.status() != Status::Idle {
            return;
        }
        let settings = self.settings.load();
        let default_model = settings.model.trim();
        if default_model.is_empty() || !self.provider_usable(&settings, &settings.provider) {
            return;
        }
        let summary = session.summary();
        let missing_model = summary.model.trim().is_empty();
        let missing_provider = !self.provider_usable(&settings, &summary.provider);
        if (missing_provider || missing_model)
            && (summary.provider != settings.provider || summary.model != default_model)
        {
            let _ = session.set_model(&settings.provider, default_model);
        }
    }

    pub fn reconcile_live_models(&self) {
        for session in self.all_live().into_values() {
            self.ensure_valid_model(&session);
        }
    }

    fn item_with_context(
        &self,
        summary: SessionSummary,
        status: Status,
        settings: &Settings,
        grants: &Grants,
        ws_cache: &mut WorkspaceItemCache,
    ) -> SessionItem {
        let window = self
            .settings
            .context_window(settings, &summary.provider, &summary.model)
            .tokens;
        let context_limit = settings.context_limit_within(window);
        let branch = ws_cache.branch(&summary.workspace);
        let vision = vision(&summary.provider, &summary.model);
        let context_ceiling = window / 5 * 4;
        let ws_path = Path::new(&summary.workspace);
        let grant_root = summary
            .worktree
            .as_ref()
            .map_or_else(|| ws_path.to_path_buf(), |wt| PathBuf::from(&wt.origin));
        let is_chat = ws_path == self.chat_folder();
        let project_trusted = is_chat
            || grants
                .get(grant_root.to_string_lossy().as_ref())
                .is_some_and(|kept| kept.contains(PROJECT_TRUST_GRANT));
        let untrusted_resources = if project_trusted {
            Vec::new()
        } else {
            ws_cache.untrusted_resources(&summary.workspace)
        };
        SessionItem {
            summary,
            status,
            context_limit,
            context_ceiling,
            branch,
            vision,
            project_trusted,
            untrusted_resources,
        }
    }

    pub fn item(&self, summary: SessionSummary, status: Status) -> SessionItem {
        let settings = self.settings.load();
        let grants = self.grants.all();
        let mut ws_cache = WorkspaceItemCache::new();
        self.item_with_context(summary, status, &settings, &grants, &mut ws_cache)
    }

    pub fn view(&self, session: &Session<L>) -> SessionView {
        SessionView {
            item: self.item(session.summary(), session.status()),
            events: session.snapshot(),
            abandoned_brief: None,
        }
    }

    pub fn list_sessions_blocking(&self) -> Vec<SessionItem> {
        let settings = self.settings.load();
        let grants = self.grants.all();
        let live = self.all_live();
        let mut ws_cache = WorkspaceItemCache::new();

        let mut items: Vec<SessionItem> = self
            .store
            .ids()
            .into_iter()
            .filter_map(|id| match live.get(&id) {
                Some(session) => {
                    Some(self.item_with_context(session.summary(), session.status(), &settings, &grants, &mut ws_cache))
                }
                None => self
                    .store
                    .peek_idle(&id)
                    .ok()
                    .map(|summary| self.item_with_context(summary, Status::Idle, &settings, &grants, &mut ws_cache)),
            })
            .filter(|item| item.summary.aside.is_none())
            .collect();
        items.sort_by(|a, b| b.summary.updated_at.cmp(&a.summary.updated_at));
        items
    }

    pub fn get_session_item_blocking(&self, id: &str) -> Result<SessionItem, String> {
        let live = self.all_live();
        match live.get(id) {
            Some(session) => Ok(self.item(session.summary(), session.status())),
            None => {
                let summary = self.store.peek_idle(id).map_err(|e| e.to_string())?;
                Ok(self.item(summary, Status::Idle))
            }
        }
    }

    pub fn search_sessions_blocking(&self, query: &str) -> Vec<String> {
        let live = self.all_live();
        self.store
            .ids()
            .into_iter()
            .filter(|id| !self.is_aside(id, &live))
            .filter(|id| match live.get(id) {
                Some(session) => odg_core::session::matches(&session.snapshot(), query),
                None => self.store.search(id, query),
            })
            .collect()
    }

    pub fn start(&self, workspace: PathBuf, isolated: bool, schedule: Option<&str>) -> Result<Session<L>, String> {
        if !workspace.is_dir() {
            return Err(format!("{} is not a directory", workspace.display()));
        }
        let settings = self.settings.load();
        if settings.model.is_empty() {
            let name = settings
                .instances()
                .into_iter()
                .find(|known| known.id == settings.provider)
                .map_or_else(|| settings.provider.clone(), |known| known.name);
            return Err(format!("{name} has no model chosen: enter a model id in settings"));
        }
        let (workspace, made) = match isolated {
            true => {
                let (workspace, made) = worktree::create(&workspace, &self.home.join("worktrees"))?;
                (workspace, Some(made))
            }
            false => (workspace, None),
        };
        let new = NewSession {
            workspace: &workspace,
            provider: &settings.provider,
            model: &settings.model,
            parent: None,
            worktree: made.clone(),
            schedule,
            aside: None,
        };
        let log = self.store.create_with(new).map_err(|error| {
            if let Some(made) = &made {
                let _ = worktree::remove(made);
            }
            error.to_string()
        })?;
        let session = self.wrap(log)?;
        session.set_mode(settings.mode);
        self.insert_loaded(session.clone());
        Ok(session)
    }

    pub fn start_aside_for(&self, main: &Session<L>) -> Result<Session<L>, String> {
        let id = main.id();
        let summary = main.summary();
        if summary.parent.is_some() || summary.aside.is_some() {
            return Err("only a session the user started can have a side conversation".to_string());
        }
        let new = NewSession {
            workspace: Path::new(&summary.workspace),
            provider: &summary.provider,
            model: &summary.model,
            parent: None,
            worktree: None,
            schedule: None,
            aside: Some(id),
        };
        let log = self.store.create_with(new).map_err(|error| error.to_string())?;
        let session = self.wrap(log)?;
        session.set_mode(Mode::Auto);
        if let Some(effort) = summary.effort {
            let _ = session.set_effort(&effort);
        }
        self.insert_loaded(session.clone());
        Ok(session)
    }

    pub fn reset_aside_blocking(&self, main_id: &str) -> Result<(), String> {
        let Some(aside) = self.find_aside(main_id) else {
            return Ok(());
        };
        let mut sessions = self.sessions.lock_unpoisoned();
        if sessions
            .get(&aside)
            .and_then(|slot| slot.cell.get())
            .is_some_and(|session| session.status() != Status::Idle)
        {
            return Err("the side conversation is answering; stop it before starting it over".to_string());
        }
        if let Some(slot) = sessions.remove(&aside)
            && let Some(session) = slot.cell.get()
        {
            session.kill_jobs();
        }
        drop(sessions);
        let safe = safe_session_id(&aside);
        if !safe.is_empty() {
            let _ = std::fs::remove_dir_all(self.versions_dir().join(safe));
        }
        self.store.delete(&aside).map_err(|error| error.to_string())
    }

    pub fn fork_from(&self, source: &Session<L>, before: Option<u64>) -> Result<SessionView, String> {
        let source_events = source.snapshot();
        let abandoned_brief = before.and_then(|turn| odg_core::export::abandoned_branch_brief(&source_events, turn));
        let log = self
            .store
            .fork(&source_events, before)
            .map_err(|error| error.to_string())?;
        let session = self.wrap(log)?;
        self.set_active_session(Some(session.id()));
        self.insert_loaded(session.clone());
        let mut out = self.view(&session);
        out.abandoned_brief = abandoned_brief;
        Ok(out)
    }

    pub fn delete_session_blocking(&self, id: &str) -> Result<(), String> {
        let live = self.all_live();
        let summary_of = |target: &str| match live.get(target) {
            Some(session) => Some(session.summary()),
            None => self.store.peek_idle(target).ok(),
        };
        let made = summary_of(id)
            .filter(|summary| summary.parent.is_none())
            .and_then(|summary| summary.worktree)
            .filter(|made| {
                !self
                    .store
                    .ids()
                    .iter()
                    .filter(|other| other.as_str() != id)
                    .filter_map(|other| summary_of(other))
                    .any(|other| other.parent.is_none() && other.worktree.as_ref() == Some(made))
            });

        let all_ids = self.store.ids();
        let mut removed = HashSet::from([id.to_string()]);
        let mut subagents: Vec<String> = Vec::new();
        loop {
            let mut added = false;
            for other in &all_ids {
                if removed.contains(other) {
                    continue;
                }
                let belongs = match live.get(other) {
                    Some(session) => {
                        session.parent().is_some_and(|p| removed.contains(p))
                            || session.aside().is_some_and(|a| removed.contains(a))
                    }
                    None => self.store.peek_idle(other).is_ok_and(|summary| {
                        summary.parent.as_deref().is_some_and(|p| removed.contains(p))
                            || summary.aside.as_deref().is_some_and(|a| removed.contains(a))
                    }),
                };
                if belongs {
                    removed.insert(other.clone());
                    subagents.push(other.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        drop(live);

        let mut sessions = self.sessions.lock_unpoisoned();
        let busy = |session: &Session<L>| {
            session.status() != Status::Idle || session.children().iter().any(|child| child.status() != Status::Idle)
        };
        if std::iter::once(id)
            .chain(subagents.iter().map(String::as_str))
            .filter_map(|sid| sessions.get(sid).and_then(|slot| slot.cell.get()))
            .any(busy)
        {
            return Err(
                "the agent of this session, one of its subagents, or its side conversation is working; stop it before deleting the session"
                    .to_string(),
            );
        }

        let versions_root = self.versions_dir();
        for sid in std::iter::once(id).chain(subagents.iter().map(String::as_str)) {
            self.pty.close_session(sid);
            if let Some(slot) = sessions.remove(sid)
                && let Some(session) = slot.cell.get()
            {
                session.kill_jobs();
            }
            let safe = safe_session_id(sid);
            if !safe.is_empty() {
                let preview_dir = versions_root.join(safe);
                if preview_dir.exists() {
                    let _ = std::fs::remove_dir_all(&preview_dir);
                }
            }
            self.store.delete(sid).map_err(|error| error.to_string())?;
        }
        drop(sessions);

        {
            let mut active = self.active_id.lock_unpoisoned();
            if active.as_deref().is_some_and(|cur| removed.contains(cur)) {
                *active = None;
            }
        }

        match made {
            Some(made) => worktree::remove(&made).map_err(|error| {
                format!(
                    "the session is deleted, but its worktree at {} could not be removed: {error}",
                    made.checkout
                )
            }),
            None => Ok(()),
        }
    }

    pub fn fire(&self, schedule: &Schedule) -> Run {
        let at = schedule::stamp(&schedule::now().to_utc());
        let last = schedule.last.as_ref().and_then(|run| run.session.clone());
        if let Some(busy) = last.filter(|id| {
            self.all_live()
                .get(id)
                .is_some_and(|session| session.status() != Status::Idle)
        }) {
            return Run {
                at,
                session: Some(busy),
                error: Some("the session it started last is still working, so this time was passed over".to_string()),
            };
        }
        match self.start(
            PathBuf::from(&schedule.workspace),
            schedule.isolated,
            Some(&schedule.name),
        ) {
            Ok(session) => {
                session.set_mode(schedule.mode);
                let _ = session.rename(&schedule.name);
                session.send(schedule.prompt.clone());
                Run {
                    at,
                    session: Some(session.id().to_string()),
                    error: None,
                }
            }
            Err(error) => Run {
                at,
                session: None,
                error: Some(error),
            },
        }
    }

    pub fn prompt_roots_for_session(&self, id: Option<&str>) -> (PathBuf, Option<PathBuf>) {
        let user_dir = self.home.join("prompts");
        let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) else {
            return (user_dir, None);
        };
        let summary = self
            .all_live()
            .get(id)
            .map(Session::summary)
            .or_else(|| self.store.peek_idle(id).ok());
        let Some(summary) = summary else {
            return (user_dir, None);
        };
        let ws_path = PathBuf::from(&summary.workspace);
        if ws_path == self.chat_folder() {
            return (user_dir, None);
        }
        let grant_root = summary
            .worktree
            .as_ref()
            .map_or_else(|| ws_path.clone(), |wt| PathBuf::from(&wt.origin));
        if self.grants.is_trusted_workspace(&grant_root) {
            (user_dir, Some(ws_path.join(".odg").join("prompts")))
        } else {
            (user_dir, None)
        }
    }

    pub async fn session_workspace(self: &Arc<Self>, id: &str) -> Result<PathBuf, String> {
        Ok(PathBuf::from(self.live(id).await?.summary().workspace))
    }

    pub fn settings_view(&self) -> SettingsView {
        let settings = self.settings.load();
        let context_window = self
            .settings
            .context_window(&settings, &settings.provider, &settings.model);
        let skills_dir = self.skills_dir();
        SettingsView {
            providers: settings.instances(),
            settings,
            keys: self.settings.key_statuses(),
            kinds: PROVIDERS,
            efforts: EFFORTS,
            home: self.home.display().to_string(),
            grants: self.grants.all(),
            search_engines: odg_core::web::SEARCH_ENGINES,
            search_ready: self.settings.search().is_some(),
            painters: odg_core::imagegen::PAINTERS
                .iter()
                .map(|painter| (painter.id, painter.name, painter.provider))
                .collect(),
            painter_ready: self.settings.painter().map(|config| config.painter.id),
            skills: odg_core::skills::listing(&odg_core::skills::discover(std::slice::from_ref(&skills_dir))),
            skills_directory: skills_dir.display().to_string(),
            chat_folder: self.chat_folder().display().to_string(),
            context_window,
            sandbox_support: odg_core::sandbox::support(),
            mcp_config: self.mcp.config_text(),
            mcp_servers: self.mcp.statuses(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

const STREAM_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(12);

fn is_coalesceable_stream(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::Stream {
            delta: odg_core::llm::StreamDelta::TextDelta { .. } | odg_core::llm::StreamDelta::ThinkingDelta { .. },
            ..
        }
    )
}

fn push_or_coalesce(batch: &mut Vec<UiEvent>, next: UiEvent) {
    use odg_core::llm::StreamDelta;
    if let Some(UiEvent::Stream {
        session_id: prev_sid,
        delta: prev_delta,
    }) = batch.last_mut()
        && let UiEvent::Stream {
            session_id: ref next_sid,
            delta: ref next_delta,
        } = next
        && prev_sid == next_sid
    {
        match (prev_delta, next_delta) {
            (StreamDelta::TextDelta { index: i1, text: acc }, StreamDelta::TextDelta { index: i2, text: more })
            | (
                StreamDelta::ThinkingDelta { index: i1, text: acc },
                StreamDelta::ThinkingDelta { index: i2, text: more },
            ) if i1 == i2 => {
                acc.push_str(more);
                return;
            }
            _ => {}
        }
    }
    batch.push(next);
}

/// Forward `UiEvent`s to `emit`, coalescing consecutive same-session same-index `TextDelta` /
/// `ThinkingDelta` chunks within a 12ms window while flushing non-coalesceable events (`Reset`,
/// `ToolUseStart`, `Log`, `Status`) immediately in strict arrival order.
pub(crate) async fn forward_session_events<F>(mut receiver: tokio::sync::mpsc::UnboundedReceiver<UiEvent>, mut emit: F)
where
    F: FnMut(UiEvent) + Send + 'static,
{
    let mut batch: Vec<UiEvent> = Vec::with_capacity(16);
    while let Some(first) = receiver.recv().await {
        if !is_coalesceable_stream(&first) {
            emit(first);
            continue;
        }
        batch.clear();
        batch.push(first);
        let deadline = tokio::time::Instant::now() + STREAM_COALESCE_WINDOW;
        let mut closed = false;

        'window: loop {
            while let Ok(ev) = receiver.try_recv() {
                let mergeable = is_coalesceable_stream(&ev);
                push_or_coalesce(&mut batch, ev);
                if !mergeable {
                    break 'window;
                }
            }
            match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(Some(ev)) => {
                    let mergeable = is_coalesceable_stream(&ev);
                    push_or_coalesce(&mut batch, ev);
                    if !mergeable {
                        break 'window;
                    }
                }
                Ok(None) => {
                    closed = true;
                    break 'window;
                }
                Err(_) => break 'window,
            }
        }

        for ev in batch.drain(..) {
            emit(ev);
        }
        if closed {
            break;
        }
    }
}
