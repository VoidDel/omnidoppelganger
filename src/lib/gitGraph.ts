import type { GitCommitItem } from "./types";

/** Harmonious VS Code–style branch lane colors matching light & dark themes. */
export const GRAPH_COLORS = [
  "#2b7a69", // jade
  "#3b82f6", // blue
  "#d9a441", // amber
  "#8b5cf6", // violet
  "#e0584e", // cinnabar
  "#0d9488", // teal
  "#ec4899", // rose
  "#64748b", // slate
];

export interface GitRefChip {
  label: string;
  kind: "head" | "branch" | "remote" | "tag";
}

export interface GraphEdge {
  fromCol: number;
  toCol: number;
  color: string;
}

export interface GraphRow {
  commit: GitCommitItem;
  col: number;
  color: string;
  isHead: boolean;
  isMerge: boolean;
  refs: GitRefChip[];
  /** Segments entering from top edge (y=0) into this commit's node (y=mid). */
  inEdges: GraphEdge[];
  /** Segments leaving this commit's node (y=mid) toward the bottom edge (y=height). */
  outEdges: GraphEdge[];
  /** Straight vertical pass-through lines from y=0 to y=height for untouched lanes. */
  passCols: { col: number; color: string }[];
  /** Continuation vertical lines (y=0..height) for the expanded commit-files area underneath this row. */
  afterCols: { col: number; color: string }[];
}

export interface GitGraphLayout {
  rows: GraphRow[];
  maxCols: number;
}

/** Parse Git `%D` ref decorations (e.g. `HEAD -> main, origin/main, tag: v0.7.16`) into display chips. */
export function parseCommitRefs(raw: string): { isHead: boolean; chips: GitRefChip[] } {
  const trimmed = raw.trim();
  if (!trimmed) return { isHead: false, chips: [] };
  let isHead = false;
  const chips: GitRefChip[] = [];
  for (const part of trimmed.split(",").map((s) => s.trim()).filter(Boolean)) {
    if (part === "HEAD") {
      isHead = true;
      chips.push({ label: "HEAD", kind: "head" });
    } else if (part.startsWith("HEAD -> ")) {
      isHead = true;
      chips.push({ label: part.slice("HEAD -> ".length).trim(), kind: "head" });
    } else if (part.startsWith("tag: ")) {
      chips.push({ label: part.slice("tag: ".length).trim(), kind: "tag" });
    } else if (/^(?:origin|upstream|remotes)\//.test(part)) {
      // Skip redundant `origin/HEAD` pointer noise.
      if (part.endsWith("/HEAD")) continue;
      chips.push({ label: part, kind: "remote" });
    } else {
      chips.push({ label: part, kind: "branch" });
    }
  }
  return { isHead, chips };
}

interface LaneSlot {
  hash: string;
  colorIdx: number;
}

/**
 * Compute a deterministic VS Code–style swimlane graph layout for a topologically
 * ordered list of Git commits.
 */
export function layoutGitGraph(commits: GitCommitItem[]): GitGraphLayout {
  const lanes: (LaneSlot | null)[] = [];
  let nextColor = 0;
  let maxCols = 1;
  const rows: GraphRow[] = [];

  const allocSlot = (hash: string, preferredColor?: number): { col: number; colorIdx: number } => {
    const colorIdx = preferredColor ?? nextColor++ % GRAPH_COLORS.length;
    const emptyAt = lanes.findIndex((s) => s === null);
    if (emptyAt >= 0) {
      lanes[emptyAt] = { hash, colorIdx };
      return { col: emptyAt, colorIdx };
    }
    lanes.push({ hash, colorIdx });
    return { col: lanes.length - 1, colorIdx };
  };

  for (const commit of commits) {
    // 1. Find all incoming lanes waiting for `commit.hash`.
    const matchingCols: number[] = [];
    for (let i = 0; i < lanes.length; i++) {
      if (lanes[i]?.hash === commit.hash) matchingCols.push(i);
    }

    let col: number;
    let colorIdx: number;
    const inEdges: GraphEdge[] = [];

    if (matchingCols.length > 0) {
      col = matchingCols[0];
      colorIdx = lanes[col]!.colorIdx;
      for (const fromCol of matchingCols) {
        inEdges.push({ fromCol, toCol: col, color: GRAPH_COLORS[lanes[fromCol]!.colorIdx] });
        if (fromCol !== col) {
          // Collapse duplicate lanes pointing to the same commit.
          lanes[fromCol] = null;
        }
      }
    } else {
      const allocated = allocSlot(commit.hash);
      col = allocated.col;
      colorIdx = allocated.colorIdx;
    }

    // Untouched lanes pass straight through from top to bottom.
    const passCols: { col: number; color: string }[] = [];
    for (let i = 0; i < lanes.length; i++) {
      if (i !== col && lanes[i] !== null) {
        passCols.push({ col: i, color: GRAPH_COLORS[lanes[i]!.colorIdx] });
      }
    }

    // 2. Wire outgoing edges to parents.
    const parents = commit.parents ?? [];
    const outEdges: GraphEdge[] = [];

    if (parents.length === 0) {
      // Root commit terminates its lane.
      lanes[col] = null;
    } else {
      const firstParent = parents[0];
      const existingFirst = lanes.findIndex((s, idx) => idx !== col && s?.hash === firstParent);
      if (existingFirst >= 0) {
        // First parent is already tracked on another lane: curve into it and free `col`.
        outEdges.push({ fromCol: col, toCol: existingFirst, color: GRAPH_COLORS[colorIdx] });
        lanes[col] = null;
      } else {
        // Continue straight down on the same lane.
        lanes[col] = { hash: firstParent, colorIdx };
        outEdges.push({ fromCol: col, toCol: col, color: GRAPH_COLORS[colorIdx] });
      }

      // Secondary parents (merge commits).
      for (let p = 1; p < parents.length; p++) {
        const parentHash = parents[p];
        const existing = lanes.findIndex((s) => s?.hash === parentHash);
        if (existing >= 0) {
          outEdges.push({ fromCol: col, toCol: existing, color: GRAPH_COLORS[lanes[existing]!.colorIdx] });
        } else {
          const spawned = allocSlot(parentHash);
          outEdges.push({ fromCol: col, toCol: spawned.col, color: GRAPH_COLORS[spawned.colorIdx] });
        }
      }
    }

    // Trim trailing empty slots to keep the rail compact.
    while (lanes.length > 0 && lanes[lanes.length - 1] === null) {
      lanes.pop();
    }

    const activeWidth = Math.max(
      col + 1,
      lanes.length,
      ...inEdges.map((e) => Math.max(e.fromCol, e.toCol) + 1),
      ...outEdges.map((e) => Math.max(e.fromCol, e.toCol) + 1),
      ...passCols.map((p) => p.col + 1),
    );
    if (activeWidth > maxCols) maxCols = activeWidth;

    const afterCols: { col: number; color: string }[] = [];
    for (let i = 0; i < lanes.length; i++) {
      if (lanes[i] !== null) {
        afterCols.push({ col: i, color: GRAPH_COLORS[lanes[i]!.colorIdx] });
      }
    }

    const { isHead, chips } = parseCommitRefs(commit.refs);
    rows.push({
      commit,
      col,
      color: GRAPH_COLORS[colorIdx],
      isHead,
      isMerge: parents.length > 1,
      refs: chips,
      inEdges,
      outEdges,
      passCols,
      afterCols,
    });
  }

  return { rows, maxCols };
}
