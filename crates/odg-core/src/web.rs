//! Reaching the web for the model: `fetch_url` reads a page, `web_search`
//! finds pages.
//!
//! The page comes back as text. HTML is reduced to its words (scripts,
//! styles, and markup gone); a PDF to the words taken out of it (see
//! [`crate::pdf`]); other text is returned as it is; anything else (an
//! image, an archive) is refused. A request leaves the machine, and its
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

/// The headers a browser sends when it opens a page. Some sites (those behind
/// Akamai's screen for robots, for one) turn a request away without all of
/// them, though a person could read the page. Nothing in them is about the
/// user or the project. Only gzip is asked for, which [`unzipped`] undoes.
const BROWSER: &[(&str, &str)] = &[
    ("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"),
    ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,application/pdf;q=0.9,*/*;q=0.8"),
    ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8"),
    ("accept-encoding", "gzip"),
    ("sec-fetch-dest", "document"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-site", "none"),
    ("sec-fetch-user", "?1"),
    ("upgrade-insecure-requests", "1"),
];

/// How much of an error page goes to the model: enough for what it says of why, or of where the page went.
const ERROR_PAGE_CHARS: usize = 3_000;

/// A body the site sent gzipped, unzipped to no more than `limit` bytes: as
/// much as comes out, when the download was cut short or is not whole.
fn unzipped(body: &[u8], limit: usize) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    // An error here leaves what was unzipped before it in `out`, which is what there is to read.
    let _ = flate2::read::MultiGzDecoder::new(body).take(limit as u64).read_to_end(&mut out);
    out
}

/// What to make of an answer that is not the page: what it means, and where to go from it.
fn advice(status: u16) -> &'static str {
    match status {
        404 | 410 => {
            " There is no page at this address: it may be mistyped, or the page may have moved. Find its address from the site's home page or a search rather than guessing another."
        }
        401 | 403 => " The site refused the request: it may show this page only to a signed-in user, or turn away programs. Look for what is on it elsewhere.",
        429 => " The site is asking for fewer requests: wait before asking it again, or look elsewhere.",
        500..=599 => " The site failed to answer: it may work later.",
        _ => "",
    }
}

/// Whether `target` stays on the same HTTP(S) host and explicit port as `origin`, so a redirect
/// cannot quietly cross to another site or local service without asking the user.
fn same_origin_host(origin: &reqwest::Url, target: &reqwest::Url) -> bool {
    matches!(target.scheme(), "http" | "https")
        && origin.host_str().map(str::to_ascii_lowercase) == target.host_str().map(str::to_ascii_lowercase)
        && origin.port() == target.port()
}

/// Get `url` and return its text from character `offset` on.
pub async fn fetch(url: &str, offset: usize, cancel: &CancellationToken) -> Fetched {
    if host(url).is_none() {
        return failed(format!("`{url}` is not an http or https address."));
    }
    let Ok(origin) = reqwest::Url::parse(url) else {
        return failed(format!("`{url}` is not an http or https address."));
    };
    let redirect_origin = origin.clone();
    // Follow redirects only on the approved host: crossing to another host (or a local port) stops so the model can ask for that address.
    let redirects = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if same_origin_host(&redirect_origin, attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let client = match reqwest::Client::builder().connect_timeout(TIMEOUT).timeout(TIMEOUT).redirect(redirects).build() {
        Ok(client) => client,
        Err(error) => return failed(format!("Cannot set up the request: {error}")),
    };
    let request = BROWSER.iter().fold(client.get(url), |request, (name, value)| request.header(*name, *value));
    let download = async {
        let mut response = request.send().await.map_err(|error| format!("Cannot reach {url}: {error}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response.headers().get(reqwest::header::LOCATION).and_then(|value| value.to_str().ok()).unwrap_or_default().trim();
            let target = if location.is_empty() {
                String::new()
            } else {
                response.url().join(location).map_or_else(|_| location.to_string(), |resolved| resolved.to_string())
            };
            return Err(if target.is_empty() {
                format!("{url} answered HTTP {} without a redirect address.", status.as_u16())
            } else if host(&target).is_some() {
                format!("{url} redirected to {target}, which is on another host. Call `fetch_url` with `{target}` if you want to read it (the user will be asked for that host).")
            } else {
                format!("{url} redirected to `{target}`, which is not an http or https address and will not be followed.")
            });
        }
        let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).unwrap_or_default().to_lowercase();
        let (kind, encoding) = (header("content-type"), header("content-encoding"));
        let landed = response.url().to_string();
        // A document is read whole or not at all, and runs larger than a page of HTML.
        let limit = if is_document_type(&kind) { crate::documents::DOCUMENT_BYTES as usize } else { BODY_BYTES };
        let mut body: Vec<u8> = Vec::new();
        let mut whole = true;
        while let Some(chunk) = response.chunk().await.map_err(|error| format!("The download of {url} broke off: {error}"))? {
            body.extend_from_slice(&chunk);
            if body.len() > limit {
                body.truncate(limit);
                whole = false;
                break;
            }
        }
        if encoding.contains("gzip") {
            let unzipped = unzipped(&body, limit + 1);
            whole &= unzipped.len() <= limit;
            body = unzipped;
            body.truncate(limit);
        }
        Ok::<_, String>((status, kind, landed, body, whole))
    };
    let (status, kind, landed, body, whole) = tokio::select! {
        outcome = download => match outcome {
            Ok(got) => got,
            Err(error) => return failed(error),
        },
        () = cancel.cancelled() => return failed("The fetch was cancelled.".to_string()),
    };
    let textual = kind.is_empty() || kind.starts_with("text/") || ["json", "xml", "javascript", "yaml"].iter().any(|word| kind.contains(word));
    let binary = body.iter().take(8192).any(|byte| *byte == 0);
    if !status.is_success() {
        let mut message = format!("{url} answered HTTP {}.{}", status.as_u16(), advice(status.as_u16()));
        // What the site sent with it: an error page often says why, or links to where the page went.
        if textual && !binary {
            let (_, text) = html_text(&String::from_utf8_lossy(&body));
            if !text.is_empty() {
                let shown: String = text.chars().take(ERROR_PAGE_CHARS).collect();
                let cut = if shown.len() < text.len() { "\n[cut]" } else { "" };
                message.push_str(&format!("\n\nThe page it sent with that:\n{shown}{cut}"));
            }
        }
        return failed(message);
    }
    // A document cut short cannot be read, and a cut zip is not even known for one: its list of parts is at its end.
    if !whole && is_document_type(&kind) {
        return failed(format!("{url} is a document larger than {} MB, too large to read here.", crate::documents::DOCUMENT_BYTES / 1024 / 1024));
    }
    let (title, text, document) = if let Some(found) = crate::documents::kind_of_bytes(&body, &landed) {
        let document = match found {
            Ok(document) if whole => document,
            Ok(_) => return failed(format!("{url} is a document larger than {} MB, too large to read here.", crate::documents::DOCUMENT_BYTES / 1024 / 1024)),
            Err(why) => return failed(format!("{url} {}.", why.describe())),
        };
        // Taking the words out of a long document takes a while: off the thread the agent runs on.
        match tokio::task::spawn_blocking(move || crate::documents::text(&body, document)).await {
            Ok(Ok(read)) => (None, format!("[It is {}]\n{}", read.describe(), read.text), Some(read)),
            Ok(Err(why)) => return failed(format!("{url} {}.", why.describe())),
            Err(error) => return failed(format!("Cannot read the document at {url}: {error}")),
        }
    } else {
        if !textual || binary {
            return failed(format!("{url} is {}, not text or a document; it cannot be read here.", if kind.is_empty() { "binary data" } else { kind.as_str() }));
        }
        let raw = String::from_utf8_lossy(&body);
        let looks_like_html = kind.contains("html") || raw.trim_start().get(..15).is_some_and(|start| start.to_lowercase().starts_with("<!doctype html") || start.to_lowercase().starts_with("<html"));
        let (title, text) = if looks_like_html { html_text(&raw) } else { (None, raw.trim().to_string()) };
        (title, text, None)
    };

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
    let mut meta = json!({ "url": landed, "title": title, "characters": total });
    if let Some(read) = document {
        meta["document"] = json!(read.kind);
        meta["parts"] = json!(read.parts);
    }
    Fetched { content, is_error: false, meta }
}

/// Whether a content type is that of a document: a PDF, or an Office or OpenDocument file.
fn is_document_type(kind: &str) -> bool {
    ["pdf", "officedocument", "msword", "ms-excel", "ms-powerpoint", "opendocument"].iter().any(|word| kind.contains(word))
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

/// The engines Zhipu's site in China offers, the plainest first: `(id, what it is)`. What each costs is Zhipu's to
/// say, and changes; it is not said here.
pub const SEARCH_ENGINES: &[(&str, &str)] = &[("search_std", "智谱基础版"), ("search_pro", "智谱高阶版"), ("search_pro_sogou", "搜狗"), ("search_pro_quark", "夸克")];
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

    #[test]
    fn allows_redirects_only_on_the_same_host_and_port() {
        let url = |text: &str| reqwest::Url::parse(text).unwrap();
        assert!(same_origin_host(&url("https://docs.rs/a"), &url("https://DOCS.RS/b")));
        assert!(same_origin_host(&url("http://docs.rs/a"), &url("https://docs.rs/a")));
        assert!(same_origin_host(&url("http://127.0.0.1:8080/a"), &url("http://127.0.0.1:8080/b")));
        assert!(!same_origin_host(&url("https://docs.rs/a"), &url("https://other.example/a")));
        assert!(!same_origin_host(&url("http://127.0.0.1:8080/a"), &url("http://127.0.0.1:9090/a")));
        assert!(!same_origin_host(&url("https://docs.rs/a"), &url("file:///etc/passwd")));
    }

    #[tokio::test]
    async fn follows_same_host_redirects_and_stops_at_another_host() {
        let cancel = CancellationToken::new();
        let (address, requests) = crate::testing::serve_bytes(vec![
            answer("302 Found", "location: /landed\r\n", b""),
            page("text/plain", "arrived").into_bytes(),
            answer("302 Found", "location: https://other.example/secret\r\n", b""),
        ])
        .await;
        let same = fetch(&format!("{address}/start"), 0, &cancel).await;
        assert!(!same.is_error && same.content == format!("[redirected to {address}/landed]\narrived"), "{}", same.content);
        let cross = fetch(&format!("{address}/bounce"), 0, &cancel).await;
        assert!(cross.is_error && cross.content.contains("on another host") && cross.content.contains("https://other.example/secret"), "{}", cross.content);
        assert_eq!(requests.await.unwrap().len(), 3);
    }

    fn page(kind: &str, body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    /// A response with this status line, these headers, and this body, which need not be text.
    fn answer(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!("HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[tokio::test]
    async fn fetches_a_page_as_text_and_refuses_what_is_not() {
        use std::io::Write;
        let cancel = CancellationToken::new();
        let long = "word ".repeat(TEXT_CHARS / 4);
        let moved = "<html><head><title>Not found</title></head><body><h1>No such page</h1><p>Datasheets are now under /docs.</p></body></html>";
        let mut zipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zipped.write_all(b"<html><head><title>Zipped</title></head><body><p>Small and squeezed</p></body></html>").unwrap();
        let (address, requests) = crate::testing::serve_bytes(vec![
            page("text/html; charset=utf-8", "<html><head><title>Docs</title></head><body><p>Hello</p><p>there</p></body></html>").into_bytes(),
            page("application/json", "{\"ok\": true}").into_bytes(),
            answer("200 OK", "content-type: application/pdf\r\n", &crate::pdf::tests::sample(&[&["Pin 1 is VIN"], &["Absolute maximum 42 V"]])),
            answer("404 Not Found", "content-type: text/html\r\n", moved.as_bytes()),
            page("text/plain", &long).into_bytes(),
            page("text/plain", &long).into_bytes(),
            answer("200 OK", "content-type: text/html\r\ncontent-encoding: gzip\r\n", &zipped.finish().unwrap()),
            answer("200 OK", "content-type: application/zip\r\n", &[b'P', b'K', 3, 4, 0, 0]),
            answer("200 OK", "content-type: application/octet-stream\r\n", &crate::pdf::tests::sample(&[&[]])),
            // A type that says xml, which is not to be read as text.
            answer(
                "200 OK",
                "content-type: application/vnd.openxmlformats-officedocument.wordprocessingml.document\r\n",
                &crate::documents::tests::word_document(&crate::documents::tests::paragraph(Some("1"), "Errata")),
            ),
        ])
        .await;
        let got = fetch(&format!("{address}/docs?page=1"), 0, &cancel).await;
        assert_eq!((got.content.as_str(), got.is_error), ("Title: Docs\n\nHello\n\nthere", false));
        assert_eq!((got.meta["title"].as_str(), got.meta["characters"].as_u64()), (Some("Docs"), Some(12)));
        assert_eq!(fetch(&format!("{address}/data.json"), 0, &cancel).await.content, "{\"ok\": true}");
        // A PDF comes back as its words, page by page.
        let pdf = fetch(&format!("{address}/a.pdf"), 0, &cancel).await;
        assert!(!pdf.is_error && pdf.content.starts_with("[It is a PDF of 2 pages. This is its text"), "{}", pdf.content);
        assert!(pdf.content.contains("[page 1 of 2]\nPin 1 is VIN") && pdf.content.contains("[page 2 of 2]\nAbsolute maximum 42 V"), "{}", pdf.content);
        assert_eq!((pdf.meta["document"].as_str(), pdf.meta["parts"].as_u64()), (Some("pdf"), Some(2)));
        // A missing page says so, with what to do instead and what the site's own page said.
        let missing = fetch(&format!("{address}/missing"), 0, &cancel).await;
        assert!(missing.is_error && missing.content.contains("answered HTTP 404. There is no page at this address") && missing.content.contains("rather than guessing"), "{}", missing.content);
        assert!(missing.content.ends_with("The page it sent with that:\nNo such page\n\nDatasheets are now under /docs."), "{}", missing.content);

        // A long page is cut, and read on from an offset.
        let total = long.trim().chars().count();
        let first = fetch(&format!("{address}/long"), 0, &cancel).await;
        assert!(first.content.ends_with(&format!("[showing characters 0-{TEXT_CHARS} of {total}; pass offset {TEXT_CHARS} to read on]")), "{}", &first.content[first.content.len() - 90..]);
        let rest = fetch(&format!("{address}/long"), TEXT_CHARS, &cancel).await;
        assert_eq!(rest.content.chars().count(), total - TEXT_CHARS);
        // A page sent gzipped is unzipped; an archive is still refused; a PDF of scans says why it gives nothing.
        assert_eq!(fetch(&format!("{address}/zipped"), 0, &cancel).await.content, "Title: Zipped\n\nSmall and squeezed");
        let archive = fetch(&format!("{address}/a.zip"), 0, &cancel).await;
        assert!(archive.is_error && archive.content.ends_with("is application/zip, not text or a document; it cannot be read here."), "{}", archive.content);
        let scan = fetch(&format!("{address}/scan"), 0, &cancel).await;
        assert!(scan.is_error && scan.content.contains("no text in it"), "{}", scan.content);
        let word = fetch(&format!("{address}/errata.docx"), 0, &cancel).await;
        assert!(!word.is_error && word.content.starts_with("[It is a Word document.") && word.content.ends_with("]\n# Errata"), "{}", word.content);

        // Asked for as a browser asks: some sites turn away anything else.
        let requests = requests.await.unwrap();
        let asked = requests[0].to_lowercase();
        assert!(requests[0].starts_with("GET /docs?page=1 "));
        assert!(["user-agent: mozilla/5.0", "accept-encoding: gzip", "accept-language: zh-cn", "sec-fetch-mode: navigate"].iter().all(|header| asked.contains(header)), "{asked}");
        assert!(fetch("file:///etc/passwd", 0, &cancel).await.is_error);
    }
}
