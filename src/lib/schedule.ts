import type { Mode, Schedule, When } from "./types";

/** The days of the week as a schedule numbers them: 1 is Monday, 7 is Sunday. */
export const WEEKDAYS: { value: number; name: string }[] = [
  { value: 1, name: "一" },
  { value: 2, name: "二" },
  { value: 3, name: "三" },
  { value: 4, name: "四" },
  { value: 5, name: "五" },
  { value: 6, name: "六" },
  { value: 7, name: "日" },
];

const two = (value: number) => String(value).padStart(2, "0");

/** A moment as a `datetime-local` field holds it: the wall clock, to the minute. */
export function localStamp(date: Date): string {
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())}T${two(date.getHours())}:${two(date.getMinutes())}`;
}

/** When a schedule runs, in a line: 每天 09:00, 每周一、三 09:00, 每隔 2 小时, 只一次：11月20日 09:00. */
export function describeWhen(when: When): string {
  switch (when.kind) {
    case "once": {
      const [date, time = ""] = when.at.split("T");
      const [, month, day] = date.split("-");
      return `只一次：${Number(month)}月${Number(day)}日 ${time.slice(0, 5)}`;
    }
    case "daily": {
      const days = WEEKDAYS.filter((day) => when.days.includes(day.value));
      if (days.length === 0 || days.length === WEEKDAYS.length) return `每天 ${when.time}`;
      return `每周${days.map((day) => day.name).join("、")} ${when.time}`;
    }
    case "every":
      return when.minutes % 60 === 0 ? `每隔 ${when.minutes / 60} 小时` : `每隔 ${when.minutes} 分钟`;
  }
}

/** A moment the store wrote, as the user reads a clock: 11月20日 周五 09:00. */
export function clock(stamp: string): string {
  const date = new Date(stamp);
  if (Number.isNaN(date.getTime())) return stamp;
  return `${date.getMonth() + 1}月${date.getDate()}日 周${WEEKDAYS[(date.getDay() + 6) % 7].name} ${two(date.getHours())}:${two(date.getMinutes())}`;
}

/** A schedule as the form holds it: every kind's fields at once, so switching kinds loses nothing typed. */
export interface Draft {
  id: string;
  name: string;
  workspace: string;
  prompt: string;
  kind: When["kind"];
  time: string;
  days: number[];
  at: string;
  /** The gap of an `every` schedule, in `unit`s. */
  gap: string;
  unit: "minutes" | "hours";
  mode: Mode;
  isolated: boolean;
  enabled: boolean;
}

/** The form for a new schedule: every morning at nine, in `workspace`. `now` sets the day a one-time defaults to: tomorrow. */
export function blank(workspace: string, mode: Mode, now: Date): Draft {
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 9, 0);
  return { id: "", name: "", workspace, prompt: "", kind: "daily", time: "09:00", days: [], at: localStamp(tomorrow), gap: "1", unit: "hours", mode, isolated: false, enabled: true };
}

/** The form for a schedule that exists. */
export function draftOf(schedule: Schedule, now: Date): Draft {
  const draft = { ...blank(schedule.workspace, schedule.mode, now), id: schedule.id, name: schedule.name, prompt: schedule.prompt, kind: schedule.when.kind, isolated: schedule.isolated, enabled: schedule.enabled };
  switch (schedule.when.kind) {
    case "once":
      return { ...draft, at: schedule.when.at.slice(0, 16) };
    case "daily":
      return { ...draft, time: schedule.when.time, days: schedule.when.days };
    case "every": {
      const hours = schedule.when.minutes % 60 === 0;
      return { ...draft, gap: String(hours ? schedule.when.minutes / 60 : schedule.when.minutes), unit: hours ? "hours" : "minutes" };
    }
  }
}

/** What the form says, as a schedule to save; or what is missing from it, in words for the user. */
export function scheduleOf(draft: Draft): Schedule | string {
  if (!draft.workspace.trim()) return "还没选洞府：功课得有个地方办。";
  if (!draft.prompt.trim()) return "还没写交办的事。";
  let when: When;
  if (draft.kind === "once") {
    if (!draft.at) return "还没定时辰。";
    when = { kind: "once", at: draft.at };
  } else if (draft.kind === "daily") {
    if (!draft.time) return "还没定时辰。";
    when = { kind: "daily", time: draft.time, days: [...draft.days].sort() };
  } else {
    const gap = Number(draft.gap);
    if (!Number.isInteger(gap) || gap < 1) return "隔多久要填一个不小于 1 的整数。";
    when = { kind: "every", minutes: draft.unit === "hours" ? gap * 60 : gap };
  }
  return { id: draft.id, name: draft.name.trim(), workspace: draft.workspace.trim(), prompt: draft.prompt.trim(), when, mode: draft.mode, isolated: draft.isolated, enabled: draft.enabled };
}
