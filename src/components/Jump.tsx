import { useEffect, useRef, useState } from "react";
import type { FoundFile, SessionItem } from "../lib/types";
import { ago } from "../lib/when";
import { Light } from "./Light";
import { Icon } from "./Icon";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;
const titleOf = (session: SessionItem) => session.title || (session.parent ? "子分身" : "新分身");

/** Somewhere to go: a 分身, a workspace file, or a page/act of the app's own. */
type Place =
  | { kind: "session"; session: SessionItem }
  | { kind: "file"; file: FoundFile; line?: number }
  | { kind: "act"; id: string; name: string; hint: string; seal: string };

interface JumpProps {
  mode?: "all" | "files";
  sessions: SessionItem[];
  /** The places that are not 分身: what each is called, and what it does. */
  acts: { id: string; name: string; hint: string; seal: string; run: () => void }[];
  onOpen: (id: string) => void;
  /** Ids of the sessions that hold the text in what was said in them, as the list's own search finds them. */
  onSearch: (query: string) => Promise<string[]>;
  /** Optional workspace file search (active when a project 洞府 is open). */
  onSearchFiles?: (query: string) => Promise<FoundFile[]>;
  onOpenFile?: (relPath: string, line?: number) => void;
  onClose: () => void;
}

/**
 * Ctrl+K (all places) & Ctrl+P (workspace files): a line to type in, and under it every place there is to go,
 * narrowed as one types. Supports `path:line` (e.g. `App.tsx:120`) to jump directly to a line in the code viewer.
 */
export function Jump({ mode = "all", sessions, acts, onOpen, onSearch, onSearchFiles, onOpenFile, onClose }: JumpProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [at, setAt] = useState(0);
  /** The sessions whose conversations hold the words, once the search has answered. */
  const [said, setSaid] = useState<Set<string>>(new Set());
  const [matchedFiles, setMatchedFiles] = useState<FoundFile[]>([]);
  const now = new Date();

  const parsed = /^(.*?)(?::(\d+))?$/.exec(query.trim());
  const fileQuery = (parsed?.[1] ?? query).trim();
  const targetLine = parsed?.[2] ? Number(parsed[2]) : undefined;
  const filesOnly = mode === "files";

  useEffect(() => {
    dialog.current?.showModal();
    input.current?.focus();
  }, []);

  useEffect(() => {
    setAt(0);
    setSaid(new Set());
    if (filesOnly || !query.trim()) return;
    let current = true;
    const timer = setTimeout(() => onSearch(query).then((ids) => current && setSaid(new Set(ids)), () => undefined), 160);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [query, filesOnly, onSearch]);

  useEffect(() => {
    if (!onSearchFiles) {
      setMatchedFiles([]);
      return;
    }
    if (!filesOnly && !fileQuery) {
      setMatchedFiles([]);
      return;
    }
    let current = true;
    const delay = fileQuery ? 100 : 0;
    const timer = setTimeout(() => {
      onSearchFiles(fileQuery).then(
        (items) => current && setMatchedFiles(items.slice(0, filesOnly ? 50 : 8)),
        () => current && setMatchedFiles([]),
      );
    }, delay);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [fileQuery, filesOnly, onSearchFiles]);

  const words = query.trim().toLowerCase();
  const holds = (text: string) => text.toLowerCase().includes(words);

  const filePlaces: Place[] = matchedFiles.map((file) => ({ kind: "file" as const, file, line: targetLine }));
  const pages = filesOnly
    ? []
    : acts.filter((act) => !words || holds(act.name) || holds(act.hint)).map(({ run: _, ...act }) => ({ kind: "act" as const, ...act }));
  const places: Place[] = filesOnly
    ? filePlaces
    : [
        ...filePlaces,
        ...sessions
          .filter((session) => !words || holds(titleOf(session)) || holds(homeOf(session)) || said.has(session.id))
          .slice(0, Math.max(5, 30 - pages.length - filePlaces.length))
          .map((session) => ({ kind: "session" as const, session })),
        ...pages,
      ];

  useEffect(() => {
    if (at >= places.length) setAt(Math.max(0, places.length - 1));
  }, [at, places.length]);

  const go = (place: Place | undefined) => {
    if (!place) return;
    onClose();
    if (place.kind === "session") onOpen(place.session.id);
    else if (place.kind === "file") onOpenFile?.(place.file.path, place.line);
    else acts.find((act) => act.id === place.id)?.run();
  };

  const list = useRef<HTMLUListElement>(null);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-at="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at]);

  return (
    <dialog
      ref={dialog}
      className="jump"
      aria-label={filesOnly ? "快速打开文件" : "去往何处"}
      onClose={onClose}
      onClick={(event) => {
        if (event.target === dialog.current) dialog.current?.close();
      }}
    >
      <div className="jump-line">
        <Icon name="search" size={15} />
        <input
          ref={input}
          className="jump-input"
          placeholder={filesOnly ? "输入文件名快速打开（支持 文件名:行号，如 App.tsx:120）" : "去往哪个分身、搜文件，或做什么"}
          value={query}
          aria-controls="jump-list"
          aria-activedescendant={places.length ? `jump-${at}` : undefined}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing) return;
            if (event.key === "ArrowDown") (event.preventDefault(), setAt((at + 1) % Math.max(places.length, 1)));
            if (event.key === "ArrowUp") (event.preventDefault(), setAt((at - 1 + places.length) % Math.max(places.length, 1)));
            if (event.key === "Enter") (event.preventDefault(), go(places[at]));
          }}
        />
        <kbd>Esc</kbd>
      </div>
      <ul id="jump-list" ref={list} className="jump-list" role="listbox">
        {places.length === 0 && <li className="jump-none">没有找到。换个词试试。</li>}
        {places.map((place, index) => (
          <li
            key={place.kind === "session" ? place.session.id : place.kind === "file" ? `file:${place.file.path}` : place.id}
            id={`jump-${index}`}
            data-at={index}
            role="option"
            aria-selected={index === at}
            className={`jump-place ${index === at ? "at" : ""}`}
            onMouseMove={() => index !== at && setAt(index)}
            onClick={() => go(place)}
          >
            {place.kind === "session" ? (
              <>
                <span className={`row-seal ${place.session.status === "waiting_approval" ? "await" : place.session.status === "running" ? "busy" : ""}`} aria-hidden="true">
                  {place.session.status === "running" && <Light />}
                  {place.session.status === "waiting_approval" ? "候" : place.session.status === "running" ? "运" : ""}
                </span>
                <span className="jump-name">{titleOf(place.session)}</span>
                <span className="jump-note">
                  {baseName(homeOf(place.session))}
                  {said.has(place.session.id) && !holds(titleOf(place.session)) ? "，说过这话" : ""}
                </span>
                <span className="jump-when">{ago(place.session.updated_at, now)}</span>
              </>
            ) : place.kind === "file" ? (
              <>
                <span className="seal small" aria-hidden="true">
                  码
                </span>
                <span className="jump-name">
                  {place.file.name}
                  {place.line ? `:${place.line}` : ""}
                </span>
                <span className="jump-note">{place.file.dir || "洞府根目录"}</span>
              </>
            ) : (
              <>
                <span className="seal small" aria-hidden="true">
                  {place.seal}
                </span>
                <span className="jump-name">{place.name}</span>
                <span className="jump-note">{place.hint}</span>
              </>
            )}
          </li>
        ))}
      </ul>
      <p className="jump-keys">
        <span>
          <kbd>↑</kbd>
          <kbd>↓</kbd> 选，<kbd>Enter</kbd> 打开，<kbd>Esc</kbd> 回
        </span>
        <span className="jump-keys-more">
          <kbd>Ctrl/⌘+P</kbd> 搜文件 · <kbd>Ctrl/⌘+B</kbd> 左栏 · <kbd>Ctrl/⌘+J</kbd> 终端 · <kbd>Shift+Tab</kbd> 禁制
        </span>
      </p>
    </dialog>
  );
}
