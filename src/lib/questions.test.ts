import { describe, expect, it } from "vitest";
import { blank, choose, isAnswered, replyOf, sayReply } from "./questions";
import type { Question } from "./types";

const options = [{ label: "一" }, { label: "二" }, { label: "三" }];
const single: Question = { header: "方案", question: "用哪个？", kind: "single", options };
const multiple: Question = { header: "测试", question: "测哪些？", kind: "multiple", options };
const text: Question = { header: "补充", question: "还有什么？", kind: "text" };

describe("answering the 分身's questions", () => {
  it("makes one choice, or ticks several in the order they are listed", () => {
    const picked = choose(single, choose(single, blank(), "一"), "三");
    expect(picked.chosen).toEqual(["三"]);
    const ticked = choose(multiple, choose(multiple, choose(multiple, blank(), "三"), "一"), "二");
    expect(ticked.chosen).toEqual(["一", "二", "三"]);
    // Pressed again, an option is unticked.
    expect(choose(multiple, ticked, "二").chosen).toEqual(["一", "三"]);
    // Choosing an option takes a single choice back from 其他.
    expect(choose(single, { chosen: [], text: "都不要", other: true }, "二")).toEqual({ chosen: ["二"], text: "都不要", other: false });
  });

  it("tells the 分身 the choice, or the words when 其他 is the choice", () => {
    expect(replyOf(single, { chosen: ["二"], text: "没选中的话", other: false })).toEqual({ chosen: ["二"], text: "" });
    expect(replyOf(single, { chosen: ["二"], text: " 都不要 ", other: true })).toEqual({ chosen: [], text: "都不要" });
    expect(replyOf(multiple, { chosen: ["一", "三"], text: "先测一", other: false })).toEqual({ chosen: ["一", "三"], text: "先测一" });
    expect(replyOf(text, { chosen: [], text: " 别动接口 ", other: false })).toEqual({ chosen: [], text: "别动接口" });
    expect(isAnswered(replyOf(single, blank()))).toBe(false);
    expect(isAnswered(replyOf(text, { chosen: [], text: "  ", other: false }))).toBe(false);
    expect(isAnswered({ chosen: ["一"] })).toBe(true);
  });

  it("says an answer in a line", () => {
    expect(sayReply({ chosen: ["一", "三"], text: "先测一" })).toBe("一、三、另：先测一");
    expect(sayReply({ text: "别动接口" })).toBe("别动接口");
    expect(sayReply(undefined)).toBe("");
  });
});
