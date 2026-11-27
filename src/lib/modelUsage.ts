import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "./api";
import type { AntigravityQuota, ModelUsageEntry, QuotaGroup } from "./types";

/** Milliseconds remaining in an entry's cooldown, or 0 if not cooling down. */
export function cooldownRemainingMs(entry: Pick<ModelUsageEntry, "cooldown_until"> | undefined, nowMs: number): number {
  if (!entry?.cooldown_until) return 0;
  const until = Date.parse(entry.cooldown_until);
  if (!Number.isFinite(until) || until <= nowMs) return 0;
  return until - nowMs;
}

/** Human-readable compact countdown string: `45s`, `12m 30s`, or `2h 15m`. */
export function formatCooldown(ms: number): string {
  const totalSec = Math.max(1, Math.ceil(ms / 1000));
  const hours = Math.floor(totalSec / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (hours > 0) return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  if (mins > 0) return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`;
  return `${secs}s`;
}

/** Find the usage record matching `(provider, model)`. */
export function findModelUsage(entries: ModelUsageEntry[], provider: string, model: string): ModelUsageEntry | undefined {
  return entries.find((e) => e.provider === provider && e.model === model);
}

/** Total tokens (input + output) for today on one entry. */
export function todayTokens(entry: ModelUsageEntry): number {
  return entry.today_input_tokens + entry.today_output_tokens;
}

/** Total all-time tokens (input + output) on one entry. */
export function allTokens(entry: ModelUsageEntry): number {
  return entry.input_tokens + entry.output_tokens;
}

/**
 * Subscribe to persisted per-model usage and live cooldown countdowns.
 * Ticks once per second ONLY while at least one model has an active cooldown.
 */
export function useModelUsage(refreshKey: string) {
  const [entries, setEntries] = useState<ModelUsageEntry[]>([]);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const refresh = useCallback(() => {
    api
      .getModelUsage()
      .then((list) => {
        setEntries(list);
        setNowMs(Date.now());
      })
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh, refreshKey]);

  const hasActiveCooldown = useMemo(() => entries.some((e) => cooldownRemainingMs(e, nowMs) > 0), [entries, nowMs]);

  useEffect(() => {
    if (!hasActiveCooldown) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [hasActiveCooldown]);

  const clearCooldown = useCallback((provider: string, model: string) => {
    api
      .clearModelCooldown(provider, model)
      .then((list) => {
        setEntries(list);
        setNowMs(Date.now());
      })
      .catch(() => undefined);
  }, []);

  const resetAll = useCallback(() => {
    api
      .resetModelUsage()
      .then((list) => {
        setEntries(list);
        setNowMs(Date.now());
      })
      .catch(() => undefined);
  }, []);

  return { entries, nowMs, refresh, clearCooldown, resetAll };
}

/** Human-readable countdown until a quota bucket's RFC3339 `reset_time` (e.g. `5天 22小时` or `3小时 11分`). */
export function formatResetIn(resetTime: string | null | undefined, nowMs: number): string | null {
  if (!resetTime) return null;
  const target = Date.parse(resetTime);
  if (!Number.isFinite(target) || target <= nowMs) return null;
  const totalMin = Math.max(1, Math.ceil((target - nowMs) / 60_000));
  const days = Math.floor(totalMin / 1440);
  const hours = Math.floor((totalMin % 1440) / 60);
  const mins = totalMin % 60;
  if (days > 0) return hours > 0 ? `${days}天 ${hours}小时` : `${days}天`;
  if (hours > 0) return mins > 0 ? `${hours}小时 ${mins}分` : `${hours}小时`;
  return `${mins}分钟`;
}

/** Pick the quota group matching `model` (supports both Antigravity CLI groups and Magpie gateway provider/pool groups). */
export function quotaGroupForModel(quota: AntigravityQuota | null, model: string): QuotaGroup | undefined {
  if (!quota || quota.groups.length === 0) return undefined;
  const lower = model.toLowerCase();
  const slash = lower.indexOf("/");
  const prefix = slash > 0 ? lower.slice(0, slash) : "";
  const bare = slash > 0 ? lower.slice(slash + 1) : lower;

  const isGemini = bare.includes("gemini");
  const isClaude = bare.includes("claude");
  const isGpt = bare.includes("gpt") || bare.includes("o1") || bare.includes("o3") || bare.includes("o4") || bare.includes("codex");

  const haystack = (g: QuotaGroup) => `${g.name} ${g.description ?? ""}`.toLowerCase();

  // 1. If the model carries a Magpie provider prefix (e.g. `agy-cli/...`, `codex/...`, `claude/...`), narrow to matching cards first.
  const prefixTokens: string[] = [];
  if (prefix) {
    prefixTokens.push(prefix);
    if (prefix === "agy-cli" || prefix === "agy") prefixTokens.push("antigravity");
    if (prefix === "zhipu" || prefix === "glm") prefixTokens.push("glm", "zhipu");
  }
  const scoped =
    prefixTokens.length > 0
      ? quota.groups.filter((g) => {
          const text = haystack(g);
          return prefixTokens.some((tok) => text.includes(tok));
        })
      : [];
  const candidates = scoped.length > 0 ? scoped : quota.groups;

  // 2. Match model family within the candidate groups (e.g. Gemini vs Claude/GPT pools).
  if (isClaude || isGpt) {
    const matched = candidates.find((g) => {
      const text = haystack(g);
      return (isClaude && text.includes("claude")) || (isGpt && (text.includes("gpt") || text.includes("codex")));
    });
    if (matched) return matched;
  }
  if (isGemini) {
    const matched = candidates.find((g) => haystack(g).includes("gemini"));
    if (matched) return matched;
  }

  return candidates[0];
}

/** Friendly Chinese label for a quota window (`5h` / `weekly`). */
export function quotaWindowLabel(window: string, fallbackName: string): string {
  if (window === "5h") return "5 小时限额";
  if (window === "weekly") return "周限额";
  return fallbackName || window;
}

export type QuotaSource =
  | { kind: "antigravity" }
  | { kind: "magpie"; providerId: string }
  | { kind: "kimi"; providerId: string }
  | null;

/** Subscribe to live subscription quota from local `agy -p /usage`, a Magpie gateway, or Kimi Code (`/coding/v1/usages`). */
export function useProviderQuota(source: QuotaSource) {
  const [quota, setQuota] = useState<AntigravityQuota | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const kind = source?.kind ?? null;
  const providerId = source && "providerId" in source ? source.providerId : "";

  const refresh = useCallback(
    (force = false) => {
      if (!kind) return;
      setLoading(true);
      setError(null);
      const req =
        kind === "magpie"
          ? api.getMagpieQuota(providerId, force)
          : kind === "kimi"
            ? api.getKimiQuota(providerId, force)
            : api.getAntigravityQuota(force);
      req
        .then((res) => {
          setQuota(res);
          setNowMs(Date.now());
        })
        .catch((err) => {
          setQuota(null);
          setError(String(err));
        })
        .finally(() => setLoading(false));
    },
    [kind, providerId],
  );

  useEffect(() => {
    if (!kind) {
      setQuota(null);
      setError(null);
      return;
    }
    refresh(false);
  }, [kind, providerId, refresh]);

  useEffect(() => {
    if (!quota) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, [quota]);

  return { quota, loading, error, nowMs, refresh };
}

/** Subscribe to live Antigravity subscription quota via `agy -p /usage`. */
export function useAntigravityQuota(enabled: boolean) {
  return useProviderQuota(enabled ? { kind: "antigravity" } : null);
}

/** Subscribe to live Kimi Code Coding Plan quota via `GET /coding/v1/usages`. */
export function useKimiQuota(providerId: string | null) {
  return useProviderQuota(providerId ? { kind: "kimi", providerId } : null);
}
