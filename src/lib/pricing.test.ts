import { describe, expect, it } from "vitest";
import { estimateTokensUsd, formatMoney, formatUsd, hydratePriceCatalog } from "./pricing";

describe("pricing", () => {
  it("computes cache-aware USD cost and skips local/CLI providers", () => {
    const tokens = { input: 1_000_000, cached: 800_000, output: 100_000 };
    const sonnet = estimateTokensUsd("anthropic", "claude-sonnet-5-5", tokens);
    expect(sonnet).not.toBeNull();
    expect(sonnet!).toBeCloseTo(2.34, 5);
    expect(formatUsd(sonnet!)).toBe("$2.34");
    expect(formatUsd(0.0042)).toBe("$0.0042");
    expect(formatMoney(13.5, "CNY")).toBe("¥13.50");

    expect(estimateTokensUsd("openrouter", "google/gemini-2.5-flash", tokens)).toBeGreaterThan(0);
    expect(estimateTokensUsd("ollama", "llama3.3", tokens)).toBeNull();
    expect(estimateTokensUsd("codex", "gpt-5.4", tokens)).toBeNull();
  });

  it("accounts for cache_write tokens and hydrated custom overrides", () => {
    hydratePriceCatalog({
      synced_at: null,
      models_dev_count: 0,
      custom_count: 1,
      usd_to_cny: 7.2,
      custom: [
        {
          key: "moonshot/kimi-custom",
          provider: "moonshot",
          model: "kimi-custom",
          rate: {
            currency: "CNY",
            input_per_m: 14.4,
            cached_per_m: 1.44,
            cache_write_per_m: 14.4,
            output_per_m: 72.0,
            source: "custom",
          },
        },
      ],
      effective: [],
    });

    const cost = estimateTokensUsd("moonshot", "kimi-custom", {
      input: 1_000_000,
      cached: 500_000,
      cache_write: 200_000,
      output: 100_000,
    });
    // 300k * 14.4 + 500k * 1.44 + 200k * 14.4 + 100k * 72 = 4.32 + 0.72 + 2.88 + 7.20 = 15.12 CNY / 7.2 = $2.10
    expect(cost).not.toBeNull();
    expect(cost!).toBeCloseTo(2.1, 5);
  });
});
