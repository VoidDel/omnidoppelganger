//! Per-million-token USD rate table for estimating session and model spend.

use crate::llm::Tokens;

/// USD rates per 1,000,000 tokens: `(uncached_input, cached_input, output)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRate {
    pub input_per_m: f64,
    pub cached_per_m: f64,
    pub output_per_m: f64,
}

/// Look up approximate public API pricing (USD per 1M tokens) for a model id.
/// Returns `None` for local CLI / local gateway models (`ollama`, `magpie`, `antigravity`, etc.)
/// or unrecognized custom models.
pub fn rate_for(provider: &str, model: &str) -> Option<ModelRate> {
    let p = provider.to_ascii_lowercase();
    if matches!(
        p.as_str(),
        "ollama" | "magpie" | "antigravity" | "codex" | "claude_code" | "cursor" | "grok" | "opencode"
    ) || p.starts_with("ollama-")
        || p.starts_with("magpie-")
    {
        return None;
    }
    let m = model.trim().to_ascii_lowercase();
    let m = m.rsplit_once('/').map_or(m.as_str(), |(_, tail)| tail);

    let (inp, cached, out) = if m.starts_with("claude-opus") || m.starts_with("claude-fable") {
        (5.0, 0.50, 25.0)
    } else if m.starts_with("claude-sonnet") {
        (3.0, 0.30, 15.0)
    } else if m.starts_with("claude-haiku") {
        (1.0, 0.10, 5.0)
    } else if m.starts_with("gpt-6.2-astra") || m.starts_with("gpt-5.4") || m.starts_with("gpt-5") {
        (2.50, 0.25, 10.0)
    } else if m.starts_with("gpt-6.2-sol") || m.starts_with("gpt-4.1-mini") || m.starts_with("o4-mini") {
        (0.60, 0.15, 2.40)
    } else if m.starts_with("gemini-2.5-pro") || m.starts_with("gemini-3-pro") {
        (1.25, 0.31, 10.0)
    } else if m.starts_with("gemini-2.5-flash") || m.starts_with("gemini-3-flash") || m.starts_with("gemini-2.0-flash") {
        (0.30, 0.075, 2.50)
    } else if m.starts_with("deepseek-v4") || m.starts_with("deepseek-chat") || m.starts_with("deepseek-reasoner") {
        (0.28, 0.028, 1.10)
    } else if m.starts_with("deepseek-flash") {
        (0.14, 0.014, 0.56)
    } else if m.starts_with("kimi-k3") {
        (0.60, 0.15, 2.50)
    } else if m.starts_with("glm-5.3-flash") || m.contains("flash") {
        (0.15, 0.03, 0.60)
    } else if m.starts_with("glm-5") || m.starts_with("glm-4") {
        (0.70, 0.14, 2.80)
    } else {
        return None;
    };

    Some(ModelRate {
        input_per_m: inp,
        cached_per_m: cached,
        output_per_m: out,
    })
}

/// Estimate USD cost for `tokens` (`tokens.input` includes `tokens.cached`).
pub fn estimate_usd(provider: &str, model: &str, tokens: Tokens) -> Option<f64> {
    let rate = rate_for(provider, model)?;
    let uncached = tokens.input.saturating_sub(tokens.cached) as f64;
    let cached = tokens.cached.min(tokens.input) as f64;
    let output = tokens.output as f64;
    let usd = (uncached * rate.input_per_m + cached * rate.cached_per_m + output * rate.output_per_m)
        / 1_000_000.0;
    Some(usd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_usd_with_cache_discount_and_skips_local_providers() {
        let tok = Tokens {
            input: 1_000_000,
            cached: 800_000,
            output: 100_000,
        };
        // Sonnet: 200k * $3/M ($0.60) + 800k * $0.30/M ($0.24) + 100k * $15/M ($1.50) = $2.34
        let sonnet = estimate_usd("anthropic", "claude-sonnet-5-5", tok).unwrap();
        assert!((sonnet - 2.34).abs() < 1e-6, "got {sonnet}");

        // OpenRouter prefixed model resolves by model slug
        let gem = estimate_usd("openrouter", "google/gemini-2.5-pro", tok).unwrap();
        assert!(gem > 0.0);

        // Local Ollama & CLI backends report no API bill
        assert!(estimate_usd("ollama", "qwen3:32b", tok).is_none());
        assert!(estimate_usd("claude_code", "claude-sonnet-4-6", tok).is_none());
    }
}
