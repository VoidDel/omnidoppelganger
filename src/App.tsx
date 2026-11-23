import { Suspense, lazy, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { Desk } from "./components/Desk";
import { Guard } from "./components/Guard";
import { Jump } from "./components/Jump";
import { McpPage } from "./components/Mcp";
import { SkillsPage } from "./components/Skills";
import { AsidePane } from "./components/AsidePane";
import { Logo, Mountains } from "./components/Backdrop";
import { Composer, type ComposerDraft } from "./components/Composer";
import { DiffViewer } from "./components/DiffViewer";
import { FileTree, type GitBadge } from "./components/FileTree";
import { GitPanel } from "./components/GitPanel";
import { SessionHeader } from "./components/SessionHeader";
import { CHAT_ICON, SIDEBAR_WIDTH, Sidebar, type SidebarTab, fitSidebar } from "./components/Sidebar";
import { PANEL_STRIP, PANEL_WIDTH, type PanelTab, SidePanel, fitPanel } from "./components/SidePanel";
import { ReplayBar, STEP_MS } from "./components/Replay";
import { SchedulesPage } from "./components/Schedules";
import { SettingsPage } from "./components/Settings";
import { Transcript } from "./components/Transcript";
import { Workbench, WorkbenchTabsBar, type WorkbenchTab } from "./components/Workbench";

const TerminalPane = lazy(() => import("./components/TerminalPane"));
import { isSetUp } from "./lib/providers";
import { api } from "./lib/api";
import { forwarded } from "./lib/aside";
import { applyDelta, changedFiles, commandRuns, fold, products, todos, usage, type StreamBlock } from "./lib/fold";
import { modeOf } from "./lib/modes";
import { replayStops, turnAt } from "./lib/replay";
import type { SlashCommand } from "./lib/slash";
import type { Decision, DiffStage, Envelope, GitStatus, Image, Mode, Reply, Schedule, SessionItem, SessionView, Settings, SettingsView, Status, UiEvent } from "./lib/types";

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
const SIDEBAR_KEY = "odg.sidebar";

function useSidebar() {
  const [sidebar, setSidebar] = useState<{ width: number; hidden: boolean }>(() => {
    try {
      const kept = JSON.parse(localStorage.getItem(SIDEBAR_KEY) ?? "{}");
      return {
        width: typeof kept.width === "number" ? fitSidebar(kept.width) : SIDEBAR_WIDTH.initial,
        hidden: kept.hidden === true,
      };
    } catch {
      return { width: SIDEBAR_WIDTH.initial, hidden: false };
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_KEY, JSON.stringify(sidebar));
    } catch {
      // Ignore storage errors
    }
  }, [sidebar]);
  return [sidebar, setSidebar] as const;
}

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
  /** Which quick-jump sheet is open: `"all"` (Ctrl+K), `"files"` (Ctrl+P), or null. */
  const [jumping, setJumping] = useState<"all" | "files" | null>(null);
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
  /** Words or file attachments to inject into the message box. */
  const [seed, setSeed] = useState<{ id: number; text?: string; append?: boolean; attachPaths?: string[] } | null>(null);
  /** Shared composer drafts by session id so toggling center/side chat keeps typed words and attachments intact. */
  const draftsRef = useRef<Map<string, ComposerDraft>>(new Map());

  const rows = useMemo(() => fold(shown), [shown]);
  const files = useMemo(() => changedFiles(rows), [rows]);
  const runs = useMemo(() => commandRuns(rows), [rows]);
  const made = useMemo(() => products(rows), [rows]);
  const spent = useMemo(() => usage(state.events), [state.events]);
  const plan = useMemo(() => todos(rows), [rows]);
  const active = state.sessions.find((session) => session.id === state.activeId);
  // Through the session on the page, steady while it is the one: the conversation's cards are not drawn again for them.
  const activeId = state.activeId;
  const openFile = useCallback(
    (path: string, reveal: boolean, snapshot?: string, version?: number) => (activeId ? api.openAttachment(activeId, path, reveal, snapshot, version) : Promise.reject("没有打开的分身")),
    [activeId],
  );
  const pictureOf = useCallback((path: string, snapshot?: string) => (activeId ? api.picture(activeId, path, snapshot) : Promise.reject("没有打开的分身")), [activeId]);
  const restoreArtifact = useCallback(
    (path: string, snapshot: string, version: number) => (activeId ? api.restoreArtifact(activeId, path, snapshot, version) : Promise.reject("没有打开的分身")),
    [activeId],
  );
  const saveArtifactCopy = useCallback(
    (path: string, snapshot: string, version: number) => (activeId ? api.saveArtifactCopy(activeId, path, snapshot, version) : Promise.reject("没有打开的分身")),
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
  const [sidebar, setSidebar] = useSidebar();
  const [leftTab, setLeftTab] = useState<SidebarTab>("sessions");
  const [panel, setPanel] = usePanel();
  const [summonPanelTab, setSummonPanelTab] = useState<{ id: number; tab: PanelTab } | null>(null);

  // Workbench tabs for the active workspace (file viewer & git diff viewer).
  const [wbTabs, setWbTabs] = useState<WorkbenchTab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [inWorkbench, setInWorkbench] = useState(false);
  const [manualTick, setManualTick] = useState(0);
  const [gitState, setGitState] = useState<GitStatus | null>(null);

  const isProjectSession = !!active && !chatting(active);
  const activeWorkspace = active?.workspace ?? "";
  const lastWorkspaceRef = useRef(activeWorkspace);
  useEffect(() => {
    if (lastWorkspaceRef.current !== activeWorkspace) {
      lastWorkspaceRef.current = activeWorkspace;
      setWbTabs([]);
      setActiveTabId(null);
      setInWorkbench(false);
      setGitState(null);
    }
    if (!isProjectSession && leftTab !== "sessions") {
      setLeftTab("sessions");
    }
  }, [activeWorkspace, isProjectSession, leftTab]);

  // Auto-refresh token for FileTree, GitPanel, and CodeViewer whenever session events or user actions touch disk.
  const fsRefreshTick = useMemo(() => {
    let count = manualTick;
    for (const env of state.events) {
      if (
        env.type === "files/made" ||
        env.type === "files/restored" ||
        env.type === "artifact/restored" ||
        (env.type === "tool/result" &&
          !env.data.is_error &&
          ["edit_file", "write_file", "delete_file", "move_file", "run_command"].includes(env.data.tool))
      ) {
        count++;
      }
    }
    return count;
  }, [state.events, manualTick]);

  const bumpRefresh = useCallback(() => setManualTick((n) => n + 1), []);

  // Watch the active project workspace on disk so external edits and PTY git/file commands auto-refresh UI.
  useEffect(() => {
    const targetId = inChat && isProjectSession && activeId ? activeId : null;
    api.watchWorkspace(targetId).catch(() => undefined);
  }, [inChat, isProjectSession, activeId, activeWorkspace]);

  useEffect(() => {
    // A change on disk refreshes the tree, Git and what is open; the list of 分身 does not depend on it.
    const stop = api.onWorkspaceChanged(() => bumpRefresh());
    return () => {
      stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [bumpRefresh]);

  // Git status: at once for a newly opened workspace, then once per burst of changes, not once per change.
  // A status that comes back after a newer one was asked for still lands, unless the workspace changed meanwhile.
  const gitKey = useRef<string | null>(null);
  useEffect(() => {
    if (!isProjectSession || !activeId) {
      gitKey.current = null;
      setGitState(null);
      return;
    }
    const key = `${activeId}\u0000${activeWorkspace}`;
    const fresh = gitKey.current !== key;
    gitKey.current = key;
    const timer = setTimeout(
      () =>
        api.gitStatus(activeId).then(
          (st) => gitKey.current === key && setGitState(st),
          () => gitKey.current === key && setGitState(null),
        ),
      fresh ? 0 : 300,
    );
    return () => clearTimeout(timer);
  }, [isProjectSession, activeId, activeWorkspace, fsRefreshTick]);

  // A branch switched outside the app (in the terminal, or another tool) shows in the title bar once Git sees it.
  const seenBranch = useRef<string | null>(null);
  useEffect(() => {
    const branch = gitState?.is_repo ? gitState.branch : null;
    if (branch && seenBranch.current && branch !== seenBranch.current) refresh();
    seenBranch.current = branch;
  }, [gitState?.is_repo, gitState?.branch, refresh]);

  const gitBadges = useMemo<Record<string, GitBadge>>(() => {
    if (!gitState?.is_repo) return {};
    const map: Record<string, GitBadge> = {};
    for (const f of gitState.staged) {
      map[f.path] = { label: f.status, tone: f.status === "A" ? "added" : f.status === "D" ? "deleted" : "modified" };
    }
    for (const f of gitState.unstaged) {
      map[f.path] = { label: f.status, tone: f.status === "D" ? "deleted" : "modified" };
    }
    for (const f of gitState.untracked) {
      map[f.path] = { label: "U", tone: "added" };
    }
    for (const f of gitState.conflicts) {
      map[f.path] = { label: "!", tone: "conflict" };
    }
    return map;
  }, [gitState]);

  const gitChangeCount = gitState?.is_repo
    ? gitState.conflicts.length + gitState.staged.length + gitState.unstaged.length + gitState.untracked.length
    : 0;

  const openWorkbenchFile = useCallback(
    (relPath: string, _pin = false, line?: number) => {
      const clean = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
      if (!clean) return;
      const name = clean.split("/").pop() || clean;
      const id = `file:${clean}`;
      setWbTabs((prev) => {
        const existing = prev.find((t) => t.id === id);
        if (existing && existing.kind === "file") {
          return prev.map((t) =>
            t.id === id && t.kind === "file"
              ? { ...t, line: line ?? t.line, jumpSeq: (t.jumpSeq ?? 0) + 1 }
              : t,
          );
        }
        const nextTab: WorkbenchTab = { id, kind: "file", path: clean, name, line, jumpSeq: line ? 1 : 0 };
        return [...prev, nextTab];
      });
      setActiveTabId(id);
      setInWorkbench(true);
    },
    [],
  );

  const openWorkbenchDiff = useCallback(
    (relPath: string, stage: DiffStage, _pin = false, commit?: string) => {
      const clean = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
      if (!clean) return;
      const name = clean.split("/").pop() || clean;
      const id = `diff:${stage}:${commit ?? ""}:${clean}`;
      setWbTabs((prev) => {
        const existing = prev.find((t) => t.id === id);
        if (existing) return prev;
        const nextTab: WorkbenchTab = { id, kind: "diff", path: clean, name, stage, commit };
        return [...prev, nextTab];
      });
      setActiveTabId(id);
      setInWorkbench(true);
    },
    [],
  );

  const moveWorkbenchTab = useCallback((fromId: string, toId: string) => {
    setWbTabs((prev) => {
      const fromIdx = prev.findIndex((t) => t.id === fromId);
      const toIdx = prev.findIndex((t) => t.id === toId);
      if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return prev;
      const next = [...prev];
      const [moved] = next.splice(fromIdx, 1);
      next.splice(toIdx, 0, moved);
      return next;
    });
  }, []);

  const closeWorkbenchTab = useCallback((id: string) => {
    setWbTabs((prev) => {
      const idx = prev.findIndex((t) => t.id === id);
      if (idx === -1) return prev;
      const next = prev.filter((t) => t.id !== id);
      setActiveTabId((cur) => {
        if (cur !== id) return cur;
        return next[Math.min(idx, next.length - 1)]?.id ?? null;
      });
      if (next.length === 0) setInWorkbench(false);
      return next;
    });
  }, []);

  const handleDeletedWorkspacePath = useCallback((relPath: string) => {
    setWbTabs((prev) => {
      const next = prev.filter((t) => t.path !== relPath && !t.path.startsWith(`${relPath}/`));
      if (next.length === 0) setInWorkbench(false);
      setActiveTabId((cur) => (next.some((t) => t.id === cur) ? cur : next[next.length - 1]?.id ?? null));
      return next;
    });
  }, []);

  const handleRenamedWorkspacePath = useCallback((fromRel: string, toRel: string) => {
    setWbTabs((prev) =>
      prev.map((t) => {
        if (t.kind !== "file") return t;
        if (t.path === fromRel) {
          const name = toRel.split("/").pop() || toRel;
          const nextId = `file:${toRel}`;
          setActiveTabId((cur) => (cur === t.id ? nextId : cur));
          return { ...t, id: nextId, path: toRel, name };
        }
        if (t.path.startsWith(`${fromRel}/`)) {
          const updated = `${toRel}${t.path.slice(fromRel.length)}`;
          const nextId = `file:${updated}`;
          setActiveTabId((cur) => (cur === t.id ? nextId : cur));
          return { ...t, id: nextId, path: updated };
        }
        return t;
      }),
    );
  }, []);

  const showWorkbench = inChat && isProjectSession && inWorkbench && wbTabs.length > 0 && !replay;
  const activeWbFile = useMemo(() => {
    if (!showWorkbench) return null;
    const cur = wbTabs.find((t) => t.id === activeTabId) ?? wbTabs[0];
    return cur?.kind === "file" ? cur.path : null;
  }, [showWorkbench, wbTabs, activeTabId]);

  const canAside = !!active && !active.parent && !replay;
  const showPanel = inChat && !!active && !panel.hidden;
  // With nothing in it, the panel stands as a strip, until a tab there is pressed; the next 分身 starts again.
  const [peek, setPeek] = useState(false);
  useEffect(() => setPeek(false), [state.activeId]);
  const strip = files.length === 0 && runs.length === 0 && plan.length === 0 && made.length === 0 && !peek;

  /** Words or files for the side conversation's box: what is quoted from the code while the code is in the middle. */
  const [asideSeed, setAsideSeed] = useState<{ id: number; text?: string; append?: boolean; attachPaths?: string[] } | null>(null);
  /** What is quoted or attached goes to the box on screen: the side conversation's while the code is in the middle. */
  const quoteIntoChat = useCallback(
    (quoteText: string) => {
      if (showWorkbench && canAside) {
        setAsideSeed({ id: Date.now(), text: quoteText, append: true });
        setPanel((cur) => ({ ...cur, hidden: false }));
        setPeek(true);
        setSummonPanelTab({ id: Date.now(), tab: "chat" });
      } else {
        if (showWorkbench) setInWorkbench(false);
        setSeed({ id: Date.now(), text: quoteText, append: true });
      }
    },
    [showWorkbench, canAside, setPanel],
  );

  const attachIntoChat = useCallback(
    (absPath: string) => {
      if (showWorkbench && canAside) {
        setAsideSeed({ id: Date.now(), attachPaths: [absPath] });
        setPanel((cur) => ({ ...cur, hidden: false }));
        setPeek(true);
        setSummonPanelTab({ id: Date.now(), tab: "chat" });
      } else {
        if (showWorkbench) setInWorkbench(false);
        setSeed({ id: Date.now(), attachPaths: [absPath] });
      }
    },
    [showWorkbench, canAside, setPanel],
  );

  const summonAside = useCallback(() => {
    dispatch({ type: "page", page: "chat" });
    setPanel((cur) => ({ ...cur, hidden: false }));
    setPeek(true);
    setSummonPanelTab({ id: Date.now(), tab: "chat" });
  }, [setPanel]);

  const summonTerminal = useCallback(() => {
    dispatch({ type: "page", page: "chat" });
    setPanel((cur) => ({ ...cur, hidden: false }));
    setPeek(true);
    setSummonPanelTab({ id: Date.now(), tab: "terminal" });
  }, [setPanel]);

  const shortcutContext = useRef({ inChat, isProjectSession, hasActive: !!active });
  shortcutContext.current = { inChat, isProjectSession, hasActive: !!active };

  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      const mod = event.ctrlKey || event.metaKey;
      const lower = event.key.toLowerCase();
      if (mod && !event.altKey && lower === "k") {
        event.preventDefault();
        if (document.querySelector("dialog[open]:not(.jump)")) return;
        setJumping((cur) => (cur === "all" ? null : "all"));
        return;
      }
      if (mod && !event.altKey && lower === "p" && shortcutContext.current.isProjectSession) {
        event.preventDefault();
        if (document.querySelector("dialog[open]:not(.jump)")) return;
        setJumping((cur) => (cur === "files" ? null : "files"));
        return;
      }
      if (mod && !event.altKey && !event.shiftKey && lower === "b") {
        event.preventDefault();
        setSidebar((cur) => ({ ...cur, hidden: !cur.hidden }));
        return;
      }
      if (
        shortcutContext.current.hasActive &&
        ((mod && !event.altKey && lower === "j") || (event.ctrlKey && !event.altKey && event.key === "`"))
      ) {
        event.preventDefault();
        summonTerminal();
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [setSidebar, summonTerminal]);
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
      {
        name: "aside",
        label: "旁聊",
        aliases: ["旁聊", "侧聊"],
        hint: "在右侧栏打开只读旁聊：不打断主分身，随时问进展、查代码",
        unavailable: session.parent ? "子分身的会话没有独立旁聊" : undefined,
        run: summonAside,
      },
      { name: "fork", label: "分叉", aliases: ["分叉"], hint: "分叉：另起一个分身，带着到现在的全部对话", unavailable: session.parent ? "子分身的会话不能分叉" : undefined, run: () => forkFrom() },
      {
        name: "files",
        label: "文件",
        aliases: ["文件", "目录"],
        hint: "在左侧栏打开洞府文件树管理",
        run: () => {
          setSidebar((cur) => ({ ...cur, hidden: false }));
          setLeftTab("files");
        },
      },
      {
        name: "git",
        label: "Git",
        aliases: ["版本", "仓库"],
        hint: "在左侧栏打开 Git 变更、提交与分支管理",
        run: () => {
          setSidebar((cur) => ({ ...cur, hidden: false }));
          setLeftTab("git");
        },
      },
      {
        name: "terminal",
        label: "终端",
        aliases: ["终端", "命令行"],
        hint: "打开右侧交互式运行终端 (Ctrl/Cmd+J)",
        run: summonTerminal,
      },
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
    // Project-only commands (`init`, `review`, `files`, `git`) are hidden in plain conversations.
    const fitting = chatting(session)
      ? builtIn.filter((command) => !["init", "review", "files", "git"].includes(command.name))
      : builtIn;
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
  const conversationPane = active ? (
    <>
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
            versionOf={versionOf}
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
            key={active.id}
            sessionKey={active.id}
            drafts={draftsRef.current}
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
  ) : null;

  return (
    <div
      className={["app", showPanel && "with-panel", sidebar.hidden && "sidebar-hidden"].filter(Boolean).join(" ")}
      style={
        {
          "--sidebar-width": `${sidebar.width}px`,
          "--panel-width": `${strip ? PANEL_STRIP : panel.width}px`,
        } as React.CSSProperties
      }
    >
      {/* Each part of the window is guarded by itself: one that cannot be drawn leaves the others in use. */}
      {!sidebar.hidden && (
        <Guard name="分身列表" className="sidebar">
          <Sidebar
            sessions={state.sessions}
            activeId={state.activeId}
            page={state.page}
            tab={leftTab}
            onTab={setLeftTab}
            width={sidebar.width}
            onResize={(width) => setSidebar({ ...sidebar, width })}
            gitCount={gitChangeCount}
            filesPane={
              isProjectSession && active ? (
                <FileTree
                  sessionId={active.id}
                  workspace={active.workspace}
                  activePath={activeWbFile}
                  refreshTick={fsRefreshTick}
                  gitBadges={gitBadges}
                  onOpenFile={(relPath, pin) => openWorkbenchFile(relPath, pin)}
                  onAttachFile={(absPath) => attachIntoChat(absPath)}
                  onDeletedPath={(rel) => {
                    handleDeletedWorkspacePath(rel);
                    bumpRefresh();
                  }}
                  onRenamedPath={(fromRel, toRel) => {
                    handleRenamedWorkspacePath(fromRel, toRel);
                    bumpRefresh();
                  }}
                  onError={fail}
                />
              ) : undefined
            }
            gitPane={
              isProjectSession && active ? (
                <GitPanel
                  sessionId={active.id}
                  isWorktree={!!active.worktree}
                  status={gitState}
                  refreshTick={fsRefreshTick}
                  working={state.status !== "idle"}
                  onStatusChanged={(next) => {
                    // Staging hands back the new status itself; anything else (commit, branch, sync) asks for it again.
                    if (next) setGitState(next);
                    else bumpRefresh();
                  }}
                  onFilesChanged={bumpRefresh}
                  onBranchSwitched={(item) => dispatch({ type: "session", item })}
                  onOpenDiff={(relPath, stage, pin, commit) => openWorkbenchDiff(relPath, stage, pin, commit)}
                  onOpenFile={(relPath, pin) => openWorkbenchFile(relPath, pin)}
                  onAskReview={() => {
                    send(REVIEW_TASK);
                    if (showWorkbench) setInWorkbench(false);
                  }}
                  onNote={(note) => dispatch({ type: "note", note })}
                  onError={fail}
                />
              ) : undefined
            }
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
      )}
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
              sidebarHidden={sidebar.hidden}
              onToggleSidebar={() => setSidebar((cur) => ({ ...cur, hidden: false }))}
              panelHidden={panel.hidden}
              onTogglePanel={() => setPanel({ ...panel, hidden: !panel.hidden })}
              onRename={(title) => api.renameSession(active.id, title).then((item) => dispatch({ type: "session", item }), fail)}
              onDelete={() => api.deleteSession(active.id).then(() => dispatch({ type: "deleted", id: active.id }), fail)}
            />
            </Guard>
            {banners}
            {isProjectSession && wbTabs.length > 0 && !replay && (
              <WorkbenchTabsBar
                tabs={wbTabs}
                inWorkbench={showWorkbench}
                activeTabId={activeTabId}
                chatStatus={state.status}
                onSelectChat={() => setInWorkbench(false)}
                onSelectTab={(id) => {
                  setActiveTabId(id);
                  setInWorkbench(true);
                }}
                onMoveTab={moveWorkbenchTab}
                onCloseTab={closeWorkbenchTab}
                onCloseAllTabs={() => {
                  setWbTabs([]);
                  setActiveTabId(null);
                  setInWorkbench(false);
                }}
              />
            )}
            {showWorkbench ? (
              <Guard name="工作台" className="fill" resetKey={active.id}>
                <Workbench
                  sessionId={active.id}
                  tabs={wbTabs}
                  activeTabId={activeTabId}
                  refreshTick={fsRefreshTick}
                  onQuoteLines={quoteIntoChat}
                  onAttachPath={(relPath) => {
                    const sep = active.workspace.includes("\\") ? "\\" : "/";
                    const abs = `${active.workspace.replace(/[\\/]+$/, "")}${sep}${relPath.replace(/\//g, sep)}`;
                    attachIntoChat(abs);
                  }}
                  onError={fail}
                  renderDiff={(tab) => (
                    <DiffViewer
                      key={tab.id}
                      sessionId={active.id}
                      path={tab.path}
                      stage={tab.stage}
                      commit={tab.commit}
                      refreshTick={fsRefreshTick}
                      onOpenSource={(relPath, line) => openWorkbenchFile(relPath, false, line)}
                      onQuoteDiff={quoteIntoChat}
                    />
                  )}
                />
              </Guard>
            ) : (
              conversationPane
            )}
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
          mode={jumping}
          sessions={state.sessions}
          acts={[
            { id: "home", name: "首页", hint: "回到山前：候旨的、运功的、将至的功课", seal: "山", run: () => dispatch({ type: "home" }) },
            { id: "new", name: "开辟洞府", hint: "选一个文件夹，化出分身", seal: "辟", run: () => void create() },
            { id: "chat", name: "闲谈", hint: "不选文件夹，直接和分身说话", seal: "谈", run: chat },
            ...(isProjectSession
              ? [
                  {
                    id: "files",
                    name: "文件树",
                    hint: "在左栏查看与管理洞府里的文件",
                    seal: "卷",
                    run: () => {
                      dispatch({ type: "page", page: "chat" });
                      setSidebar((cur) => ({ ...cur, hidden: false }));
                      setLeftTab("files");
                    },
                  },
                  {
                    id: "git",
                    name: "Git 管理",
                    hint: "在左栏暂存、提交、切换分支、拉取推送",
                    seal: "版",
                    run: () => {
                      dispatch({ type: "page", page: "chat" });
                      setSidebar((cur) => ({ ...cur, hidden: false }));
                      setLeftTab("git");
                    },
                  },
                ]
              : []),
            ...(active && !active.parent
              ? [
                  {
                    id: "aside",
                    name: "旁聊",
                    hint: "不打断主分身，在右侧栏另开只读对话问进展、查代码",
                    seal: "旁",
                    run: summonAside,
                  },
                ]
              : []),
            ...(active
              ? [
                  {
                    id: "terminal",
                    name: "运行终端",
                    hint: "打开右侧交互式本地终端 (Ctrl/Cmd+J)",
                    seal: "端",
                    run: summonTerminal,
                  },
                ]
              : []),
            { id: "skills", name: "秘籍", hint: "随程序带来的做事说明，打开才收入；还有自己的", seal: "籍", run: () => dispatch({ type: "page", page: "skills" }) },
            { id: "mcp", name: "法器", hint: "给分身添上本事的 MCP 服务：查文档、搜网页、操控浏览器", seal: "器", run: toMcp },
            { id: "schedules", name: "功课", hint: "到了时辰自行化出分身去办的事", seal: "课", run: toSchedules },
            { id: "settings", name: "设置", hint: "心法、禁制、神通、外观", seal: "设", run: toSettings },
          ]}
          onOpen={open}
          onSearch={api.searchSessions}
          onSearchFiles={isProjectSession && activeId ? (q) => api.searchWorkspaceFiles(activeId, q, 60) : undefined}
          onOpenFile={
            isProjectSession
              ? (relPath, line) => {
                  dispatch({ type: "page", page: "chat" });
                  openWorkbenchFile(relPath, false, line);
                }
              : undefined
          }
          onClose={() => setJumping(null)}
        />
      )}
      {showPanel && (
        <Guard name="侧栏" className="panel" resetKey={state.activeId}>
          <SidePanel
            files={files}
            runs={runs}
            todos={plan}
            products={made}
            workspace={active?.workspace}
            width={panel.width}
            sidebarWidth={sidebar.hidden ? 0 : sidebar.width}
            onResize={(width) => setPanel({ ...panel, width })}
            narrow={strip}
            onWiden={() => setPeek(true)}
            working={state.status === "running"}
            frozen={!!replay}
            renderAside={
              canAside && active
                ? (visible) => (
                    <AsidePane
                      key={active.id}
                      main={active}
                      visible={visible}
                      sessions={state.sessions}
                      onForward={(text) => {
                        send(forwarded(text));
                        if (state.status !== "idle") {
                          dispatch({ type: "note", note: "已作为传音转给主分身，它做完眼下这一步就会收到。" });
                        } else if (showWorkbench) {
                          dispatch({ type: "note", note: "已转给主分身。" });
                        }
                      }}
                      onError={fail}
                      renderComposer={({ aside, status: asideStatus, usage: asideUsage, onItem, onSend: sendAside, onStop: stopAside, onCompact: compactAside }) => (
                        <Composer
                          key={aside.id}
                          sessionKey={aside.id}
                          drafts={draftsRef.current}
                          status={asideStatus}
                          providers={providers}
                          listed={(provider) => !!state.settings && !!providers.find((known) => known.id === provider && isSetUp(state.settings!, known))}
                          keyed={(provider) => (state.settings?.keys[provider]?.source ?? "none") !== "none" || !!providers.find((known) => known.id === provider)?.kind.startsWith("custom")}
                          providerId={aside.provider}
                          model={aside.model}
                          effort={aside.effort ?? state.settings?.settings.effort ?? "high"}
                          effortSet={aside.effort !== null}
                          efforts={state.settings?.efforts ?? ["low", "medium", "high", "xhigh", "max"]}
                          onEffort={(effort) => api.setEffort(aside.id, effort).then(onItem, fail)}
                          listModels={api.listModels}
                          onModel={(provider, model) => api.setModel(aside.id, provider, model).then(onItem, fail)}
                          vision={aside.vision}
                          mode={aside.mode}
                          usage={asideUsage}
                          contextLimit={aside.context_limit}
                          contextCeiling={aside.context_ceiling}
                          contextSetting={state.settings?.settings.context_limit ?? aside.context_limit}
                          contextAuto={!!state.settings?.settings.context_auto}
                          onLimit={(limit) => {
                            if (state.settings)
                              saved(api.saveSettings(limit === "auto" ? { ...state.settings.settings, context_auto: true } : { ...state.settings.settings, context_limit: limit, context_auto: false })).then(() => {
                                refresh();
                                api.asideSession(active.id).then((view) => {
                                  if (!view) return;
                                  const { events: _, ...item } = view;
                                  onItem(item);
                                }, () => undefined);
                              });
                          }}
                          planReady={false}
                          onMode={(mode) => api.setMode(aside.id, mode).then(onItem, fail)}
                          onCarryOut={() => undefined}
                          onSend={sendAside}
                          onStop={stopAside}
                          onCompact={compactAside}
                          trustable={false}
                          seed={asideSeed}
                          commands={[]}
                          acceptDrops={visible && showWorkbench}
                          placeholder={
                            state.status !== "idle"
                              ? "主分身运功中，在此旁聊发问（只读，不打断它）…"
                              : "在此旁聊发问（只读洞府，自动知晓主分身进展）…"
                          }
                        />
                      )}
                    />
                  )
                : undefined
            }
            terminalPane={
              active && !replay ? (
                <Suspense fallback={<div className="viewer-placeholder muted">正在加载终端…</div>}>
                  <TerminalPane
                    sessionId={active.id}
                    runs={runs}
                    onSendToChat={quoteIntoChat}
                    onError={fail}
                  />
                </Suspense>
              ) : undefined
            }
            summonTab={summonPanelTab}
            onInspectFile={isProjectSession ? (relPath) => openWorkbenchFile(relPath, false) : undefined}
            onOpenFile={openFile}
            onRestoreArtifact={restoreArtifact}
            onSaveArtifactCopy={saveArtifactCopy}
          />
        </Guard>
      )}
    </div>
  );
}
