import { describe, expect, it } from "vitest";
import { sameDiffView, sameEntries, sameFileView } from "./refresh";
import type { FileView, GitDiffView, TreeEntry } from "./types";

const text = (content: string, modified = 1): FileView => ({
  kind: "text",
  path: "src/a.rs",
  name: "a.rs",
  language: "rust",
  content,
  lines: content.split("\n").length,
  size: content.length,
  modified_ms: modified,
  eol: "LF",
  svg_data: null,
});

const diff = (texts: string[]): GitDiffView => ({
  path: "src/a.rs",
  name: "a.rs",
  language: "rust",
  stage: "unstaged",
  commit: null,
  binary: false,
  added: texts.length,
  removed: 0,
  hunks: [{ header: "@@ -0,0 +1 @@", old_start: 0, old_lines: 0, new_start: 1, new_lines: texts.length, lines: texts.map((t, i) => ({ kind: "+" as const, old_line: null, new_line: i + 1, text: t })) }],
});

const entry = (path: string, modified = 1): TreeEntry => ({ name: path, path, kind: "file", expandable: false, size: 3, modified_ms: modified, ignored: false });

describe("background refresh comparisons", () => {
  it("keeps a file view whose contents did not change and replaces one that did", () => {
    expect(sameFileView(text("fn a() {}"), text("fn a() {}"))).toBe(true);
    expect(sameFileView(text("fn a() {}"), text("fn b() {}"))).toBe(false);
    expect(sameFileView(text("fn a() {}", 1), text("fn a() {}", 2))).toBe(false);
  });

  it("tells a changed diff from an unchanged one line by line", () => {
    expect(sameDiffView(diff(["one", "two"]), diff(["one", "two"]))).toBe(true);
    expect(sameDiffView(diff(["one", "two"]), diff(["one", "three"]))).toBe(false);
    expect(sameDiffView(diff(["one"]), diff(["one", "two"]))).toBe(false);
  });

  it("compares directory listings entry by entry", () => {
    expect(sameEntries([entry("a"), entry("b")], [entry("a"), entry("b")])).toBe(true);
    expect(sameEntries([entry("a")], [entry("a"), entry("b")])).toBe(false);
    expect(sameEntries([entry("a", 1)], [entry("a", 2)])).toBe(false);
    expect(sameEntries(undefined, [])).toBe(false);
  });
});
