import { describe, expect, it } from "vitest";
import { normalizeMath } from "./math";

describe("normalizeMath", () => {
  it("converts LaTeX paren/bracket delimiters and escapes percentage signs in math", () => {
    expect(normalizeMath("产生约 $-1.18%$ 增益误差及 $36\\text{ mV}$")).toBe("产生约 $-1.18\\%$ 增益误差及 $36\\text{ mV}$");
    expect(normalizeMath("行内 \\(f_0 = \\frac{1}{2\\pi RC}\\) 公式")).toBe("行内 $f_0 = \\frac{1}{2\\pi RC}$ 公式");
    expect(normalizeMath("块级：\n\\[E = mc^2\\]")).toBe("块级：\n$$E = mc^2$$");
  });

  it("leaves fenced code blocks and inline code untouched", () => {
    const src = "代码 `\\(x\\)` 和：\n```tex\n\\[a + b\\]\n$-1.18%$\n```\n正文 $200\\Omega$";
    expect(normalizeMath(src)).toBe(src);
  });
});
