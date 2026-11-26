import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import { CodeViewer, LINE_HEIGHT } from "./CodeViewer";

describe("CodeViewer virtual scrolling & search (P1-15 & P2-12)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("mounts fewer than 250 .code-ln nodes for a 30,000-line file and supports line jumping", async () => {
    const thirtyThousandLines = Array.from(
      { length: 30_000 },
      (_, idx) => `export const item_${idx + 1} = ${idx + 1};`,
    ).join("\n");

    vi.spyOn(api, "readWorkspaceFile").mockResolvedValue({
      kind: "text",
      path: "src/huge.ts",
      name: "huge.ts",
      language: "typescript",
      content: thirtyThousandLines,
      lines: 30_000,
      size: thirtyThousandLines.length,
      modified_ms: 1700000000000,
      eol: "LF",
      svg_data: null,
    });

    const { container, rerender } = render(
      <CodeViewer
        sessionId="sess-code"
        path="src/huge.ts"
        onQuoteToMain={vi.fn()}
        onAttachPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => {
      expect(container.querySelector(".code-lines")).not.toBeNull();
    });

    const initialLineNodes = container.querySelectorAll(".code-ln");
    expect(initialLineNodes.length).toBeGreaterThan(0);
    expect(initialLineNodes.length).toBeLessThan(250);
    expect(container.querySelector('[data-line="1"]')).not.toBeNull();
    expect(container.querySelector('[data-line="25000"]')).toBeNull();

    // Jump to line 25,000 via targetLine + jumpSeq
    rerender(
      <CodeViewer
        sessionId="sess-code"
        path="src/huge.ts"
        targetLine={25_000}
        jumpSeq={1}
        onQuoteToMain={vi.fn()}
        onAttachPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => {
      const targetBtn = container.querySelector('.code-ln[data-line="25000"]');
      expect(targetBtn).not.toBeNull();
      expect(targetBtn?.classList.contains("selected")).toBe(true);
    });

    const jumpedLineNodes = container.querySelectorAll(".code-ln");
    expect(jumpedLineNodes.length).toBeLessThan(250);
    const scroller = container.querySelector(".viewer-body") as HTMLDivElement;
    expect(scroller.scrollTop).toBeGreaterThan((25_000 - 50) * LINE_HEIGHT);
  });

  it("highlights in-file search matches and navigates across virtualized lines", async () => {
    const content = Array.from({ length: 2_000 }, (_, idx) =>
      idx + 1 === 15 || idx + 1 === 1_850 ? `const NEEDLE_TOKEN = ${idx + 1};` : `const filler_${idx + 1} = 0;`,
    ).join("\n");

    vi.spyOn(api, "readWorkspaceFile").mockResolvedValue({
      kind: "text",
      path: "src/search.ts",
      name: "search.ts",
      language: "typescript",
      content,
      lines: 2_000,
      size: content.length,
      modified_ms: null,
      eol: "LF",
      svg_data: null,
    });

    const { container } = render(
      <CodeViewer
        sessionId="sess-code"
        path="src/search.ts"
        onQuoteToMain={vi.fn()}
        onAttachPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => expect(container.querySelector(".code-lines")).not.toBeNull());

    fireEvent.click(screen.getByTitle("在文件内查找 (Ctrl/Cmd+F)"));
    const searchInput = screen.getByPlaceholderText("在当前文件内查找…");
    fireEvent.change(searchInput, { target: { value: "NEEDLE_TOKEN" } });

    await waitFor(() => {
      expect(screen.getByText("1 / 2")).toBeTruthy();
      expect(container.querySelector('.code-ln[data-line="15"]')?.classList.contains("active-match")).toBe(true);
    });

    // Press Enter to jump to second match at line 1850
    fireEvent.keyDown(searchInput, { key: "Enter" });
    await waitFor(() => {
      expect(screen.getByText("2 / 2")).toBeTruthy();
      expect(container.querySelector('.code-ln[data-line="1850"]')?.classList.contains("active-match")).toBe(true);
    });
    expect(container.querySelectorAll(".code-ln").length).toBeLessThan(250);
  });

  it("selects a continuous line range via Shift-click and quotes selected lines to main and aside", async () => {
    const content = ["line one", "line two", "line three", "line four", "line five"].join("\n");
    vi.spyOn(api, "readWorkspaceFile").mockResolvedValue({
      kind: "text",
      path: "src/range.ts",
      name: "range.ts",
      language: "typescript",
      content,
      lines: 5,
      size: content.length,
      modified_ms: null,
      eol: "LF",
      svg_data: null,
    });

    const onQuoteToMain = vi.fn();
    const onQuoteToAside = vi.fn();
    const { container } = render(
      <CodeViewer
        sessionId="sess-code"
        path="src/range.ts"
        onQuoteToMain={onQuoteToMain}
        onQuoteToAside={onQuoteToAside}
        onAttachPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => expect(container.querySelector('.code-ln[data-line="2"]')).not.toBeNull());
    fireEvent.click(container.querySelector('.code-ln[data-line="2"]')!);
    fireEvent.click(container.querySelector('.code-ln[data-line="4"]')!, { shiftKey: true });

    expect(screen.getByText("第 2–4 行")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "引用到分身" }));
    expect(onQuoteToMain).toHaveBeenCalledWith(
      expect.objectContaining({
        content: expect.stringContaining("line two"),
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "旁聊提问" }));
    expect(onQuoteToAside).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByText("第 2–4 行")).toBeNull();
  });

  it("defaults markdown files to preview mode and toggles between preview and source code", async () => {
    const md = "# 标题\n\n正文内容";
    vi.spyOn(api, "readWorkspaceFile").mockResolvedValue({
      kind: "text",
      path: "docs/README.md",
      name: "README.md",
      language: "markdown",
      content: md,
      lines: 3,
      size: md.length,
      modified_ms: null,
      eol: "LF",
      svg_data: null,
    });

    const onAttachPath = vi.fn();
    const { container } = render(
      <CodeViewer
        sessionId="sess-code"
        path="docs/README.md"
        onQuoteToMain={vi.fn()}
        onAttachPath={onAttachPath}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getByRole("heading", { name: "标题" })).toBeTruthy());
    expect(container.querySelector(".code-lines")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "源码" }));
    await waitFor(() => expect(container.querySelector(".code-lines")).not.toBeNull());

    fireEvent.click(screen.getByTitle("附入对话"));
    expect(onAttachPath).toHaveBeenCalledWith("docs/README.md");
  });

  it("shows 无匹配 when search query is absent and closes search bar on Escape", async () => {
    vi.spyOn(api, "readWorkspaceFile").mockResolvedValue({
      kind: "text",
      path: "src/tiny.ts",
      name: "tiny.ts",
      language: "typescript",
      content: "export const ok = true;",
      lines: 1,
      size: 23,
      modified_ms: null,
      eol: "LF",
      svg_data: null,
    });

    const { container } = render(
      <CodeViewer
        sessionId="sess-code"
        path="src/tiny.ts"
        onQuoteToMain={vi.fn()}
        onAttachPath={vi.fn()}
        onError={vi.fn()}
      />,
    );

    await waitFor(() => expect(container.querySelector(".code-lines")).not.toBeNull());
    fireEvent.click(screen.getByTitle("在文件内查找 (Ctrl/Cmd+F)"));
    const searchInput = screen.getByPlaceholderText("在当前文件内查找…");
    fireEvent.change(searchInput, { target: { value: "MISSING_SYMBOL" } });
    expect(screen.getByText("无匹配")).toBeTruthy();

    fireEvent.keyDown(searchInput, { key: "Escape" });
    expect(screen.queryByPlaceholderText("在当前文件内查找…")).toBeNull();
  });
});
