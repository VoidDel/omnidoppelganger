import { describe, expect, it } from "vitest";
import { appendLog, initial, reduce, type State } from "./sessionState";
import type { Envelope, SessionEvent, SessionItem, SessionView, Status } from "./types";

function env(seq: number, event: SessionEvent = { type: "user/message", data: { text: `msg-${seq}`, steering: false } }): Envelope {
  return { seq, ts: "2026-11-26T08:00:00.000Z", ...event } as Envelope;
}

function makeItem(id: string, overrides: Partial<SessionItem> = {}): SessionItem {
  return {
    id,
    title: `Session ${id}`,
    worktree: null,
    parent: null,
    schedule: null,
    fork: null,
    aside: null,
    workspace: `/ws/${id}`,
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    effort: null,
    mode: "edit",
    context_limit: 100_000,
    context_ceiling: 200_000,
    branch: "main",
    vision: true,
    created_at: "2026-11-26T08:00:00.000Z",
    updated_at: "2026-11-26T08:00:00.000Z",
    turn_open: false,
    awaiting_approval: false,
    status: "idle",
    ...overrides,
  };
}

function makeView(id: string, count: number, status: Status = "idle", overrides: Partial<SessionView> = {}): SessionView {
  return {
    ...makeItem(id, { status }),
    events: Array.from({ length: count }, (_, i) => env(i + 1)),
    ...overrides,
  };
}

describe("sessionState reducer (P1-12)", () => {
  it("ignores_stale_openSession_response_and_never_mixes_cross_session_buffer", () => {
    let state: State = {
      ...initial,
      sessions: [makeItem("A", { status: "idle" }), makeItem("B", { status: "running" })],
    };

    // 1. User clicks slow session A (navSeq=1), then quickly clicks streaming session B (navSeq=2).
    state = reduce(state, { type: "opening", id: "A", navSeq: 1 });
    state = reduce(state, { type: "opening", id: "B", navSeq: 2 });
    expect(state.openingId).toBe("B");
    expect(state.navSeq).toBe(2);

    // 2. While B is opening, B emits real-time log(seq=50) and status("waiting_approval").
    const bLiveEvent = env(50, {
      type: "approval/requested",
      data: { tool_use_id: "t50", tool: "run_command", reason: "need approval", detail: "cargo test" },
    });
    state = reduce(state, { type: "event", event: { kind: "log", session_id: "B", envelope: bLiveEvent } });
    state = reduce(state, { type: "event", event: { kind: "status", session_id: "B", status: "waiting_approval" } });

    expect(state.buffer).toEqual([{ sessionId: "B", envelope: bLiveEvent }]);
    expect(state.bufferedStatus).toEqual({ sessionId: "B", status: "waiting_approval" });

    // 3. Stale openSession("A") response (navSeq=1, 10 events) arrives first!
    const viewA = makeView("A", 10, "idle", { title: "Updated A" });
    state = reduce(state, { type: "opened", view: viewA, navSeq: 1 });

    // Must NOT switch activeId to "A", touch events, or drain B's buffer; only A's summary in sessions updates.
    expect(state.activeId).toBeNull();
    expect(state.openingId).toBe("B");
    expect(state.events).toEqual([]);
    expect(state.buffer).toEqual([{ sessionId: "B", envelope: bLiveEvent }]);
    expect(state.bufferedStatus).toEqual({ sessionId: "B", status: "waiting_approval" });
    expect(state.sessions.find((s) => s.id === "A")?.title).toBe("Updated A");

    // 4. Current openSession("B") response (navSeq=2, snapshot had 49 events and stale "running" status) arrives.
    const viewB = makeView("B", 49, "running");
    state = reduce(state, { type: "opened", view: viewB, navSeq: 2 });

    expect(state.activeId).toBe("B");
    expect(state.openingId).toBeNull();
    expect(state.buffer).toEqual([]);
    expect(state.bufferedStatus).toBeNull();
    expect(state.events).toHaveLength(50);
    expect(state.events[49]).toEqual(bLiveEvent);
    expect(state.status).toBe("waiting_approval");
    expect(state.sessions.find((s) => s.id === "B")?.status).toBe("waiting_approval");
  });

  it("never merges foreign-session buffered envelopes or bufferedStatus even if present in state", () => {
    const foreignEnv = env(99, { type: "user/message", data: { text: "from-A", steering: false } });
    const ownEnv = env(12, { type: "user/message", data: { text: "from-B", steering: false } });

    // Switching opening from A to B immediately prunes A's buffered items.
    let state = reduce(initial, { type: "opening", id: "A", navSeq: 1 });
    state = reduce(state, { type: "event", event: { kind: "log", session_id: "A", envelope: foreignEnv } });
    state = reduce(state, { type: "event", event: { kind: "status", session_id: "A", status: "running" } });
    expect(state.buffer).toHaveLength(1);

    state = reduce(state, { type: "opening", id: "B", navSeq: 2 });
    expect(state.buffer).toEqual([]);
    expect(state.bufferedStatus).toBeNull();

    // Defense-in-depth: even if state held mixed sessionIds, `opened` merges only `b.sessionId === view.id`.
    const mixed: State = {
      ...state,
      buffer: [
        { sessionId: "A", envelope: foreignEnv },
        { sessionId: "B", envelope: ownEnv },
      ],
      bufferedStatus: { sessionId: "A", status: "waiting_approval" },
    };
    const afterOpen = reduce(mixed, { type: "opened", view: makeView("B", 11, "idle"), navSeq: 2 });
    expect(afterOpen.activeId).toBe("B");
    expect(afterOpen.events.map((e) => e.seq)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    expect(afterOpen.status).toBe("idle");
  });

  it("create_or_fork_invalidates_inflight_openSession", () => {
    // Case 1: `opening("A", seq=1)` in flight -> `create` allocates `seq=2` and resolves `opened(viewNew, seq=2)`
    // before `opened(viewA, seq=1)` arrives.
    let state = reduce(initial, { type: "opening", id: "A", navSeq: 1 });
    const viewNew = makeView("new-1", 1, "idle");
    state = reduce(state, { type: "opened", view: viewNew, navSeq: 2 });
    expect(state.activeId).toBe("new-1");

    const viewA = makeView("A", 8, "running");
    state = reduce(state, { type: "opened", view: viewA, navSeq: 1 });
    expect(state.activeId).toBe("new-1");
    expect(state.events).toEqual(viewNew.events);
    expect(state.status).toBe("idle");
    expect(state.sessions.map((s) => s.id)).toEqual(["A", "new-1"]);

    // Case 2: `opening("A", seq=1)` in flight -> `fork`/`chat` starts (`opening(null, seq=2)`) ->
    // `opened(viewA, seq=1)` arrives WHILE `fork` is still running -> then `opened(viewFork, seq=2)` arrives.
    let s2: State = { ...initial, activeId: "origin", events: [env(1)] };
    s2 = reduce(s2, { type: "opening", id: "A", navSeq: 1 });
    s2 = reduce(s2, { type: "event", event: { kind: "log", session_id: "A", envelope: env(20) } });
    s2 = reduce(s2, { type: "opening", id: null, navSeq: 2 });
    expect(s2.openingId).toBeNull();
    expect(s2.buffer).toEqual([]);

    // Stale A arrives while fork is still awaiting IPC: must not hijack the screen!
    s2 = reduce(s2, { type: "opened", view: viewA, navSeq: 1 });
    expect(s2.activeId).toBe("origin");
    expect(s2.events).toEqual([env(1)]);

    // Fork finishes: cleanly switches to forked session.
    const viewFork = makeView("fork-1", 3, "idle", { abandoned_brief: "tried X" });
    s2 = reduce(s2, { type: "opened", view: viewFork, navSeq: 2 });
    expect(s2.activeId).toBe("fork-1");
    expect(s2.events).toHaveLength(3);
    expect("abandoned_brief" in (s2.sessions.find((s) => s.id === "fork-1") ?? {})).toBe(false);
  });

  it("home invalidates in-flight open, create, and fork responses", () => {
    let state: State = {
      ...initial,
      activeId: "cur",
      events: [env(1), env(2)],
      status: "running",
      page: "settings",
    };

    state = reduce(state, { type: "opening", id: "A", navSeq: 1 });
    state = reduce(state, { type: "event", event: { kind: "log", session_id: "A", envelope: env(9) } });
    state = reduce(state, { type: "home", navSeq: 2 });

    expect(state.activeId).toBeNull();
    expect(state.openingId).toBeNull();
    expect(state.buffer).toEqual([]);
    expect(state.bufferedStatus).toBeNull();
    expect(state.events).toEqual([]);
    expect(state.status).toBe("idle");
    expect(state.page).toBe("chat");

    // Late response from A must not pull the user away from the Home screen.
    state = reduce(state, { type: "opened", view: makeView("A", 8, "running"), navSeq: 1 });
    expect(state.activeId).toBeNull();
    expect(state.events).toEqual([]);
    expect(state.status).toBe("idle");
  });

  it("deleting active or opening session invalidates stale navigation without disturbing unrelated opens", () => {
    let state: State = {
      ...initial,
      sessions: [makeItem("A"), makeItem("A-child", { parent: "A" }), makeItem("B"), makeItem("C")],
      activeId: "A-child",
      events: [env(1)],
      navSeq: 1,
    };

    // Deleting parent A cascades to active child A-child and advances navSeq=2, blocking stale seq=1.
    state = reduce(state, { type: "deleted", id: "A", navSeq: 2 });
    expect(state.activeId).toBeNull();
    expect(state.events).toEqual([]);
    expect(state.sessions.map((s) => s.id)).toEqual(["B", "C"]);

    state = reduce(state, { type: "opened", view: makeView("B", 4), navSeq: 1 });
    expect(state.activeId).toBeNull();

    // Deleting unrelated C while B (navSeq=3) is opening keeps B's opening state & buffer intact.
    state = reduce(state, { type: "opening", id: "B", navSeq: 3 });
    const bEnv = env(5);
    state = reduce(state, { type: "event", event: { kind: "log", session_id: "B", envelope: bEnv } });
    state = reduce(state, { type: "deleted", id: "C" });

    expect(state.openingId).toBe("B");
    expect(state.navSeq).toBe(3);
    expect(state.buffer).toEqual([{ sessionId: "B", envelope: bEnv }]);

    state = reduce(state, { type: "opened", view: makeView("B", 4), navSeq: 3 });
    expect(state.activeId).toBe("B");
    expect(state.events).toHaveLength(5);
  });

  it("preserves live events without duplication when re-opening the already-active session", () => {
    let state = reduce(initial, { type: "opened", view: makeView("A", 3, "running"), navSeq: 1 });
    state = reduce(state, { type: "queued", id: "A", text: "steer me" });

    // Re-open A while A is already active; live seq=4 arrives before snapshot(1..3) comes back.
    state = reduce(state, { type: "opening", id: "A", navSeq: 2 });
    const steerEnv = env(4, { type: "user/message", data: { text: "steer me", steering: true } });
    state = reduce(state, { type: "event", event: { kind: "log", session_id: "A", envelope: steerEnv } });

    expect(state.events).toHaveLength(4);
    expect(state.queued).toEqual([]);

    // Snapshot taken at seq=3 arrives: seq=4 is re-applied from buffer, and seq<=3 is deduplicated.
    state = reduce(state, { type: "opened", view: makeView("A", 3, "running"), navSeq: 2 });
    expect(state.events.map((e) => e.seq)).toEqual([1, 2, 3, 4]);
    expect(appendLog(state, env(4))).toBe(state);
  });
});
