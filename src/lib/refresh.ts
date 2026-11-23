import type { FileView, GitDiffView, TreeEntry } from "./types";

/**
 * Whether a re-read file shows the same as what is on screen. A background refresh that finds nothing changed keeps
 * the old view object, so the code is not highlighted again and nothing on screen moves.
 */
export function sameFileView(a: FileView, b: FileView): boolean {
  if (a.kind !== b.kind || a.path !== b.path || a.size !== b.size || a.modified_ms !== b.modified_ms) return false;
  if (a.kind === "text" && b.kind === "text") return a.content === b.content && a.eol === b.eol && a.language === b.language;
  if (a.kind === "document" && b.kind === "document") return a.content === b.content;
  if (a.kind === "image" && b.kind === "image") return a.data === b.data;
  if (a.kind === "binary" && b.kind === "binary") return a.reason === b.reason;
  return true;
}

/** Whether a re-computed diff is the same as the one on screen. */
export function sameDiffView(a: GitDiffView, b: GitDiffView): boolean {
  if (a.path !== b.path || a.stage !== b.stage || a.commit !== b.commit || a.binary !== b.binary) return false;
  if (a.added !== b.added || a.removed !== b.removed || a.hunks.length !== b.hunks.length) return false;
  return a.hunks.every((hunk, at) => {
    const other = b.hunks[at];
    return (
      hunk.header === other.header &&
      hunk.lines.length === other.lines.length &&
      hunk.lines.every((line, i) => line.kind === other.lines[i].kind && line.text === other.lines[i].text && line.new_line === other.lines[i].new_line && line.old_line === other.lines[i].old_line)
    );
  });
}

/** Whether two listings of a directory hold the same entries in the same order, with the same flags. */
export function sameEntries(a: TreeEntry[] | undefined, b: TreeEntry[]): boolean {
  if (!a || a.length !== b.length) return false;
  return a.every((entry, at) => {
    const other = b[at];
    return entry.path === other.path && entry.kind === other.kind && entry.expandable === other.expandable && entry.ignored === other.ignored && entry.size === other.size && entry.modified_ms === other.modified_ms;
  });
}
