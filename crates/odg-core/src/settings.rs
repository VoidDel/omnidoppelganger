//! User settings and the stored API keys, both under the home directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::anthropic::DEFAULT_MODEL;
use crate::fsutil::write_atomically;
use crate::llm::Endpoint;
use crate::policy::Mode;
use crate::providers::{DEFAULT_PROVIDER, PROVIDERS, Protocol, Provider, context_window, provider};

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const THEMES: &[&str] = &["system", "light", "dark"];
/// Most models here hold a million tokens, but every request sends the whole
/// conversation, so compacting long before the window is full keeps a long
/// session affordable.
pub const DEFAULT_CONTEXT_LIMIT: u64 = 200_000;
/// Below this a summary and one file read would already be past the limit.
pub const MIN_CONTEXT_LIMIT: u64 = 20_000;

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
    /// `system`, `light`, or `dark`.
    pub theme: String,
    /// Addresses that replace a provider's default, by provider id; for an
    /// international site, a gateway, or the custom provider. Anthropic's
    /// address comes from `ANTHROPIC_BASE_URL` instead.
    #[serde(default)]
    pub base_urls: BTreeMap<String, String>,
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
            theme: "system".to_string(),
            base_urls: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Replace any value this build does not offer with its default.
    fn valid(mut self) -> Self {
        let default = Settings::default();
        let chosen = match provider(&self.provider) {
            Some(chosen) => chosen,
            None => {
                self.provider = default.provider;
                provider(DEFAULT_PROVIDER).expect("the default provider exists")
            }
        };
        self.model = self.model.trim().to_string();
        // A model another provider offers, left over from switching provider, is replaced too.
        let belongs_elsewhere = PROVIDERS.iter().any(|other| other.id != chosen.id && other.models.iter().any(|(id, _)| *id == self.model))
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
        self.context_limit = self.context_limit.max(MIN_CONTEXT_LIMIT);
        self.base_urls = std::mem::take(&mut self.base_urls)
            .into_iter()
            .map(|(id, url)| (id, url.trim().trim_end_matches('/').to_string()))
            .filter(|(id, url)| !url.is_empty() && provider(id).is_some_and(|known| known.protocol != Protocol::Anthropic))
            .collect();
        self
    }
}

impl Settings {
    /// The limit a session on this model compacts at: the one set here, or
    /// four fifths of the model's context window when that is less, which
    /// leaves room for the answer and for what the next tools return.
    pub fn context_limit_for(&self, provider: &str, model: &str) -> u64 {
        self.context_limit.min(context_window(provider, model) / 5 * 4)
    }
}

/// Where the key used for a provider's requests comes from, with a preview that reveals only its ends.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyStatus {
    /// `saved`, `env`, or `none`.
    pub source: String,
    pub preview: Option<String>,
}

pub struct SettingsStore {
    directory: PathBuf,
}

impl SettingsStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into() }
    }

    fn settings_path(&self) -> PathBuf {
        self.directory.join("settings.json")
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

    fn saved_key(&self, provider: &str) -> Option<String> {
        self.credentials().get(&format!("{provider}_api_key"))?.as_str().map(str::trim).filter(|key| !key.is_empty()).map(str::to_string)
    }

    /// The key a provider's requests use: the saved one, else the provider's environment variable.
    pub fn api_key(&self, provider_id: &str) -> Option<String> {
        self.saved_key(provider_id).or_else(|| {
            let variable = provider(provider_id)?.key_env?;
            std::env::var(variable).ok().map(|key| key.trim().to_string()).filter(|key| !key.is_empty())
        })
    }

    pub fn key_status(&self, provider: &str) -> KeyStatus {
        let source = if self.saved_key(provider).is_some() {
            "saved"
        } else if self.api_key(provider).is_some() {
            "env"
        } else {
            "none"
        };
        KeyStatus { source: source.to_string(), preview: self.api_key(provider).as_deref().map(mask) }
    }

    /// Every provider's key status, by provider id.
    pub fn key_statuses(&self) -> BTreeMap<String, KeyStatus> {
        PROVIDERS.iter().map(|provider| (provider.id.to_string(), self.key_status(provider.id))).collect()
    }

    /// Where a provider's next request goes and with which key.
    pub fn endpoint(&self, provider_id: &str) -> Endpoint {
        let base_url = self.load().base_urls.get(provider_id).cloned().or_else(|| provider(provider_id).map(|known| known.base_url.to_string())).unwrap_or_default();
        Endpoint { base_url, key: self.api_key(provider_id) }
    }

    /// Store a provider's key in a file only the current user can read (enforced on Unix).
    pub fn save_api_key(&self, provider_id: &str, key: &str) -> std::io::Result<()> {
        let key = key.trim();
        if key.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the key is empty"));
        }
        known(provider_id)?;
        let mut credentials = self.credentials();
        credentials.insert(format!("{provider_id}_api_key"), Value::String(key.to_string()));
        self.write_credentials(credentials)
    }

    pub fn delete_api_key(&self, provider_id: &str) -> std::io::Result<()> {
        known(provider_id)?;
        let mut credentials = self.credentials();
        if credentials.remove(&format!("{provider_id}_api_key")).is_none() {
            return Ok(());
        }
        if credentials.is_empty() {
            return match std::fs::remove_file(self.credentials_path()) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        self.write_credentials(credentials)
    }

    fn write_credentials(&self, credentials: Map<String, Value>) -> std::io::Result<()> {
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
