import { describe, expect, it } from "vitest";
import { detectPasteKind, nextPastedFilename, shouldCollapsePaste } from "./paste";

describe("paste detection", () => {
  it("detects SVG and collapses rich SVG snippets", () => {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200">${"<rect width='10' height='10'/>".repeat(10)}</svg>`;
    const info = detectPasteKind(svg);
    expect(info.kind).toBe("svg");
    expect(info.ext).toBe("svg");
    expect(shouldCollapsePaste(svg)).toBe(true);
  });

  it("detects full HTML documents and HTML fragments", () => {
    const html = `<!DOCTYPE html><html><head><title>Demo</title></head><body><div><p>Hello world</p><span>${"a".repeat(200)}</span></div></body></html>`;
    const info = detectPasteKind(html);
    expect(info.kind).toBe("html");
    expect(info.ext).toBe("html");
    expect(shouldCollapsePaste(html)).toBe(true);
  });

  it("detects JSON and Markdown for long pastes while keeping short plain text inline", () => {
    const json = JSON.stringify({ items: Array.from({ length: 40 }, (_, i) => ({ id: i, name: `item-${i}` })) }, null, 2);
    expect(detectPasteKind(json).kind).toBe("json");
    expect(shouldCollapsePaste(json)).toBe(true);

    const md = `# 架构设计方案\n\n- 模块一：粘贴内容自动识别\n- 模块二：折叠块预览弹窗\n- 模块三：按类型渲染预览\n\n\`\`\`ts\nexport const demo = "${"x".repeat(120)}";\n\`\`\`\n`;
    expect(detectPasteKind(md).kind).toBe("markdown");
    expect(detectPasteKind(md).ext).toBe("md");
    expect(shouldCollapsePaste(md)).toBe(true);

    expect(shouldCollapsePaste("帮我看看这段代码哪里有问题")).toBe(false);
    expect(shouldCollapsePaste(Array.from({ length: 14 }, (_, i) => `line ${i}`).join("\n"))).toBe(true);
  });

  it("picks collision-free sequential filenames", () => {
    expect(nextPastedFilename("svg", [])).toBe("pasted-1.svg");
    expect(nextPastedFilename("svg", ["pasted-1.svg", "pasted-2.svg"])).toBe("pasted-3.svg");
  });
});
