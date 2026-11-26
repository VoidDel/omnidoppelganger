import type { Tokens } from "./types";

export interface ModelRate {
  inputPerM: number;
  cachedPerM: number;
  outputPerM: number;
}

const LOCAL_OR_CLI = new Set(["ollama", "magpie", "antigravity", "codex", "claude_code", "cursor", "grok", "opencode"]);

/** Look up public API pricing (USD per 1M tokens) for a provider + model pair, or null for local/CLI/unknown models. */
export function rateForModel(provider: string, model: string): ModelRate | null {
  const p = provider.trim().toLowerCase();
  const baseKind = p.replace(/-\d+$/, "");
  if (LOCAL_OR_CLI.has(baseKind)) return null;

  const raw = model.trim().toLowerCase();
  const slug = raw.includes("/") ? raw.slice(raw.lastIndexOf("/") + 1) : raw;
  if (!slug || slug === "auto") return null;

  let tuple: [number, number, number] | null = null;
  if (slug.startsWith("claude-opus") || slug.startsWith("claude-fable")) tuple = [5.0, 0.5, 25.0];
  else if (slug.startsWith("claude-sonnet")) tuple = [3.0, 0.3, 15.0];
  else if (slug.startsWith("claude-haiku")) tuple = [1.0, 0.1, 5.0];
  else if (slug.startsWith("gpt-6.2-astra") || slug.startsWith("gpt-5")) tuple = [2.5, 0.25, 10.0];
  else if (slug.startsWith("gpt-6.2-sol") || slug.startsWith("gpt-4.1-mini") || slug.startsWith("o4-mini")) tuple = [0.6, 0.15, 2.4];
  else if (slug.startsWith("gemini-2.5-pro") || slug.startsWith("gemini-3-pro")) tuple = [1.25, 0.31, 10.0];
  else if (slug.startsWith("gemini-2.5-flash") || slug.startsWith("gemini-3-flash") || slug.startsWith("gemini-2.0-flash")) tuple = [0.3, 0.075, 2.5];
  else if (slug.startsWith("deepseek-v4") || slug.startsWith("deepseek-chat") || slug.startsWith("deepseek-reasoner")) tuple = [0.28, 0.028, 1.1];
  else if (slug.startsWith("deepseek-flash")) tuple = [0.14, 0.014, 0.56];
  else if (slug.startsWith("kimi-k3")) tuple = [0.6, 0.15, 2.5];
  else if (slug.startsWith("glm-5.3-flash") || slug.includes("flash")) tuple = [0.15, 0.03, 0.6];
  else if (slug.startsWith("glm-5") || slug.startsWith("glm-4")) tuple = [0.7, 0.14, 2.8];

  if (!tuple) return null;
  return { inputPerM: tuple[0], cachedPerM: tuple[1], outputPerM: tuple[2] };
}

/** Estimate USD cost for `tokens` (`tokens.input` includes `tokens.cached`). */
export function estimateTokensUsd(provider: string, model: string, tokens: Tokens): number | null {
  const rate = rateForModel(provider, model);
  if (!rate) return null;
  const cached = Math.min(tokens.input, Math.max(0, tokens.cached));
  const uncached = Math.max(0, tokens.input - cached);
  const output = Math.max(0, tokens.output);
  return (uncached * rate.inputPerM + cached * rate.cachedPerM + output * rate.outputPerM) / 1_000_000;
}

/** Format an estimated USD amount compactly for UI badges. */
export function formatUsd(usd: number): string {
  if (usd <= 0) return "$0.00";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  if (usd < 1) return `$${usd.toFixed(3)}`;
  return `$${usd.toFixed(2)}`;
}
