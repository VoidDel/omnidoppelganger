import { applyDelta, type StreamBlock } from "./fold";
import type { Envelope, Status, UiEvent } from "./types";

/**
 * One session followed live beside the one on the page (the side conversation): its log, what is streaming in,
 * how it stands, and what was said to it that it has not taken up yet. The same rules as the page's own session.
 */
export interface LiveLog {
  events: Envelope[];
  stream: StreamBlock[];
  status: Status;
  queued: string[];
}

export type LiveAction =
  | { type: "opened"; events: Envelope[]; status: Status }
  | { type: "event"; event: UiEvent }
  | { type: "queued"; text: string }
  | { type: "reset" };

export const emptyLive: LiveLog = { events: [], stream: [], status: "idle", queued: [] };

/** Follow a session's events; the caller passes only that session's. */
export function liveReduce(state: LiveLog, action: LiveAction): LiveLog {
  switch (action.type) {
    case "reset":
      return emptyLive;
    case "opened":
      return { events: action.events, stream: [], status: action.status, queued: [] };
    case "queued":
      return { ...state, queued: [...state.queued, action.text] };
    case "event": {
      const { event } = action;
      if (event.kind === "stream") return { ...state, stream: applyDelta(state.stream, event.delta) };
      if (event.kind === "status") {
        const idle = event.status === "idle";
        return { ...state, status: event.status, stream: idle ? [] : state.stream, queued: idle ? [] : state.queued };
      }
      const envelope = event.envelope;
      const last = state.events[state.events.length - 1]?.seq ?? 0;
      if (envelope.seq <= last) return state;
      let { stream, queued } = state;
      if (envelope.type === "assistant/message") stream = [];
      if (envelope.type === "user/message") {
        const at = queued.indexOf(envelope.data.text);
        if (at >= 0) queued = queued.filter((_, index) => index !== at);
      }
      return { ...state, events: [...state.events, envelope], stream, queued };
    }
  }
}

/** What a side conversation's answer is said with when it is handed to the main 分身. */
export function forwarded(text: string): string {
  return `（以下是本尊在旁聊里得到的结论，转给你参考，看完按需调整，不必回复旁聊）\n\n${text.trim()}`;
}
