//! User settings and the stored API keys.
//!
//! Settings are a file under the home directory. Keys go to the system's
//! credential manager when the store was given one ([`SystemVault`]): the
//! Windows Credential Manager, the macOS Keychain, or the Secret Service on
//! Linux. Without one, or when it does not work, they are kept in
//! `credentials.json` beside the settings, in plain text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::anthropic::DEFAULT_MODEL;
use crate::fsutil::write_atomically;
use crate::llm::Endpoint;
use crate::policy::Mode;
use crate::providers::{DEFAULT_PROVIDER, Instance, Listed, PROVIDERS, Provider, UNKNOWN_CONTEXT_WINDOW, documented_window, instance, is_added, provider};
use crate::web::{INTERNATIONAL_ENGINE, SEARCH_ENGINES, SearchConfig};

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const THEMES: &[&str] = &["system", "light", "dark"];
/// Most models here hold a million tokens, but every request sends the whole
/// conversation, so compacting long before the window is full keeps a long
/// session affordable.
pub const DEFAULT_CONTEXT_LIMIT: u64 = 200_000;
/// In auto mode, how many seconds a request for approval waits for an answer before it is declined by itself.
pub const DEFAULT_APPROVAL_TIMEOUT: u64 = 30;
/// Below this a summary and one file read would already be past the limit.
pub const MIN_CONTEXT_LIMIT: u64 = 20_000;
/// The smallest context window the user can set for a model: one whose four fifths is the lowest limit.
pub const MIN_CONTEXT_WINDOW: u64 = MIN_CONTEXT_LIMIT / 4 * 5;

/// The folder kept for conversations (闲谈), in the home directory: a session there is not work on a project.
pub fn chat_folder() -> PathBuf {
    home().join("闲谈")
}

/// The directory holding sessions, settings, and credentials: `$ODG_HOME`, or `~/.odg`.
pub fn home() -> PathBuf {
    std::env::var_os("ODG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".odg")))
        .unwrap_or_else(|| PathBuf::from(".odg"))
}

fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}

fn default_context_limit() -> u64 {
    DEFAULT_CONTEXT_LIMIT
}

fn default_approval_timeout() -> u64 {
    DEFAULT_APPROVAL_TIMEOUT
}

fn default_context_auto() -> bool {
    true
}

fn default_guardian() -> bool {
    true
}

fn default_search_engine() -> String {
    SEARCH_ENGINES[0].0.to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Provider of sessions created from now on. Settings saved before there
    /// were providers have none, which reads as Anthropic.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model of sessions created from now on: any model id the provider
    /// serves. A session keeps the provider and model it started with.
    pub model: String,
    /// How hard the model works on each request.
    pub effort: String,
    /// The mode sessions created from now on start in. Settings saved before
    /// there were modes have none, which reads as edit mode.
    #[serde(default)]
    pub mode: Mode,
    /// A conversation is compacted into a summary once it holds about this
    /// many tokens, or sooner on a model whose context window is smaller.
    #[serde(default = "default_context_limit")]
    pub context_limit: u64,
    /// Compact at four fifths of each model's own context window, whatever
    /// `context_limit` says: as much of the conversation as the model holds.
    /// On unless a figure is set; settings saved before it was on by default
    /// keep what they say.
    #[serde(default = "default_context_auto")]
    pub context_auto: bool,
    /// Which of Zhipu's search engines `web_search` asks; see [`SEARCH_ENGINES`].
    #[serde(default = "default_search_engine")]
    pub search_engine: String,
    /// The image model `generate_image` asks, one of [`crate::imagegen::PAINTERS`]; empty for the first whose
    /// provider has a key. See [`SettingsStore::painter`].
    #[serde(default)]
    pub painter: String,
    /// `system`, `light`, or `dark`.
    pub theme: String,
    /// Addresses that replace a provider's default, by provider id; for an
    /// international site, a gateway, or the custom provider. Anthropic's
    /// address comes from `ANTHROPIC_BASE_URL` instead.
    #[serde(default)]
    pub base_urls: BTreeMap<String, String>,
    /// The names the user gave providers, by provider id. Every provider the
    /// user added beside the first of its kind (`<kind>-<n>`) is here: that
    /// is what keeps it in the list.
    #[serde(default)]
    pub provider_names: BTreeMap<String, String>,
    /// Whether the commands the agent runs are confined to writing under the
    /// workspace; see [`crate::sandbox`]. Has no effect where the system cannot confine them.
    #[serde(default)]
    pub sandbox: bool,
    /// Directories confined commands may also write under, besides the
    /// workspace and the temporary directories: a build cache, say.
    #[serde(default)]
    pub sandbox_paths: Vec<String>,
    /// Context windows the user set by hand, in tokens, by `<provider>/<model>`:
    /// for a model whose provider does not say and that this build does not
    /// know, or one the user knows better. See [`SettingsStore::context_window`].
    #[serde(default)]
    pub context_windows: BTreeMap<String, u64>,
    /// In auto mode, have the guardian, a model, judge the commands the rules cannot pass before the
    /// user is asked. See [`crate::agent`]. On unless switched off; settings saved before it was on by
    /// default keep what they say.
    #[serde(default = "default_guardian")]
    pub guardian: bool,
    /// The model the guardian is, by provider id; a provider not listed uses the session's own model.
    #[serde(default)]
    pub guardian_models: BTreeMap<String, String>,
    /// In auto mode, the seconds a request for approval waits before it is declined by itself; 0 waits for good.
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout: u64,
}

impl Default for Settings {
    fn default() -> Self {
        // Coding responds strongly to effort, and this model's own default is one level lower.
        Self {
            provider: default_provider(),
            model: DEFAULT_MODEL.to_string(),
            effort: "high".to_string(),
            mode: Mode::Edit,
            context_limit: DEFAULT_CONTEXT_LIMIT,
            context_auto: true,
            search_engine: default_search_engine(),
            painter: String::new(),
            theme: "system".to_string(),
            base_urls: BTreeMap::new(),
            provider_names: BTreeMap::new(),
            sandbox: false,
            sandbox_paths: Vec::new(),
            context_windows: BTreeMap::new(),
            guardian: true,
            guardian_models: BTreeMap::new(),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }
}

impl Settings {
    /// Replace any value this build does not offer with its default.
    fn valid(mut self) -> Self {
        let default = Settings::default();
        self.provider_names = std::mem::take(&mut self.provider_names)
            .into_iter()
            .map(|(id, name)| (id, name.trim().to_string()))
            .filter(|(id, name)| !name.is_empty() && provider(id).is_some())
            .collect();
        // One the user added is known while it is in the list; taken out, nothing of it is kept.
        let listed = |id: &str| !is_added(id) || self.provider_names.contains_key(id);
        let chosen = match provider(&self.provider).filter(|_| listed(&self.provider)) {
            Some(chosen) => chosen,
            None => {
                self.provider = default.provider;
                provider(DEFAULT_PROVIDER).expect("the default provider exists")
            }
        };
        self.model = self.model.trim().to_string();
        // A model another provider offers, left over from switching provider, is replaced too; but a compatible
        // service, which lists no models of its own, may well serve another provider's.
        let belongs_elsewhere = !chosen.models.is_empty()
            && PROVIDERS.iter().any(|other| other.id != chosen.id && other.models.iter().any(|(id, _)| *id == self.model))
            && !chosen.models.iter().any(|(id, _)| *id == self.model);
        if self.model.is_empty() || belongs_elsewhere {
            self.model = chosen.models.first().map(|(id, _)| id.to_string()).unwrap_or_default();
        }
        if !EFFORTS.contains(&self.effort.as_str()) {
            self.effort = default.effort;
        }
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = default.theme;
        }
        let names = self.provider_names.clone();
        let listed = |id: &str| !is_added(id) || names.contains_key(id);
        self.guardian_models.retain(|provider, model| !provider.is_empty() && !model.trim().is_empty() && listed(provider));
        for model in self.guardian_models.values_mut() {
            *model = model.trim().to_string();
        }
        // Asking about nothing is switched on in one session by hand, never set for every new one.
        if self.mode == Mode::Bypass {
            self.mode = default.mode;
        }
        self.context_limit = self.context_limit.max(MIN_CONTEXT_LIMIT);
        // A window of nothing is one that was cleared; one too small to work in is raised to the smallest that does.
        self.context_windows.retain(|key, tokens| *tokens > 0 && key.split_once('/').is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty()));
        for tokens in self.context_windows.values_mut() {
            *tokens = (*tokens).max(MIN_CONTEXT_WINDOW);
        }
        if !SEARCH_ENGINES.iter().any(|(id, _)| *id == self.search_engine) {
            self.search_engine = default.search_engine;
        }
        if crate::imagegen::painter(&self.painter).is_none() {
            self.painter = default.painter;
        }
        self.base_urls = std::mem::take(&mut self.base_urls)
            .into_iter()
            .map(|(id, url)| (id, url.trim().trim_end_matches('/').to_string()))
            // The first Anthropic provider's address comes from the environment; every other has its own.
            .filter(|(id, url)| !url.is_empty() && listed(id) && provider(id).is_some() && id != DEFAULT_PROVIDER)
            .collect();
        self
    }

    /// Every provider there is to choose from: the first of each kind, then
    /// those the user added, each kind's together, by the order they were added.
    pub fn instances(&self) -> Vec<Instance> {
        let number = |id: &str| id.rsplit_once('-').and_then(|(_, n)| n.parse::<u32>().ok()).unwrap_or(0);
        PROVIDERS
            .iter()
            .flat_map(|kind| {
                let mut added: Vec<&String> = self.provider_names.keys().filter(|id| is_added(id) && provider(id).is_some_and(|of| of.id == kind.id)).collect();
                added.sort_by_key(|id| number(id));
                std::iter::once(kind.id.to_string()).chain(added.into_iter().cloned())
            })
            .filter_map(|id| instance(&id, self.provider_names.get(&id).map(String::as_str)))
            .collect()
    }
}

impl Settings {
    /// The limit a session on this model compacts at: the one set here, or
    /// four fifths of the model's context window when that is less, which
    /// leaves room for the answer and for what the next tools return.
    /// The model the guardian is for a session on `model` of `provider`; `None` while the guardian is off.
    pub fn guardian_model(&self, provider: &str, model: &str) -> Option<String> {
        self.guardian.then(|| self.guardian_models.get(provider).cloned().unwrap_or_else(|| model.to_string()))
    }

    /// How long a request for approval waits in auto mode; `None` for good.
    pub fn approval_wait(&self) -> Option<std::time::Duration> {
        (self.approval_timeout > 0).then(|| std::time::Duration::from_secs(self.approval_timeout))
    }

    pub fn context_limit_within(&self, window: u64) -> u64 {
        let ceiling = window / 5 * 4;
        if self.context_auto { ceiling } else { self.context_limit.min(ceiling) }
    }
}

/// Where the key used for a provider's requests comes from, with a preview that reveals only its ends.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyStatus {
    /// `saved`, `env`, or `none`.
    pub source: String,
    pub preview: Option<String>,
    /// The saved key sits in `credentials.json` in plain text, not in the system's credential manager.
    pub plaintext: bool,
}

/// A place that keeps secrets by name.
pub trait Vault: Send + Sync {
    /// The secret, or `None` when there is none by that name.
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, secret: &str) -> Result<(), String>;
    /// Removing a secret that is not there succeeds.
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The system's credential manager, through the `keyring` crate.
pub struct SystemVault;

impl SystemVault {
    /// The name every entry is filed under in the credential manager.
    const SERVICE: &'static str = "OmniDoppelganger";

    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(Self::SERVICE, name).map_err(|error| error.to_string())
    }
}

impl Vault for SystemVault {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match Self::entry(name)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, name: &str, secret: &str) -> Result<(), String> {
        Self::entry(name)?.set_password(secret).map_err(|error| error.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// How it is known how many tokens a model's context window holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// The user set it by hand in the settings.
    Set,
    /// The provider said so when asked for its models.
    Provider,
    /// The provider's documentation said so when this build was made.
    Documented,
    /// Nobody said: a size small enough to be safe is assumed.
    Unknown,
}

/// A model's context window, and how it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Window {
    pub tokens: u64,
    pub source: WindowSource,
}

/// Context windows the providers reported, by provider id and model id.
type Windows = BTreeMap<String, BTreeMap<String, u64>>;

/// The single entry name under [`SystemVault::SERVICE`] holding all vaulted API keys as a JSON object.
/// Storing every provider's key in one entry avoids per-provider macOS Keychain ACL prompts.
const BUNDLE_KEY: &str = "api_keys";

/// Process-lifetime cache of the vaulted key bundle so repeated lookups never re-query the OS credential manager.
enum VaultCache {
    /// Not yet read from the vault in this process.
    Unloaded,
    /// Reading the vault failed (e.g. locked or user denied); reads fall back without re-prompting,
    /// while explicit writes or `migrate_keys` still retry the vault before touching it.
    Unavailable,
    /// Authoritative snapshot of the `api_keys` bundle in the vault.
    Loaded(BTreeMap<String, String>),
}

pub struct SettingsStore {
    directory: PathBuf,
    vault: Option<Arc<dyn Vault>>,
    vault_cache: Mutex<VaultCache>,
    /// What `models.json` holds, read once: it is asked for at every request.
    windows: Mutex<Option<Windows>>,
}

impl SettingsStore {
    /// A store that keeps keys in `credentials.json` under `directory`.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into(), vault: None, vault_cache: Mutex::new(VaultCache::Unloaded), windows: Mutex::new(None) }
    }

    /// The store for this run's home directory, as the desktop app and the
    /// command line both open it. Keys go to the system's credential manager,
    /// except in a home named by `ODG_HOME` (a test's, a portable one), which
    /// keeps them in its own file so it never reads or changes the keys of
    /// the user's everyday home. Keys still in the file are moved over.
    pub fn at_home() -> Self {
        let custom = std::env::var_os("ODG_HOME").is_some_and(|value| !value.is_empty());
        let store = if custom { Self::new(home()) } else { Self::with_vault(home(), Arc::new(SystemVault)) };
        store.migrate_keys();
        store
    }

    /// A store that keeps keys in `vault`, and in `credentials.json` only when the vault fails.
    pub fn with_vault(directory: impl Into<PathBuf>, vault: Arc<dyn Vault>) -> Self {
        Self { directory: directory.into(), vault: Some(vault), vault_cache: Mutex::new(VaultCache::Unloaded), windows: Mutex::new(None) }
    }

    fn parse_bundle(raw: &str) -> BTreeMap<String, String> {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .map(|object| {
                object
                    .into_iter()
                    .filter_map(|(name, value)| {
                        let key = value.as_str()?.trim().to_string();
                        (!key.is_empty()).then_some((name, key))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Move keys from `credentials.json` and from legacy per-provider vault entries (`<provider>_api_key`)
    /// into the single `api_keys` vault bundle, removing each migrated source once safely stored.
    /// Also primes the in-memory vault cache so subsequent reads do not touch the OS keychain again.
    pub fn migrate_keys(&self) {
        let Some(vault) = &self.vault else { return };
        let mut cache = self.vault_cache.lock().unwrap();
        let (mut bundle, had_bundle) = match vault.get(BUNDLE_KEY) {
            Ok(Some(raw)) => (Self::parse_bundle(&raw), true),
            Ok(None) => (BTreeMap::new(), false),
            Err(_) => {
                *cache = VaultCache::Unavailable;
                return;
            }
        };

        // One-time migration from pre-bundle per-provider keychain entries (`<id>_api_key`).
        // Only probed when `api_keys` does not exist yet, so normal startups do a single keychain read.
        let mut legacy_names: Vec<String> = Vec::new();
        if !had_bundle {
            for provider in self.load().instances() {
                let name = format!("{}_api_key", provider.id);
                if let Ok(Some(secret)) = vault.get(&name) {
                    let trimmed = secret.trim().to_string();
                    if !trimmed.is_empty() {
                        bundle.entry(name.clone()).or_insert(trimmed);
                    }
                    legacy_names.push(name);
                }
            }
        }

        // Merge any keys still sitting in `credentials.json`.
        let mut credentials = self.credentials();
        let mut from_file: Vec<String> = Vec::new();
        for (name, value) in &credentials {
            if let Some(key) = value.as_str().map(str::trim).filter(|key| !key.is_empty()) {
                bundle.insert(name.clone(), key.to_string());
                from_file.push(name.clone());
            }
        }

        let needs_write = !legacy_names.is_empty() || !from_file.is_empty();
        if needs_write {
            let Ok(encoded) = serde_json::to_string(&bundle) else { return };
            if vault.set(BUNDLE_KEY, &encoded).is_err() {
                *cache = VaultCache::Unavailable;
                return;
            }
            if !from_file.is_empty() {
                for name in &from_file {
                    credentials.remove(name);
                }
                let _ = self.write_credentials(credentials);
            }
            for name in &legacy_names {
                let _ = vault.delete(name);
            }
        }

        *cache = VaultCache::Loaded(bundle);
    }

    fn settings_path(&self) -> PathBuf {
        self.directory.join("settings.json")
    }

    /// `models.json`: the context windows the providers reported for their models.
    fn models_path(&self) -> PathBuf {
        self.directory.join("models.json")
    }

    /// Keep what a provider said of its models' context windows, so they are
    /// known from then on without asking again. Models it gave no size for
    /// keep whatever was known of them.
    pub fn record_models(&self, provider: &str, models: &[Listed]) -> std::io::Result<()> {
        let mut held = self.windows.lock().unwrap();
        let windows = held.get_or_insert_with(|| self.read_windows());
        let mut changed = false;
        for model in models {
            if let Some(tokens) = model.context_window
                && windows.entry(provider.to_string()).or_default().insert(model.id.clone(), tokens) != Some(tokens)
            {
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
        std::fs::create_dir_all(&self.directory)?;
        write_atomically(&self.models_path(), &serde_json::to_vec_pretty(&*windows).map_err(std::io::Error::other)?)
    }

    fn read_windows(&self) -> Windows {
        std::fs::read_to_string(self.models_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// How many tokens a model's context window holds: what the user set for
    /// it in `settings`, or what its provider reported when asked for its
    /// models, or what its documentation said, or when none of these is known
    /// a size small enough to be safe.
    pub fn context_window(&self, settings: &Settings, provider: &str, model: &str) -> Window {
        if let Some(tokens) = settings.context_windows.get(&format!("{provider}/{model}")) {
            return Window { tokens: *tokens, source: WindowSource::Set };
        }
        let mut held = self.windows.lock().unwrap();
        let reported = held.get_or_insert_with(|| self.read_windows()).get(provider).and_then(|models| models.get(model)).copied();
        match (reported, documented_window(provider, model)) {
            (Some(tokens), _) => Window { tokens, source: WindowSource::Provider },
            (None, Some(tokens)) => Window { tokens, source: WindowSource::Documented },
            (None, None) => Window { tokens: UNKNOWN_CONTEXT_WINDOW, source: WindowSource::Unknown },
        }
    }

    /// `credentials.json`: one `<provider>_api_key` field per saved key, so a
    /// file written when only Anthropic existed reads unchanged.
    fn credentials_path(&self) -> PathBuf {
        self.directory.join("credentials.json")
    }

    /// Stored settings; a missing or unreadable file gives the defaults.
    pub fn load(&self) -> Settings {
        std::fs::read_to_string(self.settings_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .unwrap_or_default()
            .valid()
    }

    pub fn save(&self, settings: Settings) -> std::io::Result<Settings> {
        let settings = settings.valid();
        std::fs::create_dir_all(&self.directory)?;
        write_atomically(&self.settings_path(), &serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?)?;
        Ok(settings)
    }

    fn credentials(&self) -> Map<String, Value> {
        std::fs::read_to_string(self.credentials_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    /// Read the cached vaulted key bundle, loading it once from the OS credential manager on first use.
    fn vaulted_key(&self, name: &str) -> Option<String> {
        let vault = self.vault.as_ref()?;
        let mut cache = self.vault_cache.lock().unwrap();
        if let VaultCache::Unloaded = &*cache {
            *cache = match vault.get(BUNDLE_KEY) {
                Ok(Some(raw)) => VaultCache::Loaded(Self::parse_bundle(&raw)),
                Ok(None) => VaultCache::Loaded(BTreeMap::new()),
                Err(_) => VaultCache::Unavailable,
            };
        }
        match &*cache {
            VaultCache::Loaded(map) => map.get(name).cloned(),
            VaultCache::Unloaded | VaultCache::Unavailable => None,
        }
    }

    /// The environment variable key for the first instance of a provider kind, if set.
    fn env_key(provider_id: &str) -> Option<String> {
        let variable = provider(provider_id).filter(|kind| kind.id == provider_id)?.key_env?;
        std::env::var(variable).ok().map(|key| key.trim().to_string()).filter(|key| !key.is_empty())
    }

    /// The saved key and whether it came from the plain-text file. The vault
    /// is asked first (via its in-memory cache); a vault that fails reads as holding nothing.
    fn saved_key(&self, provider: &str) -> Option<(String, bool)> {
        let name = format!("{provider}_api_key");
        let usable = |key: String| Some(key.trim().to_string()).filter(|key| !key.is_empty());
        let vaulted = self.vaulted_key(&name).and_then(usable);
        vaulted.map(|key| (key, false)).or_else(|| self.credentials().get(&name)?.as_str().map(str::to_string).and_then(usable).map(|key| (key, true)))
    }

    /// The key a provider's requests use: the saved one, else, for the first of its kind, the kind's environment variable.
    pub fn api_key(&self, provider_id: &str) -> Option<String> {
        self.saved_key(provider_id).map(|(key, _)| key).or_else(|| Self::env_key(provider_id))
    }

    pub fn key_status(&self, provider: &str) -> KeyStatus {
        if crate::providers::provider(provider).is_some_and(|known| known.protocol == crate::providers::Protocol::Antigravity) {
            return match crate::antigravity::find_binary() {
                Some(path) => KeyStatus { source: "cli".to_string(), preview: Some(path.display().to_string()), plaintext: false },
                None => KeyStatus { source: "none".to_string(), preview: None, plaintext: false },
            };
        }
        let (key, source, plaintext) = match self.saved_key(provider) {
            Some((key, plaintext)) => (Some(key), "saved", plaintext),
            None => match Self::env_key(provider) {
                Some(key) => (Some(key), "env", false),
                None => (None, "none", false),
            },
        };
        KeyStatus { source: source.to_string(), preview: key.as_deref().map(mask), plaintext }
    }

    /// Every provider's key status, by provider id.
    pub fn key_statuses(&self) -> BTreeMap<String, KeyStatus> {
        self.load().instances().iter().map(|provider| (provider.id.clone(), self.key_status(&provider.id))).collect()
    }

    /// Put a provider of `kind` in the list, under `name`, and return its id:
    /// the first of the kind while that is not set up (no key, no address of
    /// its own, not the default), else a new one beside it, with a key and an
    /// address of its own.
    pub fn add_provider(&self, kind: &str, name: &str) -> std::io::Result<String> {
        let known = PROVIDERS.iter().find(|known| known.id == kind).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("unknown provider `{kind}`")))?;
        let mut settings = self.load();
        let unused = self.key_status(known.id).source == "none" && !settings.base_urls.contains_key(known.id) && settings.provider != known.id && !settings.provider_names.contains_key(known.id);
        let id = if unused {
            known.id.to_string()
        } else {
            (2..).map(|n| format!("{}-{n}", known.id)).find(|id| !settings.provider_names.contains_key(id) && self.saved_key(id).is_none()).expect("there is always a free number")
        };
        let name = name.trim();
        if !name.is_empty() && name != known.name {
            settings.provider_names.insert(id.clone(), name.to_string());
        } else if id != known.id {
            // One added must be named to stay in the list; its kind's name, numbered, tells it apart.
            let n = id.rsplit_once('-').map_or("", |(_, n)| n);
            settings.provider_names.insert(id.clone(), format!("{} {n}", known.name));
        }
        self.save(settings)?;
        Ok(id)
    }

    /// Take a provider out of the list: its saved key, its address, its name
    /// and the guardian chosen for it. The first of a kind stays known, as
    /// every kind is, with nothing set for it. New 分身 that came from it come
    /// from the default provider instead.
    pub fn remove_provider(&self, id: &str) -> std::io::Result<Settings> {
        known(id)?;
        self.delete_api_key(id)?;
        let mut settings = self.load();
        settings.base_urls.remove(id);
        settings.provider_names.remove(id);
        settings.guardian_models.remove(id);
        settings.context_windows.retain(|key, _| !key.starts_with(&format!("{id}/")));
        if settings.provider == id {
            settings.provider = DEFAULT_PROVIDER.to_string();
            settings.model = String::new();
        }
        self.save(settings)
    }

    /// Where a provider's next request goes and with which key.
    pub fn endpoint(&self, provider_id: &str) -> Endpoint {
        let base_url = self.load().base_urls.get(provider_id).cloned().or_else(|| provider(provider_id).map(|known| known.base_url.to_string())).unwrap_or_default();
        Endpoint { base_url, key: self.api_key(provider_id) }
    }

    /// Where `web_search` searches: Zhipu's Web Search API, at the address
    /// and with the key its models use. `None` while no GLM key is saved,
    /// which is how searching is switched on. With the address set to
    /// Zhipu's international site, that site's one engine is asked.
    pub fn search(&self) -> Option<SearchConfig> {
        let Endpoint { base_url, key } = self.endpoint("zhipu");
        let key = key?;
        let engine = if base_url.contains("z.ai") { INTERNATIONAL_ENGINE.to_string() } else { self.load().search_engine };
        Some(SearchConfig { url: format!("{}/web_search", base_url.trim_end_matches('/')), key, engine })
    }

    /// Where `generate_image` paints: the painter the user chose, while its provider has a key; else the first of
    /// [`crate::imagegen::PAINTERS`] whose provider has one. `None` while neither OpenAI nor GLM has a key, which is
    /// how painting is switched on. It goes to the provider's address, so a gateway set there serves it too.
    pub fn painter(&self) -> Option<crate::imagegen::ImageConfig> {
        let chosen = self.load().painter;
        let reachable = |painter: &'static crate::imagegen::Painter| {
            let Endpoint { base_url, key } = self.endpoint(painter.provider);
            let key = key?;
            (!base_url.is_empty()).then(|| crate::imagegen::ImageConfig { painter, url: format!("{}/images/generations", base_url.trim_end_matches('/')), key })
        };
        crate::imagegen::painter(&chosen).and_then(reachable).or_else(|| crate::imagegen::PAINTERS.iter().find_map(reachable))
    }

    /// The directories the user added for confined commands to write under,
    /// or `None` when commands are not to be confined; see [`crate::sandbox::SandboxSource`].
    pub fn sandbox(&self) -> Option<Vec<PathBuf>> {
        let settings = self.load();
        settings.sandbox.then(|| settings.sandbox_paths.iter().map(|path| path.trim()).filter(|path| !path.is_empty()).map(PathBuf::from).collect())
    }

    /// Store a provider's key: in the single `api_keys` vault bundle, or, when there is no vault
    /// or it fails, in a file only the current user can read (enforced on Unix).
    pub fn save_api_key(&self, provider_id: &str, key: &str) -> std::io::Result<()> {
        let key = key.trim();
        if key.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the key is empty"));
        }
        known(provider_id)?;
        let name = format!("{provider_id}_api_key");
        let mut credentials = self.credentials();
        if let Some(vault) = &self.vault {
            let mut cache = self.vault_cache.lock().unwrap();
            let current = match &*cache {
                VaultCache::Loaded(map) => Ok(map.clone()),
                VaultCache::Unloaded | VaultCache::Unavailable => vault.get(BUNDLE_KEY).map(|raw| raw.as_deref().map(Self::parse_bundle).unwrap_or_default()),
            };
            if let Ok(mut map) = current {
                map.insert(name.clone(), key.to_string());
                if let Ok(encoded) = serde_json::to_string(&map)
                    && vault.set(BUNDLE_KEY, &encoded).is_ok()
                {
                    *cache = VaultCache::Loaded(map);
                    drop(cache);
                    // Clean up any stale legacy single entry and plain-text file entry.
                    let _ = vault.delete(&name);
                    return if credentials.remove(&name).is_some() { self.write_credentials(credentials) } else { Ok(()) };
                }
            }
            *cache = VaultCache::Unavailable;
        }
        credentials.insert(name, Value::String(key.to_string()));
        self.write_credentials(credentials)
    }

    /// Remove a provider's saved key from the vault and from the file.
    pub fn delete_api_key(&self, provider_id: &str) -> std::io::Result<()> {
        known(provider_id)?;
        let name = format!("{provider_id}_api_key");
        if let Some(vault) = &self.vault {
            let mut cache = self.vault_cache.lock().unwrap();
            let mut map = match &*cache {
                VaultCache::Loaded(map) => map.clone(),
                VaultCache::Unloaded | VaultCache::Unavailable => {
                    vault.get(BUNDLE_KEY).map_err(std::io::Error::other)?.as_deref().map(Self::parse_bundle).unwrap_or_default()
                }
            };
            if map.remove(&name).is_some() {
                if map.is_empty() {
                    vault.delete(BUNDLE_KEY).map_err(std::io::Error::other)?;
                } else {
                    let encoded = serde_json::to_string(&map).map_err(std::io::Error::other)?;
                    vault.set(BUNDLE_KEY, &encoded).map_err(std::io::Error::other)?;
                }
            }
            *cache = VaultCache::Loaded(map);
            drop(cache);
            let _ = vault.delete(&name);
        }
        let mut credentials = self.credentials();
        if credentials.remove(&name).is_none() {
            return Ok(());
        }
        self.write_credentials(credentials)
    }

    /// Write the key file, or remove it once it holds nothing.
    fn write_credentials(&self, credentials: Map<String, Value>) -> std::io::Result<()> {
        if credentials.is_empty() {
            return match std::fs::remove_file(self.credentials_path()) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        std::fs::create_dir_all(&self.directory)?;
        let path = self.credentials_path();
        write_atomically(&path, &serde_json::to_vec_pretty(&credentials).map_err(std::io::Error::other)?)?;
        restrict(&path)
    }
}

fn known(provider_id: &str) -> std::io::Result<&'static Provider> {
    provider(provider_id).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("unknown provider `{provider_id}`")))
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Show the first 7 and last 4 characters of a key of 16 or more; hide shorter keys entirely.
fn mask(key: &str) -> String {
    let characters: Vec<char> = key.chars().collect();
    if characters.len() < 16 {
        return "•".repeat(12);
    }
    let head: String = characters[..7].iter().collect();
    let tail: String = characters[characters.len() - 4..].iter().collect();
    format!("{head}{}{tail}", "•".repeat(12))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(provider: &str, model: &str) -> Settings {
        Settings { provider: provider.into(), model: model.into(), ..Settings::default() }
    }

    #[test]
    fn stores_settings_and_rejects_values_it_does_not_offer() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("home"));
        assert_eq!(store.load(), Settings::default());
        let saved = store.save(Settings { effort: "turbo".into(), theme: "dark".into(), ..settings("anthropic", "claude-sonnet-5-5") }).unwrap();
        assert_eq!(saved, Settings { theme: "dark".into(), ..settings("anthropic", "claude-sonnet-5-5") });
        assert_eq!(store.load(), saved);
        std::fs::write(directory.path().join("home/settings.json"), "{ not json").unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn reads_settings_saved_before_providers_existed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("settings.json"), r#"{"model":"claude-sonnet-5-5","effort":"max","theme":"light"}"#).unwrap();
        let loaded = SettingsStore::new(directory.path()).load();
        assert_eq!((loaded.provider.as_str(), loaded.model.as_str(), loaded.effort.as_str()), ("anthropic", "claude-sonnet-5-5", "max"));
    }

    #[test]
    fn keeps_any_model_id_but_fixes_one_left_from_another_provider() {
        assert_eq!(settings("deepseek", "deepseek-v4-pro-0101").valid().model, "deepseek-v4-pro-0101");
        assert_eq!(settings("deepseek", "claude-opus-5-5").valid().model, "deepseek-v4-pro");
        assert_eq!(settings("moonshot", " ").valid().model, "kimi-k3.1-pro");
        assert_eq!(settings("custom", "").valid().model, "");
        assert_eq!(settings("nobody", "x").valid().provider, "anthropic");

        let mut urls = settings("zhipu", "glm-5.3");
        urls.base_urls = BTreeMap::from([
            ("zhipu".into(), " https://api.z.ai/api/paas/v4/ ".into()),
            ("custom".into(), "".into()),
            ("anthropic".into(), "https://gateway".into()),
            ("nobody".into(), "https://x".into()),
        ]);
        assert_eq!(urls.valid().base_urls, BTreeMap::from([("zhipu".into(), "https://api.z.ai/api/paas/v4".into())]));
    }

    #[test]
    fn saves_masks_and_deletes_each_provider_s_key() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        assert!(store.save_api_key("anthropic", "   ").is_err());
        assert!(store.save_api_key("nobody", "sk-x").is_err());
        store.save_api_key("anthropic", "  sk-ant-api03-abcdefghijklmnop-a41f \n").unwrap();
        store.save_api_key("deepseek", "sk-deepseek-0123456789abcdef").unwrap();
        assert_eq!(store.api_key("anthropic").as_deref(), Some("sk-ant-api03-abcdefghijklmnop-a41f"));
        assert_eq!(store.api_key("deepseek").as_deref(), Some("sk-deepseek-0123456789abcdef"));
        let status = store.key_status("anthropic");
        assert_eq!(status.source, "saved");
        assert_eq!(status.preview.as_deref(), Some("sk-ant-••••••••••••a41f"));
        assert_eq!(mask("short-key"), "••••••••••••");

        // A file written when only Anthropic existed reads unchanged.
        let file: Value = serde_json::from_str(&std::fs::read_to_string(directory.path().join("credentials.json")).unwrap()).unwrap();
        assert_eq!(file["anthropic_api_key"], "sk-ant-api03-abcdefghijklmnop-a41f");

        store.delete_api_key("anthropic").unwrap();
        store.delete_api_key("anthropic").unwrap();
        assert_ne!(store.key_status("anthropic").source, "saved");
        assert_eq!(store.key_status("deepseek").source, "saved");
        store.delete_api_key("deepseek").unwrap();
        assert!(!directory.path().join("credentials.json").exists());
    }

    #[test]
    fn adds_several_providers_of_a_kind_each_with_its_own_key_and_address() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        // The first of a kind not yet set up is the one put in the list, as before.
        assert_eq!(store.add_provider("custom", "").unwrap(), "custom");
        store.save_api_key("custom", "key-of-the-first-000000").unwrap();
        // Then another beside it, named as asked, or for its kind and number.
        assert_eq!(store.add_provider("custom", "本地中转").unwrap(), "custom-2");
        assert_eq!(store.add_provider("custom", "").unwrap(), "custom-3");
        assert!(store.add_provider("nobody", "x").is_err() && store.add_provider("custom-2", "x").is_err());
        let mut settings = store.load();
        settings.base_urls.insert("custom".into(), "http://127.0.0.1:1/v1".into());
        settings.base_urls.insert("custom-2".into(), "http://127.0.0.1:2/v1/".into());
        settings.provider = "custom-2".into();
        settings.model = "my-model".into();
        let settings = store.save(settings).unwrap();
        let listed: Vec<(String, String)> = settings.instances().iter().filter(|one| one.kind == "custom").map(|one| (one.id.clone(), one.name.clone())).collect();
        assert_eq!(listed, [("custom".into(), "OpenAI 兼容服务".into()), ("custom-2".into(), "本地中转".into()), ("custom-3".into(), "OpenAI 兼容服务 3".into())]);
        assert_eq!((settings.provider.as_str(), settings.model.as_str()), ("custom-2", "my-model"));

        // Each its own address and key; the one added has none until one is saved for it.
        store.save_api_key("custom-2", "key-of-the-second-00000").unwrap();
        let (first, second, third) = (store.endpoint("custom"), store.endpoint("custom-2"), store.endpoint("custom-3"));
        assert_eq!((first.base_url.as_str(), first.key.as_deref()), ("http://127.0.0.1:1/v1", Some("key-of-the-first-000000")));
        assert_eq!((second.base_url.as_str(), second.key.as_deref()), ("http://127.0.0.1:2/v1", Some("key-of-the-second-00000")));
        assert_eq!((third.base_url.as_str(), third.key), ("", None));
        assert!(store.key_statuses().contains_key("custom-3"));
        // A compatible service may serve another provider's model, which is kept as chosen.
        let mut through = store.load();
        (through.provider, through.model) = ("custom".into(), "deepseek-v4-pro".into());
        let mut through = store.save(through).unwrap();
        assert_eq!(through.model, "deepseek-v4-pro");
        (through.provider, through.model) = ("custom-2".into(), "my-model".into());
        store.save(through).unwrap();

        // Taken out, nothing of it is kept, and the default falls back to a provider still there.
        store.remove_provider("custom-2").unwrap();
        let settings = store.load();
        assert!(!settings.provider_names.contains_key("custom-2") && !settings.base_urls.contains_key("custom-2"));
        assert_eq!(store.api_key("custom-2"), None);
        assert_eq!(settings.provider, DEFAULT_PROVIDER);
        assert!(!settings.instances().iter().any(|one| one.id == "custom-2"));
        // Its number is free again.
        assert_eq!(store.add_provider("custom", "").unwrap(), "custom-2");
        // The first of a kind taken out while new 分身 come from it: they come from the default provider again.
        let mut first = store.load();
        (first.provider, first.model) = ("custom".into(), "my-model".into());
        store.save(first).unwrap();
        let after = store.remove_provider("custom").unwrap();
        assert_eq!((after.provider.as_str(), after.model.as_str()), (DEFAULT_PROVIDER, DEFAULT_MODEL));
        assert!(!after.base_urls.contains_key("custom") && store.api_key("custom").is_none());
    }

    #[test]
    fn reads_the_environment_key_only_for_the_first_of_a_kind() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        // SAFETY: no other test reads this variable, which is set only here.
        unsafe { std::env::set_var("MOONSHOT_API_KEY", "moonshot-from-the-environment") };
        let added = store.add_provider("moonshot", "国际站").unwrap();
        assert_eq!(added, "moonshot-2", "the first is set up by the environment's key, so another is added beside it");
        assert_eq!(store.api_key("moonshot").as_deref(), Some("moonshot-from-the-environment"));
        assert_eq!((store.api_key("moonshot-2"), store.key_status("moonshot-2").source.as_str()), (None, "none"));
        // An Anthropic provider other than the first takes an address of its own; the first's comes from the environment.
        let compatible = store.add_provider("custom_anthropic", "").unwrap();
        let mut settings = store.load();
        settings.base_urls.insert(compatible.clone(), "https://gateway.example.com/anthropic".into());
        settings.base_urls.insert("anthropic".into(), "https://ignored.example.com".into());
        let settings = store.save(settings).unwrap();
        assert_eq!(settings.base_urls.get(&compatible).map(String::as_str), Some("https://gateway.example.com/anthropic"));
        assert!(!settings.base_urls.contains_key("anthropic"));
        unsafe { std::env::remove_var("MOONSHOT_API_KEY") };
    }

    /// A vault in memory that can be told to fail and counts how many times it was read.
    #[derive(Default)]
    struct MemoryVault {
        secrets: std::sync::Mutex<BTreeMap<String, String>>,
        broken: std::sync::atomic::AtomicBool,
        gets: std::sync::atomic::AtomicUsize,
    }

    impl MemoryVault {
        fn check(&self) -> Result<(), String> {
            if self.broken.load(std::sync::atomic::Ordering::SeqCst) { Err("the vault is locked".to_string()) } else { Ok(()) }
        }

        fn get_count(&self) -> usize {
            self.gets.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Vault for MemoryVault {
        fn get(&self, name: &str) -> Result<Option<String>, String> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.check().map(|()| self.secrets.lock().unwrap().get(name).cloned())
        }

        fn set(&self, name: &str, secret: &str) -> Result<(), String> {
            self.check().map(|()| drop(self.secrets.lock().unwrap().insert(name.to_string(), secret.to_string())))
        }

        fn delete(&self, name: &str) -> Result<(), String> {
            self.check().map(|()| drop(self.secrets.lock().unwrap().remove(name)))
        }
    }

    #[test]
    fn keeps_keys_in_a_single_vault_bundle_and_moves_the_ones_a_file_still_holds() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials.json");
        // Two keys saved by a build from before the vault.
        SettingsStore::new(directory.path()).save_api_key("anthropic", "sk-ant-old-key-0000000000").unwrap();
        SettingsStore::new(directory.path()).save_api_key("zhipu", "glm-old-key-000000000000").unwrap();
        let vault = Arc::new(MemoryVault::default());
        let store = SettingsStore::with_vault(directory.path(), vault.clone());
        // Until moved they still work, and are reported as plain text.
        assert_eq!(store.key_status("zhipu"), KeyStatus { source: "saved".into(), preview: Some(mask("glm-old-key-000000000000")), plaintext: true });

        store.migrate_keys();
        assert!(!file.exists());
        // All keys live in one single vault entry (`api_keys`) so macOS Keychain only has one ACL item.
        assert_eq!(vault.secrets.lock().unwrap().len(), 1);
        assert!(vault.secrets.lock().unwrap().contains_key(BUNDLE_KEY));
        assert_eq!(store.api_key("anthropic").as_deref(), Some("sk-ant-old-key-0000000000"));
        assert!(!store.key_status("zhipu").plaintext);

        // A new key updates the single bundle and leaves no file; deleting takes it out.
        store.save_api_key("deepseek", " sk-deepseek-key-00000000 ").unwrap();
        assert!(!file.exists());
        assert_eq!(vault.secrets.lock().unwrap().len(), 1);
        let bundle: Value = serde_json::from_str(vault.secrets.lock().unwrap().get(BUNDLE_KEY).unwrap()).unwrap();
        assert_eq!(bundle["deepseek_api_key"], "sk-deepseek-key-00000000");
        store.delete_api_key("deepseek").unwrap();
        assert_eq!(store.key_status("deepseek").source, "none");
        // Deleting every remaining key removes the bundle entry completely.
        store.delete_api_key("anthropic").unwrap();
        store.delete_api_key("zhipu").unwrap();
        assert!(vault.secrets.lock().unwrap().is_empty());
    }

    #[test]
    fn migrates_legacy_per_provider_vault_entries_and_caches_reads_in_memory() {
        let directory = tempfile::tempdir().unwrap();
        let vault = Arc::new(MemoryVault::default());
        vault.secrets.lock().unwrap().insert("anthropic_api_key".into(), "sk-ant-legacy-0000000000".into());
        vault.secrets.lock().unwrap().insert("zhipu_api_key".into(), "glm-legacy-000000000000".into());

        let store = SettingsStore::with_vault(directory.path(), vault.clone());
        store.migrate_keys();
        // Legacy per-provider entries were merged into `api_keys` and removed.
        assert_eq!(vault.secrets.lock().unwrap().len(), 1);
        assert!(vault.secrets.lock().unwrap().contains_key(BUNDLE_KEY));

        // After migration, repeated reads hit only the in-memory cache (0 additional vault reads).
        let gets_after_migrate = vault.get_count();
        let statuses = store.key_statuses();
        assert_eq!(statuses["anthropic"].source, "saved");
        assert_eq!(statuses["zhipu"].source, "saved");
        assert_eq!(store.api_key("anthropic").as_deref(), Some("sk-ant-legacy-0000000000"));
        assert!(store.search().is_some());
        assert!(store.painter().is_some());
        assert_eq!(vault.get_count(), gets_after_migrate);

        // A brand-new store instance (next app launch) does just ONE single vault read for `api_keys`.
        let next_run = SettingsStore::with_vault(directory.path(), vault.clone());
        let before_next = vault.get_count();
        next_run.migrate_keys();
        let _ = next_run.key_statuses();
        let _ = next_run.search();
        let _ = next_run.painter();
        assert_eq!(vault.get_count() - before_next, 1);
    }

    #[test]
    fn falls_back_to_the_file_while_the_vault_fails() {
        let directory = tempfile::tempdir().unwrap();
        let vault = Arc::new(MemoryVault::default());
        let store = SettingsStore::with_vault(directory.path(), vault.clone());
        vault.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        store.save_api_key("zhipu", "glm-key-0000000000000000").unwrap();
        assert_eq!(store.key_status("zhipu"), KeyStatus { source: "saved".into(), preview: Some(mask("glm-key-0000000000000000")), plaintext: true });
        // Moving fails too, and loses nothing.
        store.migrate_keys();
        assert_eq!(store.api_key("zhipu").as_deref(), Some("glm-key-0000000000000000"));

        // Once the vault works again the key moves, and a newer key replaces the file's copy.
        vault.broken.store(false, std::sync::atomic::Ordering::SeqCst);
        store.migrate_keys();
        assert!(!directory.path().join("credentials.json").exists() && !store.key_status("zhipu").plaintext);
    }

    #[test]
    fn a_models_context_window_is_what_its_provider_reported_then_what_is_documented() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("home"));
        let listed = |id: &str, context_window: Option<u64>| Listed { id: id.to_string(), name: id.to_string(), context_window };
        let window = |tokens, source| Window { tokens, source };
        let plain = Settings::default();
        // Before anything is reported: the documentation, or the safe size for a model it does not list.
        assert_eq!(store.context_window(&plain, "zhipu", "glm-4.7"), window(200_000, WindowSource::Documented));
        assert_eq!(store.context_window(&plain, "zhipu", "glm-9"), window(UNKNOWN_CONTEXT_WINDOW, WindowSource::Unknown));

        // What the provider says comes first, and is kept for the next run. A model it gave no size for keeps what was known.
        store.record_models("zhipu", &[listed("glm-5.3-flash", Some(256_000)), listed("glm-4.7", Some(128_000)), listed("glm-5.3", None)]).unwrap();
        assert_eq!(store.context_window(&plain, "zhipu", "glm-5.3-flash"), window(256_000, WindowSource::Provider));
        assert_eq!(store.context_window(&plain, "zhipu", "glm-4.7"), window(128_000, WindowSource::Provider));
        assert_eq!(store.context_window(&plain, "zhipu", "glm-5.3"), window(1_000_000, WindowSource::Documented));
        // Another provider's model of the same name is another model.
        assert_eq!(store.context_window(&plain, "custom", "glm-5.3-flash").source, WindowSource::Unknown);
        let again = SettingsStore::new(directory.path().join("home"));
        assert_eq!(again.context_window(&plain, "zhipu", "glm-5.3-flash"), window(256_000, WindowSource::Provider));
        again.record_models("zhipu", &[listed("glm-5.3-flash", None)]).unwrap();
        assert_eq!(again.context_window(&plain, "zhipu", "glm-5.3-flash").tokens, 256_000);

        // What the user sets for a model comes before all of it, for that model of that provider alone; cleared, the rest decides again.
        let set = again.save(Settings { context_windows: [("zhipu/glm-5.3-flash".to_string(), 500_000), ("zhipu/tiny".to_string(), 1_000), ("zhipu/cleared".to_string(), 0), ("nowhere".to_string(), 9)].into(), ..Settings::default() }).unwrap();
        assert_eq!(set.context_windows, [("zhipu/glm-5.3-flash".to_string(), 500_000), ("zhipu/tiny".to_string(), MIN_CONTEXT_WINDOW)].into());
        assert_eq!(again.context_window(&set, "zhipu", "glm-5.3-flash"), window(500_000, WindowSource::Set));
        assert_eq!(again.context_window(&set, "zhipu", "glm-4.7"), window(128_000, WindowSource::Provider));
        assert_eq!(again.context_window(&plain, "zhipu", "glm-5.3-flash"), window(256_000, WindowSource::Provider));

        // The limit a session compacts at is the one set, or four fifths of the window when that is less.
        let settings = Settings { context_limit: 200_000, context_auto: false, ..Settings::default() };
        assert_eq!((settings.context_limit_within(1_000_000), settings.context_limit_within(128_000)), (200_000, 102_400));
        // Set to follow the model, it is four fifths of the window whatever limit is set.
        let auto = Settings { context_auto: true, ..settings };
        assert_eq!((auto.context_limit_within(1_000_000), auto.context_limit_within(128_000)), (800_000, 102_400));
        // Which is how it starts, with the guardian on; a file saved before either was on by default keeps what it says.
        let fresh = Settings::default();
        assert!(fresh.context_auto && fresh.guardian);
        let missing: Settings = serde_json::from_str(r#"{"model": "m", "effort": "high", "theme": "system"}"#).unwrap();
        assert!(missing.context_auto && missing.guardian);
        let kept: Settings = serde_json::from_str(r#"{"model": "m", "effort": "high", "theme": "system", "context_auto": false, "guardian": false}"#).unwrap();
        assert!(!kept.context_auto && !kept.guardian);
    }

    #[test]
    fn paints_with_the_chosen_model_while_its_provider_has_a_key_and_else_with_one_that_has() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        // Keys may also come from the environment, which this test does not control.
        if std::env::var_os("OPENAI_API_KEY").is_some() || std::env::var_os("ZHIPUAI_API_KEY").is_some() {
            return;
        }
        assert_eq!(store.painter(), None);
        store.save_api_key("zhipu", "glm-key-0000000000000000").unwrap();
        // Nothing chosen: the first painter whose provider has a key.
        let config = store.painter().unwrap();
        assert_eq!((config.painter.id, config.url.as_str(), config.key.as_str()), ("glm-image", "https://open.bigmodel.cn/api/paas/v4/images/generations", "glm-key-0000000000000000"));
        // Chosen, and its provider has no key: one that has is used instead.
        store.save(Settings { painter: "gpt-image-2".into(), ..Settings::default() }).unwrap();
        assert_eq!(store.painter().unwrap().painter.id, "glm-image");
        store.save_api_key("openai", "sk-test-0000000000000000").unwrap();
        let config = store.painter().unwrap();
        assert_eq!((config.painter.id, config.url.as_str()), ("gpt-image-2", "https://api.openai.com/v1/images/generations"));
        // One this build does not know is forgotten.
        assert_eq!(store.save(Settings { painter: "midjourney".into(), ..Settings::default() }).unwrap().painter, "");
    }

    #[test]
    fn searching_uses_the_glm_key_and_address_and_is_off_without_a_key() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        // The GLM key may also come from the environment, which this test does not control.
        if std::env::var_os("ZHIPUAI_API_KEY").is_none() {
            assert_eq!(store.search(), None);
        }
        store.save_api_key("zhipu", "glm-key-0000000000000000").unwrap();
        let search = store.search().unwrap();
        assert_eq!((search.url.as_str(), search.engine.as_str()), ("https://open.bigmodel.cn/api/paas/v4/web_search", "search_std"));
        assert_eq!(search.key, "glm-key-0000000000000000");

        // The chosen engine is used; one this build does not know falls back to the cheapest.
        store.save(Settings { search_engine: "search_pro".into(), ..Settings::default() }).unwrap();
        assert_eq!(store.search().unwrap().engine, "search_pro");
        assert_eq!(store.save(Settings { search_engine: "bing".into(), ..Settings::default() }).unwrap().search_engine, "search_std");
        // The international site has one engine of its own.
        let abroad = Settings { base_urls: [("zhipu".to_string(), "https://api.z.ai/api/paas/v4/".to_string())].into(), ..Settings::default() };
        store.save(abroad).unwrap();
        let search = store.search().unwrap();
        assert_eq!((search.url.as_str(), search.engine.as_str()), ("https://api.z.ai/api/paas/v4/web_search", "search-prime"));
    }

    #[test]
    fn an_endpoint_uses_the_saved_address_or_the_provider_s_own() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        assert_eq!(store.endpoint("moonshot").base_url, "https://api.moonshot.cn/v1");
        assert_eq!(store.endpoint("custom").base_url, "");
        let mut chosen = settings("moonshot", "kimi-k3");
        chosen.base_urls.insert("moonshot".into(), "https://api.moonshot.ai/v1".into());
        store.save(chosen).unwrap();
        store.save_api_key("moonshot", "sk-kimi").unwrap();
        assert_eq!(store.endpoint("moonshot"), Endpoint { base_url: "https://api.moonshot.ai/v1".into(), key: Some("sk-kimi".into()) });
    }
}
