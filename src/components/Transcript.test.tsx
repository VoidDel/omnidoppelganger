import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Row } from "../lib/fold";
import { streamStore } from "../lib/streamStore";
import { Transcript, type Props as TranscriptProps } from "./Transcript";

function makeRow(overrides: Partial<Row> = {}): Row {
  return {
    key: "row-1",
    turn: 1,
    prompt: {
      text: "请帮我跑测试",
      ts: "2026-11-26T10:00:00.000Z",
      steering: false,
      images: [],
      files: [],
    },
    items: [],
    ...overrides,
  };
}

function renderTranscript(overrides: Partial<TranscriptProps> = {}) {
  const props: TranscriptProps = {
    sessionId: "sess-t1",
    rows: [makeRow()],
    queued: [],
    status: "idle",
    mode: "edit",
    origin: "user",
    sessions: [],
    copied: 0,
    frozen: false,
    onOpen: vi.fn(),
    onDecide: vi.fn(),
    onAnswer: vi.fn(),
    onUndo: vi.fn(),
    ...overrides,
  };
  return { ...render(<Transcript {...props} />), props };
}

describe("Transcript component (P1-15 & P1-13)", () => {
  beforeEach(() => {
    streamStore.clearAll();
  });

  it("dispatches all four approval decisions (allow_once, allow_session, allow_always, deny)", () => {
    const onDecide = vi.fn();
    const row = makeRow({
      items: [
        {
          kind: "tool",
          id: "tool-cmd-1",
          name: "run_command",
          input: { command: "cargo test" },
          turn: 1,
          approval: {
            reason: "command",
            detail: "cargo test",
            grant: "command:cargo test",
          },
        },
      ],
    });
    renderTranscript({ rows: [row], status: "waiting_approval", onDecide });

    fireEvent.click(screen.getByRole("button", { name: "准这一次" }));
    expect(onDecide).toHaveBeenLastCalledWith("tool-cmd-1", "allow_once");

    fireEvent.click(screen.getByRole("button", { name: /之后以「cargo test」开头的命令都准/ }));
    expect(onDecide).toHaveBeenLastCalledWith("tool-cmd-1", "allow_session");

    fireEvent.click(screen.getByRole("button", { name: "此府永准" }));
    expect(onDecide).toHaveBeenLastCalledWith("tool-cmd-1", "allow_always");

    fireEvent.click(screen.getByRole("button", { name: "驳回" }));
    expect(onDecide).toHaveBeenLastCalledWith("tool-cmd-1", "deny");
  });

  it("submits single-select and multi-select ask_user answers and supports declining", () => {
    const onAnswer = vi.fn();
    const row = makeRow({
      items: [
        {
          kind: "tool",
          id: "ask-1",
          name: "ask_user",
          input: {},
          turn: 1,
          question: {
            questions: [
              {
                header: "数据库",
                question: "选哪种数据库？",
                kind: "single",
                options: [{ label: "SQLite" }, { label: "PostgreSQL" }],
              },
              {
                header: "特性",
                question: "开启哪些特性？",
                kind: "multiple",
                options: [{ label: "WAL" }, { label: "FTS5" }, { label: "JSON" }],
              },
            ],
          },
        },
      ],
    });
    renderTranscript({ rows: [row], status: "waiting_approval", onAnswer });

    // Pick single-select option on tab 1
    fireEvent.click(screen.getByLabelText("SQLite"));
    // Switch to tab 2 and pick two checkboxes
    fireEvent.click(screen.getByRole("tab", { name: /特性/ }));
    fireEvent.click(screen.getByLabelText("WAL"));
    fireEvent.click(screen.getByLabelText("FTS5"));

    fireEvent.click(screen.getByRole("button", { name: "呈上答复" }));
    expect(onAnswer).toHaveBeenCalledWith("ask-1", [
      { chosen: ["SQLite"], text: "" },
      { chosen: ["WAL", "FTS5"], text: "" },
    ]);
  });

  it("allows leaving ask_user questions to the agent (null reply)", () => {
    const onAnswer = vi.fn();
    const row = makeRow({
      items: [
        {
          kind: "tool",
          id: "ask-2",
          name: "ask_user",
          input: {},
          turn: 1,
          question: {
            questions: [{ header: "确认", question: "用默认端口？", kind: "single", options: [{ label: "是" }] }],
          },
        },
      ],
    });
    renderTranscript({ rows: [row], status: "waiting_approval", onAnswer });
    fireEvent.click(screen.getByRole("button", { name: "不答，交给分身自定" }));
    expect(onAnswer).toHaveBeenCalledWith("ask-2", null);
  });

  it("opens undo confirmation bubble and invokes onUndo(turn) only when confirmed", () => {
    const onUndo = vi.fn();
    const row = makeRow({
      turn: 2,
      items: [
        {
          kind: "tool",
          id: "edit-1",
          name: "edit_file",
          input: { path: "src/a.ts" },
          turn: 2,
          result: {
            content: "edited",
            isError: false,
            meta: { path: "src/a.ts", added: 3, removed: 1, after: "snap-1" },
          },
        },
      ],
    });
    renderTranscript({ rows: [row], status: "idle", onUndo });

    fireEvent.click(screen.getByRole("button", { name: "撤回这一轮起的改动" }));
    expect(screen.getByText(/分身从这一轮起改的文件都会恢复原样/)).toBeTruthy();
    expect(onUndo).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "确认撤回" }));
    expect(onUndo).toHaveBeenCalledWith(2);
  });

  it("renders live streaming blocks directly from streamStore without parent prop updates", () => {
    renderTranscript({ sessionId: "live-sess", status: "running" });

    act(() => {
      streamStore.push("live-sess", { type: "thinking_delta", index: 0, text: "正在思考方案…" });
      streamStore.push("live-sess", { type: "text_delta", index: 1, text: "这是实时流式回复。" });
      streamStore.flush();
    });

    expect(screen.getByText("正在思考方案…")).toBeTruthy();
    expect(screen.getByText("这是实时流式回复。")).toBeTruthy();
  });

  it("folds 4+ consecutive quiet read/search tools into a collapsible summary row", () => {
    const quietTools = ["read_file", "search", "find_files", "read_file"].map((name, i) => ({
      kind: "tool" as const,
      id: `q-${i}`,
      name,
      input: { path: `src/f${i}.ts`, pattern: "fn" },
      turn: 1,
      result: { content: "ok", isError: false, meta: { lines: 10, matches: 2, entries: 3 } },
    }));
    renderTranscript({ rows: [makeRow({ items: quietTools })] });

    const summaryBtn = screen.getByRole("button", { name: /查阅.*4 处/ });
    expect(summaryBtn.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(summaryBtn);
    expect(summaryBtn.getAttribute("aria-expanded")).toBe("true");
  });

  it("renders fork seam divider and fork callback button on eligible turns", () => {
    const onFork = vi.fn();
    const rows = [
      makeRow({ key: "r1", turn: 1, prompt: { text: "旧回合", ts: "2026-11-26T10:00:00Z", steering: false, images: [], files: [] } }),
      makeRow({ key: "r2", turn: 2, prompt: { text: "新分支回合", ts: "2026-11-26T10:01:00Z", steering: false, images: [], files: [] } }),
    ];
    renderTranscript({ rows, copied: 1, onFork });

    expect(screen.getByText("以上是分叉时带过来的前情，以下是这个分叉自己的")).toBeTruthy();
    const forkButtons = screen.getAllByRole("button", { name: "从这里分叉" });
    fireEvent.click(forkButtons[1]);
    expect(onFork).toHaveBeenCalledWith(2, "新分支回合");
  });

  it("renders queued steering message and triggers onDeliverNow on immediate send", () => {
    const onDeliverNow = vi.fn();
    renderTranscript({ status: "running", queued: ["先改测试文件"], onDeliverNow });

    expect(screen.getByText("先改测试文件")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "立即传音" }));
    expect(onDeliverNow).toHaveBeenCalledTimes(1);
  });
});
