import { describe, expect, it } from "vitest";
import { fold } from "./fold";
import { replayStops, stepped, turnAt } from "./replay";
import type { Envelope, SessionEvent } from "./types";

function log(...events: SessionEvent[]): Envelope[] {
  return events.map((event, index) => ({ seq: index + 1, ts: "2026-11-18T05:00:00.000Z", ...event }) as Envelope);
}

const events = log(
  { type: "session/created", data: { id: "s", workspace: "/w", model: "m", format: 10 } },
  { type: "instructions/loaded", data: { path: "AGENTS.md", content: "rules" } },
  { type: "turn/start", data: { turn: 1 } },
  { type: "user/message", data: { text: "edit it", steering: false } },
  { type: "assistant/message", data: { content: [{ type: "tool_use", id: "t1", name: "run_command", input: { command: "ls" } }], stop_reason: "tool_use", model: "m" } },
  { type: "approval/requested", data: { tool_use_id: "t1", tool: "run_command", reason: "a command", detail: "ls" } },
  { type: "approval/resolved", data: { tool_use_id: "t1", decision: "allow_once" } },
  { type: "tool/result", data: { tool_use_id: "t1", tool: "run_command", content: "a.txt", is_error: false, meta: null } },
  { type: "assistant/message", data: { content: [{ type: "text", text: "done" }], stop_reason: "end_turn", model: "m" } },
  { type: "turn/end", data: { turn: 1, reason: "completed", error: null } },
  { type: "turn/start", data: { turn: 2 } },
  { type: "user/message", data: { text: "again", steering: false } },
  { type: "turn/end", data: { turn: 2, reason: "cancelled", error: null } },
);

describe("replay", () => {
  it("stops at each event that puts something new on the page", () => {
    // Not at the header, the instructions read before the first message, a turn's start, or a turn that ended well.
    expect(replayStops(events)).toEqual([3, 4, 5, 6, 7, 8, 11, 12]);
    expect(replayStops([])).toEqual([]);
  });

  it("shows at a stop what the session showed then", () => {
    const stops = replayStops(events);
    const at = (stop: number) => fold(events.slice(0, stops[stop] + 1));
    expect(at(0).map((row) => row.items.length)).toEqual([0]);
    // The command waits to be approved, then is approved, then has run.
    const tool = (stop: number) => at(stop)[0].items.find((item) => item.kind === "tool");
    expect([tool(2)?.approval?.decision, tool(3)?.approval?.decision, !!tool(3)?.result, !!tool(4)?.result]).toEqual([undefined, "allow_once", false, true]);
    expect(at(stops.length - 1).length).toBe(2);
    expect(at(stops.length - 1)).toEqual(fold(events));
  });

  it("tells the turn a stop is in, and steps within the stops there are", () => {
    const stops = replayStops(events);
    expect(stops.map((index) => turnAt(events, index))).toEqual([1, 1, 1, 1, 1, 1, 2, 2]);
    expect(turnAt(events, 1)).toBe(0);
    expect([stepped(0, -1, 8), stepped(0, 1, 8), stepped(7, 1, 8), stepped(3, -10, 8)]).toEqual([0, 1, 7, 0]);
  });
});
