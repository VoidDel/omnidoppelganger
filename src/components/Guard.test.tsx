import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Suspense } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setCurrentLocale } from "../i18n";
import { Guard, lazyWithRetry } from "./Guard";

describe("Guard error boundary, resetKey, onDismiss & lazyWithRetry self-healing (P3-4)", () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    setCurrentLocale("zh-CN");
    consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => undefined);
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
  });

  it("re-invokes importer() on retry after an initial dynamic import() failure and self-heals", async () => {
    let attempts = 0;
    const importer = vi.fn(() => {
      attempts += 1;
      if (attempts === 1) {
        return Promise.reject(new Error("Chunk network glitch"));
      }
      return Promise.resolve({
        default: () => <div>懒加载自愈成功</div>,
      });
    });

    const FlakyLazy = lazyWithRetry(importer);

    render(
      <Guard name="测试面板">
        <Suspense fallback={<div>加载中…</div>}>
          <FlakyLazy />
        </Suspense>
      </Guard>,
    );

    await waitFor(() => {
      expect(screen.getByText("「测试面板」这一块画不出来了")).toBeTruthy();
    });
    expect(importer).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "再画一次" }));

    await waitFor(() => {
      expect(screen.getByText("懒加载自愈成功")).toBeTruthy();
    });
    expect(importer).toHaveBeenCalledTimes(2);
  });

  it("clears captured error automatically when resetKey changes and supports onDismiss for modal overlays", () => {
    let shouldThrow = true;
    function Boom() {
      if (shouldThrow) throw new Error("render boom");
      return <div>正常渲染</div>;
    }
    const onDismiss = vi.fn();

    const { rerender } = render(
      <Guard name="快速跳转面板" resetKey="k1" onDismiss={onDismiss}>
        <Boom />
      </Guard>,
    );

    expect(screen.getByText("「快速跳转面板」这一块画不出来了")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "关闭此面板" }));
    expect(onDismiss).toHaveBeenCalledTimes(1);

    // Changing resetKey recovers automatically once the underlying state changes
    shouldThrow = false;
    rerender(
      <Guard name="快速跳转面板" resetKey="k2" onDismiss={onDismiss}>
        <Boom />
      </Guard>,
    );
    expect(screen.getByText("正常渲染")).toBeTruthy();
  });
});
