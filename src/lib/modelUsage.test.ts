import { describe, expect, it } from "vitest";
import { cooldownRemainingMs, findModelUsage, formatCooldown, quotaGroupForModel } from "./modelUsage";
import type { AntigravityQuota, ModelUsageEntry } from "./types";

describe("modelUsage helpers", () => {
  it("formats cooldown durations cleanly across seconds, minutes, and hours", () => {
    expect(formatCooldown(500)).toBe("1s");
    expect(formatCooldown(45_000)).toBe("45s");
    expect(formatCooldown(12 * 60_000 + 30_000)).toBe("12m 30s");
    expect(formatCooldown(5 * 60_000)).toBe("5m");
    expect(formatCooldown(2 * 3_600_000 + 15 * 60_000)).toBe("2h 15m");
    expect(formatCooldown(3 * 3_600_000)).toBe("3h");
  });

  it("computes remaining cooldown milliseconds and ignores expired timestamps", () => {
    const now = Date.parse("2026-11-25T09:00:00Z");
    expect(cooldownRemainingMs({ cooldown_until: "2026-11-25T09:02:30Z" }, now)).toBe(150_000);
    expect(cooldownRemainingMs({ cooldown_until: "2026-11-25T08:59:59Z" }, now)).toBe(0);
    expect(cooldownRemainingMs({ cooldown_until: null }, now)).toBe(0);
  });

  it("finds matching provider/model usage entry", () => {
    const entries: ModelUsageEntry[] = [
      {
        provider: "antigravity",
        model: "gemini-4-neon",
        requests: 5,
        input_tokens: 10000,
        cached_tokens: 4000,
        output_tokens: 2000,
        today_key: "2026-11-25",
        today_requests: 5,
        today_input_tokens: 10000,
        today_cached_tokens: 4000,
        today_output_tokens: 2000,
      },
    ];
    expect(findModelUsage(entries, "antigravity", "gemini-4-neon")?.requests).toBe(5);
    expect(findModelUsage(entries, "openai", "gemini-4-neon")).toBeUndefined();
  });

  it("matches Magpie gateway quota groups by provider prefix and model family", () => {
    const quota: AntigravityQuota = {
      fetched_at: "2026-11-25T11:45:00Z",
      groups: [
        { name: "Antigravity · Gemini Models", description: "agy-cli", buckets: [] },
        { name: "Antigravity · Claude and GPT models", description: "agy-cli", buckets: [] },
        { name: "codex (work)", description: "codex", buckets: [] },
      ],
    };
    expect(quotaGroupForModel(quota, "agy-cli/gemini-4-hydrogen")?.name).toBe("Antigravity · Gemini Models");
    expect(quotaGroupForModel(quota, "agy-cli/claude-sonnet-4-6")?.name).toBe("Antigravity · Claude and GPT models");
    expect(quotaGroupForModel(quota, "codex/gpt-5.4")?.name).toBe("codex (work)");
  });
});
