/** Midnight at the start of a date's day, in local time. */
const dayOf = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();

/**
 * How long ago `ts` was, as it is said in passing: 刚刚, then minutes, then hours within the day,
 * 昨天, days within the week, and after that the date itself. Empty when `ts` is not a time.
 */
export function ago(ts: string, now: Date): string {
  const then = new Date(ts);
  const seconds = (now.getTime() - then.getTime()) / 1000;
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  const days = Math.round((dayOf(now) - dayOf(then)) / 86_400_000);
  if (days === 0) return `${Math.floor(seconds / 3600)} 小时前`;
  if (days === 1) return "昨天";
  if (days < 7) return `${days} 天前`;
  return then.getFullYear() === now.getFullYear() ? `${then.getMonth() + 1}月${then.getDate()}日` : `${then.getFullYear()}年${then.getMonth() + 1}月`;
}

/**
 * How long until `ts`, as it is said in passing: 马上, minutes, hours within the day, 明天, 后天,
 * days within the week, and after that the date. Empty when `ts` is not a time; 马上 once it is past.
 */
export function ahead(ts: string, now: Date): string {
  const then = new Date(ts);
  const seconds = (then.getTime() - now.getTime()) / 1000;
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return "马上";
  if (seconds < 3600) return `${Math.ceil(seconds / 60)} 分钟后`;
  const days = Math.round((dayOf(then) - dayOf(now)) / 86_400_000);
  if (days === 0) return `${Math.floor(seconds / 3600)} 小时后`;
  if (days === 1) return "明天";
  if (days === 2) return "后天";
  if (days < 7) return `${days} 天后`;
  return then.getFullYear() === now.getFullYear() ? `${then.getMonth() + 1}月${then.getDate()}日` : `${then.getFullYear()}年${then.getMonth() + 1}月`;
}
