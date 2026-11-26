import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CommandOutput } from "./CommandOutput";

describe("CommandOutput", () => {
  it("renders full command header, diff badge, semantic diff classes, and wrap/copy controls", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    const diffText = ["2c2", "< old line", "---", "> new line"].join("\n");
    const { container } = render(
      <CommandOutput output={diffText} command="diff /tmp/a.txt /tmp/b.txt" cwd="crates/odg-core" />,
    );

    expect(screen.getByText("diff /tmp/a.txt /tmp/b.txt")).toBeTruthy();
    expect(screen.getByText("crates/odg-core")).toBeTruthy();
    expect(screen.getByText("Diff")).toBeTruthy();
    expect(screen.getByText("4 行")).toBeTruthy();

    expect(container.querySelector(".cmd-line.kind-diff-hunk")?.textContent).toContain("@@ -2 +2 @@");
    expect(container.querySelector(".cmd-line.kind-diff-del")).toBeTruthy();
    expect(container.querySelector(".cmd-line.kind-diff-add")).toBeTruthy();
    expect(container.querySelector(".cmd-line.kind-diff-sep")).toBeNull();

    // Diff defaults to horizontal scroll (is-scroll), toggling switches to wrap (is-wrap)
    expect(container.querySelector(".cmd-out.is-scroll")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "不折行" }));
    expect(container.querySelector(".cmd-out.is-wrap")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "复制输出" }));
    expect(writeText).toHaveBeenCalledWith(diffText);
  });

  it("shows expand/collapse toggle when output exceeds threshold lines", () => {
    const longOutput = Array.from({ length: 20 }, (_, i) => `line ${i + 1}`).join("\n");
    const { container } = render(<CommandOutput output={longOutput} command="seq 1 20" />);

    expect(container.querySelector(".cmd-out.is-expanded")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "展开" }));
    expect(container.querySelector(".cmd-out.is-expanded")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "收起" }));
    expect(container.querySelector(".cmd-out.is-expanded")).toBeNull();
  });

  it("toggles line numbers and clicks file paths to open in workbench with line number", () => {
    const onOpenPath = vi.fn();
    const { container } = render(
      <CommandOutput
        output=" --> crates/odg-core/src/lib.rs:42:9"
        command="cargo check"
        onOpenPath={onOpenPath}
      />,
    );

    const pathBtn = screen.getByRole("button", { name: "crates/odg-core/src/lib.rs:42:9" });
    expect(pathBtn.classList.contains("cmd-path-btn")).toBe(true);
    fireEvent.click(pathBtn);
    expect(onOpenPath).toHaveBeenCalledWith("crates/odg-core/src/lib.rs", 42);

    expect(container.querySelector(".cmd-out.hide-line-no")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "行号" }));
    expect(container.querySelector(".cmd-out.hide-line-no")).toBeTruthy();
  });

  it("prevents wheel scroll chaining to parent transcript when terminal output is scrollable", () => {
    const longOutput = Array.from({ length: 40 }, (_, i) => `line ${i + 1}`).join("\n");
    const { container } = render(<CommandOutput output={longOutput} command="seq 1 40" />);
    const scroller = container.querySelector(".cmd-out-scroll") as HTMLDivElement;

    Object.defineProperty(scroller, "scrollHeight", { value: 800, configurable: true });
    Object.defineProperty(scroller, "clientHeight", { value: 340, configurable: true });
    scroller.scrollTop = 0;

    const upAtTop = new WheelEvent("wheel", { deltaY: -40, bubbles: true, cancelable: true });
    scroller.dispatchEvent(upAtTop);
    expect(upAtTop.defaultPrevented).toBe(true);

    const downInside = new WheelEvent("wheel", { deltaY: 40, bubbles: true, cancelable: true });
    scroller.dispatchEvent(downInside);
    expect(downInside.defaultPrevented).toBe(false);
  });
});
