import type { Envelope, Schedule, SessionItem, SessionView, SettingsView, Status, UiEvent } from "./types";

export interface BufferedEnvelope {
  sessionId: string;
  envelope: Envelope;
}

export interface State {
  sessions: SessionItem[];
  activeId: string | null;
  /** Monotonically increasing navigation generation to discard stale async session responses. */
  navSeq: number;
  /** The session being opened: its log events are kept until its snapshot arrives. */
  openingId: string | null;
  buffer: BufferedEnvelope[];
  /** The last status the session being opened reported while it was being opened: newer than the view that comes back. */
  bufferedStatus: { sessionId: string; status: Status } | null;
  events: Envelope[];
  status: Status;
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

export type Action =
  | { type: "sessions"; sessions: SessionItem[] }
  | { type: "opening"; id: string | null; navSeq: number }
  | { type: "opened"; view: SessionView; navSeq: number }
  | { type: "session"; item: SessionItem }
  | { type: "deleted"; id: string; navSeq?: number }
  | { type: "home"; navSeq: number }
  | { type: "event"; event: UiEvent }
  | { type: "queued"; id: string; text: string }
  | { type: "page"; page: State["page"] }
  | { type: "settings"; settings: SettingsView }
  | { type: "schedules"; schedules: Schedule[] }
  | { type: "error"; error: string | null }
  | { type: "note"; note: string | null };

export const initial: State = {
  sessions: [],
  activeId: null,
  navSeq: 0,
  openingId: null,
  bufferedStatus: null,
  buffer: [],
  events: [],
  status: "idle",
  queued: [],
  page: "chat",
  settings: null,
  schedules: [],
  error: null,
  note: null,
};

/** Collect `id` and all transitive child sessions whose `parent` chain leads back to `id`. */
export function removedSessionIds(sessions: SessionItem[], id: string): Set<string> {
  const removed = new Set<string>([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const session of sessions) {
      if (!removed.has(session.id) && session.parent && removed.has(session.parent)) {
        removed.add(session.id);
        grew = true;
      }
    }
  }
  return removed;
}

/** Add a log event unless the snapshot already holds it. */
export function appendLog(state: State, envelope: Envelope): State {
  const last = state.events[state.events.length - 1]?.seq ?? 0;
  if (envelope.seq <= last) return state;
  let { queued } = state;
  // A queued message joins as steering, or opens the next turn when it arrived as the last one ended.
  if (envelope.type === "user/message") {
    const at = queued.indexOf(envelope.data.text);
    if (at >= 0) queued = queued.filter((_, index) => index !== at);
  }
  return { ...state, events: [...state.events, envelope], queued };
}

export function reduce(state: State, action: Action): State {
  switch (action.type) {
    case "sessions":
      return { ...state, sessions: action.sessions };
    case "opening": {
      if (action.navSeq < state.navSeq) return state;
      return {
        ...state,
        navSeq: action.navSeq,
        openingId: action.id,
        buffer: action.id ? state.buffer.filter((b) => b.sessionId === action.id) : [],
        bufferedStatus: action.id && state.bufferedStatus?.sessionId === action.id ? state.bufferedStatus : null,
      };
    }
    case "opened": {
      const { events, abandoned_brief: _, ...view } = action.view;
      // A status reported while the view was on its way is at least as new as the view: a turn that ended
      // just after the view was taken would otherwise be shown as running for good.
      const item: SessionItem =
        state.bufferedStatus?.sessionId === view.id ? { ...view, status: state.bufferedStatus.status } : view;
      const sessions = state.sessions.some((session) => session.id === item.id)
        ? state.sessions.map((session) => (session.id === item.id ? item : session))
        : [item, ...state.sessions];
      if (action.navSeq < state.navSeq) {
        const staleSessions = state.sessions.some((session) => session.id === view.id)
          ? state.sessions.map((session) => (session.id === view.id ? { ...view, status: session.status } : session))
          : [view, ...state.sessions];
        return { ...state, sessions: staleSessions };
      }
      const bufferedForView = state.buffer.filter((b) => b.sessionId === item.id);
      const opened: State = {
        ...state,
        navSeq: action.navSeq,
        activeId: item.id,
        openingId: null,
        buffer: [],
        bufferedStatus: null,
        events,
        status: item.status,
        queued: [],
        page: "chat",
        sessions,
      };
      return bufferedForView.reduce((acc, b) => appendLog(acc, b.envelope), opened);
    }
    case "home":
      if (action.navSeq < state.navSeq) return state;
      return {
        ...state,
        navSeq: action.navSeq,
        activeId: null,
        openingId: null,
        buffer: [],
        bufferedStatus: null,
        events: [],
        queued: [],
        status: "idle",
        page: "chat",
      };
    case "deleted": {
      const removed = removedSessionIds(state.sessions, action.id);
      const sessions = state.sessions.filter((session) => !removed.has(session.id));
      const activeRemoved = !!state.activeId && removed.has(state.activeId);
      const openingRemoved = !!state.openingId && removed.has(state.openingId);
      const buffer = state.buffer.filter((b) => !removed.has(b.sessionId));
      const bufferedStatus =
        state.bufferedStatus && !removed.has(state.bufferedStatus.sessionId) ? state.bufferedStatus : null;
      const navSeq = action.navSeq !== undefined && action.navSeq > state.navSeq ? action.navSeq : state.navSeq;
      return activeRemoved
        ? {
            ...state,
            navSeq,
            sessions,
            activeId: null,
            openingId: openingRemoved ? null : state.openingId,
            buffer,
            bufferedStatus,
            events: [],
            queued: [],
            status: "idle",
          }
        : {
            ...state,
            navSeq,
            sessions,
            openingId: openingRemoved ? null : state.openingId,
            buffer,
            bufferedStatus,
          };
    }
    case "session": {
      if (action.item.aside !== null) return state;
      const exists = state.sessions.some((session) => session.id === action.item.id);
      const sessions = exists
        ? state.sessions.map((session) =>
            session.id === action.item.id ? { ...action.item, status: session.status } : session,
          )
        : [action.item, ...state.sessions];
      return { ...state, sessions };
    }
    case "event": {
      const { event } = action;
      if (event.kind === "stream") return state;
      const active = event.session_id === state.activeId;
      if (event.kind === "log") {
        const nextBuffer =
          event.session_id === state.openingId
            ? [...state.buffer, { sessionId: event.session_id, envelope: event.envelope }]
            : state.buffer;
        const nextState = nextBuffer === state.buffer ? state : { ...state, buffer: nextBuffer };
        return active ? appendLog(nextState, event.envelope) : nextState;
      }
      const sessions = state.sessions.map((session) =>
        session.id === event.session_id ? { ...session, status: event.status } : session,
      );
      const bufferedStatus =
        event.session_id === state.openingId
          ? { sessionId: event.session_id, status: event.status }
          : state.bufferedStatus;
      if (!active) return { ...state, sessions, bufferedStatus };
      const idle = event.status === "idle";
      return {
        ...state,
        sessions,
        bufferedStatus,
        status: event.status,
        queued: idle ? [] : state.queued,
      };
    }
    case "queued":
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
