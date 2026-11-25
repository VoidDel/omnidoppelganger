import { describe, expect, it } from "vitest";
import { layoutGitGraph, parseCommitRefs } from "./gitGraph";
import type { GitCommitItem } from "./types";

const makeCommit = (hash: string, parents: string[], refs = "", subject = hash): GitCommitItem => ({
  hash,
  short_hash: hash.slice(0, 7),
  parents,
  author: "dev",
  timestamp: 1700000000,
  refs,
  subject,
});

describe("gitGraph", () => {
  it("parses HEAD, branches, remotes, and tags cleanly", () => {
    const parsed = parseCommitRefs("HEAD -> main, origin/main, origin/HEAD, tag: v0.7.16, feat/graph");
    expect(parsed.isHead).toBe(true);
    expect(parsed.chips).toEqual([
      { label: "main", kind: "head" },
      { label: "origin/main", kind: "remote" },
      { label: "v0.7.16", kind: "tag" },
      { label: "feat/graph", kind: "branch" },
    ]);
  });

  it("lays out a merge commit with two lanes and collapses back to one lane at the common ancestor", () => {
    // M merges A and B, both descended from R.
    const commits = [
      makeCommit("M", ["A", "B"], "HEAD -> main"),
      makeCommit("B", ["R"], "feat/x"),
      makeCommit("A", ["R"]),
      makeCommit("R", []),
    ];
    const layout = layoutGitGraph(commits);
    expect(layout.maxCols).toBe(2);
    expect(layout.rows[0].col).toBe(0);
    expect(layout.rows[0].isHead).toBe(true);
    expect(layout.rows[0].isMerge).toBe(true);
    expect(layout.rows[0].outEdges.length).toBe(2);

    // B sits on lane 1 while lane 0 (A) passes straight through.
    expect(layout.rows[1].col).toBe(1);
    expect(layout.rows[1].passCols.map((p) => p.col)).toEqual([0]);

    // A sits on lane 0 while lane 1 (R) passes straight through, then curves into R.
    expect(layout.rows[2].col).toBe(0);

    // Root R terminates cleanly with no outgoing edges.
    expect(layout.rows[3].outEdges).toEqual([]);
  });
});
