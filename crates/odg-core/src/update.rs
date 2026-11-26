//! Check GitHub Releases for newer desktop builds of OmniDoppelganger.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_RELEASE_API: &str = "https://api.github.com/repos/VoidDel/omnidoppelganger/releases/latest";

const NOTES_MAX_CHARS: usize = 1_500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub html_url: String,
    pub notes: String,
    pub published_at: Option<String>,
}

type Semver<'a> = ((u64, u64, u64), Option<&'a str>);

fn parse_semver(raw: &str) -> Option<Semver<'_>> {
    let clean = raw.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match clean.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (clean.split('+').next().unwrap_or(clean), None),
    };
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(((major, minor, patch), pre.filter(|p| !p.is_empty())))
}

/// Returns `true` when `latest` represents a strictly newer semantic version than `current`.
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some((cur_ver, cur_pre)), Some((lat_ver, lat_pre))) => {
            if lat_ver != cur_ver {
                lat_ver > cur_ver
            } else {
                cur_pre.is_some() && lat_pre.is_none()
            }
        }
        _ => false,
    }
}

pub async fn check_latest(current_version: &str) -> Result<UpdateStatus, String> {
    check_from_url(current_version, DEFAULT_RELEASE_API).await
}

pub async fn check_from_url(current_version: &str, url: &str) -> Result<UpdateStatus, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("无法初始化网络请求：{e}"))?;

    let response = client
        .get(url)
        .header("User-Agent", format!("OmniDoppelganger/{current_version}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("无法连接 GitHub 检查更新：{e}"))?;

    if !response.status().is_success() {
        return Err(format!("检查更新失败（HTTP {}）", response.status().as_u16()));
    }

    let body: Value = response.json().await.map_err(|e| format!("解析发布信息失败：{e}"))?;

    let tag = body["tag_name"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "GitHub Release 未包含版本号".to_string())?;
    let latest_clean = tag.trim_start_matches(['v', 'V']).to_string();
    let current_clean = current_version.trim().trim_start_matches(['v', 'V']).to_string();
    let html_url = body["html_url"]
        .as_str()
        .filter(|u| !u.is_empty())
        .unwrap_or("https://github.com/VoidDel/omnidoppelganger/releases/latest")
        .to_string();
    let raw_notes = body["body"].as_str().unwrap_or_default().trim();
    let notes: String = if raw_notes.chars().count() > NOTES_MAX_CHARS {
        format!("{}…", raw_notes.chars().take(NOTES_MAX_CHARS).collect::<String>())
    } else {
        raw_notes.to_string()
    };
    let published_at = body["published_at"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(UpdateStatus {
        has_update: is_newer_version(&current_clean, &latest_clean),
        current_version: current_clean,
        latest_version: latest_clean,
        html_url,
        notes,
        published_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::serve;
    use serde_json::json;

    #[test]
    fn compares_semver_and_prerelease_tags() {
        assert!(is_newer_version("0.7.25", "v0.7.26"));
        assert!(is_newer_version("0.7.25", "0.8.0"));
        assert!(is_newer_version("0.7.25-rc.1", "0.7.25"));
        assert!(!is_newer_version("0.7.25", "v0.7.25"));
        assert!(!is_newer_version("0.7.25", "v0.7.24"));
        assert!(!is_newer_version("0.7.25", "0.7.25-beta.1"));
    }

    #[tokio::test]
    async fn parses_github_latest_release_response() {
        let payload = json!({
            "tag_name": "v0.8.0",
            "html_url": "https://github.com/VoidDel/omnidoppelganger/releases/tag/v0.8.0",
            "body": "## Highlights\n- Added batch_inspect and Gemini streaming.",
            "published_at": "2026-11-26T00:00:00Z"
        })
        .to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let (addr, _) = serve(vec![resp]).await;
        let status = check_from_url("0.7.25", &format!("{addr}/releases/latest"))
            .await
            .unwrap();
        assert!(status.has_update);
        assert_eq!(status.current_version, "0.7.25");
        assert_eq!(status.latest_version, "0.8.0");
        assert!(status.notes.contains("batch_inspect"));
    }
}
