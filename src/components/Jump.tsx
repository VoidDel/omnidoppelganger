import { useEffect, useId, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { FoundFile, SessionItem } from "../lib/types";
import { ago } from "../lib/when";
import { Icon } from "./Icon";
import { Light } from "./Light";

const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
const homeOf = (session: SessionItem) => session.worktree?.origin ?? session.workspace;

type Place =
  | { kind: "session"; session: SessionItem }
  | { kind: "file"; file: FoundFile; line?: number }
  | { kind: "act"; id: string; name: string; hint: string; seal: string };

interface JumpProps {
  mode?: "all" | "files";
  sessions: SessionItem[];
  acts: { id: string; name: string; hint: string; seal: string; run: () => void }[];
  onOpen: (id: string) => void;
  onSearch: (query: string) => Promise<string[]>;
  onSearchFiles?: (query: string) => Promise<FoundFile[]>;
  onOpenFile?: (relPath: string, line?: number) => void;
  onClose: () => void;
}

export function Jump({ mode = "all", sessions, acts, onOpen, onSearch, onSearchFiles, onOpenFile, onClose }: JumpProps) {
  const uid = useId();
  const listId = `${uid}-list`;
  const { locale, t } = useI18n();
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [at, setAt] = useState(0);
  const [said, setSaid] = useState<Set<string>>(new Set());
  const [matchedFiles, setMatchedFiles] = useState<FoundFile[]>([]);
  const now = new Date();
  const titleOf = (session: SessionItem) =>
    session.title || (session.parent ? t("nav.childDefault") : t("nav.sessionDefault"));

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
      aria-label={filesOnly ? t("jump.openFileAria") : t("jump.whereAria")}
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
          placeholder={filesOnly ? t("jump.filePlaceholder") : t("jump.allPlaceholder")}
          value={query}
          aria-controls={listId}
          aria-activedescendant={places.length ? `${uid}-jump-${at}` : undefined}
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
      <ul id={listId} ref={list} className="jump-list" role="listbox">
        {places.length === 0 && <li className="jump-none">{t("jump.empty")}</li>}
        {places.map((place, index) => (
          <li
            key={place.kind === "session" ? place.session.id : place.kind === "file" ? `file:${place.file.path}` : place.id}
            id={`${uid}-jump-${index}`}
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
                  {place.session.status === "waiting_approval"
                    ? t("nav.sealAwait")
                    : place.session.status === "running"
                      ? t("nav.sealRun")
                      : ""}
                </span>
                <span className="jump-name">{titleOf(place.session)}</span>
                <span className="jump-note">
                  {baseName(homeOf(place.session))}
                  {said.has(place.session.id) && !holds(titleOf(place.session)) ? t("jump.saidMatch") : ""}
                </span>
                <span className="jump-when">{ago(place.session.updated_at, now, locale)}</span>
              </>
            ) : place.kind === "file" ? (
              <>
                <span className="seal small" aria-hidden="true">
                  {locale === "en" ? "FL" : "码"}
                </span>
                <span className="jump-name">
                  {place.file.name}
                  {place.line ? `:${place.line}` : ""}
                </span>
                <span className="jump-note">{place.file.dir || t("jump.rootDir")}</span>
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
          <kbd>↓</kbd> {t("jump.keysSelect")}
          <kbd>Enter</kbd> {t("jump.keysOpen")}
          <kbd>Esc</kbd> {t("jump.keysClose")}
        </span>
        <span className="jump-keys-more">
          <kbd>Ctrl/⌘+P</kbd> · <kbd>Ctrl/⌘+B</kbd> · <kbd>Ctrl/⌘+J</kbd> · <kbd>Shift+Tab</kbd>
        </span>
      </p>
    </dialog>
  );
}
