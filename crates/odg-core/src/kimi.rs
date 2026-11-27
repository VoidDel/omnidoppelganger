//! Kimi Code official OAuth Device Login (`RFC 8628`) & Coding Plan quota client (`GET /coding/v1/usages`).
//!
//! - Supports both Mainland China (`auth.kimi.com` / `api.kimi.com/coding/v1`) and
//!   Global (`auth.kimi.ai` / `api.kimi.ai/coding/v1`) Kimi Code accounts.
//! - Stores OAuth tokens per provider id in `<ODG_HOME>/kimi-oauth.json` with `0600`
//!   permissions and serializes token refreshes per provider to prevent refresh-token
//!   rotation races.
//! - Fetches real-time 5-hour and weekly quota windows from `/coding/v1/usages`
//!   (and plan tier from `/coding/v1/me` when authenticated via OAuth) and normalizes
//!   them into [`AntigravityQuota`] (`QuotaGroup` / `QuotaBucket`).

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::antigravity::{AntigravityQuota, QuotaBucket, QuotaGroup};
use crate::fsutil::{ensure_private_dir, write_private_atomically};
use crate::settings::SettingsStore;
use crate::sync_ext::MutexExt;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_STORE_FILE: &str = "kimi-oauth.json";
const DEVICE_ID_FILE: &str = "kimi-device-id";
const QUOTA_CACHE_TTL: Duration = Duration::from_secs(60);
const QUOTA_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_LEEWAY_SECS: i64 = 120;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KimiRegion {
    #[default]
    MainlandCn,
    Global,
}

impl KimiRegion {
    pub fn from_str_lossy(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "global" | "international" | "ai" => Self::Global,
            _ => Self::MainlandCn,
        }
    }

    pub fn endpoints(self) -> (&'static str, &'static str) {
        match self {
            Self::MainlandCn => ("https://auth.kimi.com", "https://api.kimi.com/coding/v1"),
            Self::Global => ("https://auth.kimi.ai", "https://api.kimi.ai/coding/v1"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KimiOAuthCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    #[serde(default)]
    pub region: KimiRegion,
    pub oauth_host: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct KimiOAuthStart {
    pub flow_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct KimiOAuthPoll {
    pub authenticated: bool,
    pub interval: u64,
    pub ended: Option<String>,
    pub provider_id: Option<String>,
    pub nickname: Option<String>,
    pub plan_name: Option<String>,
}

impl KimiOAuthPoll {
    fn pending(interval: u64) -> Self {
        Self {
            authenticated: false,
            interval,
            ended: None,
            provider_id: None,
            nickname: None,
            plan_name: None,
        }
    }

    fn ended(reason: &str) -> Self {
        Self {
            authenticated: false,
            interval: 0,
            ended: Some(reason.to_string()),
            provider_id: None,
            nickname: None,
            plan_name: None,
        }
    }
}

#[derive(Clone)]
struct PendingLogin {
    id: String,
    polling: bool,
    region: KimiRegion,
    oauth_host: String,
    base_url: String,
    provider_id: Option<String>,
    custom_name: Option<String>,
    device_code: String,
    device_id: String,
    expires: Instant,
    next_poll: Instant,
    interval: u64,
}

fn pending_lock() -> &'static tokio::sync::Mutex<Option<PendingLogin>> {
    static PENDING: OnceLock<tokio::sync::Mutex<Option<PendingLogin>>> = OnceLock::new();
    PENDING.get_or_init(|| tokio::sync::Mutex::new(None))
}

fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn refresh_lock_for(provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    map.lock_unpoisoned()
        .entry(provider_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn oauth_store_path(home: &Path) -> PathBuf {
    home.join(OAUTH_STORE_FILE)
}

fn read_oauth_store(home: &Path) -> HashMap<String, KimiOAuthCredential> {
    let _guard = store_lock().lock_unpoisoned();
    fs::read_to_string(oauth_store_path(home))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn update_oauth_store(
    home: &Path,
    mutate: impl FnOnce(&mut HashMap<String, KimiOAuthCredential>),
) -> Result<(), String> {
    let _guard = store_lock().lock_unpoisoned();
    let path = oauth_store_path(home);
    let mut map: HashMap<String, KimiOAuthCredential> = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    mutate(&mut map);
    ensure_private_dir(home).map_err(|e| format!("无法准备 Kimi OAuth 配置目录：{e}"))?;
    let bytes = serde_json::to_vec_pretty(&map).map_err(|e| format!("序列化 Kimi OAuth 凭据失败：{e}"))?;
    write_private_atomically(&path, &bytes).map_err(|e| format!("保存 Kimi OAuth 凭据失败：{e}"))
}

pub fn device_id(home: &Path) -> String {
    let path = home.join(DEVICE_ID_FILE);
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let _ = ensure_private_dir(home);
    let _ = write_private_atomically(&path, id.as_bytes());
    id
}

/// Attach Kimi Code CLI identification headers required by `auth.kimi.*` and `api.kimi.*/coding/v1`.
pub fn apply_kimi_headers(request: reqwest::RequestBuilder, device_id: &str) -> reqwest::RequestBuilder {
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Unknown".to_string());
    let hostname: String = hostname
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(128)
        .collect();
    request
        .header("Accept", "application/json")
        .header("User-Agent", concat!("omnidoppelganger/", env!("CARGO_PKG_VERSION")))
        .header("X-Msh-Platform", "kimi_code_cli")
        .header("X-Msh-Version", env!("CARGO_PKG_VERSION"))
        .header("X-Msh-Device-Id", device_id)
        .header(
            "X-Msh-Device-Name",
            if hostname.is_empty() { "Unknown" } else { &hostname },
        )
        .header(
            "X-Msh-Device-Model",
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        )
        .header("X-Msh-Os-Version", std::env::consts::OS)
}

fn oauth_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(OAUTH_HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("初始化 HTTP 客户端失败：{e}"))
}

fn required_string<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Kimi OAuth 响应缺少字段 `{key}`"))
}

fn parse_positive_secs(payload: &Value, key: &str, fallback: Option<u64>) -> Result<u64, String> {
    payload[key]
        .as_u64()
        .or_else(|| payload[key].as_str()?.trim().parse::<u64>().ok())
        .or(fallback)
        .filter(|&n| n > 0 && n <= 31_536_000)
        .ok_or_else(|| format!("Kimi OAuth 响应字段 `{key}` 无效"))
}

fn parse_token_payload(
    payload: &Value,
    region: KimiRegion,
    oauth_host: &str,
    base_url: &str,
) -> Result<KimiOAuthCredential, String> {
    let access_token = required_string(payload, "access_token")?.to_string();
    let refresh_token = required_string(payload, "refresh_token")?.to_string();
    let expires_in = parse_positive_secs(payload, "expires_in", Some(3600))?;
    Ok(KimiOAuthCredential {
        access_token,
        refresh_token,
        expires_at: now_unix_secs().saturating_add(expires_in as i64),
        region,
        oauth_host: oauth_host.to_string(),
        base_url: base_url.to_string(),
        user_id: None,
        nickname: None,
        plan_name: None,
    })
}

async fn fetch_profile(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    dev_id: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let url = format!("{}/me", base_url.trim_end_matches('/'));
    let Ok(resp) = apply_kimi_headers(http.get(&url), dev_id)
        .bearer_auth(access_token)
        .send()
        .await
    else {
        return (None, None, None);
    };
    if !resp.status().is_success() {
        return (None, None, None);
    }
    let Ok(payload) = resp.json::<Value>().await else {
        return (None, None, None);
    };
    let field = |k: &str| {
        payload[k]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    (field("user_id"), field("nickname"), field("user_level_name"))
}

fn encode_form(pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("https://kimi.invalid/").expect("valid static URL");
    url.query_pairs_mut().extend_pairs(pairs);
    url.query().unwrap_or_default().to_string()
}

fn post_form(http: &reqwest::Client, url: &str, dev_id: &str, pairs: &[(&str, &str)]) -> reqwest::RequestBuilder {
    apply_kimi_headers(http.post(url), dev_id)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(encode_form(pairs))
}

/// Start Kimi Code OAuth Device Authorization flow.
pub async fn start_oauth_login(
    home: &Path,
    region: KimiRegion,
    provider_id: Option<String>,
    custom_name: Option<String>,
) -> Result<KimiOAuthStart, String> {
    let (oauth_host, base_url) = region.endpoints();
    let dev_id = device_id(home);
    let http = oauth_http_client()?;

    let mut pending = pending_lock().lock().await;
    *pending = None;

    let url = format!("{oauth_host}/api/oauth/device_authorization");
    let resp = post_form(&http, &url, &dev_id, &[("client_id", CLIENT_ID)])
        .send()
        .await
        .map_err(|e| format!("请求 Kimi OAuth 授权失败：{e}"))?;

    let status = resp.status().as_u16();
    let payload = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("解析 Kimi OAuth 响应失败（HTTP {status}）：{e}"))?;
    if status != 200 {
        let desc = payload["error_description"]
            .as_str()
            .or_else(|| payload["error"].as_str())
            .unwrap_or("设备授权请求被拒绝");
        return Err(format!("Kimi 设备授权失败（HTTP {status}）：{desc}"));
    }

    let device_code = required_string(&payload, "device_code")?.to_string();
    let user_code = required_string(&payload, "user_code")?.to_string();
    let uri = payload["verification_uri_complete"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            payload["verification_uri"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| "Kimi OAuth 响应缺少授权链接".to_string())?;
    let parsed_uri = reqwest::Url::parse(uri).map_err(|_| "Kimi 返回的授权链接格式无效".to_string())?;
    if parsed_uri.scheme() != "https" || parsed_uri.host_str().is_none() {
        return Err("Kimi 返回的授权链接不是安全的 HTTPS 地址".to_string());
    }

    let expires_in = parse_positive_secs(&payload, "expires_in", Some(600))?;
    let interval = parse_positive_secs(&payload, "interval", Some(5))?;
    let flow_id = uuid::Uuid::now_v7().to_string();

    *pending = Some(PendingLogin {
        id: flow_id.clone(),
        polling: false,
        region,
        oauth_host: oauth_host.to_string(),
        base_url: base_url.to_string(),
        provider_id: provider_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        custom_name: custom_name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        device_code,
        device_id: dev_id,
        expires: Instant::now() + Duration::from_secs(expires_in),
        next_poll: Instant::now() + Duration::from_secs(interval),
        interval,
    });

    Ok(KimiOAuthStart {
        flow_id,
        user_code,
        verification_uri: uri.to_string(),
        expires_in,
        interval,
    })
}

/// Poll an in-flight Kimi OAuth Device Authorization flow.
pub async fn poll_oauth_login(home: &Path, flow_id: &str, settings: &SettingsStore) -> Result<KimiOAuthPoll, String> {
    let mut flow = {
        let mut pending = pending_lock().lock().await;
        let Some(flow) = pending.as_mut().filter(|p| p.id == flow_id) else {
            return Ok(KimiOAuthPoll::ended("cancelled"));
        };
        if Instant::now() >= flow.expires {
            *pending = None;
            return Ok(KimiOAuthPoll::ended("expired"));
        }
        if flow.polling || Instant::now() < flow.next_poll {
            return Ok(KimiOAuthPoll::pending(flow.interval));
        }
        flow.polling = true;
        flow.clone()
    };

    let http = oauth_http_client()?;
    let url = format!("{}/api/oauth/token", flow.oauth_host);
    let resp = post_form(
        &http,
        &url,
        &flow.device_id,
        &[
            ("client_id", CLIENT_ID),
            ("device_code", flow.device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    )
    .send()
    .await;

    let mut pending = pending_lock().lock().await;
    if pending.as_ref().is_none_or(|p| p.id != flow_id) {
        return Ok(KimiOAuthPoll::ended("cancelled"));
    }

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            if let Some(p) = pending.as_mut() {
                p.polling = false;
                p.next_poll = Instant::now() + Duration::from_secs(p.interval);
            }
            return Err(format!("轮询 Kimi OAuth 令牌失败：{e}"));
        }
    };

    let status = resp.status().as_u16();
    let payload = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("解析 Kimi OAuth 令牌响应失败（HTTP {status}）：{e}"))?;

    if status != 200 {
        let code = payload["error"].as_str().unwrap_or("");
        match code {
            "authorization_pending" => {
                if let Some(p) = pending.as_mut() {
                    p.polling = false;
                    p.next_poll = Instant::now() + Duration::from_secs(p.interval);
                }
                return Ok(KimiOAuthPoll::pending(flow.interval));
            }
            "slow_down" => {
                flow.interval = flow.interval.saturating_add(5);
                if let Some(p) = pending.as_mut() {
                    p.polling = false;
                    p.interval = flow.interval;
                    p.next_poll = Instant::now() + Duration::from_secs(flow.interval);
                }
                return Ok(KimiOAuthPoll::pending(flow.interval));
            }
            "expired_token" => {
                *pending = None;
                return Ok(KimiOAuthPoll::ended("expired"));
            }
            "access_denied" => {
                *pending = None;
                return Ok(KimiOAuthPoll::ended("denied"));
            }
            _ => {
                if let Some(p) = pending.as_mut() {
                    p.polling = false;
                    p.next_poll = Instant::now() + Duration::from_secs(p.interval);
                }
                let desc = payload["error_description"].as_str().unwrap_or(code);
                return Err(format!("Kimi OAuth 授权失败（HTTP {status}）：{desc}"));
            }
        }
    }

    let mut cred = parse_token_payload(&payload, flow.region, &flow.oauth_host, &flow.base_url)?;
    // Drop the pending lock while querying `/me` so cancel remains responsive.
    drop(pending);

    let (user_id, nickname, plan_name) =
        fetch_profile(&http, &flow.base_url, &cred.access_token, &flow.device_id).await;
    cred.user_id = user_id;
    cred.nickname = nickname.clone();
    cred.plan_name = plan_name.clone();

    let mut pending = pending_lock().lock().await;
    if pending.as_ref().is_none_or(|p| p.id != flow_id) {
        return Ok(KimiOAuthPoll::ended("cancelled"));
    }

    let provider_id = match flow.provider_id.clone() {
        Some(existing) => existing,
        None => {
            let label = flow
                .custom_name
                .clone()
                .or_else(|| nickname.as_ref().map(|n| format!("Kimi ({n})")))
                .unwrap_or_default();
            settings
                .add_provider("moonshot", &label)
                .map_err(|e| format!("创建 Kimi 宗门失败：{e}"))?
        }
    };

    // Persist OAuth token under `provider_id` and point `base_urls[provider_id]` to Kimi Code endpoint.
    update_oauth_store(home, |map| {
        map.insert(provider_id.clone(), cred.clone());
    })?;

    let mut current = settings.load();
    current.base_urls.insert(provider_id.clone(), flow.base_url.clone());
    if let Some(name) = flow.custom_name.as_deref().filter(|s| !s.is_empty()) {
        current.provider_names.insert(provider_id.clone(), name.to_string());
    }
    let _ = settings.save(current);

    *pending = None;

    Ok(KimiOAuthPoll {
        authenticated: true,
        interval: 0,
        ended: None,
        provider_id: Some(provider_id),
        nickname,
        plan_name,
    })
}

/// Cancel an in-flight Kimi OAuth Device Authorization flow.
pub async fn cancel_oauth_login(flow_id: Option<&str>) {
    let mut pending = pending_lock().lock().await;
    match flow_id {
        Some(id) => {
            if pending.as_ref().is_some_and(|p| p.id == id) {
                *pending = None;
            }
        }
        None => {
            *pending = None;
        }
    }
}

/// Remove stored Kimi OAuth credentials for `provider_id`.
pub fn logout_oauth(home: &Path, provider_id: &str) -> Result<(), String> {
    let existing = read_oauth_store(home);
    if !existing.contains_key(provider_id) {
        return Ok(());
    }
    update_oauth_store(home, |map| {
        map.remove(provider_id);
    })
}

/// Read the stored OAuth credential for `provider_id`, if any.
pub fn load_oauth_credential(home: &Path, provider_id: &str) -> Option<KimiOAuthCredential> {
    read_oauth_store(home)
        .remove(provider_id)
        .filter(|c| !c.access_token.trim().is_empty() || !c.refresh_token.trim().is_empty())
}

/// Return the stored OAuth `access_token` for `provider_id`, if any.
pub fn load_oauth_access_token(home: &Path, provider_id: &str) -> Option<String> {
    load_oauth_credential(home, provider_id)
        .map(|c| c.access_token.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Display label shown in Settings `KeyStatus.preview` for an OAuth-authenticated Kimi provider.
pub fn oauth_preview(home: &Path, provider_id: &str) -> Option<String> {
    let cred = load_oauth_credential(home, provider_id)?;
    let label = cred
        .plan_name
        .as_deref()
        .or(cred.nickname.as_deref())
        .unwrap_or("已登录");
    Some(label.to_string())
}

/// Ensure the Kimi OAuth access token for `provider_id` is fresh, rotating via `refresh_token` if needed.
/// Returns `Ok(None)` if `provider_id` does not use OAuth.
pub async fn ensure_fresh_access_token(home: &Path, provider_id: &str) -> Result<Option<KimiOAuthCredential>, String> {
    let Some(cred) = load_oauth_credential(home, provider_id) else {
        return Ok(None);
    };
    let now = now_unix_secs();
    if !cred.access_token.is_empty() && cred.expires_at > now + REFRESH_LEEWAY_SECS {
        return Ok(Some(cred));
    }

    let lock = refresh_lock_for(provider_id);
    let _guard = lock.lock().await;

    // Re-check disk after acquiring the per-provider refresh lock.
    let Some(current) = load_oauth_credential(home, provider_id) else {
        return Ok(None);
    };
    let now = now_unix_secs();
    if !current.access_token.is_empty() && current.expires_at > now + REFRESH_LEEWAY_SECS {
        return Ok(Some(current));
    }
    if current.refresh_token.trim().is_empty() {
        return Err("Kimi OAuth 令牌已过期，请在设置中重新登录。".to_string());
    }

    let dev_id = device_id(home);
    let http = oauth_http_client()?;
    let url = format!("{}/api/oauth/token", current.oauth_host.trim_end_matches('/'));
    let resp = post_form(
        &http,
        &url,
        &dev_id,
        &[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
        ],
    )
    .send()
    .await
    .map_err(|e| format!("刷新 Kimi OAuth 令牌失败：{e}"))?;

    let status = resp.status().as_u16();
    let payload = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("解析 Kimi OAuth 刷新响应失败（HTTP {status}）：{e}"))?;

    if status != 200 {
        let desc = payload["error_description"]
            .as_str()
            .or_else(|| payload["error"].as_str())
            .unwrap_or("请重新登录");
        return Err(format!("Kimi OAuth 令牌刷新被拒绝（HTTP {status}）：{desc}"));
    }

    let mut refreshed = parse_token_payload(&payload, current.region, &current.oauth_host, &current.base_url)?;
    refreshed.user_id = current.user_id;
    refreshed.nickname = current.nickname;
    refreshed.plan_name = current.plan_name;

    update_oauth_store(home, |map| {
        map.insert(provider_id.to_string(), refreshed.clone());
    })?;

    Ok(Some(refreshed))
}

/// Derive the Kimi Code `/usages` endpoint URL from the configured provider `base_url`.
///
/// - If `base_url` already points to a `/coding` endpoint (e.g. `https://api.kimi.com/coding/v1`
///   or `https://api.kimi.ai/coding/v1`), appends `/usages`.
/// - If `base_url` targets the global domain (`moonshot.ai` / `kimi.ai`), maps to
///   `https://api.kimi.ai/coding/v1/usages`.
/// - Otherwise defaults to `https://api.kimi.com/coding/v1/usages`.
pub fn kimi_usages_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with("/usages") {
        return trimmed.to_string();
    }
    if lower.contains("/coding") {
        return format!("{trimmed}/usages");
    }
    if lower.contains("moonshot.ai") || lower.contains("kimi.ai") {
        return "https://api.kimi.ai/coding/v1/usages".to_string();
    }
    "https://api.kimi.com/coding/v1/usages".to_string()
}

fn parse_f64(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return n.is_finite().then_some(n);
    }
    if let Some(s) = v.as_str() {
        let parsed = s.trim().parse::<f64>().ok()?;
        return parsed.is_finite().then_some(parsed);
    }
    None
}

fn extract_reset_time(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(n) = v.as_i64().filter(|&n| n > 0) {
        let dt = if n > 10_000_000_000 {
            chrono::DateTime::from_timestamp_millis(n)
        } else {
            chrono::DateTime::from_timestamp(n, 0)
        };
        return dt.map(|t| t.to_rfc3339());
    }
    None
}

fn compute_remaining_percent(obj: &Value) -> Option<u8> {
    let limit = obj.get("limit").and_then(parse_f64)?;
    if limit <= 0.0 {
        return Some(0);
    }
    let remaining = if let Some(rem) = obj.get("remaining").and_then(parse_f64) {
        rem.max(0.0)
    } else {
        let used = obj.get("used").and_then(parse_f64)?;
        (limit - used).max(0.0)
    };
    Some(((remaining / limit) * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Parse the JSON response from `GET /coding/v1/usages` into [`AntigravityQuota`].
pub fn parse_kimi_usages_json(raw: &str) -> Result<AntigravityQuota, String> {
    let body: Value = serde_json::from_str(raw).map_err(|e| format!("解析 Kimi 额度响应失败：{e}"))?;
    let mut buckets = Vec::new();

    // 5-hour rolling window(s): `limits[].detail`
    if let Some(limits) = body.get("limits").and_then(|v| v.as_array()) {
        for (idx, item) in limits.iter().enumerate() {
            let detail = item.get("detail").unwrap_or(item);
            let Some(remaining_percent) = compute_remaining_percent(detail) else {
                continue;
            };
            let reset_time = detail
                .get("resetTime")
                .or_else(|| detail.get("reset_time"))
                .and_then(extract_reset_time);
            buckets.push(QuotaBucket {
                id: format!("kimi-5h-{idx}"),
                name: "5 小时限额".to_string(),
                description: None,
                window: "5h".to_string(),
                remaining_percent,
                reset_time,
            });
        }
    }

    // Overall / weekly window: `usage`
    if let Some(usage) = body.get("usage")
        && let Some(remaining_percent) = compute_remaining_percent(usage)
    {
        let reset_time = usage
            .get("resetTime")
            .or_else(|| usage.get("reset_time"))
            .and_then(extract_reset_time);
        buckets.push(QuotaBucket {
            id: "kimi-weekly".to_string(),
            name: "周限额".to_string(),
            description: None,
            window: "weekly".to_string(),
            remaining_percent,
            reset_time,
        });
    }

    if buckets.is_empty() {
        return Err("Kimi 额度接口未返回可用的 5 小时或周限额数据。".to_string());
    }

    Ok(AntigravityQuota {
        fetched_at: chrono::Utc::now().to_rfc3339(),
        groups: vec![QuotaGroup {
            name: "Kimi Code".to_string(),
            description: Some("Kimi Code 订阅额度（5 小时与周限额）".to_string()),
            buckets,
        }],
    })
}

fn cache_key(url: &str, api_key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    api_key.hash(&mut hasher);
    format!("{url}#{:016x}", hasher.finish())
}

/// Query real-time Kimi Code Coding Plan quota (`GET /coding/v1/usages`) using either
/// a fresh OAuth access token (when the provider uses OAuth) or a static API key.
pub async fn fetch_quota_for_provider(
    home: &Path,
    provider_id: &str,
    base_url: &str,
    api_key: Option<&str>,
    force: bool,
) -> Result<AntigravityQuota, String> {
    if let Some(oauth) = ensure_fresh_access_token(home, provider_id).await? {
        let dev_id = device_id(home);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(QUOTA_HTTP_TIMEOUT)
            .build()
            .map_err(|e| format!("初始化 HTTP 客户端失败：{e}"))?;
        let (quota_res, (_, _, plan)) = tokio::join!(
            fetch_quota_with_headers(&oauth.base_url, &oauth.access_token, Some(&dev_id), force),
            fetch_profile(&http, &oauth.base_url, &oauth.access_token, &dev_id),
        );
        let mut quota = quota_res?;
        let plan_label = plan.or(oauth.plan_name);
        if let (Some(group), Some(plan_name)) = (quota.groups.first_mut(), plan_label) {
            group.name = format!("Kimi Code · {plan_name}");
        }
        return Ok(quota);
    }

    fetch_quota(base_url, api_key, force).await
}

/// Query real-time Kimi Code Coding Plan quota (`GET /coding/v1/usages`).
/// Cached per `(endpoint, api_key)` for 60 seconds unless `force` is true.
pub async fn fetch_quota(base_url: &str, api_key: Option<&str>, force: bool) -> Result<AntigravityQuota, String> {
    let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) else {
        return Err("请先在设置中配置 Kimi Code API Key 或登录 Kimi OAuth 账号后再查询订阅额度。".to_string());
    };
    fetch_quota_with_headers(base_url, key, None, force).await
}

async fn fetch_quota_with_headers(
    base_url: &str,
    token: &str,
    dev_id: Option<&str>,
    force: bool,
) -> Result<AntigravityQuota, String> {
    let url = kimi_usages_url(base_url);
    let key_id = cache_key(&url, token);

    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, AntigravityQuota)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force
        && let Some((at, quota)) = cache.lock_unpoisoned().get(&key_id)
        && at.elapsed() < QUOTA_CACHE_TTL
    {
        return Ok(quota.clone());
    }

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(QUOTA_HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("初始化 HTTP 客户端失败：{e}"))?;

    let mut req = http
        .get(&url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(id) = dev_id {
        req = apply_kimi_headers(req, id);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("无法连接 Kimi 额度接口（{url}）：{e}"))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("读取 Kimi 额度响应失败：{e}"))?;

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(
            "Kimi 额度接口拒绝认证（401/403）：请确认使用的是 Kimi Code 订阅 API Key 或有效的 Kimi OAuth 账号。"
                .to_string(),
        );
    }
    if !status.is_success() {
        let preview: String = body.trim().chars().take(160).collect();
        return Err(format!("Kimi 额度接口返回 HTTP {}：{}", status.as_u16(), preview));
    }

    let quota = parse_kimi_usages_json(&body)?;
    cache.lock_unpoisoned().insert(key_id, (Instant::now(), quota.clone()));
    Ok(quota)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_usages_url_for_cn_global_and_coding_endpoints() {
        assert_eq!(
            kimi_usages_url("https://api.moonshot.cn/v1"),
            "https://api.kimi.com/coding/v1/usages"
        );
        assert_eq!(
            kimi_usages_url("https://api.moonshot.ai/v1"),
            "https://api.kimi.ai/coding/v1/usages"
        );
        assert_eq!(
            kimi_usages_url("https://api.kimi.com/coding/v1/"),
            "https://api.kimi.com/coding/v1/usages"
        );
        assert_eq!(
            kimi_usages_url("https://api.kimi.ai/coding/v1"),
            "https://api.kimi.ai/coding/v1/usages"
        );
        assert_eq!(
            kimi_usages_url("https://proxy.example.com/coding/v1"),
            "https://proxy.example.com/coding/v1/usages"
        );
    }

    #[test]
    fn parses_kimi_five_hour_and_weekly_windows() {
        let raw = r#"{
            "limits": [
                {
                    "detail": {
                        "limit": "100",
                        "remaining": "65",
                        "resetTime": "2030-01-01T05:00:00Z"
                    }
                }
            ],
            "usage": {
                "limit": "100",
                "remaining": "40",
                "resetTime": "2030-01-07T00:00:00Z"
            }
        }"#;
        let quota = parse_kimi_usages_json(raw).unwrap();
        assert_eq!(quota.groups.len(), 1);
        assert_eq!(quota.groups[0].name, "Kimi Code");
        let buckets = &quota.groups[0].buckets;
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].window, "5h");
        assert_eq!(buckets[0].remaining_percent, 65);
        assert_eq!(buckets[0].reset_time.as_deref(), Some("2030-01-01T05:00:00Z"));
        assert_eq!(buckets[1].window, "weekly");
        assert_eq!(buckets[1].remaining_percent, 40);
        assert_eq!(buckets[1].reset_time.as_deref(), Some("2030-01-07T00:00:00Z"));
    }

    #[test]
    fn handles_numeric_fields_used_fallback_and_epoch_reset_time() {
        let raw = r#"{
            "usage": {
                "limit": 200,
                "used": 50,
                "resetTime": 1893456000
            }
        }"#;
        let quota = parse_kimi_usages_json(raw).unwrap();
        assert_eq!(quota.groups[0].buckets.len(), 1);
        assert_eq!(quota.groups[0].buckets[0].window, "weekly");
        assert_eq!(quota.groups[0].buckets[0].remaining_percent, 75);
        assert!(
            quota.groups[0].buckets[0]
                .reset_time
                .as_deref()
                .unwrap()
                .starts_with("2030-01-01T00:00:00")
        );
    }

    #[test]
    fn errors_when_no_quota_buckets_are_present() {
        assert!(parse_kimi_usages_json("{}").is_err());
        assert!(parse_kimi_usages_json("not json").is_err());
    }

    #[test]
    fn oauth_store_roundtrip_preview_and_logout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_oauth_credential(dir.path(), "moonshot").is_none());

        let cred = KimiOAuthCredential {
            access_token: "access-123".to_string(),
            refresh_token: "refresh-456".to_string(),
            expires_at: now_unix_secs() + 3600,
            region: KimiRegion::MainlandCn,
            oauth_host: "https://auth.kimi.com".to_string(),
            base_url: "https://api.kimi.com/coding/v1".to_string(),
            user_id: Some("u-1".to_string()),
            nickname: Some("修真者".to_string()),
            plan_name: Some("Allegretto".to_string()),
        };
        update_oauth_store(dir.path(), |map| {
            map.insert("moonshot".to_string(), cred);
        })
        .unwrap();

        assert_eq!(
            load_oauth_access_token(dir.path(), "moonshot").as_deref(),
            Some("access-123")
        );
        assert_eq!(oauth_preview(dir.path(), "moonshot").as_deref(), Some("Allegretto"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(oauth_store_path(dir.path())).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        logout_oauth(dir.path(), "moonshot").unwrap();
        assert!(load_oauth_credential(dir.path(), "moonshot").is_none());
    }
}
