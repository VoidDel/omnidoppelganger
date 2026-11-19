import { useEffect, useRef, useState } from "react";
import type { SessionItem } from "../lib/types";
import { ago } from "../lib/when";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;
const titleOf = (session: SessionItem) => session.title || (session.parent ? "子分身" : "新分身");

/** Somewhere to go: a 分身, or a page or act of the app's own. */
type Place = { kind: "session"; session: SessionItem } | { kind: "act"; id: string; name: string; hint: string; seal: string };

interface JumpProps {
  sessions: SessionItem[];
  /** The places that are not 分身: what each is called, and what it does. */
  acts: { id: string; name: string; hint: string; seal: string; run: () => void }[];
  onOpen: (id: string) => void;
  /** Ids of the sessions that hold the text in what was said in them, as the list's own search finds them. */
  onSearch: (query: string) => Promise<string[]>;
  onClose: () => void;
}

/**
 * Ctrl+K: a line to type in, and under it every place there is to go, narrowed as one types. A 分身 is found by
 * its title or its 洞府 at once, and by what was said in it a moment later. Arrows move, Enter goes, Esc leaves.
 */
export function Jump({ sessions, acts, onOpen, onSearch, onClose }: JumpProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [at, setAt] = useState(0);
  /** The sessions whose conversations hold the words, once the search has answered. */
  const [said, setSaid] = useState<Set<string>>(new Set());
  const now = new Date();
  useEffect(() => {
    dialog.current?.showModal();
    input.current?.focus();
  }, []);
  useEffect(() => {
    setAt(0);
    if (!query.trim()) return setSaid(new Set());
    let current = true;
    const timer = setTimeout(() => onSearch(query).then((ids) => current && setSaid(new Set(ids)), () => undefined), 160);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [query, onSearch]);
  const words = query.trim().toLowerCase();
  const holds = (text: string) => text.toLowerCase().includes(words);
  const places: Place[] = [
    ...sessions.filter((session) => !words || holds(titleOf(session)) || holds(homeOf(session)) || said.has(session.id)).map((session) => ({ kind: "session" as const, session })),
    ...acts.filter((act) => !words || holds(act.name) || holds(act.hint)).map(({ run: _, ...act }) => ({ kind: "act" as const, ...act })),
  ].slice(0, 30);
  const go = (place: Place | undefined) => {
    if (!place) return;
    onClose();
    if (place.kind === "session") onOpen(place.session.id);
    else acts.find((act) => act.id === place.id)?.run();
  };
  // The one the arrows are on stays in sight.
  const list = useRef<HTMLUListElement>(null);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-at="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at]);
  return (
    <dialog
      ref={dialog}
      className="jump"
      aria-label="去往何处"
      onClose={onClose}
      onClick={(event) => {
        // A click on the backdrop, outside the sheet, leaves.
        if (event.target === dialog.current) dialog.current?.close();
      }}
    >
      <div className="jump-line">
        <svg aria-hidden="true" width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
          <circle cx="7" cy="7" r="4.6" />
          <path d="M10.4 10.4 14 14" />
        </svg>
        <input
          ref={input}
          className="jump-input"
          placeholder="去往哪个分身，或做什么：标题、洞府、说过的话"
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
        {places.length === 0 && <li className="jump-none">没有找到。换个说法试试。</li>}
        {places.map((place, index) => (
          <li
            key={place.kind === "session" ? place.session.id : place.id}
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
                  {place.session.status === "waiting_approval" ? "候" : place.session.status === "running" ? "运" : ""}
                </span>
                <span className="jump-name">{titleOf(place.session)}</span>
                <span className="jump-note">
                  {baseName(homeOf(place.session))}
                  {said.has(place.session.id) && !holds(titleOf(place.session)) ? "，说过这话" : ""}
                </span>
                <span className="jump-when">{ago(place.session.updated_at, now)}</span>
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
        <kbd>↑</kbd>
        <kbd>↓</kbd> 选，<kbd>Enter</kbd> 去，<kbd>Esc</kbd> 回
      </p>
    </dialog>
  );
}
