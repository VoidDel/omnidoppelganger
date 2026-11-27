import type { Currency, PriceCatalogView, PriceRate, PriceSource, Tokens } from "./types";

export interface ModelRate {
  currency: Currency;
  inputPerM: number;
  cachedPerM: number;
  cacheWritePerM: number;
  outputPerM: number;
  source: PriceSource;
}

const LOCAL_OR_CLI = new Set(["ollama", "magpie", "antigravity", "codex", "claude_code", "cursor", "grok", "opencode"]);
let usdToCny = 7.2;
const hydratedByKey = new Map<string, ModelRate>();
const hydratedByModel = new Map<string, ModelRate>();

function fromPriceRate(rate: PriceRate): ModelRate {
  return {
    currency: rate.currency ?? "USD",
    inputPerM: rate.input_per_m,
    cachedPerM: rate.cached_per_m,
    cacheWritePerM: rate.cache_write_per_m ?? rate.input_per_m,
    outputPerM: rate.output_per_m,
    source: rate.source ?? "builtin",
  };
}

/** Hydrate the frontend pricing lookup from the Rust single-source-of-truth catalog. */
export function hydratePriceCatalog(view: PriceCatalogView | null | undefined): void {
  if (!view) return;
  if (Number.isFinite(view.usd_to_cny) && view.usd_to_cny > 0) {
    usdToCny = view.usd_to_cny;
  }
  hydratedByKey.clear();
  hydratedByModel.clear();
  for (const entry of view.effective) {
    const mapped = fromPriceRate(entry.rate);
    const key = entry.key.trim().toLowerCase();
    const model = entry.model.trim().toLowerCase();
    hydratedByKey.set(key, mapped);
    // Custom entries win over non-custom entries for bare model fallback.
    const existing = hydratedByModel.get(model);
    if (!existing || mapped.source === "custom") {
      hydratedByModel.set(model, mapped);
    }
  }
  for (const entry of view.custom) {
    const mapped = fromPriceRate(entry.rate);
    hydratedByKey.set(entry.key.trim().toLowerCase(), mapped);
    hydratedByModel.set(entry.model.trim().toLowerCase(), mapped);
  }
}

function builtinFallbackRate(slug: string): ModelRate | null {
  let currency: Currency = "USD";
  let tuple: [number, number, number, number] | null = null;
  if (slug.startsWith("claude-opus") || slug.startsWith("claude-fable")) tuple = [5.0, 0.5, 6.25, 25.0];
  else if (slug.startsWith("claude-sonnet")) tuple = [3.0, 0.3, 3.75, 15.0];
  else if (slug.startsWith("claude-haiku")) tuple = [1.0, 0.1, 1.25, 5.0];
  else if (slug.startsWith("gpt-6.2-astra") || slug.startsWith("gpt-5")) tuple = [2.5, 0.25, 2.5, 10.0];
  else if (slug.startsWith("gpt-6.2-sol") || slug.startsWith("gpt-4.1-mini") || slug.startsWith("o4-mini")) tuple = [0.6, 0.15, 0.6, 2.4];
  else if (slug.startsWith("gemini-2.5-pro") || slug.startsWith("gemini-3-pro")) tuple = [1.25, 0.31, 1.25, 10.0];
  else if (slug.startsWith("gemini-2.5-flash") || slug.startsWith("gemini-3-flash") || slug.startsWith("gemini-2.0-flash")) tuple = [0.3, 0.075, 0.3, 2.5];
  else if (slug.startsWith("deepseek-v4") || slug.startsWith("deepseek-chat") || slug.startsWith("deepseek-reasoner")) tuple = [0.28, 0.028, 0.28, 1.1];
  else if (slug.startsWith("deepseek-flash")) tuple = [0.14, 0.014, 0.14, 0.56];
  else if (slug.startsWith("kimi-k3.1-pro-highspeed")) {
    currency = "CNY";
    tuple = [26.0, 1.3, 26.0, 136.0];
  } else if (slug.startsWith("kimi-k3.1-pro")) {
    currency = "CNY";
    tuple = [13.0, 1.3, 13.0, 68.0];
  } else if (slug.startsWith("kimi-k3")) {
    currency = "CNY";
    tuple = [20.0, 2.0, 20.0, 100.0];
  } else if (slug.startsWith("kimi-k2")) {
    currency = "CNY";
    tuple = [6.5, 1.3, 6.5, 27.0];
  } else if (slug.startsWith("glm-5.3-flash") || slug.includes("flash")) tuple = [0.15, 0.03, 0.15, 0.6];
  else if (slug.startsWith("glm-5") || slug.startsWith("glm-4")) tuple = [0.7, 0.14, 0.7, 2.8];

  if (!tuple) return null;
  return {
    currency,
    inputPerM: tuple[0],
    cachedPerM: tuple[1],
    cacheWritePerM: tuple[2],
    outputPerM: tuple[3],
    source: "builtin",
  };
}

/** Look up public API pricing for a provider + model pair, or null for local/CLI/unknown models. */
export function rateForModel(provider: string, model: string): ModelRate | null {
  const p = provider.trim().toLowerCase();
  const baseKind = p.replace(/-\d+$/, "");
  if (LOCAL_OR_CLI.has(baseKind)) return null;

  const raw = model.trim().toLowerCase();
  const slug = raw.includes("/") ? raw.slice(raw.lastIndexOf("/") + 1) : raw;
  if (!slug || slug === "auto") return null;

  const hydrated =
    hydratedByKey.get(`${p}/${raw}`) ??
    hydratedByKey.get(`${baseKind}/${raw}`) ??
    hydratedByKey.get(`${p}/${slug}`) ??
    hydratedByKey.get(`${baseKind}/${slug}`) ??
    hydratedByKey.get(raw) ??
    hydratedByKey.get(slug) ??
    hydratedByModel.get(slug);
  if (hydrated) return hydrated;

  return builtinFallbackRate(slug);
}

/** Estimate USD-equivalent cost for `tokens` (`tokens.input` includes `tokens.cached` and `tokens.cache_write`). */
export function estimateTokensUsd(provider: string, model: string, tokens: Tokens): number | null {
  const rate = rateForModel(provider, model);
  if (!rate) return null;
  const cached = Math.min(tokens.input, Math.max(0, tokens.cached));
  const remAfterRead = Math.max(0, tokens.input - cached);
  const cacheWrite = Math.min(remAfterRead, Math.max(0, tokens.cache_write ?? 0));
  const uncached = Math.max(0, remAfterRead - cacheWrite);
  const output = Math.max(0, tokens.output);
  const native =
    (uncached * rate.inputPerM + cached * rate.cachedPerM + cacheWrite * rate.cacheWritePerM + output * rate.outputPerM) / 1_000_000;
  return rate.currency === "CNY" ? native / usdToCny : native;
}

/** Format an amount in its native currency (`USD` or `CNY`) compactly for UI badges. */
export function formatMoney(amount: number, currency: Currency = "USD"): string {
  const sym = currency === "CNY" ? "¥" : "$";
  if (!Number.isFinite(amount) || amount <= 0) return `${sym}0.00`;
  if (amount < 0.01) return `${sym}${amount.toFixed(4)}`;
  if (amount < 1) return `${sym}${amount.toFixed(3)}`;
  return `${sym}${amount.toFixed(2)}`;
}

/** Format an estimated USD amount compactly for UI badges. */
export function formatUsd(usd: number): string {
  return formatMoney(usd, "USD");
}
