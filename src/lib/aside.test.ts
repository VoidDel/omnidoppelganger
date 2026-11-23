import { describe, expect, it } from "vitest";
import { emptyLive, forwarded, liveReduce } from "./aside";
import type { Envelope, UiEvent } from "./types";

const log = (seq: number, event: Omit<Envelope, "seq" | "ts">): UiEvent => ({ kind: "log", session_id: "a", envelope: { seq, ts: "2026-11-23T00:00:00Z", ...event } as Envelope });

describe("side conversation live log", () => {
  it("adds each log event once, in order, and starts from what it was opened with", () => {
    let state = liveReduce(emptyLive, { type: "opened", events: [{ seq: 1, ts: "t", type: "turn/start", data: { turn: 1 } }], status: "idle" });
    state = liveReduce(state, { type: "event", event: log(1, { type: "turn/start", data: { turn: 1 } }) });
    state = liveReduce(state, { type: "event", event: log(2, { type: "aside/briefed", data: { brief: "b" } }) });
    expect(state.events.map((e) => e.seq)).toEqual([1, 2]);
  });

  it("drops a queued message once it joins, and the stream once the answer is logged or the turn ends", () => {
    let state = liveReduce(emptyLive, { type: "queued", text: "and now?" });
    state = liveReduce(state, { type: "event", event: { kind: "stream", session_id: "a", delta: { type: "text_delta", index: 0, text: "Hi" } } });
    expect(state.stream).toHaveLength(1);
    state = liveReduce(state, { type: "event", event: log(1, { type: "user/message", data: { text: "and now?", steering: true } }) });
    expect(state.queued).toEqual([]);
    state = liveReduce(state, { type: "event", event: log(2, { type: "assistant/message", data: { content: [], stop_reason: "end_turn", model: "m" } }) });
    expect(state.stream).toEqual([]);
    state = liveReduce(state, { type: "queued", text: "x" });
    state = liveReduce(state, { type: "event", event: { kind: "status", session_id: "a", status: "idle" } });
    expect(state.queued).toEqual([]);
    expect(state.status).toBe("idle");
  });

  it("says where a forwarded answer comes from", () => {
    expect(forwarded("  use a map  ")).toMatch(/旁聊.*\n\nuse a map$/s);
  });
});
