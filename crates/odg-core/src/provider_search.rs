//! Searching the web through the provider a session works with, for when no GLM key is saved for Zhipu's search.
//!
//! The session's own provider and model are asked, in a request of their own, to search with the provider's web
//! search tool, which runs on the provider's side; what comes back is listed for the agent as Zhipu's results are, so
//! `web_search` reads the same whoever searched. The call is still the app's own tool, so the user is asked before
//! each search, as for Zhipu's. Only Anthropic's and OpenAI's own APIs have such a tool.

use serde_json::{Value, json};

use crate::llm::{AssistantTurn, LlmRequest};
use crate::providers::kind_id;
use crate::tools::ToolOutput;

/// The most searches the provider runs for one `web_search` call.
const SEARCHES: u32 = 2;
/// How many sources are listed by default, and at most.
const LISTED: usize = 8;
const LISTED_LIMIT: usize = 20;

/// What the searching model is told. It answers the agent, not the user.
pub const SEARCHER: &str = "You search the web for a coding agent, which reads your answer as the results of the search it asked for. Search for what the message asks, with the web search tool, once or twice. Then write, for each result worth reading, its title and in one or two sentences what it says that bears on the search, citing it. Put the most useful first. Say only what the pages say: no advice, no answer of your own, no greeting.";

/// Whether the session's provider can search the web for the agent.
pub fn can_search(provider: &str) -> bool {
    matches!(kind_id(provider), "anthropic" | "openai")
}

/// Anthropic's search tool for `model`: the version with dynamic filtering where the model takes it.
fn anthropic_tool(model: &str) -> &'static str {
    const FILTERING: &[&str] = &["claude-fable-5", "claude-mythos-5", "claude-opus-5", "claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-5", "claude-sonnet-4-6"];
    if FILTERING.iter().any(|prefix| model.starts_with(prefix)) { "web_search_20260209" } else { "web_search_20250305" }
}

/// What `web_search`'s `recency` asks for, in words the searching model is given.
fn recency(input: &Value) -> Result<Option<&'static str>, String> {
    Ok(match input["recency"].as_str() {
        None => None,
        Some("oneDay") => Some("the past day"),
        Some("oneWeek") => Some("the past week"),
        Some("oneMonth") => Some("the past month"),
        Some("oneYear") => Some("the past year"),
        Some(_) => return Err("`recency` must be one of oneDay, oneWeek, oneMonth, oneYear.".to_string()),
    })
}

/// The request that has the session's provider search for `query`, as `web_search` was called with it.
pub fn request(provider: &str, session: &str, model: &str, query: &str, input: &Value) -> Result<LlmRequest, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("The query is empty.".to_string());
    }
    let site = input["site"].as_str().map(str::trim).filter(|site| !site.is_empty());
    let mut asked = format!("Search the web for: {query}");
    if let Some(within) = recency(input)? {
        asked.push_str(&format!("\nOnly results from {within}."));
    }
    if let Some(site) = site {
        asked.push_str(&format!("\nOnly results from {site}."));
    }
    let tool = match kind_id(provider) {
        "anthropic" => {
            let mut tool = json!({ "type": anthropic_tool(model), "name": "web_search", "max_uses": SEARCHES });
            if let Some(site) = site {
                tool["allowed_domains"] = json!([site]);
            }
            tool
        }
        "openai" => {
            let mut tool = json!({ "type": "web_search" });
            if let Some(site) = site {
                tool["filters"] = json!({ "allowed_domains": [site] });
            }
            tool
        }
        _ => return Err(format!("Web search is not set up: it needs a GLM (Zhipu) API key saved in the settings, and {provider} has no search of its own.")),
    };
    Ok(LlmRequest {
        provider: provider.to_string(),
        session: session.to_string(),
        model: model.to_string(),
        system: SEARCHER.to_string(),
        messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": asked }] })],
        tools: vec![tool],
        effort: "low".to_string(),
    })
}

/// A page the search found: where, what it is called, how old it is when that is known, and what was cited from it.
#[derive(Debug, Default, PartialEq)]
struct Source {
    url: String,
    title: String,
    age: String,
    cited: Vec<String>,
}

/// What came back of a search, as `web_search` gives it to the agent: the searching model's account, then the sources,
/// with what was cited from each. `kind` is the provider's kind; `model` the model that searched.
pub fn results(turn: &AssistantTurn, query: &str, input: &Value, kind: &str, model: &str) -> ToolOutput {
    let mut sources: Vec<Source> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    let mut add = |url: &str, title: &str, age: &str, cited: Option<&str>| {
        if url.is_empty() {
            return;
        }
        let index = match sources.iter().position(|source| source.url == url) {
            Some(index) => index,
            None => {
                sources.push(Source { url: url.to_string(), ..Source::default() });
                sources.len() - 1
            }
        };
        let source = &mut sources[index];
        if source.title.is_empty() {
            source.title = title.trim().to_string();
        }
        if source.age.is_empty() {
            source.age = age.trim().to_string();
        }
        let cited = cited.map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|text| !text.is_empty());
        if let Some(cited) = cited.filter(|cited| !source.cited.contains(cited)) {
            source.cited.push(cited);
        }
    };
    for block in &turn.content {
        match block["type"].as_str() {
            // Anthropic's: the pages a search found, or what went wrong with it, which is not an error to the request.
            Some("web_search_tool_result") => match block["content"].as_array() {
                Some(found) => {
                    for result in found {
                        add(result["url"].as_str().unwrap_or_default(), result["title"].as_str().unwrap_or_default(), result["page_age"].as_str().unwrap_or_default(), None);
                    }
                }
                None => {
                    let code = block["content"]["error_code"].as_str().unwrap_or("unknown");
                    return ToolOutput::error(format!("The search through {kind} failed: {code}."));
                }
            },
            Some("text") => {
                if let Some(text) = block["text"].as_str().map(str::trim).filter(|text| !text.is_empty()) {
                    said.push(text.to_string());
                }
                // Anthropic's citations, on the text that cites them.
                for citation in block["citations"].as_array().into_iter().flatten() {
                    add(citation["url"].as_str().unwrap_or_default(), citation["title"].as_str().unwrap_or_default(), "", citation["cited_text"].as_str());
                }
                // OpenAI's, on the message the text came from.
                for part in block["item"]["content"].as_array().into_iter().flatten() {
                    for annotation in part["annotations"].as_array().into_iter().flatten().filter(|annotation| annotation["type"] == "url_citation") {
                        add(annotation["url"].as_str().unwrap_or_default(), annotation["title"].as_str().unwrap_or_default(), "", None);
                    }
                }
            }
            _ => {}
        }
    }
    let listed = usize::try_from(input["count"].as_u64().unwrap_or(LISTED as u64)).unwrap_or(LISTED).clamp(1, LISTED_LIMIT);
    let meta = |results: usize| json!({ "query": query, "results": results, "engine": kind, "model": model });
    if sources.is_empty() && said.is_empty() {
        return ToolOutput { content: format!("No results for `{query}`. Try other words."), is_error: false, meta: meta(0), images: Vec::new() };
    }
    // Pages that were cited come first: the searching model judged them worth reading.
    sources.sort_by_key(|source| source.cited.is_empty());
    sources.truncate(listed);
    let mut content = format!("Searched the web for `{query}` through {kind} ({model}).");
    if !said.is_empty() {
        content.push_str(&format!("\n\n{}", said.join("\n\n")));
    }
    if !sources.is_empty() {
        content.push_str("\n\nSources:");
        for (index, source) in sources.iter().enumerate() {
            let title = if source.title.is_empty() { "(no title)" } else { source.title.as_str() };
            let age = if source.age.is_empty() { String::new() } else { format!(" ({})", source.age) };
            content.push_str(&format!("\n{}. {title}{age}\n   {}", index + 1, source.url));
            for cited in &source.cited {
                let cited: String = cited.chars().take(300).collect();
                content.push_str(&format!("\n   “{cited}”"));
            }
        }
    }
    content.push_str("\n\nRead a page in full with fetch_url.");
    ToolOutput { content, is_error: false, meta: meta(sources.len()), images: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_each_provider_with_its_own_search_tool() {
        let input = json!({ "site": "docs.rs", "recency": "oneWeek" });
        let asked = request("anthropic", "s1", "claude-opus-5-5", " tokio 1.54 ", &input).unwrap();
        assert_eq!(asked.tools, [json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": 2, "allowed_domains": ["docs.rs"] })]);
        assert_eq!((asked.model.as_str(), asked.system.as_str(), asked.effort.as_str()), ("claude-opus-5-5", SEARCHER, "low"));
        assert_eq!(asked.messages[0]["content"][0]["text"], "Search the web for: tokio 1.54\nOnly results from the past week.\nOnly results from docs.rs.");
        // An older model gets the search without dynamic filtering; an added provider of the kind searches too.
        assert_eq!(request("anthropic-2", "s1", "claude-haiku-4-5", "x", &json!({})).unwrap().tools[0]["type"], "web_search_20250305");
        assert_eq!(request("openai", "s1", "gpt-6.2-sol", "x", &json!({ "site": "docs.rs" })).unwrap().tools, [json!({ "type": "web_search", "filters": { "allowed_domains": ["docs.rs"] } })]);
        // Those without a search of their own, and what makes no search, are refused.
        assert!(can_search("anthropic") && can_search("openai-2") && !can_search("deepseek") && !can_search("custom_anthropic"));
        assert!(request("deepseek", "s1", "m", "x", &json!({})).unwrap_err().contains("GLM"));
        assert_eq!(request("anthropic", "s1", "m", "  ", &json!({})).unwrap_err(), "The query is empty.");
        assert!(request("anthropic", "s1", "m", "x", &json!({ "recency": "oneHour" })).unwrap_err().contains("`recency`"));
    }

    fn turn(content: Vec<Value>) -> AssistantTurn {
        AssistantTurn { content, stop_reason: Some("end_turn".into()), model: "m".into(), ..Default::default() }
    }

    #[test]
    fn lists_what_anthropics_search_found_with_what_was_cited() {
        let found = turn(vec![
            json!({ "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "tokio 1.54" } }),
            json!({ "type": "web_search_tool_result", "tool_use_id": "s1", "content": [
                { "type": "web_search_result", "url": "https://tokio.rs/blog/1-54", "title": "Tokio 1.54", "page_age": "October 1, 2026", "encrypted_content": "x" },
                { "type": "web_search_result", "url": "https://example.com/other", "title": "Other", "encrypted_content": "y" },
            ] }),
            json!({ "type": "text", "text": "Tokio 1.54 adds a thing.", "citations": [
                { "type": "web_search_result_location", "url": "https://example.com/other", "title": "Other", "cited_text": "Other  page\nsays so.", "encrypted_index": "z" },
            ] }),
        ]);
        let told = results(&found, "tokio 1.54", &json!({}), "anthropic", "claude-opus-5-5");
        assert!(!told.is_error);
        assert_eq!(
            told.content,
            "Searched the web for `tokio 1.54` through anthropic (claude-opus-5-5).\n\nTokio 1.54 adds a thing.\n\nSources:\n1. Other\n   https://example.com/other\n   “Other page says so.”\n2. Tokio 1.54 (October 1, 2026)\n   https://tokio.rs/blog/1-54\n\nRead a page in full with fetch_url."
        );
        assert_eq!(told.meta, json!({ "query": "tokio 1.54", "results": 2, "engine": "anthropic", "model": "claude-opus-5-5" }));
        // As many as asked for.
        assert_eq!(results(&found, "q", &json!({ "count": 1 }), "anthropic", "m").meta["results"], 1);
        // A search that failed says why; one that found nothing says so.
        let failed = turn(vec![json!({ "type": "web_search_tool_result", "tool_use_id": "s1", "content": { "type": "web_search_tool_result_error", "error_code": "too_many_requests" } })]);
        let failed = results(&failed, "q", &json!({}), "anthropic", "m");
        assert!(failed.is_error && failed.content == "The search through anthropic failed: too_many_requests.");
        assert_eq!(results(&turn(vec![]), "q", &json!({}), "anthropic", "m").content, "No results for `q`. Try other words.");
    }

    #[test]
    fn lists_what_openais_search_found_from_the_citations_on_its_answer() {
        let message = json!({ "type": "message", "content": [{ "type": "output_text", "text": "Tokio 1.54 adds a thing.", "annotations": [
            { "type": "url_citation", "url": "https://tokio.rs/blog/1-54", "title": "Tokio 1.54", "start_index": 0, "end_index": 10 },
            { "type": "url_citation", "url": "https://tokio.rs/blog/1-54", "title": "Tokio 1.54", "start_index": 11, "end_index": 20 },
        ] }] });
        let found = turn(vec![
            json!({ "type": "openai_web_search_call", "item": { "type": "web_search_call", "status": "completed" } }),
            json!({ "type": "text", "text": "Tokio 1.54 adds a thing.", "item": message }),
        ]);
        let told = results(&found, "tokio", &json!({}), "openai", "gpt-6.2-sol");
        assert!(told.content.ends_with("Sources:\n1. Tokio 1.54\n   https://tokio.rs/blog/1-54\n\nRead a page in full with fetch_url."), "{}", told.content);
        assert_eq!(told.meta["results"], 1);
    }
}
