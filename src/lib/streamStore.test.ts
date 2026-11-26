import { act, render } from "@testing-library/react";
import { createElement } from "react";
import { describe, expect, it, vi } from "vitest";
import { type StreamStore, coalesceDeltas, createStreamStore, useSessionStream } from "./streamStore";

function makeManualScheduler() {
  let nextId = 1;
  const callbacks = new Map<number, FrameRequestCallback>();
  return {
    scheduler: {
      requestFrame: (cb: FrameRequestCallback) => {
        const id = nextId++;
        callbacks.set(id, cb);
        return id;
      },
      cancelFrame: (id: number) => {
        callbacks.delete(id);
      },
    },
    pendingFrames: () => callbacks.size,
    runFrame: (ts = 16) => {
      const entries = [...callbacks.entries()];
      callbacks.clear();
      for (const [, cb] of entries) cb(ts);
    },
  };
}

interface ProbeProps {
  sessionId: string;
  store: StreamStore;
  onRender: () => void;
}

function StreamLeafProbe({ sessionId, store, onRender }: ProbeProps) {
  onRender();
  const blocks = useSessionStream(sessionId, true, store);
  return createElement("div", { "data-testid": "stream-out" }, blocks.map((b) => b.text).join(""));
}

function StreamRootProbe({ sessionId, store, onRootRender, onLeafRender }: { sessionId: string; store: StreamStore; onRootRender: () => void; onLeafRender: () => void }) {
  onRootRender();
  return createElement(StreamLeafProbe, { sessionId, store, onRender: onLeafRender });
}

describe("streamStore (P1-13)", () => {
  it("streamStore_coalesces_burst_deltas_per_animation_frame_and_resets_on_assistant_message", () => {
    const { scheduler, pendingFrames, runFrame } = makeManualScheduler();
    const store = createStreamStore(scheduler);
    const listener = vi.fn();
    const unsub = store.subscribe("sess-1", listener);

    for (let i = 0; i < 200; i++) {
      store.push("sess-1", { type: "text_delta", index: 0, text: `tok${i}-` });
    }
    expect(pendingFrames()).toBe(1);
    expect(listener).toHaveBeenCalledTimes(0);

    runFrame();
    expect(listener).toHaveBeenCalledTimes(1);
    const snap = store.getSnapshot("sess-1");
    expect(snap).toHaveLength(1);
    expect(snap[0].text).toBe(Array.from({ length: 200 }, (_, i) => `tok${i}-`).join(""));

    store.push("sess-1", { type: "text_delta", index: 0, text: "stale" });
    store.push("sess-1", { type: "reset" });
    expect(store.getSnapshot("sess-1")).toEqual([]);
    expect(listener).toHaveBeenCalledTimes(2);

    store.push("sess-1", { type: "thinking_delta", index: 0, text: "step 1 " });
    store.push("sess-1", { type: "thinking_delta", index: 0, text: "step 2" });
    store.push("sess-1", { type: "tool_use_start", index: 1, id: "t1", name: "read_file" });
    store.push("sess-1", { type: "tool_input_delta", index: 1, partial: '{"pa' });
    store.push("sess-1", { type: "tool_input_delta", index: 1, partial: 'th":"a.ts"}' });
    runFrame();

    expect(store.getSnapshot("sess-1")).toEqual([
      { index: 0, kind: "thinking", text: "step 1 step 2" },
      { index: 1, kind: "tool", name: "read_file", text: '{"path":"a.ts"}' },
    ]);

    store.clear("sess-1");
    expect(store.getSnapshot("sess-1")).toEqual([]);
    unsub();
  });

  it("isolates per-session streams and switches active session with zero root re-renders", () => {
    const { scheduler, runFrame } = makeManualScheduler();
    const store = createStreamStore(scheduler);
    let rootRenders = 0;
    let leafRenders = 0;
    const incRoot = () => {
      rootRenders++;
    };
    const incLeaf = () => {
      leafRenders++;
    };

    const { getByTestId, rerender } = render(
      createElement(StreamRootProbe, { sessionId: "A", store, onRootRender: incRoot, onLeafRender: incLeaf }),
    );
    expect(rootRenders).toBe(1);
    expect(leafRenders).toBe(1);

    act(() => {
      for (let i = 0; i < 50; i++) {
        store.push("A", { type: "text_delta", index: 0, text: "a" });
        store.push("B", { type: "text_delta", index: 0, text: "b" });
      }
      runFrame();
    });

    expect(rootRenders).toBe(1);
    expect(leafRenders).toBe(2);
    expect(getByTestId("stream-out").textContent).toBe("a".repeat(50));

    rerender(createElement(StreamRootProbe, { sessionId: "B", store, onRootRender: incRoot, onLeafRender: incLeaf }));
    expect(getByTestId("stream-out").textContent).toBe("b".repeat(50));
  });

  it("coalesceDeltas merges adjacent same-index text, thinking, and tool_input deltas", () => {
    const merged = coalesceDeltas([
      { type: "thinking_delta", index: 0, text: "a" },
      { type: "thinking_delta", index: 0, text: "b" },
      { type: "text_delta", index: 1, text: "hel" },
      { type: "text_delta", index: 1, text: "lo" },
      { type: "tool_use_start", index: 2, id: "id1", name: "grep" },
      { type: "tool_input_delta", index: 2, partial: '{"q":' },
      { type: "tool_input_delta", index: 2, partial: '"x"}' },
    ]);
    expect(merged).toEqual([
      { type: "thinking_delta", index: 0, text: "ab" },
      { type: "text_delta", index: 1, text: "hello" },
      { type: "tool_use_start", index: 2, id: "id1", name: "grep" },
      { type: "tool_input_delta", index: 2, partial: '{"q":"x"}' },
    ]);
  });

  it("clearAll resets all active session snapshots and cancels pending animation frames", () => {
    const { scheduler, pendingFrames } = makeManualScheduler();
    const store = createStreamStore(scheduler);
    store.push("s1", { type: "text_delta", index: 0, text: "hello" });
    store.push("s2", { type: "thinking_delta", index: 0, text: "hmm" });
    store.flush();
    expect(store.getSnapshot("s1")).toHaveLength(1);
    expect(store.getSnapshot("s2")).toHaveLength(1);

    store.push("s1", { type: "text_delta", index: 0, text: " world" });
    expect(pendingFrames()).toBe(1);
    store.clearAll();
    expect(pendingFrames()).toBe(0);
    expect(store.getSnapshot("s1")).toEqual([]);
    expect(store.getSnapshot("s2")).toEqual([]);
  });
});
