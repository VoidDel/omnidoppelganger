import { formatMessage, getCurrentLocale, intlLocaleOf, type Locale } from "../i18n";
import type { Mode, Schedule, When } from "./types";

/** The days of the week as a schedule numbers them: 1 is Monday, 7 is Sunday. */
export const WEEKDAYS: { value: number; name: string; enName: string }[] = [
  { value: 1, name: "一", enName: "Mon" },
  { value: 2, name: "二", enName: "Tue" },
  { value: 3, name: "三", enName: "Wed" },
  { value: 4, name: "四", enName: "Thu" },
  { value: 5, name: "五", enName: "Fri" },
  { value: 6, name: "六", enName: "Sat" },
  { value: 7, name: "日", enName: "Sun" },
];

export function weekdaysFor(locale: Locale = getCurrentLocale()): { value: number; name: string }[] {
  // Reference Monday 2026-11-16 .. Sunday 2026-11-22 formatted via Intl.DateTimeFormat
  if (locale === "en") {
    const fmt = new Intl.DateTimeFormat(intlLocaleOf(locale), { weekday: "short" });
    return WEEKDAYS.map((d) => ({
      value: d.value,
      name: fmt.format(new Date(2026, 10, 15 + d.value, 12, 0)),
    }));
  }
  return WEEKDAYS.map((d) => ({ value: d.value, name: d.name }));
}

const two = (value: number) => String(value).padStart(2, "0");

/** A moment as a `datetime-local` field holds it: the wall clock, to the minute. */
export function localStamp(date: Date): string {
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())}T${two(date.getHours())}:${two(date.getMinutes())}`;
}

/** When a schedule runs, in a line, formatted via `Intl.DateTimeFormat` and `i18n`. */
export function describeWhen(when: When, locale: Locale = getCurrentLocale()): string {
  const tag = intlLocaleOf(locale);
  switch (when.kind) {
    case "once": {
      const [datePart, time = ""] = when.at.split("T");
      const [year, month, day] = datePart.split("-").map(Number);
      const parsed = new Date(year || 2026, (month || 1) - 1, day || 1, 12, 0);
      const formattedDate = Number.isNaN(parsed.getTime())
        ? datePart
        : new Intl.DateTimeFormat(tag, { month: "short", day: "numeric" }).format(parsed);
      return formatMessage(locale, "schedule.onceAt", { date: formattedDate, time: time.slice(0, 5) });
    }
    case "daily": {
      const days = weekdaysFor(locale).filter((day) => when.days.includes(day.value));
      if (days.length === 0 || days.length === WEEKDAYS.length) {
        return formatMessage(locale, "schedule.everyDay", { time: when.time });
      }
      const sep = locale === "en" ? ", " : "、";
      return formatMessage(locale, "schedule.everyWeek", {
        days: days.map((day) => day.name).join(sep),
        time: when.time,
      });
    }
    case "every":
      return when.minutes % 60 === 0
        ? formatMessage(locale, "schedule.everyHours", { hours: when.minutes / 60 })
        : formatMessage(locale, "schedule.everyMinutes", { minutes: when.minutes });
  }
}

/** A moment the store wrote, formatted via `Intl.DateTimeFormat`. */
export function clock(stamp: string, locale: Locale = getCurrentLocale()): string {
  const date = new Date(stamp);
  if (Number.isNaN(date.getTime())) return stamp;
  const tag = intlLocaleOf(locale);
  if (locale === "en") {
    return new Intl.DateTimeFormat(tag, {
      weekday: "short",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).format(date);
  }
  const md = new Intl.DateTimeFormat(tag, { month: "short", day: "numeric" }).format(date);
  const wk = new Intl.DateTimeFormat(tag, { weekday: "short" }).format(date);
  const hm = new Intl.DateTimeFormat(tag, { hour: "2-digit", minute: "2-digit", hour12: false }).format(date);
  return `${md} ${wk} ${hm}`;
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
export function scheduleOf(draft: Draft, locale: Locale = getCurrentLocale()): Schedule | string {
  if (!draft.workspace.trim()) return formatMessage(locale, "schedule.errNoWorkspace");
  if (!draft.prompt.trim()) return formatMessage(locale, "schedule.errNoPrompt");
  let when: When;
  if (draft.kind === "once") {
    if (!draft.at) return formatMessage(locale, "schedule.errNoTime");
    when = { kind: "once", at: draft.at };
  } else if (draft.kind === "daily") {
    if (!draft.time) return formatMessage(locale, "schedule.errNoTime");
    when = { kind: "daily", time: draft.time, days: [...draft.days].sort() };
  } else {
    const gap = Number(draft.gap);
    if (!Number.isInteger(gap) || gap < 1) return formatMessage(locale, "schedule.errBadGap");
    when = { kind: "every", minutes: draft.unit === "hours" ? gap * 60 : gap };
  }
  return { id: draft.id, name: draft.name.trim(), workspace: draft.workspace.trim(), prompt: draft.prompt.trim(), when, mode: draft.mode, isolated: draft.isolated, enabled: draft.enabled };
}
