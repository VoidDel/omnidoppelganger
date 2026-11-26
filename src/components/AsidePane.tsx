import { type ReactNode, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { api } from "../lib/api";
import { emptyLive, liveReduce } from "../lib/aside";
import { type FoldCache, type Usage, foldIncremental, usage } from "../lib/fold";
import { streamStore } from "../lib/streamStore";
import type { Image, SessionItem, SessionView, Status, UiEvent } from "../lib/types";
import { ConfirmBubble } from "./ConfirmBubble";
import { Transcript } from "./Transcript";

/** What the pane gives the window to draw the side conversation's message box with. */
export interface AsideComposerArgs {
  aside: SessionItem;
  status: Status;
  usage: Usage;
  onItem: (item: SessionItem) => void;
  onSend: (text: string, images: Image[], files: string[]) => void;
  onStop: () => void;
  onCompact: (focus?: string) => void;
}

export interface AsidePaneProps {
  /** The session on the page, which this side conversation is beside. */
  main: SessionItem;
  /** The pane is on screen: the side conversation is opened, or made, the first time it is. */
  visible: boolean;
  sessions: SessionItem[];
  renderComposer: (args: AsideComposerArgs) => ReactNode;
  /** Optional callback notifying parent of the opened aside session id for draft lifecycle cleanup. */
  onAsideId?: (asideId: string | null) => void;
  /** Hand an answer to the main 分身. */
  onForward: (text: string) => void;
  onError: (error: string) => void;
}

const itemOf = ({ events: _, abandoned_brief: __, ...item }: SessionView): SessionItem => item;

const ASK_FIRST = ["它现在做到哪一步了？", "刚才那几步为什么这么改？", "它接下来打算做什么？"];

export function AsidePane({ main, visible, sessions, renderComposer, onAsideId, onForward, onError }: AsidePaneProps) {
  const [aside, setAside] = useState<SessionItem | null>(null);
  const [live, dispatch] = useReducer(liveReduce, emptyLive);
  const [opening, setOpening] = useState(false);
  const openingRef = useRef(false);
  const [resetting, setResetting] = useState(false);
  const asideId = useRef<string | null>(null);
  asideId.current = aside?.id ?? null;
  const earlyQueueRef = useRef<UiEvent[]>([]);
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const onAsideIdRef = useRef(onAsideId);
  onAsideIdRef.current = onAsideId;

  // Register listener up front so events arriving while `startAside` is in flight are buffered in `earlyQueueRef`.
  useEffect(() => {
    const stop = api.onSessionEvent((event) => {
      const currentId = asideId.current;
      if (currentId === null) {
        if (event.session_id !== main.id) {
          earlyQueueRef.current.push(event);
        }
        return;
      }
      if (event.session_id !== currentId) return;
      if (event.kind === "stream") {
        streamStore.push(currentId, event.delta);
        return;
      }
      if (event.kind === "log" && event.envelope.type === "assistant/message") {
        streamStore.clear(currentId);
      } else if (event.kind === "status" && event.status === "idle") {
        streamStore.clear(currentId);
      }
      dispatch({ type: "event", event });
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [main.id]);

  useEffect(() => {
    if (!visible || aside || openingRef.current) return;
    let alive = true;
    openingRef.current = true;
    setOpening(true);
    api
      .startAside(main.id)
      .then(
        (view) => {
          if (!alive) return;
          asideId.current = view.id;
          onAsideIdRef.current?.(view.id);
          setAside(itemOf(view));
          const lastSeq = view.events[view.events.length - 1]?.seq ?? 0;
          dispatch({ type: "opened", events: view.events, status: view.status });

          const queued = earlyQueueRef.current.splice(0);
          for (const ev of queued) {
            if (ev.session_id !== view.id) continue;
            if (ev.kind === "stream") {
              streamStore.push(view.id, ev.delta);
            } else if (ev.kind === "log") {
              if (ev.envelope.seq > lastSeq) {
                if (ev.envelope.type === "assistant/message") streamStore.clear(view.id);
                dispatch({ type: "event", event: ev });
              }
            } else if (ev.kind === "status") {
              if (ev.status === "idle") streamStore.clear(view.id);
              dispatch({ type: "event", event: ev });
            }
          }
        },
        (error) => alive && onErrorRef.current(String(error)),
      )
      .finally(() => {
        openingRef.current = false;
        if (alive) setOpening(false);
      });
    return () => {
      alive = false;
      openingRef.current = false;
    };
  }, [visible, aside, main.id]);

  const foldCacheRef = useRef<FoldCache | null>(null);
  const rows = useMemo(() => {
    const next = foldIncremental(foldCacheRef.current, live.events);
    foldCacheRef.current = next;
    return next.rows;
  }, [live.events]);
  const spent = useMemo(() => usage(live.events), [live.events]);

  const send = (text: string, images: Image[] = [], files: string[] = []) => {
    const id = asideId.current;
    if (!id) return;
    api.sendMessage(id, text, images, files).then(
      (queued) => queued && dispatch({ type: "queued", text: text.trim() }),
      (error) => onError(String(error)),
    );
  };

  const startOver = () => {
    setResetting(false);
    const prevId = asideId.current;
    api.resetAside(main.id).then(
      () => {
        if (prevId) streamStore.clear(prevId);
        asideId.current = null;
        earlyQueueRef.current = [];
        onAsideIdRef.current?.(null);
        dispatch({ type: "reset" });
        setAside(null);
      },
      (error) => onError(String(error)),
    );
  };

  const mainBusy = main.status !== "idle";
  return (
    <div className="aside-pane">
      <div className="aside-head">
        <div className="aside-title">
          <strong>旁聊</strong>
          <span className="aside-note">{mainBusy ? "主分身在运功，这里说话不打断它" : "只读：能看洞府、看主分身的进展，不改文件"}</span>
        </div>
        {aside && rows.length > 0 && (
          <span className="bubble-anchor">
            <button
              type="button"
              className="button text small"
              disabled={live.status !== "idle"}
              title={live.status !== "idle" ? "旁聊还在回答，先收回它" : "清空这段旁聊，重新开始"}
              aria-expanded={resetting}
              onClick={() => setResetting(!resetting)}
            >
              清空重来
            </button>
            {resetting && (
              <ConfirmBubble label="清空旁聊" confirm="清空" onConfirm={startOver} onCancel={() => setResetting(false)}>
                <p>这段旁聊的记录会删掉，主分身那边不受影响。</p>
              </ConfirmBubble>
            )}
          </span>
        )}
      </div>
      {!aside ? (
        <div className="viewer-placeholder muted">{opening ? "正在打开旁聊…" : ""}</div>
      ) : rows.length === 0 && live.queued.length === 0 ? (
        <div className="aside-empty">
          <p>主分身干活时，在这里另开一段对话问问题，不会打断它。</p>
          <p className="muted">每次发问都会自动附上主分身眼下的任务、计划和最近几步；旁聊只读洞府，不改文件。满意的回答可以一键「转给主分身」。</p>
          <div className="aside-starters">
            {ASK_FIRST.map((question) => (
              <button key={question} type="button" className="button outline small" onClick={() => send(question)}>
                {question}
              </button>
            ))}
          </div>
        </div>
      ) : (
        <Transcript
          sessionId={aside.id}
          rows={rows}
          queued={live.queued}
          status={live.status}
          mode={aside.mode}
          origin="user"
          sessions={sessions}
          copied={0}
          frozen={false}
          onOpen={() => undefined}
          onDecide={(toolUseId, decision) => api.resolveApproval(aside.id, toolUseId, decision).catch((error) => onError(String(error)))}
          onAnswer={(toolUseId, answers) => api.answerQuestions(aside.id, toolUseId, answers).catch((error) => onError(String(error)))}
          onUndo={() => undefined}
          onOpenFile={(path, reveal, snapshot, version) => api.openAttachment(aside.id, path, reveal, snapshot, version)}
          onPicture={(path, snapshot) => api.picture(aside.id, path, snapshot)}
          onForward={onForward}
          onDeliverNow={() => api.deliverQueued(aside.id).catch((error) => onError(String(error)))}
        />
      )}
      {aside &&
        renderComposer({
          aside,
          status: live.status,
          usage: spent,
          onItem: (item) => setAside(item),
          onSend: send,
          onStop: () => api.cancelTurn(aside.id).catch((error) => onError(String(error))),
          onCompact: (focus) => api.compactSession(aside.id, focus).catch((error) => onError(String(error))),
        })}
    </div>
  );
}
