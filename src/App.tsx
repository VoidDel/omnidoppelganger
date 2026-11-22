import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { Desk } from "./components/Desk";
import { Guard } from "./components/Guard";
import { Jump } from "./components/Jump";
import { McpPage } from "./components/Mcp";
import { SkillsPage } from "./components/Skills";
import { Logo, Mountains } from "./components/Backdrop";
import { Composer } from "./components/Composer";
import { SessionHeader } from "./components/SessionHeader";
import { CHAT_ICON, Sidebar } from "./components/Sidebar";
import { PANEL_STRIP, PANEL_WIDTH, SidePanel, fitPanel } from "./components/SidePanel";
import { ReplayBar, STEP_MS } from "./components/Replay";
import { SchedulesPage } from "./components/Schedules";
import { SettingsPage } from "./components/Settings";
import { Transcript } from "./components/Transcript";
import { isSetUp } from "./lib/providers";
import { api } from "./lib/api";
import { applyDelta, changedFiles, commandRuns, fold, products, todos, usage, type StreamBlock } from "./lib/fold";
import { modeOf } from "./lib/modes";
import { replayStops, turnAt } from "./lib/replay";
import type { SlashCommand } from "./lib/slash";
import type { Decision, Envelope, Image, Mode, Reply, Schedule, SessionItem, SessionView, Settings, SettingsView, Status, UiEvent } from "./lib/types";

/** What "/init" hands the 分身: learn the project, and write down what a 分身 needs to know of it. */
const INIT_TASK =
  "读懂这个项目，在洞府根目录写一份 AGENTS.md，给以后在这里干活的分身看：项目是做什么的，目录怎么分，怎么构建、测试、运行，代码有哪些约定，哪里容易出错。只写读代码读不出来、或者要读很久才明白的；已经有 AGENTS.md 的话，补上缺的、改掉过时的，别的照旧。";
/** What "/review" hands the 分身: look over what has changed and not been committed, and change nothing. */
const REVIEW_TASK =
  "审查洞府里还没提交的改动（git diff，连同暂存的）：找出 bug、漏掉的边界情况、和项目约定不符的地方，以及缺了的测试。按严重程度列出，每条写明文件和行号、会出什么问题、怎么改。只看不改。";

interface State {
  sessions: SessionItem[];
  activeId: string | null;
  /** The session being opened: its log events are kept until its snapshot arrives. */
  openingId: string | null;
  buffer: Envelope[];
  /** The last status the session being opened reported while it was being opened: newer than the view that comes back. */
  bufferedStatus: Status | null;
  events: Envelope[];
  status: Status;
  stream: StreamBlock[];
  /** Messages sent during a turn that the agent has not picked up yet. */
  queued: string[];
  page: "chat" | "settings" | "schedules" | "mcp" | "skills";
  settings: SettingsView | null;
  /** The 功课, read when the app starts and again whenever one comes due. */
  schedules: Schedule[];
  error: string | null;
  /** Something that went well and is worth a line: shown until dismissed. */
  note: string | null;
}

type Action =
  | { type: "sessions"; sessions: SessionItem[] }
  | { type: "opening"; id: string }
  | { type: "opened"; view: SessionView }
  | { type: "session"; item: SessionItem }
  | { type: "deleted"; id: string }
  | { type: "home" }
  | { type: "event"; event: UiEvent }
  | { type: "queued"; id: string; text: string }
  | { type: "page"; page: State["page"] }
  | { type: "settings"; settings: SettingsView }
  | { type: "schedules"; schedules: Schedule[] }
  | { type: "error"; error: string | null }
  | { type: "note"; note: string | null };

const initial: State = {
  sessions: [],
  activeId: null,
  openingId: null,
  bufferedStatus: null,
  buffer: [],
  events: [],
  status: "idle",
  stream: [],
  queued: [],
  page: "chat",
  settings: null,
  schedules: [],
  error: null,
  note: null,
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
      return { ...state, openingId: action.id, buffer: [], bufferedStatus: null };
    case "opened": {
      const { events, ...view } = action.view;
      // A status reported while the view was on its way is at least as new as the view: a turn that ended
      // just after the view was taken would otherwise be shown as running for good.
      const item = state.openingId === view.id && state.bufferedStatus ? { ...view, status: state.bufferedStatus } : view;
      const opened: State = {
        ...state,
        activeId: item.id,
        openingId: null,
        buffer: [],
        bufferedStatus: null,
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
    case "home":
      // No 分身 on screen: the home screen, with what stands on its desk.
      return { ...state, activeId: null, openingId: null, buffer: [], bufferedStatus: null, events: [], stream: [], queued: [], status: "idle", page: "chat" };
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
      // Kept for the session being opened, which may be the one already on screen, opened again.
      const bufferedStatus = event.session_id === state.openingId ? event.status : state.bufferedStatus;
      if (!active) return { ...state, sessions, bufferedStatus };
      const idle = event.status === "idle";
      return { ...state, sessions, bufferedStatus, status: event.status, stream: idle ? [] : state.stream, queued: idle ? [] : state.queued };
    }
    case "queued":
      // Said to a 分身 that has since left the screen, it is not shown under another's work.
      return action.id === state.activeId ? { ...state, queued: [...state.queued, action.text] } : state;
    case "page":
      return { ...state, page: action.page };
    case "settings":
      return { ...state, settings: action.settings };
    case "schedules":
      return { ...state, schedules: action.schedules };
    case "error":
      return { ...state, error: action.error };
    case "note":
      return { ...state, note: action.note };
  }
}

/** How the user left the side panel, kept in the window's own storage: it is about this screen, not the agent. */
const PANEL_KEY = "odg.panel";

function usePanel() {
  const [panel, setPanel] = useState<{ width: number; hidden: boolean }>(() => {
    try {
      const kept = JSON.parse(localStorage.getItem(PANEL_KEY) ?? "{}");
      return { width: typeof kept.width === "number" ? fitPanel(kept.width) : PANEL_WIDTH.initial, hidden: kept.hidden === true };
    } catch {
      return { width: PANEL_WIDTH.initial, hidden: false };
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(PANEL_KEY, JSON.stringify(panel));
    } catch {
      // Storage may be unavailable; the panel then starts out as it does the first time.
    }
  }, [panel]);
  return [panel, setPanel] as const;
}

/** Where the pointer last went down: a change of theme spreads from there. */
const pressed = { x: 0, y: 0 };
if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", (event) => Object.assign(pressed, { x: event.clientX, y: event.clientY }), true);
}

/**
 * Switch the page to a theme. Once the page is up, the new one spreads over the old as ink does, from where the
 * pointer last went down, wherever the browser can draw that and the system has not asked for stillness.
 */
function setTheme(next: "light" | "dark", spread: boolean) {
  const root = document.documentElement;
  if (root.dataset.theme === next) return;
  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const swap = () => {
    root.dataset.theme = next;
  };
  if (!spread || still || !("startViewTransition" in document)) return swap();
  const x = pressed.x || window.innerWidth - 40;
  const y = pressed.y || 40;
  const reach = Math.hypot(Math.max(x, window.innerWidth - x), Math.max(y, window.innerHeight - y));
  root.style.setProperty("--spread-x", `${x}px`);
  root.style.setProperty("--spread-y", `${y}px`);
  root.style.setProperty("--spread-r", `${reach}px`);
  (document as Document & { startViewTransition: (update: () => void) => unknown }).startViewTransition(swap);
}

function useTheme(theme: Settings["theme"] | undefined) {
  /** Whether the saved theme has been read: until then, and at the reading itself, the page is only being put right. */
  const read = useRef(false);
  useEffect(() => {
    const system = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = (spread: boolean) => setTheme(theme === "dark" || (theme !== "light" && system.matches) ? "dark" : "light", spread);
    apply(read.current);
    if (theme !== undefined) read.current = true;
    const follow = () => apply(true);
    system.addEventListener("change", follow);
    return () => system.removeEventListener("change", follow);
  }, [theme]);
}

export default function App() {
  const [state, dispatch] = useReducer(reduce, initial);
  /** Whether the Ctrl+K sheet, the way to go anywhere, is open. */
  const [jumping, setJumping] = useState(false);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        // Not over a dialog that is asking something: what it asks is about the 分身 on screen.
        if (document.querySelector("dialog[open]:not(.jump)")) return;
        setJumping((open) => !open);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, []);
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
    const readSchedules = () => api.listSchedules().then((schedules) => dispatch({ type: "schedules", schedules }), fail);
    readSchedules();
    // A 功课 that came due started a 分身 nobody opened: the list shows it once it is read again.
    const stopSchedules = api.onSchedulesChanged(() => {
      readSchedules();
      refresh();
    });
    return () => {
      stop.then((unlisten) => unlisten());
      stopSchedules.then((unlisten) => unlisten());
    };
  }, [refresh, fail]);

  useTheme(state.settings?.settings.theme);

  /** The replay of the session on the page: which stop it is at. Null while the page shows the present. */
  const [replay, setReplay] = useState<{ at: number; playing: boolean; speed: number } | null>(null);
  // A replay is of one session: opening another, or leaving the conversation, ends it.
  useEffect(() => setReplay(null), [state.activeId, state.page]);
  const stops = useMemo(() => replayStops(state.events), [state.events]);
  const playing = !!replay?.playing;
  const speed = replay?.speed ?? 1;
  useEffect(() => {
    if (!playing) return;
    const timer = setInterval(() => setReplay((now) => (now && now.at < stops.length - 1 ? { ...now, at: now.at + 1 } : now && { ...now, playing: false })), STEP_MS / speed);
    return () => clearInterval(timer);
  }, [playing, speed, stops.length]);
  /** What the page shows: the whole log, or in a replay the log as far as the stop it is at. */
  const shown = useMemo(() => (replay ? state.events.slice(0, (stops[replay.at] ?? -1) + 1) : state.events), [replay?.at, !replay, state.events, stops]);
  /** Words a fork brings to the message box. */
  const [seed, setSeed] = useState<{ id: number; text: string } | null>(null);

  const rows = useMemo(() => fold(shown), [shown]);
  const files = useMemo(() => changedFiles(rows), [rows]);
  const runs = useMemo(() => commandRuns(rows), [rows]);
  const made = useMemo(() => products(rows), [rows]);
  const spent = useMemo(() => usage(state.events), [state.events]);
  const plan = useMemo(() => todos(rows), [rows]);
  const active = state.sessions.find((session) => session.id === state.activeId);
  // Through the session on the page, steady while it is the one: the conversation's cards are not drawn again for them.
  const activeId = state.activeId;
  const openFile = useCallback((path: string, reveal: boolean) => (activeId ? api.openAttachment(activeId, path, reveal) : Promise.reject("没有打开的分身")), [activeId]);
  const pictureOf = useCallback((path: string) => (activeId ? api.picture(activeId, path) : Promise.reject("没有打开的分身")), [activeId]);

  const open = (id: string) => {
    dispatch({ type: "opening", id });
    api.openSession(id).then((view) => dispatch({ type: "opened", view }), fail);
  };
  /** Fork the session on the page: before `turn`, bringing that turn's message to the box; or all of it. */
  const forkFrom = (turn?: number, text?: string) => {
    if (!state.activeId) return;
    api.forkSession(state.activeId, turn).then((view) => {
      dispatch({ type: "opened", view });
      if (text !== undefined) setSeed({ id: Date.now(), text });
      const point = turn === undefined ? "带着到现在为止的全部对话" : turn > 1 ? `带着第 ${turn} 轮之前的对话，那一轮的话放回了输入框` : "从头来过，头一句话放回了输入框";
      dispatch({ type: "note", note: `已分叉：这是一个新的分身，${point}。只分了对话：洞府里的文件是同一份，还是现在的样子。` });
    }, fail);
  };
  /** Start a conversation: a 分身 in the folder kept for them, which is made the first time. */
  const chat = () => api.startChat().then((view) => dispatch({ type: "opened", view }), fail);
  /** Whether a session works in the folder kept for conversations. */
  const chatting = (session: SessionItem) => {
    const plain = (path: string) => path.replace(/[\\/]+$/, "").toLowerCase();
    return !!state.settings && plain(session.workspace) === plain(state.settings.chat_folder);
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
  const send = (text: string, images: Image[] = [], files: string[] = []) => {
    const id = state.activeId;
    if (!id) return;
    api.sendMessage(id, text, images, files).then((queued) => queued && dispatch({ type: "queued", id, text: text.trim() }), fail);
  };
  const decide = (toolUseId: string, decision: Decision) => {
    if (state.activeId) api.resolveApproval(state.activeId, toolUseId, decision).catch(fail);
  };
  const answer = (toolUseId: string, answers: Reply[] | null) => {
    if (state.activeId) api.answerQuestions(state.activeId, toolUseId, answers).catch(fail);
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
  // The last turn in plan mode ended with the 分身's plan, presented as one or written as its answer; not a stop or an error.
  const lastItems = rows[rows.length - 1]?.items ?? [];
  const last = lastItems[lastItems.length - 1];
  const planReady = active?.mode === "plan" && (last?.kind === "text" || (last?.kind === "tool" && last.name === "present_plan" && !!last.result && !last.result.isError));
  const refreshSettings = useCallback(() => {
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
  }, [fail]);
  const saved = (request: Promise<SettingsView>) => request.then((settings) => dispatch({ type: "settings", settings }), fail);
  /** Take the list a change to the 功课 came back with; resolves with what went wrong, for the page to show where the change was made. */
  const scheduled = (request: Promise<Schedule[]>) =>
    request.then(
      (schedules) => {
        dispatch({ type: "schedules", schedules });
        return null;
      },
      (error) => String(error),
    );

  const providers = state.settings?.providers ?? [];
  const defaultProvider = providers.find((provider) => provider.id === state.settings?.settings.provider);
  const missingKey = !!defaultProvider && state.settings?.keys[defaultProvider.id]?.source === "none";
  const inSettings = state.page === "settings";
  const inChat = state.page === "chat";
  const [panel, setPanel] = usePanel();
  const showPanel = inChat && !!active && !panel.hidden;
  // With nothing in it, the panel stands as a strip, until a tab there is pressed; the next 分身 starts again.
  const [peek, setPeek] = useState(false);
  useEffect(() => setPeek(false), [state.activeId]);
  const strip = files.length === 0 && runs.length === 0 && plan.length === 0 && made.length === 0 && !peek;
  /** To the 功课 page and the settings, each read again on the way: either may have changed since it was last shown. */
  const toSchedules = () => {
    api.listSchedules().then((schedules) => dispatch({ type: "schedules", schedules }), fail);
    dispatch({ type: "page", page: "schedules" });
  };
  const toSettings = () => {
    // A command may have been allowed for good since the page was last shown.
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
    dispatch({ type: "page", page: "settings" });
  };
  /** To the 法器, read again on the way: a server may have connected, or stopped, since. */
  const toMcp = () => {
    api.getSettings().then((settings) => dispatch({ type: "settings", settings }), fail);
    dispatch({ type: "page", page: "mcp" });
  };
  // The skills the 分身 was last told of, its project's among them; before its first turn, the user's own.
  const listedSkills = useMemo(() => {
    for (let at = state.events.length - 1; at >= 0; at--) {
      const event = state.events[at];
      if (event.type === "skills/listed") return event.data.skills;
    }
    return null;
  }, [state.events]);
  /** The "/" commands the window gives the box: what acts on this 分身, the pages, tasks to hand it, and its skills. */
  const slashCommands = (session: SessionItem): SlashCommand[] => {
    const working = state.status !== "idle" ? "分身运功时用不了，等它收功，或先收回它" : undefined;
    const builtIn: SlashCommand[] = [
      {
        name: "new",
        label: "新分身",
        aliases: ["新分身", "化出"],
        hint: "在这处洞府再化出一个分身",
        run: () => {
          api.createSession(session.worktree?.origin ?? session.workspace, false).then((view) => dispatch({ type: "opened", view }), fail);
        },
      },
      { name: "fork", label: "分叉", aliases: ["分叉"], hint: "分叉：另起一个分身，带着到现在的全部对话", unavailable: session.parent ? "子分身的会话不能分叉" : undefined, run: () => forkFrom() },
      {
        name: "rename",
        label: "改名",
        aliases: ["改名"],
        hint: "给这个分身改名；不写名字就用第一句话作名",
        arg: "<新名字>",
        run: (title) => {
          api.renameSession(session.id, title).then((item) => dispatch({ type: "session", item }), fail);
        },
      },
      {
        name: "replay",
        label: "回溯",
        aliases: ["回溯"],
        hint: "回溯：从头一步步看这个分身做过什么",
        unavailable: stops.length === 0 ? "这个分身还没做过什么，没有可回溯的" : undefined,
        run: () => setReplay({ at: 0, playing: true, speed: 1 }),
      },
      { name: "init", label: "立规矩", aliases: ["立规矩", "规矩"], hint: "让分身读懂这个项目，写一份 AGENTS.md", unavailable: working, run: () => send(INIT_TASK) },
      { name: "review", label: "审查", aliases: ["审查"], hint: "让分身审查还没提交的改动，只看不改", unavailable: working, run: () => send(REVIEW_TASK) },
      { name: "mcp", label: "法器", aliases: ["法器"], hint: "打开法器页：MCP 服务", run: toMcp },
      { name: "skills", label: "秘籍", aliases: ["秘籍"], hint: "打开秘籍页：收入或收起秘籍", run: () => dispatch({ type: "page", page: "skills" }) },
      { name: "schedule", label: "功课", aliases: ["功课"], hint: "打开功课页：到点自行去办的事", run: toSchedules },
      { name: "settings", label: "设置", aliases: ["设置"], hint: "打开设置", run: toSettings },
    ];
    const taken = new Set([...builtIn.map((command) => command.name), "compact", "model", "effort", "mode"]);
    // Each skill by its name, with what it is to do after it; one named as a command is reached by its alias.
    const skills: SlashCommand[] = (listedSkills ?? state.settings?.skills ?? []).map((skill) => {
      const name = skill.name.trim().replace(/\s+/g, "-");
      return {
        name: taken.has(name.toLowerCase()) ? `skill-${name}` : name,
        label: "秘籍",
        aliases: [skill.name],
        hint: `秘籍：${skill.description}`,
        arg: "<要办的事>",
        unavailable: working,
        run: (task) => send(task ? `按秘籍「${skill.name}」（skill）办：${task}` : `按秘籍「${skill.name}」（skill）办。`),
      };
    });
    // Learning the project and reviewing its changes are for a project, not a conversation.
    const fitting = chatting(session) ? builtIn.filter((command) => command.name !== "init" && command.name !== "review") : builtIn;
    return [...fitting, ...skills];
  };
  const banners = (
    <>
        {state.error && (
          <div className="error-banner" role="alert">
            <span style={{ flex: 1, overflowWrap: "anywhere" }}>{state.error}</span>
            {/* 本尊's seal on what was read: stamping it puts the notice away. */}
            <button type="button" className="seal seal-button" title="阅过了，收起这一条" onClick={() => dispatch({ type: "error", error: null })}>
              阅<span className="sr-only">过了，收起这一条</span>
            </button>
          </div>
        )}
        {state.note && (
          <div className="error-banner note" role="status">
            <span style={{ flex: 1, overflowWrap: "anywhere" }}>{state.note}</span>
            {/* 本尊's seal on what was read: stamping it puts the notice away. */}
            <button type="button" className="seal seal-button" title="阅过了，收起这一条" onClick={() => dispatch({ type: "note", note: null })}>
              阅<span className="sr-only">过了，收起这一条</span>
            </button>
          </div>
        )}
    </>
  );
  return (
    <div className={`app ${showPanel ? "with-panel" : ""}`} style={{ "--panel-width": `${strip ? PANEL_STRIP : panel.width}px` } as React.CSSProperties}>
      {/* Each part of the window is guarded by itself: one that cannot be drawn leaves the others in use. */}
      <Guard name="分身列表" className="sidebar">
      <Sidebar
        sessions={state.sessions}
        activeId={state.activeId}
        page={state.page}
        onHome={() => dispatch({ type: "home" })}
        onMcp={toMcp}
        onSkills={() => dispatch({ type: "page", page: "skills" })}
        onSchedules={toSchedules}
        onChat={chat}
        onOpen={open}
        onNew={create}
        onNewIn={(workspace, isolated) => api.createSession(workspace, isolated).then((view) => dispatch({ type: "opened", view }), fail)}
        onSettings={toSettings}
        onSearch={api.searchSessions}
      />
      </Guard>
      <main className="main">
        {/* What went wrong, or went well: under the session's title where there is one, at the top of the page otherwise. */}
        {!(inChat && active) && banners}
        {inSettings && state.settings ? (
          <Guard name="设置" className="fill" resetKey={state.page}>
          <SettingsPage
            view={state.settings}
            onSave={(settings) => saved(api.saveSettings(settings))}
            onSaveKey={(provider, key) => saved(api.saveApiKey(provider, key))}
            onAddProvider={(kind, name) =>
              api.addProvider(kind, name).then(
                (added) => {
                  dispatch({ type: "settings", settings: added.view });
                  return added;
                },
                (error) => {
                  fail(error);
                  return null;
                },
              )
            }
            onRemoveProvider={(provider) => saved(api.removeProvider(provider))}
            onRevoke={(workspace, grant) => saved(api.revokeGrant(workspace, grant))}
            onRefresh={refreshSettings}
            listModels={api.listModels}
          />
          </Guard>
        ) : state.page === "mcp" && state.settings ? (
          <Guard name="法器" className="fill" resetKey={state.page}>
          <McpPage
            config={state.settings.mcp_config}
            servers={state.settings.mcp_servers}
            home={state.settings.home}
            onRefresh={refreshSettings}
            onSave={(config) =>
              api.saveMcpConfig(config).then(
                (settings) => {
                  dispatch({ type: "settings", settings });
                  return null;
                },
                (error) => String(error),
              )
            }
          />
          </Guard>
        ) : state.page === "skills" ? (
          <Guard name="秘籍" className="fill" resetKey={state.page}>
            <SkillsPage onChanged={refreshSettings} />
          </Guard>
        ) : state.page === "schedules" ? (
          <Guard name="功课" className="fill" resetKey={state.page}>
          <SchedulesPage
            schedules={state.schedules}
            sessions={state.sessions}
            mode={state.settings?.settings.mode ?? "edit"}
            workspace={active ? (active.worktree?.origin ?? active.workspace) : (state.sessions.find((session) => !session.parent)?.workspace ?? "")}
            onOpen={open}
            onPick={api.pickWorkspace}
            onSave={(schedule) => scheduled(api.saveSchedule(schedule))}
            onDelete={(id) => scheduled(api.deleteSchedule(id))}
            onRun={(id) =>
              api.runSchedule(id).then(
                (schedules) => {
                  dispatch({ type: "schedules", schedules });
                  // Asked for by hand, so shown: the 分身 it started, when it started one.
                  const last = schedules.find((schedule) => schedule.id === id)?.last;
                  if (last?.session && !last.error) open(last.session);
                  return last?.error ?? null;
                },
                (error) => String(error),
              )
            }
          />
          </Guard>
        ) : active ? (
          <>
            <Guard name="标题栏" className="bar" resetKey={active.id}>
            <SessionHeader
              session={active}
              parent={state.sessions.find((session) => session.id === active.parent)}
              onOpenParent={() => active.parent && open(active.parent)}
              source={state.sessions.find((session) => session.id === active.fork?.session)}
              onOpenSource={() => active.fork && open(active.fork.session)}
              onFork={() => forkFrom()}
              replaying={!!replay}
              onReplay={() => setReplay(replay || stops.length === 0 ? null : { at: 0, playing: true, speed: 1 })}
              onLand={() =>
                api.landSession(active.id).then((landed) => {
                  const note = landed.commits ? `已并回：${landed.commits} 个提交合并进了洞府的 ${landed.into} 分支。` : "这个分身的工作树里没有新的改动，没什么可并回的。";
                  dispatch({ type: "note", note });
                  refresh();
                }, fail)
              }
              panelHidden={panel.hidden}
              onTogglePanel={() => setPanel({ ...panel, hidden: !panel.hidden })}
              onRename={(title) => api.renameSession(active.id, title).then((item) => dispatch({ type: "session", item }), fail)}
              onDelete={() => api.deleteSession(active.id).then(() => dispatch({ type: "deleted", id: active.id }), fail)}
            />
            </Guard>
            {banners}
            {rows.length === 0 && state.queued.length === 0 && chatting(active) ? (
              <div className="empty">
                <h1>有话直说</h1>
                <p>
                  这是闲谈，不绑哪个项目：问什么答什么，要查的它会上网查，附上的文件它会读。它写出的文件放在 <span className="mono">{state.settings?.chat_folder}</span>。要它在某个项目里干活，就用「开辟洞府」选那个文件夹。
                </p>
              </div>
            ) : rows.length === 0 && state.queued.length === 0 ? (
              <div className="empty">
                <h1>分身已入驻这处洞府</h1>
                <p>
                  吩咐一件事吧。眼下的禁制是「{modeOf(active.mode).name}」：{modeOf(active.mode).hint}。
                  {active.mode === "bypass"
                    ? "它不会停下来请示任何事，碰洞府之外的东西、改 .git 也一样。"
                    : "碰这个文件夹之外的东西、改 .git 里的文件，无论哪种禁制都要先请本尊定夺。"}
                  输入框里按 Shift+Tab 换禁制。
                </p>
              </div>
            ) : (
              <Guard name="对话" className="fill" resetKey={active.id}>
              <Transcript
                key={active.id}
                rows={rows}
                stream={replay ? [] : state.stream}
                queued={replay ? [] : state.queued}
                status={replay ? "idle" : state.status}
                mode={active.mode}
                origin={active.parent ? "agent" : active.schedule ? "schedule" : "user"}
                sessions={state.sessions}
                copied={active.fork?.turn ?? 0}
                frozen={!!replay}
                onOpen={open}
                onDecide={decide}
                onAnswer={answer}
                onUndo={(turn) => api.undoTurn(active.id, turn).catch(fail)}
                onFork={active.parent ? undefined : forkFrom}
                onOpenFile={openFile}
                onPicture={pictureOf}
              />
              </Guard>
            )}
            {replay ? (
              <ReplayBar
                at={replay.at}
                count={stops.length}
                playing={replay.playing}
                speed={replay.speed}
                turn={turnAt(state.events, stops[replay.at] ?? 0)}
                time={new Date(state.events[stops[replay.at] ?? 0]?.ts ?? Date.now()).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
                onSeek={(at) => setReplay((now) => now && { ...now, at })}
                onPlaying={(playing) => setReplay((now) => now && { ...now, playing })}
                onSpeed={(speed) => setReplay((now) => now && { ...now, speed })}
                onExit={() => setReplay(null)}
              />
            ) : null}
            {/* Kept while a replay stands in its place, so what was being typed is still there afterwards. */}
            <div style={{ display: replay ? "none" : "contents" }}>
            <Guard name="输入框" className="under" resetKey={active.id}>
            <Composer
              status={state.status}
              providers={providers}
              // The 宗门 the user added are offered; one can be moved to once it has a key, but a compatible service (a local server) may need none.
              listed={(provider) => !!state.settings && !!providers.find((known) => known.id === provider && isSetUp(state.settings!, known))}
              keyed={(provider) => (state.settings?.keys[provider]?.source ?? "none") !== "none" || !!providers.find((known) => known.id === provider)?.kind.startsWith("custom")}
              providerId={active.provider}
              model={active.model}
              effort={active.effort ?? state.settings?.settings.effort ?? "high"}
              effortSet={active.effort !== null}
              efforts={state.settings?.efforts ?? ["low", "medium", "high", "xhigh", "max"]}
              onEffort={(effort) => api.setEffort(active.id, effort).then((item) => dispatch({ type: "session", item }), fail)}
              listModels={api.listModels}
              onModel={(provider, model) => api.setModel(active.id, provider, model).then((item) => dispatch({ type: "session", item }), fail)}
              vision={active.vision}
              mode={active.mode}
              usage={spent}
              contextLimit={active.context_limit}
              contextCeiling={active.context_ceiling}
              contextSetting={state.settings?.settings.context_limit ?? active.context_limit}
              contextAuto={!!state.settings?.settings.context_auto}
              onLimit={(limit) => {
                // The limit is one setting for every session; each session's own figure comes back with the list.
                if (state.settings)
                  saved(api.saveSettings(limit === "auto" ? { ...state.settings.settings, context_auto: true } : { ...state.settings.settings, context_limit: limit, context_auto: false })).then(refresh);
              }}
              planReady={planReady}
              onMode={(mode) => switchMode(mode).catch(fail)}
              onCarryOut={carryOut}
              onSend={send}
              onStop={() => api.cancelTurn(active.id).catch(fail)}
              onCompact={() => api.compactSession(active.id).catch(fail)}
              trustable={!!active.worktree}
              seed={seed}
              commands={slashCommands(active)}
            />
            </Guard>
            </div>
          </>
        ) : (
          <>
            <Mountains />
            <div className="empty">
            <Logo size={64} alive />
            <h1>选一处洞府，化出分身来干活</h1>
            <p>白纸是本尊的法旨，墨迹是分身的手笔，见黄符朱砂处候本尊定夺。</p>
            {missingKey && <p className="muted">还没有 {defaultProvider?.name} 的密钥，分身出不了关，先到设置里填上。</p>}
            <div className="actions">
              <button type="button" className="button primary" onClick={create}>
                选文件夹作洞府，化出分身
              </button>
              <button type="button" className="button outline" onClick={chat}>
                {CHAT_ICON}
                不选文件夹，直接闲谈
              </button>
              {missingKey && (
                <button type="button" className="button outline" onClick={() => dispatch({ type: "page", page: "settings" })}>
                  去填密钥
                </button>
              )}
            </div>
            <Desk sessions={state.sessions} schedules={state.schedules} onOpen={open} onSchedules={toSchedules} />
            </div>
          </>
        )}
      </main>
      {jumping && (
        <Jump
          sessions={state.sessions}
          acts={[
            { id: "home", name: "首页", hint: "回到山前：候旨的、运功的、将至的功课", seal: "山", run: () => dispatch({ type: "home" }) },
            { id: "new", name: "开辟洞府", hint: "选一个文件夹，化出分身", seal: "辟", run: () => void create() },
            { id: "chat", name: "闲谈", hint: "不选文件夹，直接和分身说话", seal: "谈", run: chat },
            { id: "skills", name: "秘籍", hint: "随程序带来的做事说明，打开才收入；还有自己的", seal: "籍", run: () => dispatch({ type: "page", page: "skills" }) },
            { id: "mcp", name: "法器", hint: "给分身添上本事的 MCP 服务：查文档、搜网页、操控浏览器", seal: "器", run: toMcp },
            { id: "schedules", name: "功课", hint: "到了时辰自行化出分身去办的事", seal: "课", run: toSchedules },
            { id: "settings", name: "设置", hint: "心法、禁制、神通、外观", seal: "设", run: toSettings },
          ]}
          onOpen={open}
          onSearch={api.searchSessions}
          onClose={() => setJumping(false)}
        />
      )}
      {showPanel && (
        <Guard name="侧栏" className="panel" resetKey={state.activeId}>
          <SidePanel files={files} runs={runs} todos={plan} products={made} workspace={active?.workspace} width={panel.width} onResize={(width) => setPanel({ ...panel, width })} narrow={strip} onWiden={() => setPeek(true)} working={state.status === "running"} onOpenFile={openFile} />
        </Guard>
      )}
    </div>
  );
}
