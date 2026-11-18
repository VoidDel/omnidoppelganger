import { useCallback, useEffect, useMemo, useReducer } from "react";
import { Composer, Logo, Mountains, SessionHeader, SidePanel, Sidebar } from "./components/Chrome";
import { SettingsPage } from "./components/Settings";
import { Transcript } from "./components/Transcript";
import { api } from "./lib/api";
import { applyDelta, changedFiles, commandRuns, fold, usage, type StreamBlock } from "./lib/fold";
import { modeOf } from "./lib/modes";
import type { Decision, Envelope, Mode, SessionItem, SessionView, Settings, SettingsView, Status, UiEvent } from "./lib/types";

interface State {
  sessions: SessionItem[];
  activeId: string | null;
  /** The session being opened: its log events are kept until its snapshot arrives. */
  openingId: string | null;
  buffer: Envelope[];
  events: Envelope[];
  status: Status;
  stream: StreamBlock[];
  /** Messages sent during a turn that the agent has not picked up yet. */
  queued: string[];
  page: "chat" | "settings";
  settings: SettingsView | null;
  error: string | null;
}

type Action =
  | { type: "sessions"; sessions: SessionItem[] }
  | { type: "opening"; id: string }
  | { type: "opened"; view: SessionView }
  | { type: "session"; item: SessionItem }
  | { type: "deleted"; id: string }
  | { type: "event"; event: UiEvent }
  | { type: "queued"; text: string }
  | { type: "page"; page: State["page"] }
  | { type: "settings"; settings: SettingsView }
  | { type: "error"; error: string | null };

const initial: State = {
  sessions: [],
  activeId: null,
  openingId: null,
  buffer: [],
  events: [],
  status: "idle",
  stream: [],
  queued: [],
  page: "chat",
  settings: null,
  error: null,
};

/** Add a log event unless the snapshot already holds it. */
function appendLog(state: State, envelope: Envelope): State {
  const last = state.events[state.events.length - 1]?.seq ?? 0;
  if (envelope.seq <= last) return state;
  let { stream, queued } = state;
  if (envelope.type === "assistant/message") stream = [];
  // A queued message joins as steering, or opens the next turn when it arrived as the last one ended.
  if (envelope.type === "user/message") {
    const at = queued.indexOf(envelope.data.text);
    if (at >= 0) queued = queued.filter((_, index) => index !== at);
  }
  return { ...state, events: [...state.events, envelope], stream, queued };
}

function reduce(state: State, action: Action): State {
  switch (action.type) {
    case "sessions":
      return { ...state, sessions: action.sessions };
    case "opening":
      return { ...state, openingId: action.id, buffer: [] };
    case "opened": {
      const { events, ...item } = action.view;
      const opened: State = {
        ...state,
        activeId: item.id,
        openingId: null,
        buffer: [],
        events,
        status: item.status,
        stream: [],
        queued: [],
        page: "chat",
        sessions: state.sessions.some((session) => session.id === item.id)
          ? state.sessions.map((session) => (session.id === item.id ? item : session))
          : [item, ...state.sessions],
      };
      return state.buffer.reduce(appendLog, opened);
    }
    case "deleted": {
      const sessions = state.sessions.filter((session) => session.id !== action.id);
      return state.activeId === action.id ? { ...state, sessions, activeId: null, events: [], stream: [], queued: [], status: "idle" } : { ...state, sessions };
    }
    case "session":
      return { ...state, sessions: state.sessions.map((session) => (session.id === action.item.id ? { ...action.item, status: session.status } : session)) };
    case "event": {
      const { event } = action;
      const active = event.session_id === state.activeId;
      if (event.kind === "log") {
        if (active) return appendLog(state, event.envelope);
        return event.session_id === state.openingId ? { ...state, buffer: [...state.buffer, event.envelope] } : state;
      }
      if (event.kind === "stream") return active ? { ...state, stream: applyDelta(state.stream, event.delta) } : state;
      const sessions = state.sessions.map((session) => (session.id === event.session_id ? { ...session, status: event.status } : session));
      if (!active) return { ...state, sessions };
      const idle = event.status === "idle";
      return { ...state, sessions, status: event.status, stream: idle ? [] : state.stream, queued: idle ? [] : state.queued };
    }
    case "queued":
      return { ...state, queued: [...state.queued, action.text] };
    case "page":
      return { ...state, page: action.page };
    case "settings":
      return { ...state, settings: action.settings };
    case "error":
      return { ...state, error: action.error };
  }
}

function useTheme(theme: Settings["theme"] | undefined) {
  useEffect(() => {
    const system = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.theme = theme === "dark" || (theme !== "light" && system.matches) ? "dark" : "light";
    };
    apply();
    system.addEventListener("change", apply);
    return () => system.removeEventListener("change", apply);
  }, [theme]);
}

export default function App() {
  const [state, dispatch] = useReducer(reduce, initial);
  const fail = useCallback((error: unknown) => dispatch({ type: "error", error: String(error) }), []);
  const refresh = useCallback(() => api.listSessions().then((sessions) => dispatch({ type: "sessions", sessions }), fail), [fail]);

  useEffect(() => {
    refresh();
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
    const stop = api.onSessionEvent((event) => {
      dispatch({ type: "event", event });
      // Titles and list order come from the log, so the list is read again when a turn starts or ends.
      if (event.kind === "status") refresh();
    });
    return () => {
      stop.then((unlisten) => unlisten());
    };
  }, [refresh, fail]);

  useTheme(state.settings?.settings.theme);

  const rows = useMemo(() => fold(state.events), [state.events]);
  const files = useMemo(() => changedFiles(rows), [rows]);
  const runs = useMemo(() => commandRuns(rows), [rows]);
  const spent = useMemo(() => usage(state.events), [state.events]);
  const active = state.sessions.find((session) => session.id === state.activeId);

  const open = (id: string) => {
    dispatch({ type: "opening", id });
    api.openSession(id).then((view) => dispatch({ type: "opened", view }), fail);
  };
  const create = async () => {
    try {
      const workspace = await api.pickWorkspace();
      if (!workspace) return;
      dispatch({ type: "opened", view: await api.createSession(workspace) });
    } catch (error) {
      fail(error);
    }
  };
  const send = (text: string) => {
    if (!state.activeId) return;
    api.sendMessage(state.activeId, text).then((queued) => queued && dispatch({ type: "queued", text: text.trim() }), fail);
  };
  const decide = (toolUseId: string, decision: Decision) => {
    if (state.activeId) api.resolveApproval(state.activeId, toolUseId, decision).catch(fail);
  };
  /** Switch the active session's mode; resolves once the session has it, so a message sent after records it. */
  const switchMode = (mode: Mode) =>
    state.activeId ? api.setMode(state.activeId, mode).then((item) => dispatch({ type: "session", item })) : Promise.resolve();
  const carryOut = (mode: Mode) => {
    if (!state.activeId) return;
    const id = state.activeId;
    switchMode(mode)
      .then(() => api.sendMessage(id, "照这个计划动手。"))
      .catch(fail);
  };
  // The last turn in plan mode ended with the 分身's answer, not a stop or an error.
  const lastItems = rows[rows.length - 1]?.items ?? [];
  const planReady = active?.mode === "plan" && lastItems[lastItems.length - 1]?.kind === "text";
  const saved = (request: Promise<SettingsView>) => request.then((settings) => dispatch({ type: "settings", settings }), fail);

  const providers = state.settings?.providers ?? [];
  const defaultProvider = providers.find((provider) => provider.id === state.settings?.settings.provider);
  const missingKey = !!defaultProvider && state.settings?.keys[defaultProvider.id]?.source === "none";
  /** What the composer shows for a session's model: the provider's name too, unless it is Anthropic. */
  const modelLabel = (session: SessionItem) => {
    const provider = providers.find((known) => known.id === session.provider);
    return provider && provider.id !== "anthropic" ? `${provider.name} · ${session.model}` : session.model;
  };
  const inSettings = state.page === "settings";
  const showPanel = !inSettings && !!active;
  return (
    <div className={`app ${showPanel ? "with-panel" : ""}`}>
      <Sidebar
        sessions={state.sessions}
        activeId={state.activeId}
        inSettings={inSettings}
        onOpen={open}
        onNew={create}
        onSettings={() => {
          // Read again: a command may have been allowed for good since the page was last shown.
          api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
          dispatch({ type: "page", page: "settings" });
        }}
        onSearch={api.searchSessions}
      />
      <main className="main">
        {state.error && (
          <div className="error-banner" role="alert">
            <span style={{ flex: 1, overflowWrap: "anywhere" }}>{state.error}</span>
            <button type="button" className="button text" onClick={() => dispatch({ type: "error", error: null })}>
              知道了
            </button>
          </div>
        )}
        {inSettings && state.settings ? (
          <SettingsPage
            view={state.settings}
            onSave={(settings) => saved(api.saveSettings(settings))}
            onSaveKey={(provider, key) => saved(api.saveApiKey(provider, key))}
            onDeleteKey={(provider) => saved(api.deleteApiKey(provider))}
            onRevoke={(workspace, grant) => saved(api.revokeGrant(workspace, grant))}
          />
        ) : active ? (
          <>
            <SessionHeader
              session={active}
              onRename={(title) => api.renameSession(active.id, title).then((item) => dispatch({ type: "session", item }), fail)}
              onDelete={() => api.deleteSession(active.id).then(() => dispatch({ type: "deleted", id: active.id }), fail)}
            />
            {rows.length === 0 && state.queued.length === 0 ? (
              <div className="empty">
                <h1>分身已入驻这处洞府</h1>
                <p>
                  吩咐一件事吧。眼下的禁制是「{modeOf(active.mode).name}」：{modeOf(active.mode).hint}。碰这个文件夹之外的东西、改 .git
                  里的文件，无论哪种禁制都要先请本尊定夺。输入框里按 Shift+Tab 换禁制。
                </p>
              </div>
            ) : (
              <Transcript rows={rows} stream={state.stream} queued={state.queued} status={state.status} mode={active.mode} onDecide={decide} onUndo={(turn) => api.undoTurn(active.id, turn).catch(fail)} />
            )}
            <Composer
              status={state.status}
              model={modelLabel(active)}
              mode={active.mode}
              usage={spent}
              contextLimit={active.context_limit}
              planReady={planReady}
              onMode={(mode) => switchMode(mode).catch(fail)}
              onCarryOut={carryOut}
              onSend={send}
              onStop={() => api.cancelTurn(active.id).catch(fail)}
            />
          </>
        ) : (
          <>
            <Mountains />
            <div className="empty">
            <Logo size={64} />
            <h1>选一处洞府，化出分身来干活</h1>
            <p>白纸是本尊的法旨，黄符是分身的手笔，见朱砂处候本尊定夺。</p>
            {missingKey && <p className="muted">还没有 {defaultProvider?.name} 的密钥，分身出不了关，先到设置里填上。</p>}
            <div className="actions">
              <button type="button" className="button primary" onClick={create}>
                选文件夹作洞府，化出分身
              </button>
              {missingKey && (
                <button type="button" className="button outline" onClick={() => dispatch({ type: "page", page: "settings" })}>
                  去填密钥
                </button>
              )}
            </div>
            </div>
          </>
        )}
      </main>
      {showPanel && <SidePanel files={files} runs={runs} />}
    </div>
  );
}
