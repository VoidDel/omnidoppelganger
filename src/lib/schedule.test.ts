import { describe, expect, it } from "vitest";
import { blank, clock, describeWhen, draftOf, localStamp, scheduleOf } from "./schedule";
import type { Schedule } from "./types";

const now = new Date(2026, 10, 19, 22, 30);

describe("schedules", () => {
  it("says when a schedule runs in a line", () => {
    expect(describeWhen({ kind: "daily", time: "09:00", days: [] })).toBe("每天 09:00");
    expect(describeWhen({ kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5, 6, 7] })).toBe("每天 09:00");
    expect(describeWhen({ kind: "daily", time: "18:30", days: [5, 1] })).toBe("每周一、五 18:30");
    expect(describeWhen({ kind: "every", minutes: 120 })).toBe("每隔 2 小时");
    expect(describeWhen({ kind: "every", minutes: 90 })).toBe("每隔 90 分钟");
    expect(describeWhen({ kind: "once", at: "2026-11-20T09:05:30" })).toBe("只一次：11月20日 09:05");
  });

  it("reads a stored moment on the user's own clock", () => {
    // Built from local parts, so the test holds in any time zone.
    expect(clock(new Date(2026, 10, 20, 9, 5).toISOString())).toBe("11月20日 周五 09:05");
    expect(clock(new Date(2026, 10, 22, 23, 0).toISOString())).toBe("11月22日 周日 23:00");
    expect(clock("soon")).toBe("soon");
  });

  it("starts a new schedule at nine every morning, with tomorrow at nine ready for a one-time", () => {
    const draft = blank("D:/work", "auto", now);
    expect([draft.kind, draft.time, draft.at, draft.mode, draft.enabled]).toEqual(["daily", "09:00", "2026-11-20T09:00", "auto", true]);
    expect(localStamp(now)).toBe("2026-11-19T22:30");
  });

  it("turns the form into a schedule, and says what is missing", () => {
    const draft = { ...blank("D:/work", "edit", now), prompt: " 跑测试 " };
    expect(scheduleOf(draft)).toEqual({ id: "", name: "", workspace: "D:/work", prompt: "跑测试", when: { kind: "daily", time: "09:00", days: [] }, mode: "edit", isolated: false, enabled: true });
    expect(scheduleOf({ ...draft, kind: "every", gap: "2", unit: "hours" })).toMatchObject({ when: { kind: "every", minutes: 120 } });
    expect(scheduleOf({ ...draft, kind: "every", gap: "45", unit: "minutes" })).toMatchObject({ when: { kind: "every", minutes: 45 } });
    expect(scheduleOf({ ...draft, kind: "once" })).toMatchObject({ when: { kind: "once", at: "2026-11-20T09:00" } });
    expect(scheduleOf({ ...draft, prompt: " " })).toBe("还没写交办的事。");
    expect(scheduleOf({ ...draft, workspace: "" })).toBe("还没选洞府：功课得有个地方办。");
    expect(scheduleOf({ ...draft, kind: "every", gap: "0.5" })).toBe("隔多久要填一个不小于 1 的整数。");
  });

  it("opens a kept schedule in the form as it was saved", () => {
    const kept: Schedule = { id: "s1", name: "夜课", workspace: "D:/work", prompt: "跑测试", when: { kind: "every", minutes: 180 }, mode: "auto", isolated: true, enabled: false };
    const draft = draftOf(kept, now);
    expect([draft.gap, draft.unit, draft.isolated, draft.enabled]).toEqual(["3", "hours", true, false]);
    expect(scheduleOf(draft)).toEqual(kept);
    const weekly: Schedule = { ...kept, when: { kind: "daily", time: "07:15", days: [1, 3] } };
    expect(scheduleOf(draftOf(weekly, now))).toEqual(weekly);
    const once: Schedule = { ...kept, when: { kind: "once", at: "2026-12-01T08:00" } };
    expect(scheduleOf(draftOf(once, now))).toEqual(once);
  });
});
