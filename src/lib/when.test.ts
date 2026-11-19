import { describe, expect, it } from "vitest";
import { ago, ahead } from "./when";

describe("ago", () => {
  const now = new Date(2026, 10, 19, 15, 30, 0);
  const before = (ms: number) => new Date(now.getTime() - ms).toISOString();
  const MINUTE = 60_000;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;

  it("says how long ago in minutes, then hours, within the day", () => {
    expect(ago(before(20_000), now)).toBe("刚刚");
    expect(ago(before(5 * MINUTE + 10_000), now)).toBe("5 分钟前");
    expect(ago(before(3 * HOUR + 59 * MINUTE), now)).toBe("3 小时前");
  });

  it("goes by the calendar after that: 昨天 is the day before, however few hours ago", () => {
    // 23:50 the evening before, seen at 00:10: twenty minutes, not a day.
    const early = new Date(2026, 10, 19, 0, 10, 0);
    expect(ago(new Date(2026, 10, 18, 23, 50, 0).toISOString(), early)).toBe("20 分钟前");
    expect(ago(new Date(2026, 10, 18, 22, 0, 0).toISOString(), early)).toBe("昨天");
    expect(ago(before(16 * HOUR), now)).toBe("昨天");
    expect(ago(before(3 * DAY), now)).toBe("3 天前");
  });

  it("gives the date once it is a week or more, and the year once it is another year", () => {
    expect(ago(new Date(2026, 10, 2, 9, 0, 0).toISOString(), now)).toBe("11月2日");
    expect(ago(new Date(2025, 2, 8, 9, 0, 0).toISOString(), now)).toBe("2025年3月");
  });

  it("says nothing of what is not a time", () => {
    expect(ago("", now)).toBe("");
  });
});

describe("ahead", () => {
  const now = new Date(2026, 10, 19, 15, 30, 0);
  const after = (ms: number) => new Date(now.getTime() + ms).toISOString();
  const MINUTE = 60_000;
  const HOUR = 60 * MINUTE;

  it("says how long until, in minutes, then hours, within the day", () => {
    expect(ahead(after(-5 * MINUTE), now)).toBe("马上");
    expect(ahead(after(30_000), now)).toBe("马上");
    expect(ahead(after(4 * MINUTE + 10_000), now)).toBe("5 分钟后");
    expect(ahead(after(3 * HOUR + 10 * MINUTE), now)).toBe("3 小时后");
    // The last minute before the hour is the hour, not sixty minutes.
    expect(ahead(after(59 * MINUTE + 30_000), now)).toBe("1 小时后");
  });

  it("goes by the calendar after that", () => {
    expect(ahead(new Date(2026, 10, 20, 9, 0, 0).toISOString(), now)).toBe("明天");
    expect(ahead(new Date(2026, 10, 21, 9, 0, 0).toISOString(), now)).toBe("后天");
    expect(ahead(new Date(2026, 10, 24, 9, 0, 0).toISOString(), now)).toBe("5 天后");
    expect(ahead(new Date(2026, 11, 3, 9, 0, 0).toISOString(), now)).toBe("12月3日");
    expect(ahead(new Date(2027, 0, 3, 9, 0, 0).toISOString(), now)).toBe("2027年1月");
  });
});
