import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import type { PtyInfo, PtyOutputPayload } from "../lib/types";

const termWrites: string[] = [];

vi.mock("@xterm/xterm", () => {
  class MockTerminal {
    cols = 80;
    rows = 24;
    options: Record<string, unknown> = {};
    loadAddon() {}
    open() {}
    write(data: string) {
      termWrites.push(data);
    }
    onData() {
      return { dispose() {} };
    }
    onSelectionChange() {
      return { dispose() {} };
    }
    getSelection() {
      return "";
    }
    focus() {}
    clear() {}
    dispose() {}
  }
  return { Terminal: MockTerminal };
});

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/addon-web-links", () => ({
  WebLinksAddon: class {},
}));

import TerminalPane, { filterIncrementalPtyOutput } from "./TerminalPane";

describe("TerminalPane session guard & offset deduplication (P1-15 & P2-4)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    termWrites.length = 0;
  });

  it("deduplicates overlapping backlog offsets and slices partial chunks without dropping bytes", async () => {
    let outputHandler: ((p: PtyOutputPayload) => void) | null = null;
    vi.spyOn(api, "onPtyOutput").mockImplementation(async (fn) => {
      outputHandler = fn;
      return () => undefined;
    });
    vi.spyOn(api, "onPtyExit").mockResolvedValue(() => undefined);
    vi.spyOn(api, "ptyResize").mockResolvedValue(undefined);

    let resolveList!: (list: PtyInfo[]) => void;
    vi.spyOn(api, "ptyList").mockImplementation(
      () =>
        new Promise<PtyInfo[]>((res) => {
          resolveList = res;
        }),
    );

    render(<TerminalPane sessionId="sess-1" onSendToChat={vi.fn()} onError={vi.fn()} />);

    // While `ptyList` is in flight, two chunks arrive: 80..100 (already in backlog) and 100..150 (fresh delta).
    const backlogText = "A".repeat(100);
    const overlapChunk = "A".repeat(20);
    const freshChunk = "B".repeat(50);

    act(() => {
      outputHandler?.({
        id: "pty-1",
        session_id: "sess-1",
        data: overlapChunk,
        start_offset: 80,
        end_offset: 100,
      });
      outputHandler?.({
        id: "pty-1",
        session_id: "sess-1",
        data: freshChunk,
        start_offset: 100,
        end_offset: 150,
      });
    });

    await act(async () => {
      resolveList([
        {
          id: "pty-1",
          session_id: "sess-1",
          title: "zsh 1",
          shell: "/bin/zsh",
          cwd: "/ws",
          exited: null,
          backlog: backlogText,
          backlog_end_offset: 100,
        },
      ]);
    });

    await waitFor(() => {
      expect(screen.getByRole("tab", { name: "zsh 1" })).toBeTruthy();
    });
    expect(termWrites.join("")).toBe(backlogText + freshChunk);

    // Also verify partial UTF-8 byte overlap slicing in pure helper
    const partial = filterIncrementalPtyOutput(
      { backlog: "hello ", backlog_end_offset: 6 },
      [{ id: "p", session_id: "s", data: "hello world", start_offset: 0, end_offset: 11 }],
    );
    expect(partial).toBe("world");
  });

  it("closes orphan initial PTY and never pollutes new session tabs when switching sessions mid-spawn", async () => {
    vi.spyOn(api, "onPtyOutput").mockResolvedValue(() => undefined);
    vi.spyOn(api, "onPtyExit").mockResolvedValue(() => undefined);
    vi.spyOn(api, "ptyResize").mockResolvedValue(undefined);
    const closeSpy = vi.spyOn(api, "ptyClose").mockResolvedValue(undefined);

    let resolveSpawnA!: (info: PtyInfo) => void;
    vi.spyOn(api, "ptyList").mockImplementation(async (id) => {
      if (id === "sess-A") return [];
      return [
        {
          id: "pty-B",
          session_id: "sess-B",
          title: "zsh B",
          shell: "/bin/zsh",
          cwd: "/ws/B",
          exited: null,
          backlog: "$ ",
          backlog_end_offset: 2,
        },
      ];
    });
    vi.spyOn(api, "ptySpawn").mockImplementation(
      () =>
        new Promise<PtyInfo>((res) => {
          resolveSpawnA = res;
        }),
    );

    const { rerender } = render(<TerminalPane sessionId="sess-A" onSendToChat={vi.fn()} onError={vi.fn()} />);
    await waitFor(() => expect(api.ptySpawn).toHaveBeenCalledWith("sess-A", 80, 24));

    // User switches rapidly to sess-B before sess-A's initial ptySpawn resolves!
    rerender(<TerminalPane sessionId="sess-B" onSendToChat={vi.fn()} onError={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("tab", { name: "zsh B" })).toBeTruthy());

    // Now sess-A's late spawn arrives: must be closed immediately and MUST NOT appear in sess-B's tabs!
    await act(async () => {
      resolveSpawnA({
        id: "pty-orphan-A",
        session_id: "sess-A",
        title: "zsh orphan A",
        shell: "/bin/zsh",
        cwd: "/ws/A",
        exited: null,
        backlog: "",
        backlog_end_offset: 0,
      });
    });

    expect(closeSpy).toHaveBeenCalledWith("pty-orphan-A");
    expect(screen.queryByRole("tab", { name: "zsh orphan A" })).toBeNull();
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  it("closes orphan initial PTY when TerminalPane unmounts before ptySpawn resolves", async () => {
    vi.spyOn(api, "onPtyOutput").mockResolvedValue(() => undefined);
    vi.spyOn(api, "onPtyExit").mockResolvedValue(() => undefined);
    vi.spyOn(api, "ptyResize").mockResolvedValue(undefined);
    vi.spyOn(api, "ptyList").mockResolvedValue([]);
    const closeSpy = vi.spyOn(api, "ptyClose").mockResolvedValue(undefined);

    let resolveSpawn!: (info: PtyInfo) => void;
    vi.spyOn(api, "ptySpawn").mockImplementation(
      () =>
        new Promise<PtyInfo>((res) => {
          resolveSpawn = res;
        }),
    );

    const { unmount } = render(<TerminalPane sessionId="sess-unmount" onSendToChat={vi.fn()} onError={vi.fn()} />);
    await waitFor(() => expect(api.ptySpawn).toHaveBeenCalledTimes(1));

    unmount();

    await act(async () => {
      resolveSpawn({
        id: "pty-after-unmount",
        session_id: "sess-unmount",
        title: "zsh 1",
        shell: "/bin/zsh",
        cwd: "/ws",
        exited: null,
        backlog: "",
        backlog_end_offset: 0,
      });
    });

    expect(closeSpy).toHaveBeenCalledWith("pty-after-unmount");
  });

  it("filterIncrementalPtyOutput safely handles legacy chunks without offsets and multi-byte UTF-8 overlap", () => {
    // Legacy payload without offsets avoids duplicating non-empty backlog on reopen, while replaying on fresh spawn
    const reopenLegacy = filterIncrementalPtyOutput(
      { backlog: "$ " },
      [{ id: "p1", session_id: "s1", data: "$ " }],
      false,
    );
    expect(reopenLegacy).toBe("");

    const spawnLegacy = filterIncrementalPtyOutput(
      { backlog: "" },
      [{ id: "p1", session_id: "s1", data: "ls -la\r\n" }],
      true,
    );
    expect(spawnLegacy).toBe("ls -la\r\n");

    // Multi-byte UTF-8 characters ("修真" = 6 bytes in UTF-8): backlog_end_offset = 3 ("修") slices out "真\n"
    const utf8 = filterIncrementalPtyOutput(
      { backlog: "修", backlog_end_offset: 3 },
      [{ id: "p2", session_id: "s1", data: "修真\n", start_offset: 0, end_offset: 7 }],
    );
    expect(utf8).toBe("真\n");
  });
});
