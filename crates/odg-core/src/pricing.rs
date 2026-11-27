//! Model pricing catalog, custom price overrides, `models.dev` sync cache, and token cost estimation.
//!
//! Three-tier pricing resolution (highest priority first):
//! 1. User custom overrides persisted in `~/.odg/custom-pricing.json`
//! 2. Synced `models.dev` catalog cached in `~/.odg/pricing-cache.json`
//! 3. Built-in compile-time fallback table (works completely offline)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::Tokens;
use crate::sync_ext::MutexExt;

/// Default USD -> CNY exchange rate used when summarizing mixed-currency spend into USD equivalent.
pub const DEFAULT_USD_TO_CNY: f64 = 7.2;

/// Upstream `models.dev` open model catalog JSON endpoint.
pub const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

/// Currency in which a [`ModelRate`] is denominated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    #[default]
    Usd,
    Cny,
}

impl Currency {
    pub fn symbol(self) -> &'static str {
        match self {
            Currency::Usd => "$",
            Currency::Cny => "¥",
        }
    }

    pub fn to_usd(self, amount: f64) -> f64 {
        match self {
            Currency::Usd => amount,
            Currency::Cny => amount / DEFAULT_USD_TO_CNY,
        }
    }
}

/// Where a resolved [`ModelRate`] originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    Custom,
    ModelsDev,
    #[default]
    Builtin,
}

/// How usage for a provider is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingMode {
    /// Pay-as-you-go API key billing.
    #[default]
    Api,
    /// Flat-rate subscription or CLI pool (costs represent API-equivalent value).
    Subscription,
    /// Local self-hosted model (e.g. Ollama) with no external API bill.
    Local,
}

/// Rates per 1,000,000 tokens: `(uncached_input, cached_read, cache_write, output)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelRate {
    #[serde(default)]
    pub currency: Currency,
    pub input_per_m: f64,
    pub cached_per_m: f64,
    #[serde(default)]
    pub cache_write_per_m: f64,
    pub output_per_m: f64,
    #[serde(default)]
    pub source: PriceSource,
}

impl ModelRate {
    pub const fn usd(input_per_m: f64, cached_per_m: f64, cache_write_per_m: f64, output_per_m: f64) -> Self {
        Self {
            currency: Currency::Usd,
            input_per_m,
            cached_per_m,
            cache_write_per_m,
            output_per_m,
            source: PriceSource::Builtin,
        }
    }

    pub const fn cny(input_per_m: f64, cached_per_m: f64, cache_write_per_m: f64, output_per_m: f64) -> Self {
        Self {
            currency: Currency::Cny,
            input_per_m,
            cached_per_m,
            cache_write_per_m,
            output_per_m,
            source: PriceSource::Builtin,
        }
    }
}

/// Calculated cost for one request/turn in both native currency and USD equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub amount: f64,
    pub currency: Currency,
    pub usd_equiv: f64,
    pub source: PriceSource,
}

/// One model entry exposed to the UI catalog and price editor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCatalogEntry {
    /// Lookup key: `"provider/model"` or bare `"model"`.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rate: ModelRate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Snapshot of the effective pricing catalog sent to the frontend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCatalogView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<String>,
    pub models_dev_count: usize,
    pub custom_count: usize,
    pub usd_to_cny: f64,
    pub custom: Vec<PriceCatalogEntry>,
    pub effective: Vec<PriceCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedCatalogModel {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rate: ModelRate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CustomPricingFile {
    #[serde(default)]
    rates: BTreeMap<String, ModelRate>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SyncedCatalogFile {
    #[serde(default)]
    synced_at: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, CachedCatalogModel>,
}

struct BuiltinSpec {
    provider: &'static str,
    model: &'static str,
    name: &'static str,
    rate: ModelRate,
    context_window: u64,
}

const BUILTIN_SPECS: &[BuiltinSpec] = &[
    BuiltinSpec {
        provider: "anthropic",
        model: "claude-opus-4-6",
        name: "Claude Opus",
        rate: ModelRate::usd(5.0, 0.50, 6.25, 25.0),
        context_window: 200_000,
    },
    BuiltinSpec {
        provider: "anthropic",
        model: "claude-sonnet-4-6",
        name: "Claude Sonnet",
        rate: ModelRate::usd(3.0, 0.30, 3.75, 15.0),
        context_window: 200_000,
    },
    BuiltinSpec {
        provider: "anthropic",
        model: "claude-haiku-4-5",
        name: "Claude Haiku",
        rate: ModelRate::usd(1.0, 0.10, 1.25, 5.0),
        context_window: 200_000,
    },
    BuiltinSpec {
        provider: "openai",
        model: "gpt-5.4",
        name: "GPT-5.4",
        rate: ModelRate::usd(2.50, 0.25, 2.50, 10.0),
        context_window: 400_000,
    },
    BuiltinSpec {
        provider: "openai",
        model: "gpt-4.1-mini",
        name: "GPT-4.1 Mini",
        rate: ModelRate::usd(0.60, 0.15, 0.60, 2.40),
        context_window: 1_000_000,
    },
    BuiltinSpec {
        provider: "gemini",
        model: "gemini-2.5-pro",
        name: "Gemini 2.5 Pro",
        rate: ModelRate::usd(1.25, 0.31, 1.25, 10.0),
        context_window: 1_048_576,
    },
    BuiltinSpec {
        provider: "gemini",
        model: "gemini-2.5-flash",
        name: "Gemini 2.5 Flash",
        rate: ModelRate::usd(0.30, 0.075, 0.30, 2.50),
        context_window: 1_048_576,
    },
    BuiltinSpec {
        provider: "deepseek",
        model: "deepseek-chat",
        name: "DeepSeek Chat",
        rate: ModelRate::usd(0.28, 0.028, 0.28, 1.10),
        context_window: 128_000,
    },
    BuiltinSpec {
        provider: "deepseek",
        model: "deepseek-reasoner",
        name: "DeepSeek Reasoner",
        rate: ModelRate::usd(0.28, 0.028, 0.28, 1.10),
        context_window: 128_000,
    },
    BuiltinSpec {
        provider: "moonshot",
        model: "kimi-k3.1-pro",
        name: "Kimi K3.1 Pro",
        rate: ModelRate::cny(13.0, 1.30, 13.0, 68.0),
        context_window: 1_048_576,
    },
    BuiltinSpec {
        provider: "moonshot",
        model: "kimi-k3.1-pro-highspeed",
        name: "Kimi K3.1 Pro 高速版（2× 用量）",
        rate: ModelRate::cny(26.0, 1.30, 26.0, 136.0),
        context_window: 1_048_576,
    },
    BuiltinSpec {
        provider: "moonshot",
        model: "kimi-k3",
        name: "Kimi K3",
        rate: ModelRate::cny(20.0, 2.00, 20.0, 100.0),
        context_window: 1_048_576,
    },
    BuiltinSpec {
        provider: "moonshot",
        model: "kimi-k2.7-code",
        name: "Kimi K2.7 Code",
        rate: ModelRate::cny(6.50, 1.30, 6.50, 27.0),
        context_window: 262_144,
    },
    BuiltinSpec {
        provider: "zhipu",
        model: "glm-5",
        name: "GLM-5",
        rate: ModelRate::usd(0.70, 0.14, 0.70, 2.80),
        context_window: 128_000,
    },
    BuiltinSpec {
        provider: "zhipu",
        model: "glm-5.3-flash",
        name: "GLM-5.3 Flash",
        rate: ModelRate::usd(0.15, 0.03, 0.15, 0.60),
        context_window: 128_000,
    },
];

/// Normalize a model identifier to its lowercase bare slug (stripping any `provider/` prefix).
fn bare_slug(model: &str) -> String {
    let m = model.trim().to_ascii_lowercase();
    m.rsplit_once('/').map_or(m.clone(), |(_, tail)| tail.to_string())
}

/// Strip trailing date stamps like `-20250514` or `-2025-04-14` for fuzzy catalog lookup.
fn strip_date_suffix(slug: &str) -> &str {
    if let Some(prefix) = slug.rsplit_once('-').and_then(|(head, tail)| {
        (tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit())).then_some(head)
    }) {
        return prefix;
    }
    if slug.len() > 11 {
        let tail = &slug[slug.len() - 11..];
        let bytes = tail.as_bytes();
        if bytes[0] == b'-'
            && bytes[5] == b'-'
            && bytes[8] == b'-'
            && tail[1..5].chars().all(|c| c.is_ascii_digit())
            && tail[6..8].chars().all(|c| c.is_ascii_digit())
            && tail[9..11].chars().all(|c| c.is_ascii_digit())
        {
            return &slug[..slug.len() - 11];
        }
    }
    slug
}

/// Normalize a user-supplied price key (`"provider/model"` or `"model"`).
pub fn normalize_price_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

/// Map `models.dev` provider IDs to OmniDoppelganger provider kinds.
fn map_models_dev_provider(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openai"),
        "google" | "google-vertex" => Some("gemini"),
        "deepseek" => Some("deepseek"),
        "moonshotai" | "moonshotai-cn" | "kimi" | "kimi-for-coding" => Some("moonshot"),
        "zhipuai" | "zai" | "zhipu" => Some("zhipu"),
        "openrouter" => Some("openrouter"),
        _ => None,
    }
}

/// Determine how a provider is billed (`Api`, `Subscription`, or `Local`).
pub fn billing_mode_for(provider: &str, home: Option<&Path>) -> BillingMode {
    let p = provider.trim().to_ascii_lowercase();
    let kind = crate::providers::kind_id(&p);
    if kind == "ollama" {
        return BillingMode::Local;
    }
    if matches!(
        kind,
        "magpie" | "antigravity" | "codex" | "claude_code" | "cursor" | "grok" | "opencode"
    ) {
        return BillingMode::Subscription;
    }
    if kind == "moonshot"
        && let Some(dir) = home
        && crate::kimi::load_oauth_credential(dir, provider).is_some()
    {
        return BillingMode::Subscription;
    }
    BillingMode::Api
}

/// Built-in static fallback rate by normalized model slug.
pub fn builtin_rate_for(model: &str) -> Option<ModelRate> {
    let m = bare_slug(model);
    if m.is_empty() || m == crate::providers::AUTO_MODEL {
        return None;
    }
    let rate = if m.starts_with("claude-opus") || m.starts_with("claude-fable") {
        ModelRate::usd(5.0, 0.50, 6.25, 25.0)
    } else if m.starts_with("claude-sonnet") {
        ModelRate::usd(3.0, 0.30, 3.75, 15.0)
    } else if m.starts_with("claude-haiku") {
        ModelRate::usd(1.0, 0.10, 1.25, 5.0)
    } else if m.starts_with("gpt-6.2-astra") || m.starts_with("gpt-5.4") || m.starts_with("gpt-5") {
        ModelRate::usd(2.50, 0.25, 2.50, 10.0)
    } else if m.starts_with("gpt-6.2-sol") || m.starts_with("gpt-4.1-mini") || m.starts_with("o4-mini") {
        ModelRate::usd(0.60, 0.15, 0.60, 2.40)
    } else if m.starts_with("gemini-2.5-pro") || m.starts_with("gemini-3-pro") {
        ModelRate::usd(1.25, 0.31, 1.25, 10.0)
    } else if m.starts_with("gemini-2.5-flash") || m.starts_with("gemini-3-flash") || m.starts_with("gemini-2.0-flash")
    {
        ModelRate::usd(0.30, 0.075, 0.30, 2.50)
    } else if m.starts_with("deepseek-v4") || m.starts_with("deepseek-chat") || m.starts_with("deepseek-reasoner") {
        ModelRate::usd(0.28, 0.028, 0.28, 1.10)
    } else if m.starts_with("deepseek-flash") {
        ModelRate::usd(0.14, 0.014, 0.14, 0.56)
    } else if m.starts_with("kimi-k3.1-pro-highspeed") {
        ModelRate::cny(26.0, 1.30, 26.0, 136.0)
    } else if m.starts_with("kimi-k3.1-pro") {
        ModelRate::cny(13.0, 1.30, 13.0, 68.0)
    } else if m.starts_with("kimi-k3") {
        ModelRate::cny(20.0, 2.00, 20.0, 100.0)
    } else if m.starts_with("kimi-k2") {
        ModelRate::cny(6.50, 1.30, 6.50, 27.0)
    } else if m.starts_with("glm-5.3-flash") || m.contains("flash") {
        ModelRate::usd(0.15, 0.03, 0.15, 0.60)
    } else if m.starts_with("glm-5") || m.starts_with("glm-4") {
        ModelRate::usd(0.70, 0.14, 0.70, 2.80)
    } else {
        return None;
    };
    Some(rate)
}

/// Parse the JSON payload returned by `https://models.dev/api.json`.
pub fn parse_models_dev_json(raw: &str) -> Result<BTreeMap<String, CachedCatalogModel>, String> {
    let root: Value = serde_json::from_str(raw.trim()).map_err(|e| format!("解析 models.dev JSON 失败：{e}"))?;
    let Some(providers) = root.as_object() else {
        return Err("models.dev 返回格式不是对象".to_string());
    };
    let mut out = BTreeMap::new();
    for (raw_provider, pval) in providers {
        let Some(mapped_provider) = map_models_dev_provider(raw_provider) else {
            continue;
        };
        let Some(models_obj) = pval.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (raw_model_id, mval) in models_obj {
            let Some(cost) = mval.get("cost").and_then(Value::as_object) else {
                continue;
            };
            let Some(input) = cost.get("input").and_then(Value::as_f64) else {
                continue;
            };
            let Some(output) = cost.get("output").and_then(Value::as_f64) else {
                continue;
            };
            if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
                continue;
            }
            let cache_read = cost
                .get("cache_read")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(input);
            let cache_write = cost
                .get("cache_write")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(input);
            let context_window = mval
                .get("limit")
                .and_then(|l| l.get("context"))
                .and_then(Value::as_u64)
                .filter(|&v| v > 0);
            let name = mval
                .get("name")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let model_id = mval
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(raw_model_id)
                .trim()
                .to_ascii_lowercase();
            if model_id.is_empty() {
                continue;
            }
            let slug = bare_slug(&model_id);
            let key = format!("{mapped_provider}/{slug}");
            let entry = CachedCatalogModel {
                provider: mapped_provider.to_string(),
                model: slug,
                name,
                rate: ModelRate {
                    currency: Currency::Usd,
                    input_per_m: input,
                    cached_per_m: cache_read,
                    cache_write_per_m: cache_write,
                    output_per_m: output,
                    source: PriceSource::ModelsDev,
                },
                context_window,
            };
            // Prefer primary provider over secondary alias (e.g. `google` over `google-vertex`).
            out.entry(key).or_insert(entry);
        }
    }
    if out.is_empty() {
        return Err("models.dev 目录中未解析到任何有效价格条目".to_string());
    }
    Ok(out)
}

/// Thread-safe pricing catalog store backed by `~/.odg/custom-pricing.json` and `~/.odg/pricing-cache.json`.
pub struct PriceCatalogStore {
    custom_path: PathBuf,
    cache_path: PathBuf,
    state: Mutex<CatalogState>,
}

struct CatalogState {
    custom: CustomPricingFile,
    synced: SyncedCatalogFile,
}

impl PriceCatalogStore {
    /// Process-wide pricing catalog backed by `~/.odg/`.
    pub fn global() -> &'static Self {
        static STORE: OnceLock<PriceCatalogStore> = OnceLock::new();
        STORE.get_or_init(|| Self::at(crate::settings::home()))
    }

    /// Open (or initialize) a pricing store in an explicit directory.
    pub fn at(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let custom_path = home.join("custom-pricing.json");
        let cache_path = home.join("pricing-cache.json");
        let custom = load_json::<CustomPricingFile>(&custom_path);
        let synced = load_json::<SyncedCatalogFile>(&cache_path);
        Self {
            custom_path,
            cache_path,
            state: Mutex::new(CatalogState { custom, synced }),
        }
    }

    /// Resolve a rate across Custom -> Synced `models.dev` -> Builtin tiers.
    pub fn resolve(&self, provider: &str, model: &str) -> Option<ModelRate> {
        let raw_model = model.trim().to_ascii_lowercase();
        let slug = bare_slug(model);
        if slug.is_empty() || slug == crate::providers::AUTO_MODEL {
            return None;
        }
        let p = provider.trim().to_ascii_lowercase();
        let kind = crate::providers::kind_id(&p).to_string();
        let stripped = strip_date_suffix(&slug);

        let guard = self.state.lock_unpoisoned();

        // 1. User custom overrides (exact provider/model -> kind/model -> model -> slug -> date-stripped).
        let custom_keys = [
            format!("{p}/{raw_model}"),
            format!("{kind}/{raw_model}"),
            format!("{p}/{slug}"),
            format!("{kind}/{slug}"),
            raw_model.clone(),
            slug.clone(),
            format!("{kind}/{stripped}"),
            stripped.to_string(),
        ];
        for key in &custom_keys {
            if let Some(rate) = guard.custom.rates.get(key) {
                let mut resolved = *rate;
                resolved.source = PriceSource::Custom;
                return Some(resolved);
            }
        }

        // 2. Synced models.dev cache (provider-scoped first, then bare slug across providers).
        let synced_keys = [
            format!("{kind}/{slug}"),
            format!("{kind}/{stripped}"),
            format!("openrouter/{slug}"),
        ];
        for key in &synced_keys {
            if let Some(entry) = guard.synced.models.get(key) {
                let mut resolved = entry.rate;
                resolved.source = PriceSource::ModelsDev;
                return Some(resolved);
            }
        }
        if let Some(entry) = guard
            .synced
            .models
            .values()
            .find(|e| e.model == slug || e.model == stripped)
        {
            let mut resolved = entry.rate;
            resolved.source = PriceSource::ModelsDev;
            return Some(resolved);
        }

        drop(guard);

        // 3. Built-in compile-time fallback.
        builtin_rate_for(&slug)
    }

    /// Save or update a user custom price override.
    pub fn set_custom(&self, key: &str, mut rate: ModelRate) -> Result<PriceCatalogView, String> {
        let norm = normalize_price_key(key);
        if norm.is_empty() {
            return Err("模型标识不能为空".to_string());
        }
        if !rate.input_per_m.is_finite()
            || !rate.cached_per_m.is_finite()
            || !rate.cache_write_per_m.is_finite()
            || !rate.output_per_m.is_finite()
            || rate.input_per_m < 0.0
            || rate.cached_per_m < 0.0
            || rate.cache_write_per_m < 0.0
            || rate.output_per_m < 0.0
        {
            return Err("单价必须是非负有限数字".to_string());
        }
        rate.source = PriceSource::Custom;
        {
            let mut guard = self.state.lock_unpoisoned();
            guard.custom.rates.insert(norm, rate);
            save_json(&self.custom_path, &guard.custom)?;
        }
        Ok(self.view())
    }

    /// Remove a user custom price override.
    pub fn delete_custom(&self, key: &str) -> Result<PriceCatalogView, String> {
        let norm = normalize_price_key(key);
        {
            let mut guard = self.state.lock_unpoisoned();
            guard.custom.rates.remove(&norm);
            save_json(&self.custom_path, &guard.custom)?;
        }
        Ok(self.view())
    }

    /// Replace the synced `models.dev` cache from parsed entries (used by sync and tests).
    pub fn replace_synced(
        &self,
        models: BTreeMap<String, CachedCatalogModel>,
        synced_at: String,
    ) -> Result<PriceCatalogView, String> {
        {
            let mut guard = self.state.lock_unpoisoned();
            guard.synced = SyncedCatalogFile {
                synced_at: Some(synced_at),
                models,
            };
            save_json(&self.cache_path, &guard.synced)?;
        }
        Ok(self.view())
    }

    /// Fetch `https://models.dev/api.json`, parse it, persist to `~/.odg/pricing-cache.json`, and return the new view.
    pub async fn sync_models_dev(&self) -> Result<PriceCatalogView, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
        let resp = client
            .get(MODELS_DEV_API_URL)
            .header("Accept", "application/json")
            .header("User-Agent", format!("OmniDoppelganger/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .map_err(|e| format!("拉取 models.dev 价格目录失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("models.dev 返回 HTTP {}", resp.status().as_u16()));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("读取 models.dev 响应失败：{e}"))?;
        let parsed = parse_models_dev_json(&body)?;
        self.replace_synced(parsed, Utc::now().to_rfc3339())
    }

    /// Build a UI-friendly snapshot of custom and effective catalog entries.
    pub fn view(&self) -> PriceCatalogView {
        let guard = self.state.lock_unpoisoned();
        let mut effective_map: BTreeMap<String, PriceCatalogEntry> = BTreeMap::new();

        // 1. Seed with built-in defaults.
        for spec in BUILTIN_SPECS {
            let key = format!("{}/{}", spec.provider, spec.model);
            effective_map.insert(
                key.clone(),
                PriceCatalogEntry {
                    key,
                    provider: Some(spec.provider.to_string()),
                    model: spec.model.to_string(),
                    name: Some(spec.name.to_string()),
                    rate: spec.rate,
                    context_window: Some(spec.context_window),
                },
            );
        }

        // 2. Overlay synced models.dev entries.
        for (key, item) in &guard.synced.models {
            let mut rate = item.rate;
            rate.source = PriceSource::ModelsDev;
            effective_map.insert(
                key.clone(),
                PriceCatalogEntry {
                    key: key.clone(),
                    provider: Some(item.provider.clone()),
                    model: item.model.clone(),
                    name: item.name.clone(),
                    rate,
                    context_window: item.context_window,
                },
            );
        }

        // 3. Overlay user custom entries.
        let mut custom = Vec::with_capacity(guard.custom.rates.len());
        for (key, raw_rate) in &guard.custom.rates {
            let mut rate = *raw_rate;
            rate.source = PriceSource::Custom;
            let (provider, model) = match key.split_once('/') {
                Some((p, m)) => (Some(p.to_string()), m.to_string()),
                None => (None, key.clone()),
            };
            let existing_meta = effective_map.get(key);
            let entry = PriceCatalogEntry {
                key: key.clone(),
                provider,
                model,
                name: existing_meta.and_then(|e| e.name.clone()),
                rate,
                context_window: existing_meta.and_then(|e| e.context_window),
            };
            custom.push(entry.clone());
            effective_map.insert(key.clone(), entry);
        }

        PriceCatalogView {
            synced_at: guard.synced.synced_at.clone(),
            models_dev_count: guard.synced.models.len(),
            custom_count: custom.len(),
            usd_to_cny: DEFAULT_USD_TO_CNY,
            custom,
            effective: effective_map.into_values().collect(),
        }
    }
}

/// Look up public API pricing for a `(provider, model)` pair.
/// Returns `None` for local / CLI / gateway providers (`ollama`, `magpie`, `antigravity`, etc.)
/// so pay-as-you-go API bill estimation stays accurate.
pub fn rate_for(provider: &str, model: &str) -> Option<ModelRate> {
    let p = provider.to_ascii_lowercase();
    let kind = crate::providers::kind_id(&p);
    if matches!(
        kind,
        "ollama" | "magpie" | "antigravity" | "codex" | "claude_code" | "cursor" | "grok" | "opencode"
    ) {
        return None;
    }
    PriceCatalogStore::global().resolve(provider, model)
}

/// Look up catalog pricing for any non-local provider (including subscriptions & CLIs)
/// so subscription API-equivalent value can be tracked in the usage ledger.
pub fn catalog_rate_for(provider: &str, model: &str) -> Option<ModelRate> {
    let p = provider.to_ascii_lowercase();
    if crate::providers::kind_id(&p) == "ollama" {
        return None;
    }
    PriceCatalogStore::global().resolve(provider, model)
}

/// Compute exact cost from a [`ModelRate`] and [`Tokens`] (`tokens.input` includes `cached` and `cache_write`).
pub fn estimate_cost(rate: ModelRate, tokens: Tokens) -> CostEstimate {
    let cached_read = tokens.cached.min(tokens.input);
    let remaining_after_read = tokens.input.saturating_sub(cached_read);
    let cache_write = tokens.cache_write.min(remaining_after_read);
    let uncached = remaining_after_read.saturating_sub(cache_write);
    let output = tokens.output;

    let amount = (uncached as f64 * rate.input_per_m
        + cached_read as f64 * rate.cached_per_m
        + cache_write as f64 * rate.cache_write_per_m
        + output as f64 * rate.output_per_m)
        / 1_000_000.0;

    CostEstimate {
        amount,
        currency: rate.currency,
        usd_equiv: rate.currency.to_usd(amount),
        source: rate.source,
    }
}

/// Estimate USD cost for pay-as-you-go API providers (`tokens.input` includes `cached` and `cache_write`).
pub fn estimate_usd(provider: &str, model: &str, tokens: Tokens) -> Option<f64> {
    let rate = rate_for(provider, model)?;
    Some(estimate_cost(rate, tokens).usd_equiv)
}

/// Estimate native + USD-equivalent cost for any non-local provider (API or subscription equivalent).
pub fn estimate_catalog_cost(provider: &str, model: &str, tokens: Tokens) -> Option<CostEstimate> {
    let rate = catalog_rate_for(provider, model)?;
    Some(estimate_cost(rate, tokens))
}

fn load_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else {
        return T::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let body = serde_json::to_string_pretty(value).map_err(|e| format!("序列化价格文件失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写入临时价格文件失败：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("保存价格文件失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_usd_with_cache_read_and_write_and_skips_local_providers() {
        let tok = Tokens {
            input: 1_000_000,
            cached: 800_000,
            cache_write: 0,
            output: 100_000,
        };
        // Sonnet: 200k * $3/M ($0.60) + 800k * $0.30/M ($0.24) + 100k * $15/M ($1.50) = $2.34
        let sonnet = estimate_cost(builtin_rate_for("claude-sonnet-5-5").unwrap(), tok);
        assert!((sonnet.usd_equiv - 2.34).abs() < 1e-6, "got {}", sonnet.usd_equiv);

        // With cache_write (100k @ $3.75/M = $0.375, 100k uncached @ $3/M = $0.30, 800k cached @ $0.30/M = $0.24, 100k out = $1.50 => $2.415)
        let tok_write = Tokens {
            input: 1_000_000,
            cached: 800_000,
            cache_write: 100_000,
            output: 100_000,
        };
        let sonnet_w = estimate_cost(builtin_rate_for("claude-sonnet-4-6").unwrap(), tok_write);
        assert!((sonnet_w.usd_equiv - 2.415).abs() < 1e-6, "got {}", sonnet_w.usd_equiv);

        // OpenRouter prefixed model resolves by model slug
        let gem = estimate_usd("openrouter", "google/gemini-2.5-pro", tok).unwrap();
        assert!(gem > 0.0);

        // Local Ollama & CLI backends report no API bill
        assert!(estimate_usd("ollama", "qwen3:32b", tok).is_none());
        assert!(estimate_usd("claude_code", "claude-sonnet-4-6", tok).is_none());

        // Subscription CLI still resolves catalog rate for API-equivalent tracking
        assert!(estimate_catalog_cost("claude_code", "claude-sonnet-4-6", tok).is_some());
        assert!(estimate_catalog_cost("ollama", "qwen3:32b", tok).is_none());
    }

    #[test]
    fn three_tier_resolution_prefers_custom_then_models_dev_then_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PriceCatalogStore::at(tmp.path());

        // Builtin Kimi K3.1 Pro is native CNY (¥13 / ¥1.3 / ¥13 / ¥68)
        let builtin = store.resolve("moonshot", "kimi-k3.1-pro").unwrap();
        assert_eq!(builtin.currency, Currency::Cny);
        assert_eq!(builtin.source, PriceSource::Builtin);
        assert!((builtin.input_per_m - 13.0).abs() < 1e-6);

        // Sync models.dev overrides builtin
        let raw = r#"{
            "moonshotai": {
                "models": {
                    "kimi-k3.1-pro": {
                        "id": "kimi-k3.1-pro",
                        "name": "Kimi K3.1 Pro",
                        "cost": { "input": 1.9, "output": 9.9, "cache_read": 0.2, "cache_write": 1.9 },
                        "limit": { "context": 1048576 }
                    }
                }
            }
        }"#;
        let parsed = parse_models_dev_json(raw).unwrap();
        store.replace_synced(parsed, "2026-11-27T00:00:00Z".into()).unwrap();
        let synced = store.resolve("moonshot", "kimi-k3.1-pro").unwrap();
        assert_eq!(synced.source, PriceSource::ModelsDev);
        assert_eq!(synced.currency, Currency::Usd);
        assert!((synced.input_per_m - 1.9).abs() < 1e-6);

        // Custom override beats models.dev
        let custom_rate = ModelRate {
            currency: Currency::Cny,
            input_per_m: 10.0,
            cached_per_m: 1.0,
            cache_write_per_m: 10.0,
            output_per_m: 50.0,
            source: PriceSource::Custom,
        };
        store.set_custom("moonshot/kimi-k3.1-pro", custom_rate).unwrap();
        let custom = store.resolve("moonshot", "kimi-k3.1-pro").unwrap();
        assert_eq!(custom.source, PriceSource::Custom);
        assert_eq!(custom.currency, Currency::Cny);
        assert!((custom.output_per_m - 50.0).abs() < 1e-6);

        // Deleting custom falls back cleanly to models.dev
        store.delete_custom("moonshot/kimi-k3.1-pro").unwrap();
        let after_del = store.resolve("moonshot", "kimi-k3.1-pro").unwrap();
        assert_eq!(after_del.source, PriceSource::ModelsDev);
    }
}
