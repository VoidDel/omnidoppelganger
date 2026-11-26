//! Magpie local/remote gateway quota client (`GET /v1/magpie/quotas`).
//!
//! Fetches real-time subscription and plan quota windows from a running Magpie
//! gateway and normalizes them into [`AntigravityQuota`] (`QuotaGroup` / `QuotaBucket`)
//! so the UI renders them with the same 5h / 7d progress bars.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::antigravity::{AntigravityQuota, QuotaBucket, QuotaGroup};

const QUOTA_CACHE_TTL: Duration = Duration::from_secs(60);
const QUOTA_HTTP_TIMEOUT: Duration = Duration::from_secs(12);

/// Derive the Magpie gateway origin (without trailing `/` or `/v1`) from a configured `base_url`.
fn magpie_origin(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Magpie 网关地址为空，请先在设置中填写。".to_string());
    }
    let origin = trimmed.strip_suffix("/v1").unwrap_or(trimmed).trim_end_matches('/');
    if origin.is_empty() {
        return Err("Magpie 网关地址无效。".to_string());
    }
    Ok(origin.to_string())
}

/// Classify a Magpie quota window name / Go `time.Duration` span (ns) into `"5h"`, `"weekly"`, or its own label.
fn classify_window(name: &str, span_ns: Option<i64>) -> String {
    let lower = name.trim().to_lowercase();
    if lower == "5h"
        || lower.contains("5-hour")
        || lower.contains("5 hour")
        || lower.contains("5小时")
        || lower.contains("5hr")
        || lower == "primary"
    {
        return "5h".to_string();
    }
    if lower == "7d"
        || lower == "weekly"
        || lower.contains("week")
        || lower.contains("7 day")
        || lower.contains("7-day")
        || lower.contains("7天")
        || lower.contains("周")
        || lower == "secondary"
    {
        return "weekly".to_string();
    }
    if let Some(ns) = span_ns.filter(|&n| n > 0) {
        let secs = ns / 1_000_000_000;
        if (14_400..=21_600).contains(&secs) {
            return "5h".to_string();
        }
        if (518_400..=691_200).contains(&secs) {
            return "weekly".to_string();
        }
    }
    if lower.is_empty() {
        "quota".to_string()
    } else {
        name.trim().to_string()
    }
}

/// Extract remaining percentage (`0..=100`) from a Magpie `QuotaWindow` JSON object.
fn extract_remaining_percent(w: &Value) -> Option<u8> {
    if w["unlimited"].as_bool() == Some(true) || w["text"].as_bool() == Some(true) {
        // Text-only or unlimited entries carry no percentage bar.
        if w["remaining"].as_f64().is_none()
            && w["remaining_fraction"].as_f64().is_none()
            && w["remainingFraction"].as_f64().is_none()
            && w["usedPercent"].as_f64().is_none()
        {
            return None;
        }
    }

    if let Some(frac) = w["remaining_fraction"]
        .as_f64()
        .or_else(|| w["remainingFraction"].as_f64())
        .filter(|f| f.is_finite())
    {
        let pct = if frac <= 1.0 { frac * 100.0 } else { frac };
        return Some(pct.clamp(0.0, 100.0).round() as u8);
    }

    let limit = w["limit"].as_f64().filter(|v| v.is_finite() && *v > 0.0);
    let remaining = w["remaining"].as_f64().filter(|v| v.is_finite());
    let used = w["used"].as_f64().filter(|v| v.is_finite());
    let amount = w["amount"].as_f64().filter(|v| v.is_finite());

    if let (Some(rem), Some(lim)) = (remaining, limit) {
        return Some(((rem / lim) * 100.0).clamp(0.0, 100.0).round() as u8);
    }
    if let (Some(amt), Some(lim)) = (amount, limit) {
        let pct = if w["barUsed"].as_bool() == Some(true) {
            (1.0 - amt / lim) * 100.0
        } else {
            (amt / lim) * 100.0
        };
        return Some(pct.clamp(0.0, 100.0).round() as u8);
    }
    if let Some(rem) = remaining {
        if let Some(u) = used.filter(|&u| u > 0.0 && rem + u > 1.5) {
            return Some(((rem / (rem + u)) * 100.0).clamp(0.0, 100.0).round() as u8);
        }
        let pct = if rem <= 1.0 && used.is_none_or(|u| u <= 1.0) && rem >= 0.0 {
            // Distinguish `0` (0% remaining) vs `0.95` (95% remaining) vs `95` (95% remaining).
            if rem == 0.0 && used.is_some_and(|u| u > 1.5) {
                0.0
            } else if rem <= 1.0 && (rem > 0.0 || used.is_some_and(|u| u > 0.0 && u <= 1.0)) {
                rem * 100.0
            } else {
                rem
            }
        } else {
            rem
        };
        return Some(pct.clamp(0.0, 100.0).round() as u8);
    }

    if let Some(used_pct) = w["usedPercent"]
        .as_f64()
        .or_else(|| w["used_percent"].as_f64())
        .or_else(|| w["utilization"].as_f64())
        .or(used)
        .filter(|v| v.is_finite())
    {
        let rem_pct = if (0.0..=1.0).contains(&used_pct) {
            (1.0 - used_pct) * 100.0
        } else {
            100.0 - used_pct
        };
        return Some(rem_pct.clamp(0.0, 100.0).round() as u8);
    }

    None
}

fn extract_reset_time(w: &Value) -> Option<String> {
    for key in [
        "resetsAt",
        "resetAt",
        "reset_at",
        "reset_time",
        "resetTime",
        "runsOutAt",
    ] {
        if let Some(s) = w[key].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
    }
    if let Some(secs) = w["resetSecs"]
        .as_i64()
        .or_else(|| w["reset_secs"].as_i64())
        .filter(|&s| s > 0)
    {
        let target = chrono::Utc::now() + chrono::Duration::seconds(secs);
        return Some(target.to_rfc3339());
    }
    None
}

fn first_non_empty<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| obj[*k].as_str().map(str::trim).filter(|s| !s.is_empty()))
}

/// Parse the JSON payload returned by Magpie's `GET /v1/magpie/quotas` endpoint.
pub fn parse_magpie_quotas_json(raw: &str) -> Result<AntigravityQuota, String> {
    let parsed: Value = serde_json::from_str(raw.trim()).map_err(|e| format!("解析 Magpie 额度 JSON 失败：{e}"))?;

    let items: &[Value] = if let Some(arr) = parsed.as_array() {
        arr
    } else if let Some(arr) = parsed["quotas"]
        .as_array()
        .or_else(|| parsed["data"].as_array())
        .or_else(|| parsed["cards"].as_array())
        .or_else(|| parsed["items"].as_array())
        .or_else(|| parsed["groups"].as_array())
    {
        arr
    } else {
        return Err("Magpie /v1/magpie/quotas 未返回有效的额度列表。".to_string());
    };

    let mut groups: Vec<QuotaGroup> = Vec::new();
    let mut group_idx: HashMap<String, usize> = HashMap::new();

    for card in items {
        let provider_label =
            first_non_empty(card, &["name", "title", "label", "provider", "kind", "Kind", "id"]).unwrap_or("Magpie");
        let account = first_non_empty(card, &["account", "email", "accountName", "profile"]);

        let windows = card["windows"]
            .as_array()
            .or_else(|| card["buckets"].as_array())
            .or_else(|| card["limits"].as_array());
        let Some(windows) = windows else {
            continue;
        };

        for (idx, w) in windows.iter().enumerate() {
            let Some(remaining_percent) = extract_remaining_percent(w) else {
                continue;
            };
            let win_name = first_non_empty(w, &["name", "window", "windowType", "id"]).unwrap_or("");
            let span_ns = w["span"].as_i64();
            let window = classify_window(win_name, span_ns);
            let pool = first_non_empty(w, &["pool", "model", "group"]);

            let group_name = match (pool, account) {
                (Some(p), _) if !p.eq_ignore_ascii_case(provider_label) => {
                    if p.to_lowercase().contains(&provider_label.to_lowercase()) {
                        p.to_string()
                    } else {
                        format!("{provider_label} · {p}")
                    }
                }
                (_, Some(acc)) if !acc.eq_ignore_ascii_case("default") => {
                    format!("{provider_label} ({acc})")
                }
                _ => provider_label.to_string(),
            };

            let bucket = QuotaBucket {
                id: first_non_empty(w, &["id", "name", "window"])
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{window}-{idx}")),
                name: if win_name.is_empty() {
                    window.clone()
                } else {
                    win_name.to_string()
                },
                window,
                remaining_percent,
                reset_time: extract_reset_time(w),
                description: first_non_empty(w, &["display", "description", "holdsWhy"]).map(str::to_string),
            };

            if let Some(&existing) = group_idx.get(&group_name) {
                groups[existing].buckets.push(bucket);
            } else {
                group_idx.insert(group_name.clone(), groups.len());
                groups.push(QuotaGroup {
                    name: group_name,
                    description: first_non_empty(card, &["provider", "kind", "Kind", "description"])
                        .map(str::to_string),
                    buckets: vec![bucket],
                });
            }
        }
    }

    if groups.is_empty() {
        return Err("Magpie 网关暂未返回可用的额度窗口。".to_string());
    }

    Ok(AntigravityQuota {
        fetched_at: chrono::Utc::now().to_rfc3339(),
        groups,
    })
}

/// Query real-time subscription quotas from a Magpie gateway (`GET /v1/magpie/quotas`).
/// Cached per `base_url` for 60 seconds unless `force` is true.
pub async fn fetch_quota(base_url: &str, api_key: Option<&str>, force: bool) -> Result<AntigravityQuota, String> {
    let origin = magpie_origin(base_url)?;

    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, AntigravityQuota)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force
        && let Some((at, quota)) = cache.lock().unwrap().get(&origin)
        && at.elapsed() < QUOTA_CACHE_TTL
    {
        return Ok(quota.clone());
    }

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(QUOTA_HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("初始化 HTTP 客户端失败：{e}"))?;

    let key = api_key.map(str::trim).filter(|k| !k.is_empty());

    if force {
        // Best-effort upstream refresh trigger; older Magpie builds may not expose this route.
        let mut req = http.post(format!("{origin}/v1/magpie/quotas/refresh"));
        if let Some(k) = key {
            req = req.bearer_auth(k).header("x-api-key", k);
        }
        let _ = req.send().await;
    }

    let url = format!("{origin}/v1/magpie/quotas");
    let mut req = http.get(&url);
    if let Some(k) = key {
        req = req.bearer_auth(k).header("x-api-key", k);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("无法连接 Magpie 网关（{url}）：{e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取 Magpie 额度响应失败：{e}"))?;

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(
            "Magpie 网关拒绝读取额度（401/403），请确认 Gateway Key 已开启额度权限：magpie gateway-key quotas <id> on"
                .to_string(),
        );
    }
    if !status.is_success() {
        let preview: String = body.trim().chars().take(160).collect();
        return Err(format!("Magpie 网关返回 HTTP {}：{}", status.as_u16(), preview));
    }

    let quota = parse_magpie_quotas_json(&body)?;
    cache.lock().unwrap().insert(origin, (Instant::now(), quota.clone()));
    Ok(quota)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_magpie_origin_from_v1_or_bare_url() {
        assert_eq!(
            magpie_origin("http://127.0.0.1:3425/v1").unwrap(),
            "http://127.0.0.1:3425"
        );
        assert_eq!(
            magpie_origin("http://127.0.0.1:3425/v1/").unwrap(),
            "http://127.0.0.1:3425"
        );
        assert_eq!(
            magpie_origin("http://192.168.1.10:3425/").unwrap(),
            "http://192.168.1.10:3425"
        );
    }

    #[test]
    fn parses_magpie_quotas_array_with_pools_and_windows() {
        let sample = r#"[
          {
            "provider": "agy-cli",
            "name": "Antigravity",
            "windows": [
              { "name": "5h", "pool": "Gemini Models", "remaining": 0.95, "resetsAt": "2026-11-25T14:00:00Z" },
              { "name": "7d", "pool": "Gemini Models", "remaining": 93, "used": 7, "resetsAt": "2026-12-01T10:00:00Z" },
              { "name": "5-hour", "pool": "Claude and GPT models", "usedPercent": 12.5, "resetsAt": "2026-11-25T15:00:00Z" }
            ]
          },
          {
            "provider": "codex",
            "account": "work",
            "windows": [
              { "name": "primary", "remaining": 80, "limit": 100, "resetSecs": 7200 },
              { "name": "secondary", "remaining": 45, "limit": 100 }
            ]
          }
        ]"#;

        let quota = parse_magpie_quotas_json(sample).unwrap();
        assert_eq!(quota.groups.len(), 3);
        assert_eq!(quota.groups[0].name, "Antigravity · Gemini Models");
        assert_eq!(quota.groups[0].buckets.len(), 2);
        assert_eq!(quota.groups[0].buckets[0].window, "5h");
        assert_eq!(quota.groups[0].buckets[0].remaining_percent, 95);
        assert_eq!(quota.groups[0].buckets[1].window, "weekly");
        assert_eq!(quota.groups[0].buckets[1].remaining_percent, 93);

        assert_eq!(quota.groups[1].name, "Antigravity · Claude and GPT models");
        assert_eq!(quota.groups[1].buckets[0].window, "5h");
        assert_eq!(quota.groups[1].buckets[0].remaining_percent, 88);

        assert_eq!(quota.groups[2].name, "codex (work)");
        assert_eq!(quota.groups[2].buckets[0].window, "5h");
        assert_eq!(quota.groups[2].buckets[0].remaining_percent, 80);
        assert!(quota.groups[2].buckets[0].reset_time.is_some());
        assert_eq!(quota.groups[2].buckets[1].window, "weekly");
        assert_eq!(quota.groups[2].buckets[1].remaining_percent, 45);
    }
}
