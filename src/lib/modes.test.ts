import { describe, expect, it } from "vitest";
import { modeHintFor, modeOf, nextMode } from "./modes";

describe("modes", () => {
  it("cycles only the four standard modes on Shift+Tab", () => {
    expect(nextMode("manual")).toBe("edit");
    expect(nextMode("edit")).toBe("plan");
    expect(nextMode("plan")).toBe("auto");
    expect(nextMode("auto")).toBe("manual");
    expect(nextMode("bypass")).toBe("manual");
  });

  it("returns accurate headless CLI mode hints per backend", () => {
    expect(modeHintFor("edit", null)).toBe(modeOf("edit").hint);
    expect(modeHintFor("manual", "claude_code")).toContain("只读不改");
    expect(modeHintFor("edit", "cursor")).toContain("不自动放行命令");
    expect(modeHintFor("edit", "codex")).toContain("workspace-write");
    expect(modeHintFor("auto", "grok")).toContain("自动/审核模式");
    expect(modeHintFor("bypass", "antigravity")).toContain("跳过本机 CLI 的全部权限询问");
  });
});
