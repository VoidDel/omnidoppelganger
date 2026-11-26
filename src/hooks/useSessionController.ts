import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { ComposerDraft, ComposerSeed } from "../components/Composer";
import { STEP_MS } from "../components/Replay";
import { api } from "../lib/api";
import {
  type FoldCache,
  changedFiles,
  commandRuns,
  foldIncremental,
  products,
  sessionCostUsd,
  todos,
  usage,
} from "../lib/fold";
import { notifyBackground } from "../lib/notify";
import { replayStops } from "../lib/replay";
import { initial, reduce, removedSessionIds } from "../lib/sessionState";
import { streamStore } from "../lib/streamStore";
import type { Decision, Image, Mode, Reply, Schedule, SessionItem, SettingsView } from "../lib/types";

export function useSessionController() {
  const [state, dispatch] = useReducer(reduce, initial);
  const fail = useCallback((error: unknown) => dispatch({ type: "error", error: String(error) }), []);
  const refresh = useCallback(
    () => api.listSessions().then((sessions) => dispatch({ type: "sessions", sessions }), fail),
    [fail],
  );

  const stateRef = useRef(state);
  stateRef.current = state;
  const navSeqRef = useRef(0);
  const nextNavSeq = useCallback(() => ++navSeqRef.current, []);
  const lastStatusRef = useRef<Map<string, string>>(new Map());
  const draftsRef = useRef<Map<string, ComposerDraft>>(new Map());
  const asideIdsRef = useRef<Map<string, string>>(new Map());
  const [seed, setSeed] = useState<ComposerSeed | null>(null);
  const [asideSeed, setAsideSeed] = useState<ComposerSeed | null>(null);

  useEffect(() => {
    refresh();
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
    const stop = api.onSessionEvent((event) => {
      if (event.kind === "stream") {
        streamStore.push(event.session_id, event.delta);
        return;
      }
      if (event.kind === "log" && event.envelope.type === "assistant/message") {
        streamStore.clear(event.session_id);
      }
      dispatch({ type: "event", event });
      if (event.kind === "status") {
        if (event.status === "idle") streamStore.clear(event.session_id);
        const prev = lastStatusRef.current.get(event.session_id);
        lastStatusRef.current.set(event.session_id, event.status);
        const title = stateRef.current.sessions.find((s) => s.id === event.session_id)?.title || "分身";
        if (event.status === "waiting_approval" && prev !== "waiting_approval") {
          notifyBackground(`分身候旨 · ${title}`, "有操作或提问等待本尊定夺。");
        } else if (event.status === "idle" && prev === "running") {
          notifyBackground(`分身收功 · ${title}`, "本轮任务已完成。");
        }
        api.getSessionItem(event.session_id).then(
          (item) => dispatch({ type: "session", item }),
          () => refresh(),
        );
      }
    });
    const readSchedules = () => api.listSchedules().then((schedules) => dispatch({ type: "schedules", schedules }), fail);
    readSchedules();
    const stopSchedules = api.onSchedulesChanged(() => {
      readSchedules();
      refresh();
    });
    return () => {
      stop.then((unlisten) => unlisten());
      stopSchedules.then((unlisten) => unlisten());
    };
  }, [refresh, fail]);

  const [replay, setReplay] = useState<{ at: number; playing: boolean; speed: number } | null>(null);
  useEffect(() => setReplay(null), [state.activeId, state.page]);
  const stops = useMemo(() => replayStops(state.events), [state.events]);
  const playing = !!replay?.playing;
  const speed = replay?.speed ?? 1;
  useEffect(() => {
    if (!playing) return;
    const timer = setInterval(
      () =>
        setReplay((now) =>
          now && now.at < stops.length - 1 ? { ...now, at: now.at + 1 } : now && { ...now, playing: false },
        ),
      STEP_MS / speed,
    );
    return () => clearInterval(timer);
  }, [playing, speed, stops.length]);

  const shown = useMemo(
    () => (replay ? state.events.slice(0, (stops[replay.at] ?? -1) + 1) : state.events),
    [replay, state.events, stops],
  );

  const foldCacheRef = useRef<FoldCache | null>(null);
  const rows = useMemo(() => {
    const next = foldIncremental(foldCacheRef.current, shown);
    foldCacheRef.current = next;
    return next.rows;
  }, [shown]);

  const files = useMemo(() => changedFiles(rows), [rows]);
  const runs = useMemo(() => commandRuns(rows), [rows]);
  const made = useMemo(() => products(rows), [rows]);
  const spent = useMemo(() => usage(state.events), [state.events]);
  const plan = useMemo(() => todos(rows), [rows]);
  const active = state.sessions.find((session) => session.id === state.activeId);
  const activeId = state.activeId;
  const costUsd = useMemo(
    () => (active ? sessionCostUsd(state.events, active.provider, active.model) : null),
    [state.events, active?.provider, active?.model],
  );

  const openFile = useCallback(
    (path: string, reveal: boolean, snapshot?: string, version?: number) =>
      activeId ? api.openAttachment(activeId, path, reveal, snapshot, version) : Promise.reject("没有打开的分身"),
    [activeId],
  );
  const pictureOf = useCallback(
    (path: string, snapshot?: string) => (activeId ? api.picture(activeId, path, snapshot) : Promise.reject("没有打开的分身")),
    [activeId],
  );
  const restoreArtifact = useCallback(
    (path: string, snapshot: string, version: number) =>
      activeId ? api.restoreArtifact(activeId, path, snapshot, version) : Promise.reject("没有打开的分身"),
    [activeId],
  );
  const saveArtifactCopy = useCallback(
    (path: string, snapshot: string, version: number) =>
      activeId ? api.saveArtifactCopy(activeId, path, snapshot, version) : Promise.reject("没有打开的分身"),
    [activeId],
  );
  const versionOf = useCallback(
    (path: string, snapshot?: string) => {
      if (!snapshot) return undefined;
      const product = made.find((entry) => entry.path === path);
      if (!product || product.versions.length <= 1) return undefined;
      return product.versions.find((entry) => entry.snapshot === snapshot)?.version;
    },
    [made],
  );

  const dispatchDeleted = useCallback(
    (id: string) => {
      const cur = stateRef.current;
      const removed = removedSessionIds(cur.sessions, id);
      for (const rid of removed) {
        draftsRef.current.delete(rid);
        streamStore.clear(rid);
        const asideId = asideIdsRef.current.get(rid);
        if (asideId) {
          draftsRef.current.delete(asideId);
          streamStore.clear(asideId);
          asideIdsRef.current.delete(rid);
        }
      }
      const affectsNav =
        (cur.activeId !== null && removed.has(cur.activeId)) || (cur.openingId !== null && removed.has(cur.openingId));
      dispatch({ type: "deleted", id, navSeq: affectsNav ? nextNavSeq() : undefined });
    },
    [nextNavSeq],
  );

  const cleanScheduledSessions = useCallback(
    async (ids: string[]) => {
      let removed = 0;
      for (const id of ids) {
        try {
          await api.deleteSession(id);
          dispatchDeleted(id);
          removed++;
        } catch (error) {
          fail(error);
          break;
        }
      }
      if (removed > 0) dispatch({ type: "note", note: `已清扫 ${removed} 个已收功的功课分身。` });
    },
    [dispatchDeleted, fail],
  );

  const goHome = useCallback(() => dispatch({ type: "home", navSeq: nextNavSeq() }), [nextNavSeq]);
  const open = useCallback(
    (id: string) => {
      const navSeq = nextNavSeq();
      dispatch({ type: "opening", id, navSeq });
      api.openSession(id).then((view) => dispatch({ type: "opened", view, navSeq }), fail);
    },
    [nextNavSeq, fail],
  );

  const forkFrom = useCallback(
    (turn?: number, text?: string) => {
      const id = stateRef.current.activeId;
      if (!id) return;
      const navSeq = nextNavSeq();
      dispatch({ type: "opening", id: null, navSeq });
      api.forkSession(id, turn).then((view) => {
        dispatch({ type: "opened", view, navSeq });
        if (navSeq !== navSeqRef.current) return;
        const quote = view.abandoned_brief
          ? { label: "原分支试错摘要", meta: "点 × 可不带入", content: view.abandoned_brief }
          : undefined;
        if (text !== undefined || quote) setSeed({ id: Date.now(), text, quote });
        const point =
          turn === undefined
            ? "带着到现在为止的全部对话"
            : turn > 1
              ? `带着第 ${turn} 轮之前的对话，那一轮的话放回了输入框`
              : "从头来过，头一句话放回了输入框";
        const extra = quote ? "（输入框已附上原分支试错摘要，不需要可点 × 移除）" : "";
        dispatch({
          type: "note",
          note: `已分叉：这是一个新的分身，${point}${extra}。只分了对话：洞府里的文件是同一份，还是现在的样子。`,
        });
      }, fail);
    },
    [nextNavSeq, fail],
  );

  const forkSessionById = useCallback(
    (id: string) => {
      const navSeq = nextNavSeq();
      dispatch({ type: "opening", id: null, navSeq });
      api.forkSession(id).then((view) => {
        dispatch({ type: "opened", view, navSeq });
        if (navSeq === navSeqRef.current) {
          dispatch({
            type: "note",
            note: "已分叉：这是一个新的分身，带着到现在为止的全部对话。只分了对话：洞府里的文件是同一份，还是现在的样子。",
          });
        }
      }, fail);
    },
    [nextNavSeq, fail],
  );

  const exportSession = useCallback(
    (id: string, title: string) => {
      api.exportSessionMarkdown(id, title).then((savedPath) => {
        if (savedPath) dispatch({ type: "note", note: `已导出会话为 Markdown：${savedPath}` });
      }, fail);
    },
    [fail],
  );

  const chat = useCallback(() => {
    const navSeq = nextNavSeq();
    dispatch({ type: "opening", id: null, navSeq });
    api.startChat().then((view) => dispatch({ type: "opened", view, navSeq }), fail);
  }, [nextNavSeq, fail]);

  const chatting = useCallback(
    (session: SessionItem) => {
      const plain = (path: string) => path.replace(/[\\/]+$/, "").toLowerCase();
      return !!state.settings && plain(session.workspace) === plain(state.settings.chat_folder);
    },
    [state.settings],
  );

  const createIn = useCallback(
    (workspace: string, isolated?: boolean) => {
      const navSeq = nextNavSeq();
      dispatch({ type: "opening", id: null, navSeq });
      api.createSession(workspace, isolated).then((view) => dispatch({ type: "opened", view, navSeq }), fail);
    },
    [nextNavSeq, fail],
  );

  const create = useCallback(async () => {
    try {
      const workspace = await api.pickWorkspace();
      if (!workspace) return;
      const navSeq = nextNavSeq();
      dispatch({ type: "opening", id: null, navSeq });
      dispatch({ type: "opened", view: await api.createSession(workspace), navSeq });
    } catch (error) {
      fail(error);
    }
  }, [nextNavSeq, fail]);

  const send = useCallback(
    (text: string, images: Image[] = [], attached: string[] = []) => {
      const id = stateRef.current.activeId;
      if (!id) return;
      api.sendMessage(id, text, images, attached).then(
        (queued) => queued && dispatch({ type: "queued", id, text: text.trim() }),
        fail,
      );
    },
    [fail],
  );

  const decide = useCallback(
    (toolUseId: string, decision: Decision) => {
      if (activeId) api.resolveApproval(activeId, toolUseId, decision).catch(fail);
    },
    [activeId, fail],
  );
  const answer = useCallback(
    (toolUseId: string, answers: Reply[] | null) => {
      if (activeId) api.answerQuestions(activeId, toolUseId, answers).catch(fail);
    },
    [activeId, fail],
  );

  const switchMode = useCallback(
    (mode: Mode) => (activeId ? api.setMode(activeId, mode).then((item) => dispatch({ type: "session", item })) : Promise.resolve()),
    [activeId],
  );
  const carryOut = useCallback(
    (mode: Mode) => {
      if (!activeId) return;
      switchMode(mode).then(() => api.sendMessage(activeId, "照这个计划动手。")).catch(fail);
    },
    [activeId, switchMode, fail],
  );

  const refreshSettings = useCallback(() => {
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
  }, [fail]);
  const saved = useCallback(
    (request: Promise<SettingsView>) => request.then((settings) => dispatch({ type: "settings", settings }), fail),
    [fail],
  );
  const scheduled = useCallback(
    (request: Promise<Schedule[]>) =>
      request.then(
        (schedules) => (dispatch({ type: "schedules", schedules }), null),
        (error) => String(error),
      ),
    [],
  );

  const lastItems = rows[rows.length - 1]?.items ?? [];
  const last = lastItems[lastItems.length - 1];
  const planReady =
    active?.mode === "plan" &&
    (last?.kind === "text" ||
      (last?.kind === "tool" && last.name === "present_plan" && !!last.result && !last.result.isError));

  return {
    state,
    stateRef,
    dispatch,
    fail,
    refresh,
    refreshSettings,
    saved,
    scheduled,
    draftsRef,
    asideIdsRef,
    seed,
    setSeed,
    asideSeed,
    setAsideSeed,
    replay,
    setReplay,
    stops,
    rows,
    files,
    runs,
    made,
    spent,
    plan,
    active,
    activeId,
    costUsd,
    planReady,
    openFile,
    pictureOf,
    restoreArtifact,
    saveArtifactCopy,
    versionOf,
    dispatchDeleted,
    cleanScheduledSessions,
    goHome,
    open,
    forkFrom,
    forkSessionById,
    exportSession,
    chat,
    chatting,
    createIn,
    create,
    send,
    decide,
    answer,
    switchMode,
    carryOut,
  };
}
