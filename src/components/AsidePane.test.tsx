import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import type { SessionView, UiEvent } from "../lib/types";
import { makeMockSessionItem, makeMockSessionView } from "../test/mockApi";
import { AsidePane } from "./AsidePane";

describe("AsidePane earlyQueue & forwarding (P1-15 & P2-6)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("buffers events arriving before startAside resolves so in-flight turn completion settles to idle", async () => {
    let sessionEventHandler: ((ev: UiEvent) => void) | null = null;
    vi.spyOn(api, "onSessionEvent").mockImplementation(async (handler) => {
      sessionEventHandler = handler;
      return () => undefined;
    });

    let resolveStartAside!: (view: SessionView) => void;
    vi.spyOn(api, "startAside").mockImplementation(
      () =>
        new Promise<SessionView>((res) => {
          resolveStartAside = res;
        }),
    );

    const main = makeMockSessionItem({ id: "main-1", status: "running" });
    let renderedStatus = "";

    render(
      <AsidePane
        main={main}
        visible
        sessions={[main]}
        onForward={vi.fn()}
        onError={vi.fn()}
        renderComposer={({ status }) => {
          renderedStatus = status;
          return <div data-testid="aside-composer-status">{status}</div>;
        }}
      />,
    );

    // Events for `aside-1` arrive BEFORE `startAside` Promise resolves!
    act(() => {
      sessionEventHandler?.({
        kind: "log",
        session_id: "aside-1",
        envelope: {
          seq: 2,
          ts: "2026-11-26T10:00:01.000Z",
          type: "assistant/message",
          data: {
            content: [{ type: "text", text: "主分身正在修改状态层。" }],
            stop_reason: "end_turn",
            model: "claude-sonnet-4-6",
          },
        },
      });
      sessionEventHandler?.({
        kind: "log",
        session_id: "aside-1",
        envelope: {
          seq: 3,
          ts: "2026-11-26T10:00:02.000Z",
          type: "turn/end",
          data: { turn: 1, reason: "completed", error: null },
        },
      });
      sessionEventHandler?.({
        kind: "status",
        session_id: "aside-1",
        status: "idle",
      });
    });

    // Snapshot taken earlier at seq=1 with stale status="running" now resolves:
    await act(async () => {
      resolveStartAside(
        makeMockSessionView({
          id: "aside-1",
          aside: "main-1",
          status: "running",
          events: [
            {
              seq: 1,
              ts: "2026-11-26T10:00:00.000Z",
              type: "user/message",
              data: { text: "它做到哪了？", steering: false },
            },
          ],
        }),
      );
    });

    await waitFor(() => {
      expect(screen.getByText("主分身正在修改状态层。")).toBeTruthy();
      expect(renderedStatus).toBe("idle");
      expect(screen.getByTestId("aside-composer-status").textContent).toBe("idle");
    });
  });

  it("forwards an aside answer back to the main session on click", async () => {
    vi.spyOn(api, "onSessionEvent").mockResolvedValue(() => undefined);
    vi.spyOn(api, "startAside").mockResolvedValue(
      makeMockSessionView({
        id: "aside-2",
        aside: "main-2",
        status: "idle",
        events: [
          {
            seq: 1,
            ts: "2026-11-26T10:00:00.000Z",
            type: "user/message",
            data: { text: "怎么改？", steering: false },
          },
          {
            seq: 2,
            ts: "2026-11-26T10:00:01.000Z",
            type: "assistant/message",
            data: {
              content: [{ type: "text", text: "建议拆成独立 Hook。" }],
              stop_reason: "end_turn",
              model: "claude-sonnet-4-6",
            },
          },
        ],
      }),
    );

    const onForward = vi.fn();
    const main = makeMockSessionItem({ id: "main-2" });
    render(
      <AsidePane
        main={main}
        visible
        sessions={[main]}
        onForward={onForward}
        onError={vi.fn()}
        renderComposer={() => null}
      />,
    );

    const btn = await screen.findByRole("button", { name: /转给主分身/ });
    fireEvent.click(btn);
    expect(onForward).toHaveBeenCalledWith("建议拆成独立 Hook。");
  });

  it("sends starter question on empty aside and resets aside history after confirmation", async () => {
    vi.spyOn(api, "onSessionEvent").mockResolvedValue(() => undefined);
    const sendSpy = vi.spyOn(api, "sendMessage").mockResolvedValue(false);
    const resetSpy = vi.spyOn(api, "resetAside").mockResolvedValue(undefined);
    let callCount = 0;
    vi.spyOn(api, "startAside").mockImplementation(async () => {
      callCount += 1;
      return makeMockSessionView({
        id: `aside-reset-${callCount}`,
        aside: "main-3",
        status: "idle",
        events:
          callCount === 1
            ? [
                {
                  seq: 1,
                  ts: "2026-11-26T10:00:00.000Z",
                  type: "user/message",
                  data: { text: "旧问题", steering: false },
                },
              ]
            : [],
      });
    });

    const main = makeMockSessionItem({ id: "main-3" });
    render(
      <AsidePane
        main={main}
        visible
        sessions={[main]}
        onForward={vi.fn()}
        onError={vi.fn()}
        renderComposer={() => null}
      />,
    );

    const clearTrigger = await screen.findByRole("button", { name: "清空重来" });
    fireEvent.click(clearTrigger);
    fireEvent.click(screen.getByRole("button", { name: "清空" }));

    await waitFor(() => expect(resetSpy).toHaveBeenCalledWith("main-3"));
    const starterBtn = await screen.findByRole("button", { name: "它现在做到哪一步了？" });
    fireEvent.click(starterBtn);
    expect(sendSpy).toHaveBeenCalledWith("aside-reset-2", "它现在做到哪一步了？", [], []);
  });

  it("reports startAside rejection via onError", async () => {
    vi.spyOn(api, "onSessionEvent").mockResolvedValue(() => undefined);
    vi.spyOn(api, "startAside").mockRejectedValue(new Error("aside unavailable"));
    const onError = vi.fn();
    const main = makeMockSessionItem({ id: "main-err" });

    render(
      <AsidePane
        main={main}
        visible
        sessions={[main]}
        onForward={vi.fn()}
        onError={onError}
        renderComposer={() => null}
      />,
    );

    await waitFor(() => expect(onError).toHaveBeenCalledWith(expect.stringContaining("aside unavailable")));
  });
});
