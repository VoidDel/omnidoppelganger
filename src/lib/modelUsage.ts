import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "./api";
import type { ModelUsageEntry } from "./types";

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
