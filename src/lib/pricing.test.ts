import { describe, expect, it } from "vitest";
import { estimateTokensUsd, formatUsd } from "./pricing";

describe("pricing", () => {
  it("computes cache-aware USD cost and skips local/CLI providers", () => {
    const tokens = { input: 1_000_000, cached: 800_000, output: 100_000 };
    const sonnet = estimateTokensUsd("anthropic", "claude-sonnet-5-5", tokens);
    expect(sonnet).not.toBeNull();
    expect(sonnet!).toBeCloseTo(2.34, 5);
    expect(formatUsd(sonnet!)).toBe("$2.34");
    expect(formatUsd(0.0042)).toBe("$0.0042");

    expect(estimateTokensUsd("openrouter", "google/gemini-2.5-flash", tokens)).toBeGreaterThan(0);
    expect(estimateTokensUsd("ollama", "llama3.3", tokens)).toBeNull();
    expect(estimateTokensUsd("codex", "gpt-5.4", tokens)).toBeNull();
  });
});
