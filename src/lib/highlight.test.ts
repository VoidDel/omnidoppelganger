import { describe, expect, it } from "vitest";
import { findInLines, highlightLines, splitHighlightedHtmlByLine } from "./highlight";

describe("highlight", () => {
  it("balances multi-line span tags across line boundaries", () => {
    const html = '<span class="hljs-comment">/* first\nsecond\nthird */</span>\nconst x = 1;';
    const lines = splitHighlightedHtmlByLine(html);
    expect(lines).toEqual([
      '<span class="hljs-comment">/* first</span>',
      '<span class="hljs-comment">second</span>',
      '<span class="hljs-comment">third */</span>',
      "const x = 1;",
    ]);
  });

  it("highlights known languages and escapes unknown ones safely", () => {
    const rust = highlightLines("fn main() {\n  let x = 42;\n}", "rust");
    expect(rust).toHaveLength(3);
    expect(rust[0]).toContain("hljs-keyword");
    expect(rust[1]).toContain("42");

    const unknown = highlightLines("<script>alert(1)</script>", "no-such-lang");
    expect(unknown).toEqual(["&lt;script&gt;alert(1)&lt;/script&gt;"]);
  });

  it("finds case-insensitive line matches with occurrence counts", () => {
    const matches = findInLines(["Alpha beta", "nothing here", "ALPHA and alpha"], "alpha");
    expect(matches).toEqual([
      { line: 1, count: 1 },
      { line: 3, count: 2 },
    ]);
  });
});
