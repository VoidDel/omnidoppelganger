//! Per-model token usage statistics and rate-limit / quota cooldown tracking.
//!
//! Persisted in `~/.odg/model-usage.json` so usage totals (today + all-time) and
//! active cooldown timers survive restarts and are shared across every session,
//! API provider, and external CLI engine (`agy`, `codex`, `claude`, `cursor`, `grok`, `opencode`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Duration, Local, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::llm::Tokens;

/// Persisted usage and cooldown snapshot for one `(provider, model)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsageEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub today_key: String,
    #[serde(default)]
    pub today_requests: u64,
    #[serde(default)]
    pub today_input_tokens: u64,
    #[serde(default)]
    pub today_cached_tokens: u64,
    #[serde(default)]
    pub today_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    entries: BTreeMap<String, ModelUsageEntry>,
}

fn entry_key(provider: &str, model: &str) -> String {
    format!("{provider}\0{model}")
}

fn today_string() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn rollover_today(entry: &mut ModelUsageEntry, today: &str) {
    if entry.today_key != today {
        entry.today_key = today.to_string();
        entry.today_requests = 0;
        entry.today_input_tokens = 0;
        entry.today_cached_tokens = 0;
        entry.today_output_tokens = 0;
    }
}

fn clear_expired_cooldown(entry: &mut ModelUsageEntry, now: DateTime<Utc>) {
    if let Some(ref until) = entry.cooldown_until {
        let expired = DateTime::parse_from_rfc3339(until)
            .map(|dt| dt.with_timezone(&Utc) <= now)
            .unwrap_or(true);
        if expired {
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
        }
    }
}

const USAGE_FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

struct StoreInner {
    path: PathBuf,
    data: Mutex<UsageFile>,
    dirty: AtomicBool,
    flush_scheduled: AtomicBool,
}

impl StoreInner {
    fn flush_sync(&self) {
        let guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if self.dirty.swap(false, Ordering::AcqRel) {
            save_file(&self.path, &guard);
        }
    }

    fn run_scheduled_flush(&self) {
        loop {
            self.flush_sync();
            self.flush_scheduled.store(false, Ordering::Release);
            if !self.dirty.load(Ordering::Acquire) {
                break;
            }
            if self
                .flush_scheduled
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                break;
            }
        }
    }
}

/// Thread-safe store of per-model token usage and cooldown timers.
pub struct ModelUsageStore {
    inner: Arc<StoreInner>,
}

impl Drop for ModelUsageStore {
    fn drop(&mut self) {
        self.flush_sync();
    }
}

impl ModelUsageStore {
    /// Process-wide store backed by `~/.odg/model-usage.json`.
    pub fn global() -> &'static Self {
        static STORE: OnceLock<ModelUsageStore> = OnceLock::new();
        STORE.get_or_init(|| Self::at(crate::settings::home().join("model-usage.json")))
    }

    /// Open (or create) a store at an explicit path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let file = load_file(&path);
        Self {
            inner: Arc::new(StoreInner {
                path,
                data: Mutex::new(file),
                dirty: AtomicBool::new(false),
                flush_scheduled: AtomicBool::new(false),
            }),
        }
    }

    fn schedule_debounced_flush(&self) {
        if self
            .inner
            .flush_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(USAGE_FLUSH_DEBOUNCE).await;
                if let Some(inner) = weak.upgrade() {
                    let _ = tokio::task::spawn_blocking(move || inner.run_scheduled_flush()).await;
                }
            });
        } else {
            let _ = std::thread::Builder::new()
                .name("odg-usage-flush".to_string())
                .spawn(move || {
                    std::thread::sleep(USAGE_FLUSH_DEBOUNCE);
                    if let Some(inner) = weak.upgrade() {
                        inner.run_scheduled_flush();
                    }
                });
        }
    }

    /// Synchronously flush any pending in-memory `record_usage` updates to disk.
    pub fn flush_sync(&self) {
        self.inner.flush_sync();
    }

    /// Record token usage from one completed assistant message / turn.
    pub fn record_usage(&self, provider: &str, model: &str, tokens: &Tokens) {
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return;
        }
        let today = today_string();
        let now = Utc::now().to_rfc3339();
        {
            let mut guard = self.inner.data.lock().unwrap();
            let key = entry_key(provider, model);
            let entry = guard.entries.entry(key).or_insert_with(|| ModelUsageEntry {
                provider: provider.to_string(),
                model: model.to_string(),
                requests: 0,
                input_tokens: 0,
                cached_tokens: 0,
                output_tokens: 0,
                today_key: today.clone(),
                today_requests: 0,
                today_input_tokens: 0,
                today_cached_tokens: 0,
                today_output_tokens: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
            });
            rollover_today(entry, &today);
            entry.requests = entry.requests.saturating_add(1);
            entry.input_tokens = entry.input_tokens.saturating_add(tokens.input);
            entry.cached_tokens = entry.cached_tokens.saturating_add(tokens.cached);
            entry.output_tokens = entry.output_tokens.saturating_add(tokens.output);
            entry.today_requests = entry.today_requests.saturating_add(1);
            entry.today_input_tokens = entry.today_input_tokens.saturating_add(tokens.input);
            entry.today_cached_tokens = entry.today_cached_tokens.saturating_add(tokens.cached);
            entry.today_output_tokens = entry.today_output_tokens.saturating_add(tokens.output);
            entry.last_used_at = Some(now);
            // A successful response means the model is no longer cooling down.
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            self.inner.dirty.store(true, Ordering::Release);
        }
        self.schedule_debounced_flush();
    }

    /// Record an explicit cooldown deadline for `(provider, model)`.
    pub fn record_cooldown(&self, provider: &str, model: &str, until: DateTime<Utc>, reason: String) {
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return;
        }
        let today = today_string();
        let mut guard = self.inner.data.lock().unwrap();
        let key = entry_key(provider, model);
        let entry = guard.entries.entry(key).or_insert_with(|| ModelUsageEntry {
            provider: provider.to_string(),
            model: model.to_string(),
            requests: 0,
            input_tokens: 0,
            cached_tokens: 0,
            output_tokens: 0,
            today_key: today.clone(),
            today_requests: 0,
            today_input_tokens: 0,
            today_cached_tokens: 0,
            today_output_tokens: 0,
            last_used_at: Some(Utc::now().to_rfc3339()),
            cooldown_until: None,
            cooldown_reason: None,
        });
        rollover_today(entry, &today);
        entry.cooldown_until = Some(until.to_rfc3339());
        entry.cooldown_reason = Some(reason);
        self.inner.dirty.store(false, Ordering::Release);
        save_file(&self.inner.path, &guard);
    }

    /// Inspect an error string from an API or CLI turn; if it indicates a rate limit or quota
    /// cooldown, record the cooldown deadline and return `true`.
    pub fn record_error_if_rate_limited(&self, provider: &str, model: &str, error: &str) -> bool {
        if let Some((until, reason)) = parse_cooldown_from_error(error, Utc::now()) {
            self.record_cooldown(provider, model, until, reason);
            true
        } else {
            false
        }
    }

    /// Manually clear cooldown for one `(provider, model)`.
    pub fn clear_cooldown(&self, provider: &str, model: &str) {
        let mut guard = self.inner.data.lock().unwrap();
        if let Some(entry) = guard.entries.get_mut(&entry_key(provider.trim(), model.trim())) {
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            self.inner.dirty.store(false, Ordering::Release);
            save_file(&self.inner.path, &guard);
        }
    }

    /// Clear all usage statistics and cooldowns.
    pub fn reset_all(&self) {
        let mut guard = self.inner.data.lock().unwrap();
        guard.entries.clear();
        self.inner.dirty.store(false, Ordering::Release);
        save_file(&self.inner.path, &guard);
    }

    /// Return all entries, rolling over stale `today_*` counters and pruning expired cooldowns.
    /// Sorted with active cooldowns first, then by `last_used_at` descending.
    pub fn list(&self) -> Vec<ModelUsageEntry> {
        let today = today_string();
        let now = Utc::now();
        let mut guard = self.inner.data.lock().unwrap();
        let mut changed = false;
        for entry in guard.entries.values_mut() {
            if entry.today_key != today {
                rollover_today(entry, &today);
                changed = true;
            }
            if entry.cooldown_until.is_some() {
                clear_expired_cooldown(entry, now);
                if entry.cooldown_until.is_none() {
                    changed = true;
                }
            }
        }
        if changed {
            self.inner.dirty.store(false, Ordering::Release);
            save_file(&self.inner.path, &guard);
        }
        let mut list: Vec<ModelUsageEntry> = guard.entries.values().cloned().collect();
        list.sort_by(|a, b| {
            let a_cool = a.cooldown_until.is_some();
            let b_cool = b.cooldown_until.is_some();
            b_cool
                .cmp(&a_cool)
                .then_with(|| b.last_used_at.cmp(&a.last_used_at))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.model.cmp(&b.model))
        });
        list
    }
}

/// Detect rate-limit / quota exhaustion errors from API providers or CLI engines and extract
/// `(cooldown_until, short_reason)`.
pub fn parse_cooldown_from_error(error: &str, now: DateTime<Utc>) -> Option<(DateTime<Utc>, String)> {
    let lower = error.to_ascii_lowercase();
    let is_quota = lower.contains("quota")
        || lower.contains("resource_exhausted")
        || lower.contains("resource exhausted")
        || error.contains("配额")
        || error.contains("额度");
    let is_rate_limit = lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
        || lower.contains("overloaded")
        || lower.contains("cooldown")
        || error.contains("限流")
        || error.contains("访问量过大")
        || error.contains("频率")
        || error.contains("冷却");

    if !is_quota && !is_rate_limit {
        return None;
    }

    let reason = if is_quota {
        "配额已用尽".to_string()
    } else if lower.contains("overloaded") {
        "服务繁忙".to_string()
    } else {
        "触发限流".to_string()
    };

    // 1. Explicit ISO-8601 / RFC3339 reset timestamp in the error message.
    static ISO_RE: OnceLock<Regex> = OnceLock::new();
    let iso_re = ISO_RE
        .get_or_init(|| Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})").unwrap());
    if let Some(m) = iso_re.find(error)
        && let Ok(parsed) = DateTime::parse_from_rfc3339(m.as_str())
    {
        let utc = parsed.with_timezone(&Utc);
        if utc > now {
            return Some((utc, reason));
        }
    }

    // 2. Explicit compound duration (e.g. `2h 15m 30s`, `1h30m`, `45m`, `30 seconds`, `2 小时 10 分`, `5 分钟`).
    if let Some(secs) = parse_duration_seconds(error)
        && secs > 0
    {
        return Some((now + Duration::seconds(secs as i64), reason));
    }

    // 3. Fallback default cooldown when the error clearly indicates throttling without a specific duration.
    let fallback = if is_quota { 300 } else { 60 };
    Some((now + Duration::seconds(fallback), reason))
}

const MAX_PARSED_DURATION_SECS: u64 = 86_400;

fn parse_duration_seconds(text: &str) -> Option<u64> {
    static EN_RE: OnceLock<Regex> = OnceLock::new();
    static ZH_RE: OnceLock<Regex> = OnceLock::new();
    let en_re = EN_RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?-u:\b)(?:(\d+)\s*(?:hours?|hrs?|h)(?:\s*(\d+)\s*(?:minutes?|mins?|m))?(?:\s*(\d+(?:\.\d+)?)\s*(?:seconds?|secs?|s))?|(\d+)\s*(?:minutes?|mins?|m)(?:\s*(\d+(?:\.\d+)?)\s*(?:seconds?|secs?|s))?|(\d+(?:\.\d+)?)\s*(?:seconds?|secs?|s))(?-u:\b)",
        )
        .unwrap()
    });
    let valid_start = |start: usize| start == 0 || !matches!(text.as_bytes()[start - 1], b'-' | b'/' | b'.');
    let mut best: u64 = 0;
    for caps in en_re.captures_iter(text) {
        let Some(m0) = caps.get(0) else { continue };
        if !valid_start(m0.start()) {
            continue;
        }
        let parse_u64 = |idx: usize| caps.get(idx).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
        let parse_secs = |idx: usize| {
            caps.get(idx)
                .and_then(|m| m.as_str().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(|v| v.ceil() as u64)
                .unwrap_or(0)
        };
        let h = parse_u64(1);
        let m = parse_u64(2).max(parse_u64(4));
        let s = parse_secs(3).max(parse_secs(5)).max(parse_secs(6));
        let total = h
            .saturating_mul(3600)
            .saturating_add(m.saturating_mul(60))
            .saturating_add(s);
        if total > best {
            best = total;
        }
    }
    let zh_re = ZH_RE.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)(?:(\d+)\s*小时(?:\s*(\d+)\s*分钟?)?(?:\s*(\d+)\s*秒钟?)?|(\d+)\s*分钟?(?:\s*(\d+)\s*秒钟?)?|(\d+)\s*秒钟?)",
        )
        .unwrap()
    });
    for caps in zh_re.captures_iter(text) {
        let Some(m0) = caps.get(0) else { continue };
        if !valid_start(m0.start()) {
            continue;
        }
        let parse_u64 = |idx: usize| caps.get(idx).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
        let h = parse_u64(1);
        let m = parse_u64(2).max(parse_u64(4));
        let s = parse_u64(3).max(parse_u64(5)).max(parse_u64(6));
        let total = h
            .saturating_mul(3600)
            .saturating_add(m.saturating_mul(60))
            .saturating_add(s);
        if total > best {
            best = total;
        }
    }
    (best > 0).then(|| best.min(MAX_PARSED_DURATION_SECS))
}

fn load_file(path: &Path) -> UsageFile {
    let Ok(text) = std::fs::read_to_string(path) else {
        return UsageFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_file(path: &Path, file: &UsageFile) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(file) {
        let _ = crate::fsutil::write_atomically(path, json.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_usage_clears_cooldown_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-usage.json");
        let store = ModelUsageStore::at(&path);

        store.record_cooldown(
            "antigravity",
            "gemini-4-neon",
            Utc::now() + Duration::seconds(180),
            "配额已用尽".into(),
        );
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].cooldown_until.is_some());
        assert_eq!(listed[0].cooldown_reason.as_deref(), Some("配额已用尽"));

        store.record_usage(
            "antigravity",
            "gemini-4-neon",
            &Tokens {
                input: 1000,
                cached: 400,
                output: 250,
            },
        );
        store.record_usage(
            "antigravity",
            "gemini-4-neon",
            &Tokens {
                input: 500,
                cached: 100,
                output: 50,
            },
        );
        store.flush_sync();

        let reloaded = ModelUsageStore::at(&path);
        let list = reloaded.list();
        assert_eq!(list.len(), 1);
        let entry = &list[0];
        assert_eq!(entry.requests, 2);
        assert_eq!(entry.today_requests, 2);
        assert_eq!(entry.input_tokens, 1500);
        assert_eq!(entry.cached_tokens, 500);
        assert_eq!(entry.output_tokens, 300);
        assert!(entry.cooldown_until.is_none());
    }

    #[tokio::test]
    async fn debounces_usage_writes_and_flushes_on_demand_or_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debounced-usage.json");
        let store = ModelUsageStore::at(&path);

        store.record_usage(
            "anthropic",
            "claude-sonnet-5-5",
            &Tokens {
                input: 300,
                cached: 100,
                output: 40,
            },
        );
        // Updated in memory immediately without blocking on synchronous disk fsync.
        assert_eq!(store.list()[0].input_tokens, 300);
        assert!(
            !path.exists(),
            "record_usage must not synchronously write disk before debounce"
        );

        // Explicit flush_sync persists immediately.
        store.flush_sync();
        assert!(path.exists());
        assert_eq!(ModelUsageStore::at(&path).list()[0].input_tokens, 300);

        // A second usage update coalesces into the background debounce flush.
        store.record_usage(
            "anthropic",
            "claude-sonnet-5-5",
            &Tokens {
                input: 200,
                cached: 0,
                output: 10,
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
        assert_eq!(ModelUsageStore::at(&path).list()[0].input_tokens, 500);
    }

    #[test]
    fn parses_cooldown_from_api_and_cli_errors() {
        let now = DateTime::parse_from_rfc3339("2026-11-25T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let (until, reason) = parse_cooldown_from_error("HTTP 429 rate limit exceeded, retry after 45s", now).unwrap();
        assert_eq!(reason, "触发限流");
        assert_eq!((until - now).num_seconds(), 45);

        let (until, reason) = parse_cooldown_from_error("Quota exhausted. Resets in 2h 15m.", now).unwrap();
        assert_eq!(reason, "配额已用尽");
        assert_eq!((until - now).num_seconds(), 2 * 3600 + 15 * 60);

        let (until, _) = parse_cooldown_from_error("Rate limit reached, resets at 2026-11-25T10:30:00Z", now).unwrap();
        assert_eq!((until - now).num_seconds(), 90 * 60);

        let (until, reason) = parse_cooldown_from_error("该模型当前访问量过大，请 5 分钟后再试", now).unwrap();
        assert_eq!(reason, "触发限流");
        assert_eq!((until - now).num_seconds(), 300);

        assert!(parse_cooldown_from_error("file not found: foo.rs", now).is_none());
    }

    #[test]
    fn cooldown_parser_ignores_hex_request_ids_and_uuids() {
        let now = DateTime::parse_from_rfc3339("2026-11-25T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let (until, reason) = parse_cooldown_from_error(
            "HTTP 429 rate limit: retry after 5s (request_id: req_a8f72h, trace: 9b-45m-10s)",
            now,
        )
        .unwrap();
        assert_eq!(reason, "触发限流");
        assert_eq!((until - now).num_seconds(), 5);

        // Compound forms (`1h30m`, `2h 15m 30s`, `2小时15分`) and 24h cap (`86_400`s).
        assert_eq!(parse_duration_seconds("retry in 1h30m"), Some(5400));
        assert_eq!(parse_duration_seconds("retry in 2h 15m 30s"), Some(8130));
        assert_eq!(parse_duration_seconds("请等待2小时15分后重试"), Some(8100));
        assert_eq!(parse_duration_seconds("retry after 72h"), Some(86_400));

        // Pure trace/path/version tokens without a real duration fall back to the 60s default.
        let (until, _) = parse_cooldown_from_error(
            "HTTP 429 Too Many Requests (req_a8f72h, id: 9b-45m-10s, route: /api/v2m/10s, ver: v1.2s)",
            now,
        )
        .unwrap();
        assert_eq!((until - now).num_seconds(), 60);
    }
}
