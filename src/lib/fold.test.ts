import { describe, expect, it } from "vitest";
import { applyDelta, changedFiles, commandRuns, fold, pendingApproval, undoableTurns, usage, type StreamBlock, type ToolItem } from "./fold";
import type { Envelope, SessionEvent } from "./types";

/** A log from its events, numbered from 1 like the real one. */
function log(...events: SessionEvent[]): Envelope[] {
  return events.map((event, index) => ({ seq: index + 1, ts: "2026-11-18T05:00:00.000Z", ...event }) as Envelope);
}

const user = (text: string, steering = false): SessionEvent => ({ type: "user/message", data: { text, steering } });
const said = (...content: Extract<SessionEvent, { type: "assistant/message" }>["data"]["content"]): SessionEvent => ({
  type: "assistant/message",
  data: { content, stop_reason: "end_turn", model: "m" },
});
const call = (id: string, name: string, input: Record<string, unknown>) => ({ type: "tool_use" as const, id, name, input });
const result = (id: string, tool: string, meta: object | null, isError = false): SessionEvent => ({
  type: "tool/result",
  data: { tool_use_id: id, tool, content: "", is_error: isError, meta },
});
const edit = (id: string, path: string, added: number, removed: number) => [
  said(call(id, "edit_file", { path })),
  result(id, "edit_file", { path, added, removed }),
];

describe("undo", () => {
  const turn = (number: number): SessionEvent => ({ type: "turn/start", data: { turn: number } });
  const edited = (id: string, path: string): SessionEvent[] => [said(call(id, "edit_file", { path })), result(id, "edit_file", { path, added: 1, removed: 0, after: "f" })];

  it("offers every turn up to the last one with an edit, and marks what was undone", () => {
    const events = log(turn(1), user("one"), ...edited("t1", "a.rs"), turn(2), user("two"), ...edited("t2", "b.rs"), turn(3), user("three"), said({ type: "text", text: "hi" }));
    expect([...undoableTurns(fold(events))]).toEqual([1, 2]);

    const undone = fold([...events, ...log({ type: "files/restored", data: { turn: 2, restored: ["b.rs"], skipped: [] } })].map((event, index) => ({ ...event, seq: index + 1 })));
    expect([...undoableTurns(undone)]).toEqual([1]);
    expect(changedFiles(undone).map((file) => file.path)).toEqual(["a.rs"]);
    const tools = undone.flatMap((row) => row.items).filter((item): item is ToolItem => item.kind === "tool");
    expect(tools.map((tool) => [tool.turn, !!tool.undone])).toEqual([
      [1, false],
      [2, true],
    ]);
    expect(undone[2].items[undone[2].items.length - 1]).toEqual({ kind: "restored", turn: 2, restored: ["b.rs"], skipped: [] });
  });

  it("treats a whole-file write like an edit", () => {
    const written: SessionEvent[] = [said(call("w1", "write_file", { path: "c.rs", content: "x" })), result("w1", "write_file", { path: "c.rs", added: 1, removed: 0, after: "f" })];
    const rows = fold(log(turn(1), user("one"), ...written));
    expect(changedFiles(rows).map((file) => file.path)).toEqual(["c.rs"]);
    expect([...undoableTurns(rows)]).toEqual([1]);
  });

  it("offers nothing for edits logged before snapshots existed", () => {
    expect(undoableTurns(fold(log(turn(1), user("one"), ...edit("t1", "a.rs", 1, 0)))).size).toBe(0);
  });
});

describe("usage", () => {
  const counted = (input: number, cached: number, output: number): SessionEvent => ({
    type: "assistant/message",
    data: { content: [{ type: "text", text: "x" }], stop_reason: "end_turn", model: "m", tokens: { input, cached, output } },
  });

  it("sums what every request cost and reports what the conversation holds now", () => {
    const events = log(user("one"), counted(1000, 0, 50), user("two"), counted(1100, 1000, 60));
    expect(usage(events)).toEqual({ context: 1160, spent: { input: 2100, cached: 1000, output: 110 } });
    // A message logged before counts existed adds nothing and leaves the context as it was.
    expect(usage([...events, ...log(said({ type: "text", text: "old" }))])).toEqual(usage(events));
    expect(usage([])).toEqual({ context: 0, spent: { input: 0, cached: 0, output: 0 } });
  });

  it("starts the context over at a compaction, whose own request still counts as spent", () => {
    const events = log(
      user("one"),
      counted(9000, 0, 100),
      { type: "context/compacted", data: { summary: "s", tokens_before: 9100, tokens: { input: 9200, cached: 9000, output: 300 } } },
      counted(700, 0, 40),
    );
    expect(usage(events.slice(0, 3)).context).toBe(300);
    expect(usage(events)).toEqual({ context: 740, spent: { input: 18900, cached: 9000, output: 440 } });
    const rows = fold(events);
    expect(rows[0].items.map((item) => item.kind)).toEqual(["text", "compacted", "text"]);
  });
});

describe("fold", () => {
  it("puts what the agent did under the message it answered", () => {
    const rows = fold(
      log(
        { type: "session/created", data: { id: "s", workspace: "/w", model: "m", format: 1 } },
        { type: "turn/start", data: { turn: 1 } },
        user("read a"),
        said({ type: "thinking", thinking: "  " }, { type: "text", text: "Reading." }, call("t1", "read_file", { path: "a" })),
        result("t1", "read_file", { path: "a", lines: 3 }),
        user("also b", true),
        said({ type: "text", text: "Done." }),
        { type: "turn/end", data: { turn: 1, reason: "completed", error: null } },
      ),
    );
    expect(rows.map((row) => [row.prompt.text, row.prompt.steering])).toEqual([
      ["read a", false],
      ["also b", true],
    ]);
    // Empty thinking is left out; the tool carries its result.
    expect(rows[0].items.map((item) => item.kind)).toEqual(["text", "tool"]);
    expect((rows[0].items[1] as ToolItem).result?.meta?.lines).toBe(3);
    expect(rows[1].items).toEqual([{ kind: "text", text: "Done." }]);
  });

  it("attaches approvals with their grant and decision, and shows why a turn stopped", () => {
    const events = log(
      user("run it"),
      said(call("t1", "run_command", { command: "cargo test" })),
      { type: "approval/requested", data: { tool_use_id: "t1", tool: "run_command", reason: "command", detail: "cargo test", grant: "command:cargo test" } },
    );
    const waiting = fold(events);
    expect(pendingApproval(waiting)?.approval).toEqual({ reason: "command", detail: "cargo test", grant: "command:cargo test" });

    const rows = fold([
      ...events,
      ...log(
        { type: "approval/resolved", data: { tool_use_id: "t1", decision: "deny" } },
        result("t1", "run_command", null, true),
        { type: "turn/end", data: { turn: 1, reason: "cancelled", error: null } },
      ).map((envelope) => ({ ...envelope, seq: envelope.seq + events.length })),
    ]);
    expect(pendingApproval(rows)).toBeUndefined();
    expect((rows[0].items[0] as ToolItem).approval?.decision).toBe("deny");
    expect(rows[0].items[1]).toEqual({ kind: "notice", reason: "cancelled", error: null });
  });

  it("reads an approval logged before grants existed as having none", () => {
    const rows = fold(
      log(user("go"), said(call("t1", "run_command", { command: "ls" })), {
        type: "approval/requested",
        data: { tool_use_id: "t1", tool: "run_command", reason: "command", detail: "ls" },
      }),
    );
    expect(pendingApproval(rows)?.approval?.grant).toBeUndefined();
  });
});

describe("applyDelta", () => {
  it("builds the message being generated and starts over on a retry", () => {
    let blocks: StreamBlock[] = [];
    blocks = applyDelta(blocks, { type: "thinking_delta", index: 0, text: "hm" });
    blocks = applyDelta(blocks, { type: "text_delta", index: 1, text: "Hel" });
    blocks = applyDelta(blocks, { type: "text_delta", index: 1, text: "lo" });
    blocks = applyDelta(blocks, { type: "tool_use_start", index: 2, id: "t1", name: "read_file" });
    blocks = applyDelta(blocks, { type: "tool_input_delta", index: 2, partial: '{"path"' });
    blocks = applyDelta(blocks, { type: "block_stop", index: 2 });
    expect(blocks).toEqual([
      { index: 0, kind: "thinking", text: "hm" },
      { index: 1, kind: "text", text: "Hello" },
      { index: 2, kind: "tool", text: '{"path"', name: "read_file" },
    ]);
    expect(applyDelta(blocks, { type: "reset" })).toEqual([]);
  });
});

describe("side panel", () => {
  it("sums edits per file in first-edit order and skips failed ones", () => {
    const rows = fold(
      log(
        user("edit"),
        ...edit("t1", "b.rs", 2, 1),
        ...edit("t2", "a.rs", 5, 0),
        ...edit("t3", "b.rs", 1, 1),
        said(call("t4", "edit_file", { path: "c.rs" })),
        result("t4", "edit_file", { path: "c.rs", added: 9, removed: 9 }, true),
      ),
    );
    expect(changedFiles(rows)).toEqual([
      { path: "b.rs", added: 3, removed: 2 },
      { path: "a.rs", added: 5, removed: 0 },
    ]);
  });

  it("lists only commands that ran", () => {
    const rows = fold(
      log(
        user("run"),
        said(call("t1", "run_command", { command: "ls" }), call("t2", "run_command", { command: "rm x" })),
        result("t1", "run_command", { command: "ls", exit_code: 0, output: "a" }),
        result("t2", "run_command", null, true),
      ),
    );
    expect(commandRuns(rows).map((tool) => tool.id)).toEqual(["t1"]);
  });
});
