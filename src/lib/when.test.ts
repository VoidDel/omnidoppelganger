import { describe, expect, it } from "vitest";
import { ago, ahead } from "./when";

describe("ago (Intl.RelativeTimeFormat & Intl.DateTimeFormat)", () => {
  const now = new Date(2026, 10, 19, 15, 30, 0);
  const before = (ms: number) => new Date(now.getTime() - ms).toISOString();
  const MINUTE = 60_000;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;

  it("formats relative minutes and hours within the day in zh-CN and en", () => {
    expect(ago(before(20_000), now, "zh-CN")).toBe("刚刚");
    expect(ago(before(5 * MINUTE + 10_000), now, "zh-CN")).toBe("5分钟前");
    expect(ago(before(3 * HOUR + 59 * MINUTE), now, "zh-CN")).toBe("3小时前");

    expect(ago(before(20_000), now, "en")).toBe("just now");
    expect(ago(before(5 * MINUTE + 10_000), now, "en")).toBe("5 minutes ago");
    expect(ago(before(3 * HOUR + 59 * MINUTE), now, "en")).toBe("3 hours ago");
  });

  it("goes by calendar days after that", () => {
    const early = new Date(2026, 10, 19, 0, 10, 0);
    expect(ago(new Date(2026, 10, 18, 23, 50, 0).toISOString(), early, "zh-CN")).toBe("20分钟前");
    expect(ago(new Date(2026, 10, 18, 22, 0, 0).toISOString(), early, "zh-CN")).toBe("昨天");
    expect(ago(before(16 * HOUR), now, "zh-CN")).toBe("昨天");
    expect(ago(before(3 * DAY), now, "zh-CN")).toBe("3天前");

    expect(ago(before(16 * HOUR), now, "en")).toBe("yesterday");
    expect(ago(before(3 * DAY), now, "en")).toBe("3 days ago");
  });

  it("gives the date once it is a week or more, and the year once it is another year", () => {
    expect(ago(new Date(2026, 10, 2, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("11月2日");
    expect(ago(new Date(2025, 2, 8, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("2025年3月");
    expect(ago(new Date(2026, 10, 2, 9, 0, 0).toISOString(), now, "en")).toBe("Nov 2");
    expect(ago(new Date(2025, 2, 8, 9, 0, 0).toISOString(), now, "en")).toBe("Mar 2025");
  });

  it("says nothing of what is not a time", () => {
    expect(ago("", now)).toBe("");
  });
});

describe("ahead (Intl.RelativeTimeFormat & Intl.DateTimeFormat)", () => {
  const now = new Date(2026, 10, 19, 15, 30, 0);
  const after = (ms: number) => new Date(now.getTime() + ms).toISOString();
  const MINUTE = 60_000;
  const HOUR = 60 * MINUTE;

  it("says how long until, in minutes, then hours, within the day", () => {
    expect(ahead(after(-5 * MINUTE), now, "zh-CN")).toBe("马上");
    expect(ahead(after(30_000), now, "zh-CN")).toBe("马上");
    expect(ahead(after(4 * MINUTE + 10_000), now, "zh-CN")).toBe("5分钟后");
    expect(ahead(after(3 * HOUR + 10 * MINUTE), now, "zh-CN")).toBe("3小时后");
    expect(ahead(after(59 * MINUTE + 30_000), now, "zh-CN")).toBe("1小时后");

    expect(ahead(after(30_000), now, "en")).toBe("soon");
    expect(ahead(after(4 * MINUTE + 10_000), now, "en")).toBe("in 5 minutes");
  });

  it("goes by the calendar after that", () => {
    expect(ahead(new Date(2026, 10, 20, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("明天");
    expect(ahead(new Date(2026, 10, 21, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("后天");
    expect(ahead(new Date(2026, 10, 24, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("5天后");
    expect(ahead(new Date(2026, 11, 3, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("12月3日");
    expect(ahead(new Date(2027, 0, 3, 9, 0, 0).toISOString(), now, "zh-CN")).toBe("2027年1月");

    expect(ahead(new Date(2026, 10, 20, 9, 0, 0).toISOString(), now, "en")).toBe("tomorrow");
    expect(ahead(new Date(2026, 10, 24, 9, 0, 0).toISOString(), now, "en")).toBe("in 5 days");
  });
});
