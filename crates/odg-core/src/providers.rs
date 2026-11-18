//! The model providers a session can run on, and the [`Router`] that sends
//! each request to the client speaking its provider's protocol.
//!
//! A session keeps the provider it was created with, so its log only ever
//! holds one provider's content blocks and nothing is converted between them.

use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::anthropic::{self, AnthropicClient};
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
        name: "其他 OpenAI 兼容服务",
        protocol: Protocol::Chat(Flavor::Generic),
        base_url: "",
        other_urls: &[],
        key_env: None,
        models: &[],
    },
];

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.id == id)
}

/// The context window assumed for a model no provider here documents.
pub const UNKNOWN_CONTEXT_WINDOW: u64 = 128_000;

/// How many tokens a model's context window holds, from each provider's
/// documentation as of November 2026. A model that is not listed (a newer
/// one typed in by hand, or anything on a custom service) gets
/// [`UNKNOWN_CONTEXT_WINDOW`], which is small enough to be safe.
pub fn context_window(provider: &str, model: &str) -> u64 {
    match (provider, model) {
        ("anthropic", model) if anthropic::MODELS.iter().any(|(id, _)| *id == model) => 1_000_000,
        ("openai", "gpt-6.2-astra" | "gpt-6.2-sol") => 1_050_000,
        ("deepseek", "deepseek-v4-pro" | "deepseek-flash") => 1_000_000,
        ("moonshot", "kimi-k3.1-pro" | "kimi-k3.1-pro-highspeed" | "kimi-k3") => 1_048_576,
        ("zhipu", "glm-5.3" | "glm-5.2") => 1_000_000,
        ("zhipu", "glm-4.7") => 200_000,
        _ => UNKNOWN_CONTEXT_WINDOW,
    }
}

/// Whether a model takes pictures, from each provider's documentation as of
/// November 2026: `Some(false)` where it says the model is text-only,
/// `None` where it does not say (a model typed in by hand, a custom service),
/// in which case a picture is sent and the provider decides.
pub fn vision(provider: &str, model: &str) -> Option<bool> {
    match (provider, model) {
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
    };
    (found.input > 0 || found.output > 0).then_some(found)
}

/// Sends each request to the client for its session's provider.
pub struct Router {
    anthropic: AnthropicClient,
    chat: ChatClient,
    responses: ResponsesClient,
}

impl Router {
    /// `anthropic` is built on its own: its address comes from `ANTHROPIC_BASE_URL`.
    /// `endpoints` gives every other provider's address and key per request.
    pub fn new(anthropic: AnthropicClient, endpoints: EndpointSource) -> Result<Self, LlmError> {
        Ok(Self { anthropic, chat: ChatClient::new(endpoints.clone())?, responses: ResponsesClient::new(endpoints)? })
    }
}

impl Llm for Router {
    async fn stream(&self, request: LlmRequest, deltas: UnboundedSender<StreamDelta>, cancel: CancellationToken) -> Result<AssistantTurn, LlmError> {
        let Some(provider) = provider(&request.provider) else {
            return Err(LlmError::Protocol(format!("this build does not know the provider `{}`", request.provider)));
        };
        match provider.protocol {
            Protocol::Anthropic => self.anthropic.stream(request, deltas, cancel).await,
            Protocol::Responses => self.responses.stream(request, deltas, cancel).await,
            Protocol::Chat(flavor) => self.chat.stream(flavor, request, deltas, cancel).await,
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
        Protocol::Anthropic | Protocol::Responses => Some(level(["low", "medium", "high", "xhigh", "max"])),
        // DeepSeek takes none/low/high/max and itself maps medium and xhigh to high.
        Protocol::Chat(Flavor::DeepSeek) => Some(level(["low", "high", "high", "high", "max"])),
        // Kimi K3 and GLM-5.3 take low/high/max.
        Protocol::Chat(Flavor::Kimi | Flavor::Glm) => Some(level(["low", "high", "high", "max", "max"])),
        Protocol::Chat(Flavor::Generic) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_is_reachable_and_has_a_default_model_or_asks_for_one() {
        for provider in PROVIDERS {
            assert_eq!(super::provider(provider.id).map(|found| found.id), Some(provider.id));
            assert_eq!(provider.base_url.is_empty(), provider.id == "custom", "{}", provider.id);
            assert_eq!(provider.models.is_empty(), provider.id == "custom", "{}", provider.id);
        }
        assert_eq!(provider(DEFAULT_PROVIDER).unwrap().protocol, Protocol::Anthropic);
        assert!(provider("nobody").is_none());
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
