//! The model providers a session can run on, and the [`Router`] that sends
//! each request to the client speaking its provider's protocol.
//!
//! A session keeps the provider it was created with, so its log only ever
//! holds one provider's content blocks and nothing is converted between them.

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::anthropic::{self, AnthropicClient};
use crate::llm::{AssistantTurn, EndpointSource, Llm, LlmError, LlmRequest, StreamDelta};
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
        models: &[("glm-5.3", "GLM-5.3"), ("glm-5.2", "GLM-5.2"), ("glm-4.7", "GLM-4.7")],
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
    fn maps_effort_to_the_levels_each_provider_accepts() {
        let all = |protocol| crate::settings::EFFORTS.iter().map(|effort| effort_for(protocol, effort)).collect::<Vec<_>>();
        assert_eq!(all(Protocol::Responses), [Some("low"), Some("medium"), Some("high"), Some("xhigh"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::DeepSeek)), [Some("low"), Some("high"), Some("high"), Some("high"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::Kimi)), [Some("low"), Some("high"), Some("high"), Some("max"), Some("max")]);
        assert_eq!(all(Protocol::Chat(Flavor::Generic)), [None; 5]);
    }
}
