import { MarkdownContent } from "./Markdown";

interface MemoryViewerProps {
  /** Persisted Markdown value from disk. */
  value: string;
  emptyText: string;
}

/**
 * Read-only Markdown viewer for 识海 memory files (`global.md` and project `.md`).
 */
export function MemoryViewer({ value, emptyText }: MemoryViewerProps) {
  return (
    <div className="memory-preview prose">
      {value.trim() ? <MarkdownContent text={value} /> : <p className="memory-empty">{emptyText}</p>}
    </div>
  );
}
