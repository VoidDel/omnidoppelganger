//! User settings and the stored API key, both under the home directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::anthropic::{DEFAULT_MODEL, MODELS};

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const THEMES: &[&str] = &["system", "light", "dark"];

/// The directory holding sessions, settings, and credentials: `$ODG_HOME`, or `~/.odg`.
pub fn home() -> PathBuf {
    std::env::var_os("ODG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".odg")))
        .unwrap_or_else(|| PathBuf::from(".odg"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Model of sessions created from now on. A session keeps the model it started with.
    pub model: String,
    /// How hard the model works on each request.
    pub effort: String,
    /// `system`, `light`, or `dark`.
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        // Coding responds strongly to effort, and this model's own default is one level lower.
        Self { model: DEFAULT_MODEL.to_string(), effort: "high".to_string(), theme: "system".to_string() }
    }
}

impl Settings {
    /// Replace any value this build does not offer with its default.
    fn valid(mut self) -> Self {
        let default = Settings::default();
        if !MODELS.iter().any(|(id, _)| *id == self.model) {
            self.model = default.model;
        }
        if !EFFORTS.contains(&self.effort.as_str()) {
            self.effort = default.effort;
        }
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = default.theme;
        }
        self
    }
}

/// Where the key used for requests comes from, with a preview that reveals only its ends.
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
        std::fs::write(self.settings_path(), serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?)?;
        Ok(settings)
    }

    fn saved_key(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.credentials_path()).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value["anthropic_api_key"].as_str().map(str::trim).filter(|key| !key.is_empty()).map(str::to_string)
    }

    /// The key requests use: the saved one, else `ANTHROPIC_API_KEY`.
    pub fn api_key(&self) -> Option<String> {
        self.saved_key().or_else(|| std::env::var("ANTHROPIC_API_KEY").ok().map(|key| key.trim().to_string()).filter(|key| !key.is_empty()))
    }

    pub fn key_status(&self) -> KeyStatus {
        let source = if self.saved_key().is_some() {
            "saved"
        } else if self.api_key().is_some() {
            "env"
        } else {
            "none"
        };
        KeyStatus { source: source.to_string(), preview: self.api_key().as_deref().map(mask) }
    }

    /// Store the key in a file only the current user can read (enforced on Unix).
    pub fn save_api_key(&self, key: &str) -> std::io::Result<()> {
        let key = key.trim();
        if key.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the key is empty"));
        }
        std::fs::create_dir_all(&self.directory)?;
        let path = self.credentials_path();
        std::fs::write(&path, serde_json::to_vec_pretty(&json!({ "anthropic_api_key": key })).map_err(std::io::Error::other)?)?;
        restrict(&path)
    }

    pub fn delete_api_key(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.credentials_path()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
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

    #[test]
    fn stores_settings_and_rejects_values_it_does_not_offer() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("home"));
        assert_eq!(store.load(), Settings::default());
        let saved = store.save(Settings { model: "claude-sonnet-5-5".into(), effort: "turbo".into(), theme: "dark".into() }).unwrap();
        assert_eq!(saved, Settings { model: "claude-sonnet-5-5".into(), effort: "high".into(), theme: "dark".into() });
        assert_eq!(store.load(), saved);
        std::fs::write(directory.path().join("home/settings.json"), "{ not json").unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn saves_masks_and_deletes_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path());
        assert!(store.save_api_key("   ").is_err());
        store.save_api_key("  sk-ant-api03-abcdefghijklmnop-a41f \n").unwrap();
        assert_eq!(store.api_key().as_deref(), Some("sk-ant-api03-abcdefghijklmnop-a41f"));
        let status = store.key_status();
        assert_eq!(status.source, "saved");
        assert_eq!(status.preview.as_deref(), Some("sk-ant-••••••••••••a41f"));
        assert_eq!(mask("short-key"), "••••••••••••");
        store.delete_api_key().unwrap();
        store.delete_api_key().unwrap();
        assert_ne!(store.key_status().source, "saved");
    }
}
