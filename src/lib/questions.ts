// What 本尊 writes into the form an `ask_user` call puts up, and what the 分身 is told of it.

import type { Question, Reply } from "./types";

/** What 本尊 has put to one question so far: the options ticked, the words written, and for a single choice whether
 *  the words are the choice ("其他"). */
export interface Draft {
  chosen: string[];
  text: string;
  other: boolean;
}

export const blank = (): Draft => ({ chosen: [], text: "", other: false });

/** A draft after an option is pressed: for one choice it is the choice, for several it is ticked or unticked. */
export function choose(question: Question, draft: Draft, label: string): Draft {
  if (question.kind === "single") return { ...draft, chosen: [label], other: false };
  const chosen = draft.chosen.includes(label) ? draft.chosen.filter((known) => known !== label) : [...draft.chosen, label];
  // Kept in the order the options are listed, whatever order they were ticked in.
  const order = (question.options ?? []).map((option) => option.label);
  return { ...draft, chosen: chosen.sort((a, b) => order.indexOf(a) - order.indexOf(b)) };
}

/** A draft as the 分身 is told it. For a single choice the words count only when "其他" is the choice. */
export function replyOf(question: Question, draft: Draft): Reply {
  const text = draft.text.trim();
  if (question.kind === "text") return { chosen: [], text };
  if (question.kind === "single") return draft.other ? { chosen: [], text } : { chosen: draft.chosen.slice(0, 1), text: "" };
  return { chosen: draft.chosen, text };
}

export const isAnswered = (reply: Reply) => !!reply.chosen?.length || !!reply.text?.trim();

/** One answer in a line, as it is shown once given: the options chosen, then any words. */
export function sayReply(reply: Reply | undefined): string {
  const parts = [...(reply?.chosen ?? [])];
  const text = reply?.text?.trim();
  if (text) parts.push(parts.length ? `另：${text}` : text);
  return parts.join("、");
}
