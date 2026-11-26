import { describe, expect, it } from "vitest";
import { detectOutputKind, parseTerminalLines, stripAnsi } from "./terminalOutput";

describe("terminalOutput", () => {
  it("detects normal diff output and classifies hunks, additions, deletions, and separators", () => {
    const lines = [
      "2c2",
      "<     let mut accumulator = StreamAccumulator::default();",
      "---",
      ">     let mut accumulator = ResponsesAccumulator::default();",
      "16,17c16,19",
      "<     let data = old();",
      "---",
      ">     let data = next();",
      "29d21",
    ];
    const kind = detectOutputKind(lines);
    expect(kind).toBe("normal-diff");

    const parsed = parseTerminalLines(lines, kind);
    expect(parsed.map((l) => l.kind)).toEqual([
      "diff-hunk",
      "diff-del",
      "diff-sep",
      "diff-add",
      "diff-hunk",
      "diff-del",
      "diff-sep",
      "diff-add",
      "diff-hunk",
    ]);
    expect(parsed[0].segments[0].text).toBe("@@ -2 +2 @@");
    expect(parsed[1].segments[0].text).toBe("-     let mut accumulator = StreamAccumulator::default();");
    expect(parsed[3].segments[0].text).toBe("+     let mut accumulator = ResponsesAccumulator::default();");
    expect(parsed[4].segments[0].text).toBe("@@ -16,2 +16,4 @@");
    expect(parsed[8].segments[0].text).toBe("@@ -29 +21,0 @@");
  });

  it("detects unified diff output and classifies headers, hunks, additions, and deletions", () => {
    const lines = [
      "diff --git a/src/a.ts b/src/a.ts",
      "--- a/src/a.ts",
      "+++ b/src/a.ts",
      "@@ -1,2 +1,2 @@",
      "-const a = 1;",
      "+const a = 2;",
      " export default a;",
    ];
    const kind = detectOutputKind(lines);
    expect(kind).toBe("unified-diff");

    const parsed = parseTerminalLines(lines, kind);
    expect(parsed.map((l) => l.kind)).toEqual([
      "diff-meta",
      "diff-meta",
      "diff-meta",
      "diff-hunk",
      "diff-del",
      "diff-add",
      "normal",
    ]);
  });

  it("parses ANSI SGR colors across lines and strips non-SGR control codes", () => {
    const lines = ["\x1b[1;31merror:\x1b[0m failed", "\x1b[32mgreen start", "still green\x1b[0m plain", "\x1b[2Kcleaned"];
    const parsed = parseTerminalLines(lines, "plain");

    expect(parsed[0].segments).toEqual([
      { text: "error:", style: { bold: true, fg: 1 } },
      { text: " failed" },
    ]);
    expect(parsed[1].segments).toEqual([{ text: "green start", style: { fg: 2 } }]);
    expect(parsed[2].segments).toEqual([
      { text: "still green", style: { fg: 2 } },
      { text: " plain" },
    ]);
    expect(parsed[3].segments).toEqual([{ text: "cleaned" }]);
    expect(stripAnsi(lines[0])).toBe("error: failed");
  });

  it("highlights plain log errors and warnings when no ANSI styling is present", () => {
    const lines = ["Compiling odg-core", "warning: unused variable", "error[E0308]: mismatched types"];
    const parsed = parseTerminalLines(lines, "plain");
    expect(parsed.map((l) => l.kind)).toEqual(["normal", "log-warn", "log-error"]);
  });

  it("highlights inline file paths with line/col numbers without matching URLs", () => {
    const lines = [
      " --> crates/odg-core/src/tools.rs:120:14",
      "see https://example.com/foo.rs for docs",
    ];
    const parsed = parseTerminalLines(lines, "plain");
    expect(parsed[0].segments).toEqual([
      { text: " --> " },
      { text: "crates/odg-core/src/tools.rs:120:14", token: "path" },
    ]);
    expect(parsed[1].segments).toEqual([{ text: "see https://example.com/foo.rs for docs" }]);
  });
});
