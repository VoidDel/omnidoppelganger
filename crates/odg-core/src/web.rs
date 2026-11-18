//! Reaching the web for the model: `fetch_url` reads a page, `web_search`
//! finds pages.
//!
//! The page comes back as text. HTML is reduced to its words (scripts,
//! styles, and markup gone); other text is returned as it is; anything else
//! (a PDF, an image) is refused. A request leaves the machine, and its
//! address can carry whatever the model puts in it, so every host asks the
//! user first (see [`crate::policy`]).

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A page larger than this is not downloaded to the end.
const BODY_BYTES: usize = 5 * 1024 * 1024;
/// A result is replayed in every later request, like a file read.
const TEXT_CHARS: usize = 60_000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// The host of an http(s) address, lower-cased, with its port when it names one; `None` for anything else.
pub fn host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // What comes before an `@` is a user name, not the host.
    let host = authority.rsplit('@').next()?.to_lowercase();
    (!host.is_empty() && !host.contains(char::is_whitespace)).then_some(host)
}

/// The words of an HTML page: its title, and its text with one block per line.
pub fn html_text(html: &str) -> (Option<String>, String) {
    static HIDDEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<!--.*?-->|<(script|style|noscript|svg|template|head)\b.*?</(script|style|noscript|svg|template|head)\s*>").expect("valid pattern"));
    static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title\s*>").expect("valid pattern"));
    static BREAK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)</?(p|div|br|li|tr|h[1-6]|section|article|header|footer|main|nav|aside|pre|blockquote|ul|ol|table|dl|dt|dd|hr|form)\b[^>]*>").expect("valid pattern")
    });
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("valid pattern"));
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);").expect("valid pattern"));
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\r\u{a0}]+").expect("valid pattern"));

    let decode = |text: &str| {
        ENTITY
            .replace_all(text, |found: &regex::Captures| {
                let name = &found[1];
                let character = match name {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ if name.starts_with("#x") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                    _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                    _ => None,
                };
                // An entity this does not know stays as written.
                character.map_or_else(|| found[0].to_string(), String::from)
            })
            .into_owned()
    };
    let title = TITLE.captures(html).map(|found| SPACES.replace_all(decode(&TAG.replace_all(&found[1], "")).trim(), " ").into_owned()).filter(|title| !title.is_empty());
    let visible = HIDDEN.replace_all(html, " ");
    let broken = BREAK.replace_all(&visible, "\n");
    let text = decode(&TAG.replace_all(&broken, ""));
    // One block per line, and no more than one empty line between blocks.
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines().map(|line| SPACES.replace_all(line, " ").trim().to_string()) {
        if !line.is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
            lines.push(line);
        }
    }
    (title, lines.join("\n").trim().to_string())
}

/// What `fetch_url` returns: the text for the model, whether it is an error, and facts for the window.
pub struct Fetched {
    pub content: String,
    pub is_error: bool,
    pub meta: Value,
}

fn failed(message: String) -> Fetched {
    Fetched { content: message, is_error: true, meta: Value::Null }
}

/// Get `url` and return its text from character `offset` on.
pub async fn fetch(url: &str, offset: usize, cancel: &CancellationToken) -> Fetched {
    if host(url).is_none() {
        return failed(format!("`{url}` is not an http or https address."));
    }
    let client = match reqwest::Client::builder().connect_timeout(TIMEOUT).timeout(TIMEOUT).redirect(reqwest::redirect::Policy::limited(5)).build() {
        Ok(client) => client,
        Err(error) => return failed(format!("Cannot set up the request: {error}")),
    };
    // A plain client name: nothing about the user or the project goes with the request.
    let request = client.get(url).header("user-agent", "odg-fetch/0.1").header("accept", "text/html, text/plain, application/json, text/*;q=0.8");
    let download = async {
        let mut response = request.send().await.map_err(|error| format!("Cannot reach {url}: {error}"))?;
        let status = response.status();
        let kind = response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or_default().to_lowercase();
        let landed = response.url().to_string();
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| format!("The download of {url} broke off: {error}"))? {
            body.extend_from_slice(&chunk);
            if body.len() > BODY_BYTES {
                body.truncate(BODY_BYTES);
                break;
            }
        }
        Ok::<_, String>((status, kind, landed, body))
    };
    let (status, kind, landed, body) = tokio::select! {
        outcome = download => match outcome {
            Ok(got) => got,
            Err(error) => return failed(error),
        },
        () = cancel.cancelled() => return failed("The fetch was cancelled.".to_string()),
    };
    if !status.is_success() {
        return failed(format!("{url} answered HTTP {}.", status.as_u16()));
    }
    let textual = kind.is_empty() || kind.starts_with("text/") || ["json", "xml", "javascript", "yaml"].iter().any(|word| kind.contains(word));
    if !textual || body.iter().take(8192).any(|byte| *byte == 0) {
        return failed(format!("{url} is {}, not text; it cannot be read here.", if kind.is_empty() { "binary data" } else { kind.as_str() }));
    }
    let raw = String::from_utf8_lossy(&body);
    let looks_like_html = kind.contains("html") || raw.trim_start().get(..15).is_some_and(|start| start.to_lowercase().starts_with("<!doctype html") || start.to_lowercase().starts_with("<html"));
    let (title, text) = if looks_like_html { html_text(&raw) } else { (None, raw.trim().to_string()) };

    let total = text.chars().count();
    let shown: String = text.chars().skip(offset).take(TEXT_CHARS).collect();
    let end = offset + shown.chars().count();
    let mut content = String::new();
    if landed != url {
        content.push_str(&format!("[redirected to {landed}]\n"));
    }
    if let Some(title) = &title {
        content.push_str(&format!("Title: {title}\n\n"));
    }
    if shown.is_empty() {
        content.push_str(&if total == 0 { "(the page has no text)".to_string() } else { format!("The page has {total} characters; nothing at offset {offset}.") });
    } else {
        content.push_str(&shown);
        if end < total {
            content.push_str(&format!("\n[showing characters {offset}-{end} of {total}; pass offset {end} to read on]"));
        }
    }
    Fetched { content, is_error: false, meta: json!({ "url": landed, "title": title, "characters": total }) }
}

/// Where a search is sent. Today the one source is Zhipu's Web Search API,
/// which takes the same key as its models; another source would be another
/// way to build one of these and to read its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConfig {
    /// The search endpoint.
    pub url: String,
    pub key: String,
    /// The engine to ask, one of [`SEARCH_ENGINES`], or `search-prime` on Zhipu's international site.
    pub engine: String,
}

/// Gives the search configuration in force, or `None` when searching is not set up.
pub type SearchSource = std::sync::Arc<dyn Fn() -> Option<SearchConfig> + Send + Sync>;

/// The engines Zhipu's site in China offers, the cheapest first: `(id, what it is)`.
pub const SEARCH_ENGINES: &[(&str, &str)] =
    &[("search_std", "智谱基础版，约 0.01 元一次"), ("search_pro", "智谱高阶版，约 0.03 元一次"), ("search_pro_sogou", "搜狗，约 0.05 元一次"), ("search_pro_quark", "夸克，约 0.05 元一次")];
/// The one engine of Zhipu's international site, which also takes fewer fields.
pub const INTERNATIONAL_ENGINE: &str = "search-prime";
const RECENCY: &[&str] = &["oneDay", "oneWeek", "oneMonth", "oneYear"];
const SEARCH_RESULTS: u64 = 20;

/// Search the web for `query` and list what was found: title, source, date, address, and summary.
pub async fn search(config: &SearchConfig, query: &str, input: &Value, cancel: &CancellationToken) -> Fetched {
    let query = query.trim();
    if query.is_empty() {
        return failed("The query is empty.".to_string());
    }
    let mut count = input["count"].as_u64().unwrap_or(8).clamp(1, SEARCH_RESULTS);
    if config.engine == "search_pro_sogou" {
        // Sogou takes result counts in tens only.
        count = count.div_ceil(10) * 10;
    }
    let mut body = json!({ "search_engine": config.engine, "search_query": query, "count": count });
    if config.engine != INTERNATIONAL_ENGINE {
        // Search as asked, without the service first judging whether a search is needed.
        body["search_intent"] = json!(false);
        body["content_size"] = json!("medium");
    }
    if let Some(recency) = input["recency"].as_str() {
        if !RECENCY.contains(&recency) {
            return failed(format!("`recency` must be one of {}.", RECENCY.join(", ")));
        }
        body["search_recency_filter"] = json!(recency);
    }
    if let Some(site) = input["site"].as_str().map(str::trim).filter(|site| !site.is_empty()) {
        body["search_domain_filter"] = json!(site);
    }

    let client = match reqwest::Client::builder().connect_timeout(TIMEOUT).timeout(TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => return failed(format!("Cannot set up the request: {error}")),
    };
    let request = client.post(&config.url).bearer_auth(config.key.trim()).json(&body);
    let answered = async {
        let response = request.send().await.map_err(|error| format!("Cannot reach the search service: {error}"))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| format!("The search service's answer broke off: {error}"))?;
        Ok::<_, String>((status, text))
    };
    let (status, text) = tokio::select! {
        outcome = answered => match outcome {
            Ok(got) => got,
            Err(error) => return failed(error),
        },
        () = cancel.cancelled() => return failed("The search was cancelled.".to_string()),
    };
    let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    // The site in China wraps an error as {"error": {code, message}}; the international one puts both at the top.
    let error = if answer["error"].is_object() { &answer["error"] } else { &answer };
    if !status.is_success() || error["message"].is_string() && !answer["search_result"].is_array() {
        let code = error["code"].as_str().map(str::to_string).or_else(|| error["code"].as_i64().map(|code| code.to_string())).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        let message = error["message"].as_str().unwrap_or("the search service gave no reason");
        return failed(format!("The search failed ({code}): {message}"));
    }
    let results = answer["search_result"].as_array().map(Vec::as_slice).unwrap_or_default();
    if results.is_empty() {
        return Fetched { content: format!("No results for `{query}`. Try other words."), is_error: false, meta: json!({ "query": query, "results": 0 }) };
    }
    let listed: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let field = |name: &str| result[name].as_str().map(str::trim).filter(|value| !value.is_empty());
            let from: Vec<&str> = [field("media"), field("publish_date")].into_iter().flatten().collect();
            let heading = format!("{}. {}{}", index + 1, field("title").unwrap_or("(no title)"), if from.is_empty() { String::new() } else { format!(" ({})", from.join(", ")) });
            // A summary is enough to choose by; the page itself is read with fetch_url.
            let summary: String = field("content").unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(700).collect();
            format!("{heading}\n   {}\n   {summary}", field("link").unwrap_or("(no address)"))
        })
        .collect();
    Fetched { content: listed.join("\n\n"), is_error: false, meta: json!({ "query": query, "results": results.len() }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::serve;

    #[tokio::test]
    async fn searches_through_zhipus_api_and_lists_the_results() {
        let cancel = CancellationToken::new();
        let found = json!({ "id": "1", "created": 1, "search_result": [
            { "title": "Tokio 1.54 released", "content": "The  release\nadds a thing.", "link": "https://tokio.rs/blog/1-54", "media": "Tokio", "publish_date": "2026-10-01", "refer": "ref_1" },
            { "title": "", "content": "", "link": "https://example.com/x", "media": "", "publish_date": "" },
        ] });
        let busy = r#"{"error":{"code":"1701","message":"too many searches at once"}}"#;
        let (address, requests) = serve(vec![
            page("application/json", &found.to_string()),
            page("application/json", &json!({ "search_result": [] }).to_string()),
            format!("HTTP/1.1 429 Too Many\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{busy}", busy.len()),
            page("application/json", r#"{"code":1702,"message":"no engine"}"#),
        ])
        .await;
        let config = |engine: &str| SearchConfig { url: format!("{address}/paas/v4/web_search"), key: " glm-key ".into(), engine: engine.into() };

        let got = search(&config("search_std"), " tokio release ", &json!({ "count": 3, "recency": "oneMonth", "site": "tokio.rs" }), &cancel).await;
        assert_eq!(got.content, "1. Tokio 1.54 released (Tokio, 2026-10-01)\n   https://tokio.rs/blog/1-54\n   The release adds a thing.\n\n2. (no title)\n   https://example.com/x\n   ");
        assert_eq!((got.is_error, got.meta["results"].as_u64()), (false, Some(2)));
        // The international site's engine takes fewer fields; Sogou takes counts in tens.
        let none = search(&config(INTERNATIONAL_ENGINE), "nothing", &json!({}), &cancel).await;
        assert_eq!((none.content.as_str(), none.is_error), ("No results for `nothing`. Try other words.", false));
        assert_eq!(search(&config("search_pro_sogou"), "q", &json!({ "count": 12 }), &cancel).await.content, "The search failed (1701): too many searches at once");
        assert_eq!(search(&config("search_std"), "q", &json!({}), &cancel).await.content, "The search failed (1702): no engine");
        assert!(search(&config("search_std"), "q", &json!({ "recency": "lately" }), &cancel).await.content.starts_with("`recency` must be one of"));
        assert!(search(&config("search_std"), "  ", &json!({}), &cancel).await.is_error);

        let requests = requests.await.unwrap();
        let body = |request: &str| -> Value { serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap() };
        assert!(requests[0].starts_with("POST /paas/v4/web_search ") && requests[0].to_lowercase().contains("authorization: bearer glm-key\r\n"));
        assert_eq!(body(&requests[0]), json!({ "search_engine": "search_std", "search_query": "tokio release", "count": 3, "search_intent": false, "content_size": "medium", "search_recency_filter": "oneMonth", "search_domain_filter": "tokio.rs" }));
        assert_eq!(body(&requests[1]), json!({ "search_engine": "search-prime", "search_query": "nothing", "count": 8 }));
        assert_eq!(body(&requests[2])["count"], 20);
    }

    #[test]
    fn reduces_html_to_its_words() {
        let html = r#"<!DOCTYPE html><html><head><title> Release &amp; notes </title><style>p { color: red }</style></head>
            <body><script>track("x < y")</script><!-- hidden --><h1>Version 2</h1>
            <p>Fixes   the <b>lexer</b>&nbsp;bug &#8212; see <a href="/issues/1">issue&nbsp;#1</a>.</p>
            <ul><li>One</li><li>Two &lt;tag&gt; &unknown;</li></ul><div>分身 &#x5206;</div></body></html>"#;
        let (title, text) = html_text(html);
        assert_eq!(title.as_deref(), Some("Release & notes"));
        assert_eq!(text, "Version 2\n\nFixes the lexer bug — see issue #1.\n\nOne\n\nTwo <tag> &unknown;\n\n分身 分");
        assert_eq!(html_text("just words"), (None, "just words".to_string()));
    }

    #[test]
    fn reads_the_host_of_an_address() {
        assert_eq!(host("https://Docs.RS/serde/latest?x=1").as_deref(), Some("docs.rs"));
        assert_eq!(host("http://localhost:1420/").as_deref(), Some("localhost:1420"));
        assert_eq!(host("https://user:pw@example.com/a").as_deref(), Some("example.com"));
        for bad in ["ftp://example.com", "file:///etc/passwd", "example.com", "https://", "https:// bad/"] {
            assert_eq!(host(bad), None, "{bad}");
        }
    }

    fn page(kind: &str, body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn fetches_a_page_as_text_and_refuses_what_is_not() {
        let cancel = CancellationToken::new();
        let long = "word ".repeat(TEXT_CHARS / 4);
        let (address, requests) = serve(vec![
            page("text/html; charset=utf-8", "<html><head><title>Docs</title></head><body><p>Hello</p><p>there</p></body></html>"),
            page("application/json", "{\"ok\": true}"),
            page("application/pdf", "%PDF-1.7"),
            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string(),
            page("text/plain", &long),
            page("text/plain", &long),
        ])
        .await;
        let got = fetch(&format!("{address}/docs?page=1"), 0, &cancel).await;
        assert_eq!((got.content.as_str(), got.is_error), ("Title: Docs\n\nHello\n\nthere", false));
        assert_eq!((got.meta["title"].as_str(), got.meta["characters"].as_u64()), (Some("Docs"), Some(12)));
        assert_eq!(fetch(&format!("{address}/data.json"), 0, &cancel).await.content, "{\"ok\": true}");
        let pdf = fetch(&format!("{address}/a.pdf"), 0, &cancel).await;
        assert!(pdf.is_error && pdf.content.ends_with("is application/pdf, not text; it cannot be read here."), "{}", pdf.content);
        assert!(fetch(&format!("{address}/missing"), 0, &cancel).await.content.ends_with("answered HTTP 404."));

        // A long page is cut, and read on from an offset.
        let total = long.trim().chars().count();
        let first = fetch(&format!("{address}/long"), 0, &cancel).await;
        assert!(first.content.ends_with(&format!("[showing characters 0-{TEXT_CHARS} of {total}; pass offset {TEXT_CHARS} to read on]")), "{}", &first.content[first.content.len() - 90..]);
        let rest = fetch(&format!("{address}/long"), TEXT_CHARS, &cancel).await;
        assert_eq!(rest.content.chars().count(), total - TEXT_CHARS);

        let requests = requests.await.unwrap();
        assert!(requests[0].starts_with("GET /docs?page=1 ") && requests[0].to_lowercase().contains("user-agent: odg-fetch/0.1"));
        assert!(fetch("file:///etc/passwd", 0, &cancel).await.is_error);
    }
}
