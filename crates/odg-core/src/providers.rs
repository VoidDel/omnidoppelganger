//! The model providers a session can run on, and the [`Router`] that sends
//! each request to the client speaking its provider's protocol.
//!
//! A session keeps the provider it was created with, so its log only ever
//! holds one provider's content blocks and nothing is converted between them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::anthropic::{self, AnthropicClient};
use crate::cli_agents::CliKind;
use crate::llm::{AssistantTurn, EndpointSource, Llm, LlmError, LlmRequest, StreamDelta, Tokens};
use crate::openai_chat::ChatClient;
use crate::openai_responses::ResponsesClient;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Responses API. OpenAI's reasoning models call tools only through it.
    Responses,
    /// OpenAI-style Chat Completions, with the provider's own extensions.
    Chat(Flavor),
    /// Local coding-agent CLI (`agy`, `codex`, `claude`, `cursor-agent`, `grok`) running headless JSON streaming.
    Cli(CliKind),
}

/// Which Chat Completions dialect a provider speaks. They differ in the
/// effort levels they accept and in a few request fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    DeepSeek,
    Kimi,
    Glm,
    /// Any other OpenAI-compatible server: only standard fields are sent.
    Generic,
}

#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    /// Where requests go unless settings name another address. Empty when the user must give one.
    pub base_url: &'static str,
    /// Other addresses the provider serves, shown as hints: `(address, what it is)`.
    pub other_urls: &'static [(&'static str, &'static str)],
    /// Environment variable read when no key is saved.
    pub key_env: Option<&'static str>,
    /// Suggested models, `(id, display name)`, the default first. Any other model id can be typed in.
    pub models: &'static [(&'static str, &'static str)],
}

pub const DEFAULT_PROVIDER: &str = "anthropic";

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "anthropic",
        name: "Anthropic",
        protocol: Protocol::Anthropic,
        base_url: anthropic::DEFAULT_BASE_URL,
        other_urls: &[],
        key_env: Some("ANTHROPIC_API_KEY"),
        models: anthropic::MODELS,
    },
    Provider {
        id: "openai",
        name: "OpenAI",
        protocol: Protocol::Responses,
        base_url: "https://api.openai.com/v1",
        other_urls: &[],
        key_env: Some("OPENAI_API_KEY"),
        models: &[("gpt-6.2-astra", "GPT-6.2 Astra"), ("gpt-6.2-sol", "GPT-6.2 Sol")],
    },
    Provider {
        id: "deepseek",
        name: "DeepSeek",
        protocol: Protocol::Chat(Flavor::DeepSeek),
        base_url: "https://api.deepseek.com",
        other_urls: &[],
        key_env: Some("DEEPSEEK_API_KEY"),
        models: &[("deepseek-v4-pro", "DeepSeek V4 Pro"), ("deepseek-flash", "DeepSeek Flash")],
    },
    Provider {
        id: "moonshot",
        name: "Kimi（月之暗面）",
        protocol: Protocol::Chat(Flavor::Kimi),
        base_url: "https://api.moonshot.cn/v1",
        other_urls: &[("https://api.moonshot.ai/v1", "国际站")],
        key_env: Some("MOONSHOT_API_KEY"),
        models: &[("kimi-k3.1-pro", "Kimi K3.1 Pro"), ("kimi-k3.1-pro-highspeed", "Kimi K3.1 Pro 高速版"), ("kimi-k3", "Kimi K3")],
    },
    Provider {
        id: "zhipu",
        name: "GLM（智谱）",
        protocol: Protocol::Chat(Flavor::Glm),
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        other_urls: &[("https://api.z.ai/api/paas/v4", "国际站 Z.ai")],
        key_env: Some("ZHIPUAI_API_KEY"),
        models: &[("glm-5.3", "GLM-5.3"), ("glm-5.3-flash", "GLM-5.3 Flash（能看图）"), ("glm-5.2", "GLM-5.2"), ("glm-4.7", "GLM-4.7")],
    },
    Provider {
        id: "custom",
        name: "OpenAI 兼容服务",
        protocol: Protocol::Chat(Flavor::Generic),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[],
    },
    // A gateway or another provider's endpoint that speaks Anthropic's Messages API.
    Provider {
        id: "custom_anthropic",
        name: "Anthropic 兼容服务",
        protocol: Protocol::Anthropic,
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[],
    },
    Provider {
        id: "magpie",
        name: "Magpie（本机网关）",
        protocol: Protocol::Chat(Flavor::Generic),
        base_url: "http://127.0.0.1:3425/v1",
        other_urls: &[("http://localhost:3425/v1", "localhost 别名")],
        key_env: Some("MAGPIE_API_KEY"),
        models: &[],
    },
    Provider {
        id: "antigravity",
        name: "Antigravity (agy CLI)",
        protocol: Protocol::Cli(CliKind::Antigravity),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[],
    },
    Provider {
        id: "codex",
        name: "Codex (CLI)",
        protocol: Protocol::Cli(CliKind::Codex),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[
            ("gpt-5.4", "GPT-5.4"),
            ("gpt-5.3-codex", "GPT-5.3 Codex"),
            ("gpt-5.2-codex", "GPT-5.2 Codex"),
            ("o4-mini", "o4-mini"),
        ],
    },
    Provider {
        id: "claude_code",
        name: "Claude Code (CLI)",
        protocol: Protocol::Cli(CliKind::ClaudeCode),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[
            ("opus", "Claude Opus（最新）"),
            ("sonnet", "Claude Sonnet（最新）"),
            ("claude-opus-4-7", "Claude Opus 4.7"),
            ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ("haiku", "Claude Haiku（最新）"),
        ],
    },
    Provider {
        id: "cursor",
        name: "Cursor (cursor-agent)",
        protocol: Protocol::Cli(CliKind::Cursor),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[
            ("auto", "Auto（Cursor 默认）"),
            ("claude-4.6-sonnet-thinking", "Claude Sonnet Thinking"),
            ("claude-4.6-opus-thinking", "Claude Opus Thinking"),
            ("gpt-5.4", "GPT-5.4"),
            ("gemini-3.1-pro", "Gemini Pro"),
        ],
    },
    Provider {
        id: "grok",
        name: "Grok (CLI)",
        protocol: Protocol::Cli(CliKind::Grok),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[
            ("grok-4.7", "Grok 4.7"),
            ("grok-4-fast", "Grok 4 Fast"),
            ("grok-code-fast-1", "Grok Code Fast"),
        ],
    },
    Provider {
        id: "opencode",
        name: "OpenCode (CLI)",
        protocol: Protocol::Cli(CliKind::OpenCode),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[],
    },
];

/// The kind of provider `id` names: a kind's own id, which is also the first
/// of that kind, or `<kind>-<n>` (n from 2) for another one the user added,
/// with a key and address of its own.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| {
        provider.id == id || id.strip_prefix(provider.id).and_then(|rest| rest.strip_prefix('-')).is_some_and(|n| !n.starts_with('0') && n.parse::<u32>().is_ok_and(|n| n >= 2))
    })
}

/// The id of the kind of provider `id` names; `id` itself where it names none.
pub fn kind_id(id: &str) -> &str {
    provider(id).map_or(id, |kind| kind.id)
}

/// Whether `id` names a provider the user added beside the first of its kind.
pub fn is_added(id: &str) -> bool {
    provider(id).is_some_and(|kind| kind.id != id)
}

/// A provider as the settings list it: one of a kind, under the name the user
/// gave it or its kind's, with the kind's protocol and models.
#[derive(Debug, Clone, Serialize)]
pub struct Instance {
    pub id: String,
    /// The id of its kind, which is its own id for the first of a kind.
    pub kind: &'static str,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: &'static str,
    pub other_urls: &'static [(&'static str, &'static str)],
    /// The environment variable its key is read from when none is saved: only the first of a kind has one.
    pub key_env: Option<&'static str>,
    pub models: &'static [(&'static str, &'static str)],
}

/// The provider `id` as the settings list it, under `name`, or its kind's name when none is given.
pub fn instance(id: &str, name: Option<&str>) -> Option<Instance> {
    let kind = provider(id)?;
    Some(Instance {
        id: id.to_string(),
        kind: kind.id,
        name: name.map(str::trim).filter(|name| !name.is_empty()).unwrap_or(kind.name).to_string(),
        protocol: kind.protocol,
        base_url: kind.base_url,
        other_urls: kind.other_urls,
        key_env: if kind.id == id { kind.key_env } else { None },
        models: kind.models,
    })
}

/// The context window assumed for a model no provider here documents.
pub const UNKNOWN_CONTEXT_WINDOW: u64 = 128_000;

/// How many tokens a model's context window holds, from each provider's
/// documentation as of November 2026. `None` for a model that is not listed:
/// a newer one typed in by hand, or anything on a custom service.
pub fn documented_window(provider: &str, model: &str) -> Option<u64> {
    if let Some(Protocol::Cli(kind)) = self::provider(provider).map(|known| known.protocol) {
        return Some(crate::cli_agents::context_window(kind));
    }
    match (kind_id(provider), model) {
        ("anthropic", model) if anthropic::MODELS.iter().any(|(id, _)| *id == model) => Some(1_000_000),
        ("openai", "gpt-6.2-astra" | "gpt-6.2-sol") => Some(1_050_000),
        ("deepseek", "deepseek-v4-pro" | "deepseek-flash") => Some(1_000_000),
        ("moonshot", "kimi-k3.1-pro" | "kimi-k3.1-pro-highspeed" | "kimi-k3") => Some(1_048_576),
        // Zhipu's model overview, which is the only place it says: its list of models carries no sizes.
        ("zhipu", "glm-5.3" | "glm-5.3-flash" | "glm-5.3-flashx" | "glm-5.3-flashx-legal" | "glm-5.2" | "glm-4-long") => Some(1_000_000),
        ("zhipu", "glm-5.1" | "glm-5v-turbo" | "glm-5-turbo" | "glm-5" | "glm-4.7" | "glm-4.7-flashx" | "glm-4.7-flash" | "glm-4.6") => Some(200_000),
        // Smaller than what an unlisted model is assumed to hold, so these must be said.
        ("zhipu", "glm-4.1v-thinking-flashx" | "glm-4.1v-thinking-flash") => Some(64_000),
        _ => None,
    }
}

/// [`documented_window`], or for a model that is not listed
/// [`UNKNOWN_CONTEXT_WINDOW`], which is small enough to be safe. What a
/// provider says of its own models comes before either: see
/// [`crate::settings::SettingsStore::context_window`].
pub fn context_window(provider: &str, model: &str) -> u64 {
    documented_window(provider, model).unwrap_or(UNKNOWN_CONTEXT_WINDOW)
}

/// A model as its provider lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    /// The name to show; the id where the provider gives none.
    pub name: String,
    /// How many tokens its context window holds, where the provider says.
    pub context_window: Option<u64>,
}

/// The fields a model's context window is listed under: Anthropic's,
/// DeepSeek's, and Kimi's, the last of which gateways use too. OpenAI's list
/// has none, and neither has any that Zhipu documents.
const WINDOW_FIELDS: [&str; 3] = ["max_input_tokens", "context_window", "context_length"];

/// Whether a model takes pictures, from each provider's documentation as of
/// November 2026: `Some(false)` where it says the model is text-only,
/// `None` where it does not say (a model typed in by hand, a custom service),
/// in which case a picture is sent and the provider decides.
pub fn vision(provider: &str, model: &str) -> Option<bool> {
    if matches!(self::provider(provider).map(|known| known.protocol), Some(Protocol::Cli(_))) {
        return Some(true);
    }
    match (kind_id(provider), model) {
        ("anthropic", model) if anthropic::MODELS.iter().any(|(id, _)| *id == model) => Some(true),
        ("openai", "gpt-6.2-astra" | "gpt-6.2-sol") => Some(true),
        ("deepseek", "deepseek-flash") => Some(true),
        ("deepseek", "deepseek-v4-pro") => Some(false),
        ("moonshot", "kimi-k3.1-pro" | "kimi-k3") => Some(true),
        ("zhipu", "glm-5.3-flash") => Some(true),
        ("zhipu", "glm-5.3") => Some(false),
        _ => None,
    }
}

/// Ask a provider which models it serves, as the provider orders them.
/// Every provider here has a list endpoint in the OpenAI shape,
/// `{"data": [{"id": ...}]}`; Anthropic's adds a display name and wants its
/// own headers, and some say how large each model's context window is. The
/// list holds whatever the key may use, which for some providers includes
/// models that are not for chat.
pub async fn list_models(provider_id: &str, base_url: &str, key: Option<&str>) -> Result<Vec<Listed>, String> {
    let known = provider(provider_id).ok_or_else(|| format!("there is no provider `{provider_id}`"))?;
    if let Protocol::Cli(kind) = known.protocol {
        return kind.list_models(known.models).await;
    }
    let key = key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .or_else(|| (known.id == "magpie").then_some("magpie"))
        .ok_or("no API key is saved for this provider")?;
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("this provider has no address yet".to_string());
    }
    let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).timeout(std::time::Duration::from_secs(30)).build().map_err(|error| error.to_string())?;
    let request = match known.protocol {
        Protocol::Anthropic => http.get(format!("{base}/v1/models?limit=1000")).header("x-api-key", key).header("anthropic-version", anthropic::API_VERSION),
        Protocol::Responses | Protocol::Chat(_) => http.get(format!("{base}/models")).bearer_auth(key),
        Protocol::Cli(_) => unreachable!("handled above"),
    };
    let response = request.send().await.map_err(|error| format!("cannot reach the provider: {error}"))?;
    if !response.status().is_success() {
        let (error, _) = crate::llm::http_failure(response).await;
        return Err(error.to_string());
    }
    let listed: Value = response.json().await.map_err(|error| format!("the provider's answer is not JSON: {error}"))?;
    let models: Vec<Listed> = listed["data"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|model| {
            let id = model["id"].as_str().filter(|id| !id.is_empty())?;
            let context_window = WINDOW_FIELDS.iter().find_map(|field| model[*field].as_u64()).filter(|tokens| *tokens > 0);
            Some(Listed { id: id.to_string(), name: model["display_name"].as_str().unwrap_or(id).to_string(), context_window })
        })
        .collect();
    if models.is_empty() { Err("the provider listed no models".to_string()) } else { Ok(models) }
}

/// Read a provider's `usage` object into [`Tokens`]. The providers differ in
/// field names and in whether the prompt count includes what was read from
/// cache: Anthropic's does not, the others' do. `None` when it holds no count.
pub fn tokens(provider_id: &str, usage: &Value) -> Option<Tokens> {
    let count = |value: &Value| value.as_u64().unwrap_or(0);
    let protocol = provider(provider_id).map_or(Protocol::Anthropic, |known| known.protocol);
    let found = match protocol {
        Protocol::Anthropic => {
            let cached = count(&usage["cache_read_input_tokens"]);
            Tokens { input: count(&usage["input_tokens"]) + cached + count(&usage["cache_creation_input_tokens"]), cached, output: count(&usage["output_tokens"]) }
        }
        Protocol::Responses => {
            Tokens { input: count(&usage["input_tokens"]), cached: count(&usage["input_tokens_details"]["cached_tokens"]), output: count(&usage["output_tokens"]) }
        }
        Protocol::Chat(_) => Tokens {
            input: count(&usage["prompt_tokens"]),
            // DeepSeek reports the cache hit on its own field as well.
            cached: count(&usage["prompt_tokens_details"]["cached_tokens"]).max(count(&usage["prompt_cache_hit_tokens"])),
            output: count(&usage["completion_tokens"]),
        },
        Protocol::Cli(_) => {
            let cached = count(&usage["cache_read_tokens"]).max(count(&usage["cache_read_input_tokens"])).max(count(&usage["cached_input_tokens"]));
            Tokens { input: count(&usage["input_tokens"]) + count(&usage["cache_read_tokens"]) + count(&usage["cache_read_input_tokens"]), cached, output: count(&usage["output_tokens"]) }
        }
    };
    (found.input > 0 || found.output > 0).then_some(found)
}

/// Sends each request to the client for its session's provider.
pub struct Router {
    anthropic: AnthropicClient,
    chat: ChatClient,
    responses: ResponsesClient,
    endpoints: EndpointSource,
    /// A client for each Anthropic provider the user added, by id, with the address it was made for.
    added: Mutex<HashMap<String, (String, Arc<AnthropicClient>)>>,
}

impl Router {
    /// `anthropic` is built on its own: its address comes from `ANTHROPIC_BASE_URL`.
    /// `endpoints` gives every other provider's address and key per request,
    /// Anthropic ones the user added among them.
    pub fn new(anthropic: AnthropicClient, endpoints: EndpointSource) -> Result<Self, LlmError> {
        Ok(Self { anthropic, chat: ChatClient::new(endpoints.clone())?, responses: ResponsesClient::new(endpoints.clone())?, endpoints, added: Mutex::default() })
    }

    /// The client for an Anthropic provider other than the first, made again when its address changed.
    fn added_anthropic(&self, id: &str) -> Result<Arc<AnthropicClient>, LlmError> {
        let base_url = (self.endpoints)(id).base_url;
        let mut added = self.added.lock().unwrap();
        if let Some((made_for, client)) = added.get(id)
            && *made_for == base_url
        {
            return Ok(client.clone());
        }
        let (endpoints, key_of) = (self.endpoints.clone(), id.to_string());
        let client = Arc::new(AnthropicClient::new(base_url.clone(), Arc::new(move || endpoints(&key_of).key))?);
        added.insert(id.to_string(), (base_url, client.clone()));
        Ok(client)
    }
}

impl Llm for Router {
    async fn stream(&self, request: LlmRequest, deltas: UnboundedSender<StreamDelta>, cancel: CancellationToken) -> Result<AssistantTurn, LlmError> {
        let Some(provider) = provider(&request.provider) else {
            return Err(LlmError::Protocol(format!("this build does not know the provider `{}`", request.provider)));
        };
        match provider.protocol {
            // Any but the first Anthropic provider, whose address comes from the environment, has an address of its own.
            Protocol::Anthropic if request.provider != DEFAULT_PROVIDER => {
                let client = self.added_anthropic(&request.provider)?;
                client.stream(request, deltas, cancel).await
            }
            Protocol::Anthropic => self.anthropic.stream(request, deltas, cancel).await,
            Protocol::Responses => self.responses.stream(request, deltas, cancel).await,
            Protocol::Chat(flavor) => self.chat.stream(flavor, request, deltas, cancel).await,
            Protocol::Cli(kind) => Err(LlmError::Protocol(format!("{} runs through its local CLI backend", kind.display_name()))),
        }
    }
}

/// Which of our five effort levels a provider receives, for providers that
/// accept fewer. `None` sends no effort at all.
pub fn effort_for(protocol: Protocol, effort: &str) -> Option<&'static str> {
    let level = |levels: [&'static str; 5]| {
        let index = crate::settings::EFFORTS.iter().position(|known| *known == effort).unwrap_or(2);
        levels[index]
    };
    match protocol {
        // These accept all five as they are.
        Protocol::Anthropic | Protocol::Responses | Protocol::Cli(CliKind::Antigravity | CliKind::ClaudeCode) => Some(level(["low", "medium", "high", "xhigh", "max"])),
        Protocol::Cli(CliKind::Codex) => Some(level(["low", "medium", "high", "xhigh", "xhigh"])),
        // DeepSeek takes none/low/high/max and itself maps medium and xhigh to high.
        Protocol::Chat(Flavor::DeepSeek) => Some(level(["low", "high", "high", "high", "max"])),
        // Kimi K3 and GLM-5.3 take low/high/max.
        Protocol::Chat(Flavor::Kimi | Flavor::Glm) => Some(level(["low", "high", "high", "max", "max"])),
        Protocol::Chat(Flavor::Generic) | Protocol::Cli(CliKind::Cursor | CliKind::Grok | CliKind::OpenCode) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_is_reachable_and_has_a_default_model_or_asks_for_one() {
        for provider in PROVIDERS {
            assert_eq!(super::provider(provider.id).map(|found| found.id), Some(provider.id));
            // Only custom services and local CLI backends have no HTTP base URL; custom services, local gateways (Magpie),
            // and dynamic CLIs (Antigravity, OpenCode) fetch models dynamically instead of keeping a stale static list.
            let custom = provider.id.starts_with("custom");
            let cli = matches!(provider.protocol, Protocol::Cli(_));
            assert_eq!(provider.base_url.is_empty(), custom || cli, "{}", provider.id);
            assert_eq!(provider.models.is_empty(), custom || matches!(provider.id, "magpie" | "antigravity" | "opencode"), "{}", provider.id);
        }
        assert_eq!(provider(DEFAULT_PROVIDER).unwrap().protocol, Protocol::Anthropic);
        assert_eq!(provider("custom_anthropic").unwrap().protocol, Protocol::Anthropic);
        assert!(provider("nobody").is_none());
    }

    #[test]
    fn names_another_provider_of_a_kind_by_its_number() {
        // `<kind>-<n>` from 2 is another of the kind; anything else is not, nor is one kind taken for another.
        for (id, kind) in [("zhipu-2", "zhipu"), ("custom-17", "custom"), ("custom_anthropic-3", "custom_anthropic"), ("anthropic-2", "anthropic")] {
            assert_eq!(provider(id).map(|found| found.id), Some(kind), "{id}");
            assert!(is_added(id) && kind_id(id) == kind, "{id}");
        }
        for id in ["zhipu-1", "zhipu-02", "zhipu-x", "zhipu-", "zhipu2", "custom_anthropic_2"] {
            assert!(provider(id).is_none(), "{id}");
        }
        assert!(!is_added("zhipu") && kind_id("nobody") == "nobody");
        // What is known of a kind's models holds for each of it.
        assert_eq!(documented_window("zhipu-2", "glm-5.3"), documented_window("zhipu", "glm-5.3"));
        assert_eq!(vision("deepseek-2", "deepseek-v4-pro"), Some(false));
        let added = instance("openai-2", Some("  公司账号 ")).unwrap();
        assert_eq!((added.kind, added.name.as_str(), added.key_env), ("openai", "公司账号", None));
        let first = instance("openai", None).unwrap();
        assert_eq!((first.name.as_str(), first.key_env), ("OpenAI", Some("OPENAI_API_KEY")));
    }

    #[test]
    fn reads_each_providers_usage_into_the_same_counts() {
        use serde_json::json;
        // Anthropic counts what it read from and wrote to the cache beside the prompt, not inside it.
        let anthropic = json!({ "input_tokens": 10, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 90, "output_tokens": 40 });
        assert_eq!(tokens("anthropic", &anthropic), Some(Tokens { input: 1000, cached: 900, output: 40 }));
        let chat = json!({ "prompt_tokens": 1000, "completion_tokens": 40, "prompt_tokens_details": { "cached_tokens": 900 } });
        assert_eq!(tokens("zhipu", &chat), Some(Tokens { input: 1000, cached: 900, output: 40 }));
        let deepseek = json!({ "prompt_tokens": 1000, "completion_tokens": 40, "prompt_cache_hit_tokens": 900 });
        assert_eq!(tokens("deepseek", &deepseek), Some(Tokens { input: 1000, cached: 900, output: 40 }));
        let responses = json!({ "input_tokens": 1000, "output_tokens": 40, "input_tokens_details": { "cached_tokens": 900 } });
        assert_eq!(tokens("openai", &responses), Some(Tokens { input: 1000, cached: 900, output: 40 }));
        assert_eq!(tokens("zhipu", &json!({})), None);

        assert_eq!((vision("zhipu", "glm-5.3"), vision("zhipu", "glm-5.3-flash"), vision("zhipu", "glm-5.2")), (Some(false), Some(true), None));
        assert_eq!((vision("anthropic", "claude-opus-5-5"), vision("deepseek", "deepseek-v4-pro"), vision("custom", "x")), (Some(true), Some(false), None));
        assert_eq!(context_window("zhipu", "glm-4.7"), 200_000);
        assert_eq!(context_window("anthropic", "claude-opus-5-5"), 1_000_000);
        assert_eq!(context_window("zhipu", "glm-9"), UNKNOWN_CONTEXT_WINDOW);
        assert_eq!(context_window("custom", "anything"), UNKNOWN_CONTEXT_WINDOW);
        assert_eq!((documented_window("zhipu", "glm-4.7"), documented_window("zhipu", "glm-5.3-flash"), documented_window("zhipu", "glm-9")), (Some(200_000), Some(1_000_000), None));
    }

    #[tokio::test]
    async fn asks_each_kind_of_provider_for_its_models() {
        use crate::testing::{error, serve};
        use serde_json::json;
        let listing = |body: Value| {
            let body = body.to_string();
            format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
        };
        let (address, requests) = serve(vec![
            listing(json!({ "data": [{ "id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "max_input_tokens": 1_000_000, "max_tokens": 128_000 }, { "id": "claude-next", "max_input_tokens": null }], "has_more": false })),
            listing(json!({ "object": "list", "data": [{ "id": "glm-5.3", "object": "model" }, { "id": "glm-5.3-flash" }, { "object": "model" }] })),
            // Each provider's own field for the window: DeepSeek's, then Kimi's. Nothing is read from a count of zero.
            listing(json!({ "data": [{ "id": "deepseek-v4-pro", "context_window": 1_048_576, "max_output_tokens": 393_216 }, { "id": "deepseek-flash", "context_window": 0 }] })),
            listing(json!({ "data": [{ "id": "kimi-k3", "context_length": 262_144, "supports_image_in": true }] })),
            error(401, r#"{"error":{"message":"bad key","type":"authentication_error"}}"#),
            listing(json!({ "data": [] })),
        ])
        .await;
        let named = |id: &str, name: &str, context_window: Option<u64>| Listed { id: id.to_string(), name: name.to_string(), context_window };
        assert_eq!(list_models("anthropic", &address, Some("sk-ant")).await.unwrap(), [named("claude-opus-5-5", "Claude Opus 5.5", Some(1_000_000)), named("claude-next", "claude-next", None)]);
        assert_eq!(list_models("zhipu", &format!("{address}/api/paas/v4/"), Some(" glm-key ")).await.unwrap(), [named("glm-5.3", "glm-5.3", None), named("glm-5.3-flash", "glm-5.3-flash", None)]);
        assert_eq!(list_models("deepseek", &address, Some("k")).await.unwrap(), [named("deepseek-v4-pro", "deepseek-v4-pro", Some(1_048_576)), named("deepseek-flash", "deepseek-flash", None)]);
        assert_eq!(list_models("moonshot", &address, Some("k")).await.unwrap(), [named("kimi-k3", "kimi-k3", Some(262_144))]);
        assert_eq!(list_models("deepseek", &address, Some("k")).await.unwrap_err(), "authentication_error: bad key");
        assert_eq!(list_models("openai", &address, Some("k")).await.unwrap_err(), "the provider listed no models");
        // What cannot be asked is said without a request.
        assert!(list_models("zhipu", &address, None).await.unwrap_err().contains("no API key"));
        assert!(list_models("custom", "", Some("k")).await.unwrap_err().contains("no address"));
        assert!(list_models("nobody", &address, Some("k")).await.is_err());

        let requests = requests.await.unwrap();
        let lower: Vec<String> = requests.iter().map(|request| request.to_lowercase()).collect();
        assert!(lower[0].starts_with("get /v1/models?limit=1000 ") && lower[0].contains("x-api-key: sk-ant") && lower[0].contains("anthropic-version:"));
        assert!(lower[1].starts_with("get /api/paas/v4/models ") && lower[1].contains("authorization: bearer glm-key\r\n"), "{}", lower[1]);
    }

    #[test]
    fn maps_effort_to_the_levels_each_provider_accepts() {
        let all = |protocol| crate::settings::EFFORTS.iter().map(|effort| effort_for(protocol, effort)).collect::<Vec<_>>();
        assert_eq!(all(Protocol::Responses), [Some("low"), Some("medium"), Some("high"), Some("xhigh"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::DeepSeek)), [Some("low"), Some("high"), Some("high"), Some("high"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::Kimi)), [Some("low"), Some("high"), Some("high"), Some("max"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::Generic)), [None; 5]);
    }
}
