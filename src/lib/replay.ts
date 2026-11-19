import type { Envelope } from "./types";

/**
 * Where a replay stops: the index of each event that puts something new on
 * the page. Showing a session "as of stop n" is showing its log up to and
 * including that event.
 */
export function replayStops(events: Envelope[]): number[] {
  const stops: number[] = [];
  events.forEach((event, index) => {
    switch (event.type) {
      case "user/message":
      case "assistant/message":
      case "tool/result":
      case "guardian/reviewed":
      case "approval/requested":
      case "approval/resolved":
      case "context/compacted":
      case "files/restored":
        stops.push(index);
        break;
      case "instructions/loaded":
        // Shown under a message, so the one read before the first message has nowhere to appear.
        if (stops.length > 0) stops.push(index);
        break;
      case "turn/end":
        // A turn that ended well adds nothing to the page; any other end is said.
        if (event.data.reason !== "completed") stops.push(index);
        break;
      default:
        break;
    }
  });
  return stops;
}

/** The turn an event belongs to: that of the last `turn/start` at or before it; 0 before the first. */
export function turnAt(events: Envelope[], index: number): number {
  for (let at = Math.min(index, events.length - 1); at >= 0; at--) {
    const event = events[at];
    if (event.type === "turn/start") return event.data.turn;
  }
  return 0;
}

/** The stop a replay is at after a step of `by` from `at`, kept within the stops there are. */
export const stepped = (at: number, by: number, count: number) => Math.max(0, Math.min(count - 1, at + by));
