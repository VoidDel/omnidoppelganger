import { type ReactNode, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { api } from "../lib/api";
import { emptyLive, liveReduce } from "../lib/aside";
import { fold, type Usage, usage } from "../lib/fold";
import type { Image, SessionItem, SessionView, Status } from "../lib/types";
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
  /** Hand an answer to the main 分身. */
  onForward: (text: string) => void;
  onError: (error: string) => void;
}

const itemOf = ({ events: _, abandoned_brief: __, ...item }: SessionView): SessionItem => item;

const ASK_FIRST = ["它现在做到哪一步了？", "刚才那几步为什么这么改？", "它接下来打算做什么？"];

/**
 * 旁聊: a side conversation beside the session on the page. It talks while the main 分身 works without interrupting
 * it, only looks at the workspace, and is told how the main 分身 is getting on before each message. What it says
 * can be handed to the main 分身 with a click. It is kept with the main session until it is started over.
 */
export function AsidePane({ main, visible, sessions, renderComposer, onForward, onError }: AsidePaneProps) {
  const [aside, setAside] = useState<SessionItem | null>(null);
  const [live, dispatch] = useReducer(liveReduce, emptyLive);
  const [opening, setOpening] = useState(false);
  const [resetting, setResetting] = useState(false);
  const asideId = useRef<string | null>(null);
  asideId.current = aside?.id ?? null;

  const open = (view: SessionView) => {
    asideId.current = view.id;
    setAside(itemOf(view));
    dispatch({ type: "opened", events: view.events, status: view.status });
  };

  // Opened, or made, the first time the pane is on screen; not before, so no empty one is made for every session looked at.
  useEffect(() => {
    if (!visible || aside || opening) return;
    let alive = true;
    setOpening(true);
    api.startAside(main.id).then(
      (view) => alive && open(view),
      (error) => alive && onError(String(error)),
    ).finally(() => alive && setOpening(false));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, aside, main.id]);

  useEffect(() => {
    const stop = api.onSessionEvent((event) => {
      if (event.session_id !== asideId.current) return;
      dispatch({ type: "event", event });
    });
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, []);

  const rows = useMemo(() => fold(live.events), [live.events]);
  const spent = useMemo(() => usage(live.events), [live.events]);

  const send = (text: string, images: Image[] = [], files: string[] = []) => {
    const id = asideId.current;
    if (!id) return;
    api.sendMessage(id, text, images, files).then((queued) => queued && dispatch({ type: "queued", text: text.trim() }), (error) => onError(String(error)));
  };

  const startOver = () => {
    setResetting(false);
    api.resetAside(main.id).then(
      () => {
        asideId.current = null;
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
            <button type="button" className="button text small" disabled={live.status !== "idle"} title={live.status !== "idle" ? "旁聊还在回答，先收回它" : "清空这段旁聊，重新开始"} aria-expanded={resetting} onClick={() => setResetting(!resetting)}>
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
          rows={rows}
          stream={live.stream}
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
