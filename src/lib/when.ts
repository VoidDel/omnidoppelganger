import { formatMessage, getCurrentLocale, intlLocaleOf, type Locale } from "../i18n";

/** Midnight at the start of a date's day, in local time. */
const dayOf = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();

const rtfCache = new Map<string, Intl.RelativeTimeFormat>();
const mdFormatCache = new Map<string, Intl.DateTimeFormat>();
const ymFormatCache = new Map<string, Intl.DateTimeFormat>();

function getRtf(locale: Locale): Intl.RelativeTimeFormat {
  const tag = intlLocaleOf(locale);
  let fmt = rtfCache.get(tag);
  if (!fmt) {
    fmt = new Intl.RelativeTimeFormat(tag, { numeric: "auto" });
    rtfCache.set(tag, fmt);
  }
  return fmt;
}

function formatCalendarDate(then: Date, now: Date, locale: Locale): string {
  const tag = intlLocaleOf(locale);
  if (then.getFullYear() === now.getFullYear()) {
    let md = mdFormatCache.get(tag);
    if (!md) {
      md = new Intl.DateTimeFormat(tag, { month: "short", day: "numeric" });
      mdFormatCache.set(tag, md);
    }
    return md.format(then);
  }
  let ym = ymFormatCache.get(tag);
  if (!ym) {
    ym = new Intl.DateTimeFormat(tag, { year: "numeric", month: "short" });
    ymFormatCache.set(tag, ym);
  }
  return ym.format(then);
}

/**
 * How long ago `ts` was, formatted via native `Intl.RelativeTimeFormat` and `Intl.DateTimeFormat`.
 */
export function ago(ts: string, now: Date, locale: Locale = getCurrentLocale()): string {
  const then = new Date(ts);
  const seconds = (now.getTime() - then.getTime()) / 1000;
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return formatMessage(locale, "when.justNow");
  const rtf = getRtf(locale);
  if (seconds < 3600) return rtf.format(-Math.floor(seconds / 60), "minute");
  const days = Math.round((dayOf(now) - dayOf(then)) / 86_400_000);
  if (days === 0) return rtf.format(-Math.floor(seconds / 3600), "hour");
  if (days < 7) return rtf.format(-days, "day");
  return formatCalendarDate(then, now, locale);
}

/**
 * How long until `ts`, formatted via native `Intl.RelativeTimeFormat` and `Intl.DateTimeFormat`.
 */
export function ahead(ts: string, now: Date, locale: Locale = getCurrentLocale()): string {
  const then = new Date(ts);
  const seconds = (then.getTime() - now.getTime()) / 1000;
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return formatMessage(locale, "when.soon");
  const rtf = getRtf(locale);
  // Rounded up, so that what is said is never sooner than it is; the last minute of an hour is then the hour.
  const minutes = Math.ceil(seconds / 60);
  if (minutes < 60) return rtf.format(minutes, "minute");
  const days = Math.round((dayOf(then) - dayOf(now)) / 86_400_000);
  if (days === 0) return rtf.format(Math.max(1, Math.floor(seconds / 3600)), "hour");
  if (days < 7) return rtf.format(days, "day");
  return formatCalendarDate(then, now, locale);
}
