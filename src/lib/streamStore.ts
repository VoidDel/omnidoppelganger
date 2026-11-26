import { useCallback, useSyncExternalStore } from "react";
import { applyDelta, type StreamBlock } from "./fold";
import type { StreamDelta } from "./types";

export const EMPTY_STREAM: StreamBlock[] = [];

type Listener = () => void;

export interface StreamStoreScheduler {
  requestFrame: (cb: FrameRequestCallback) => number;
  cancelFrame: (id: number) => void;
}

const defaultScheduler: StreamStoreScheduler = {
  requestFrame: (cb) =>
    typeof requestAnimationFrame === "function"
      ? requestAnimationFrame(cb)
      : (setTimeout(() => cb(Date.now()), 16) as unknown as number),
  cancelFrame: (id) => {
    if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(id);
    else clearTimeout(id);
  },
};

/** Coalesce adjacent same-index text/thinking/tool_input deltas in a burst queue before applying. */
export function coalesceDeltas(deltas: StreamDelta[]): StreamDelta[] {
  if (deltas.length <= 1) return deltas;
  const out: StreamDelta[] = [];
  for (const delta of deltas) {
    if (delta.type === "reset") {
      out.length = 0;
      out.push(delta);
      continue;
    }
    const prev = out[out.length - 1];
    if (prev && delta.type === "text_delta" && prev.type === "text_delta" && prev.index === delta.index) {
      out[out.length - 1] = { type: "text_delta", index: delta.index, text: prev.text + delta.text };
    } else if (prev && delta.type === "thinking_delta" && prev.type === "thinking_delta" && prev.index === delta.index) {
      out[out.length - 1] = { type: "thinking_delta", index: delta.index, text: prev.text + delta.text };
    } else if (prev && delta.type === "tool_input_delta" && prev.type === "tool_input_delta" && prev.index === delta.index) {
      out[out.length - 1] = { type: "tool_input_delta", index: delta.index, partial: prev.partial + delta.partial };
    } else {
      out.push(delta);
    }
  }
  return out;
}

export interface StreamStore {
  push: (sessionId: string, delta: StreamDelta) => void;
  clear: (sessionId: string) => void;
  clearAll: () => void;
  flush: () => void;
  getSnapshot: (sessionId: string | null) => StreamBlock[];
  subscribe: (sessionId: string | null, listener: Listener) => () => void;
}

export function createStreamStore(scheduler: StreamStoreScheduler = defaultScheduler): StreamStore {
  const snapshots = new Map<string, StreamBlock[]>();
  const queues = new Map<string, StreamDelta[]>();
  const listeners = new Map<string, Set<Listener>>();
  let frameId: number | null = null;

  const notify = (sessionId: string) => {
    const set = listeners.get(sessionId);
    if (!set) return;
    for (const listener of set) listener();
  };

  const flush = () => {
    if (frameId !== null) {
      scheduler.cancelFrame(frameId);
      frameId = null;
    }
    if (queues.size === 0) return;
    const entries = [...queues.entries()];
    queues.clear();
    for (const [sessionId, rawDeltas] of entries) {
      if (rawDeltas.length === 0) continue;
      const merged = coalesceDeltas(rawDeltas);
      const prev = snapshots.get(sessionId) ?? EMPTY_STREAM;
      let next = prev;
      for (const delta of merged) {
        next = applyDelta(next, delta);
      }
      const normalized = next.length === 0 ? EMPTY_STREAM : next;
      if (normalized !== prev) {
        if (normalized === EMPTY_STREAM) snapshots.delete(sessionId);
        else snapshots.set(sessionId, normalized);
        notify(sessionId);
      }
    }
  };

  const schedule = () => {
    if (frameId !== null) return;
    frameId = scheduler.requestFrame(() => {
      frameId = null;
      flush();
    });
  };

  const push = (sessionId: string, delta: StreamDelta) => {
    if (!sessionId) return;
    if (delta.type === "reset") {
      queues.delete(sessionId);
      const had = snapshots.has(sessionId);
      snapshots.delete(sessionId);
      if (had) notify(sessionId);
      return;
    }
    const existing = queues.get(sessionId);
    if (existing) {
      existing.push(delta);
    } else {
      queues.set(sessionId, [delta]);
    }
    schedule();
  };

  const clear = (sessionId: string) => {
    if (!sessionId) return;
    queues.delete(sessionId);
    const had = snapshots.has(sessionId);
    snapshots.delete(sessionId);
    if (had) notify(sessionId);
  };

  const clearAll = () => {
    if (frameId !== null) {
      scheduler.cancelFrame(frameId);
      frameId = null;
    }
    const dirtyIds = new Set<string>(snapshots.keys());
    queues.clear();
    snapshots.clear();
    for (const id of dirtyIds) notify(id);
  };

  const getSnapshot = (sessionId: string | null): StreamBlock[] => {
    if (!sessionId) return EMPTY_STREAM;
    return snapshots.get(sessionId) ?? EMPTY_STREAM;
  };

  const subscribe = (sessionId: string | null, listener: Listener): (() => void) => {
    if (!sessionId) return () => undefined;
    let set = listeners.get(sessionId);
    if (!set) {
      set = new Set();
      listeners.set(sessionId, set);
    }
    set.add(listener);
    return () => {
      const cur = listeners.get(sessionId);
      if (!cur) return;
      cur.delete(listener);
      if (cur.size === 0) listeners.delete(sessionId);
    };
  };

  return { push, clear, clearAll, flush, getSnapshot, subscribe };
}

export const streamStore = createStreamStore();

/** Subscribe only the leaf `<Streaming>` component to a session's 60fps rAF-coalesced stream blocks. */
export function useSessionStream(
  sessionId: string | null,
  enabled = true,
  store: StreamStore = streamStore,
): StreamBlock[] {
  const effectiveId = enabled ? sessionId : null;
  const subscribe = useCallback(
    (onStoreChange: Listener) => store.subscribe(effectiveId, onStoreChange),
    [store, effectiveId],
  );
  const getSnapshot = useCallback(() => store.getSnapshot(effectiveId), [store, effectiveId]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
