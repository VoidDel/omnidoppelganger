import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  MarkdownContent,
  ModelText,
  StreamingMarkdown,
  hasMathDelimiters,
  loadMathPlugins,
  splitStreamingMarkdown,
} from "./Markdown";

describe("Markdown streaming & lazy KaTeX (P1-13 & P2-15)", () => {
  it("splitStreamingMarkdown_never_splits_inside_open_code_fence_or_math_block", async () => {
    await loadMathPlugins();

    const step1 = "First paragraph.\n\n```rust\nfn main() {\n\n    let x = 1;";
    const split1 = splitStreamingMarkdown(step1);
    expect(split1.frozen).toEqual(["First paragraph."]);
    expect(split1.tail).toBe("```rust\nfn main() {\n\n    let x = 1;");

    const step2 = `${step1}\n}\n\`\`\`\n\n$$\na + b =\n\nc + d`;
    const split2 = splitStreamingMarkdown(step2);
    expect(split2.frozen).toEqual(["First paragraph.", "```rust\nfn main() {\n\n    let x = 1;\n}\n```"]);
    expect(split2.tail).toBe("$$\na + b =\n\nc + d");

    const full = `${step2}\n$$\n\nFinal summary.`;
    const split3 = splitStreamingMarkdown(full);
    expect(split3.frozen).toHaveLength(3);
    expect(split3.tail).toBe("Final summary.");

    const streamed = render(<StreamingMarkdown text={full} />);
    const whole = render(<div className="prose"><MarkdownContent text={full} /></div>);
    expect(streamed.container.querySelector(".prose")?.innerHTML).toBe(
      whole.container.querySelector(".prose")?.innerHTML,
    );
  });

  it("keeps tilde fences and bracket display math intact until closed", () => {
    const partialTilde = "Heading\n\n~~~python\ndef f():\n\n    return 1";
    expect(splitStreamingMarkdown(partialTilde)).toEqual({
      frozen: ["Heading"],
      tail: "~~~python\ndef f():\n\n    return 1",
    });

    const partialBracket = "Intro\n\n\\[\nx^2 +\n\ny^2";
    expect(splitStreamingMarkdown(partialBracket)).toEqual({
      frozen: ["Intro"],
      tail: "\\[\nx^2 +\n\ny^2",
    });
  });

  it("detects math delimiters accurately and lazy-loads KaTeX to render .katex nodes", async () => {
    expect(hasMathDelimiters("Plain programming text without formulas")).toBe(false);
    expect(hasMathDelimiters("Inline $E=mc^2$ formula")).toBe(true);
    expect(hasMathDelimiters("Escaped \\(a+b\\) formula")).toBe(true);
    expect(hasMathDelimiters("Display \\[x=1\\] formula")).toBe(true);

    const { container } = render(<ModelText text="Energy is $E=mc^2$ in physics." />);
    await waitFor(() => {
      expect(container.querySelector(".katex")).not.toBeNull();
    });
  });

  it("highlights fenced code blocks via the shared lowlight grammar registry", () => {
    const { container } = render(<ModelText text={"```rust\nfn add(a: i32) -> i32 { a + 1 }\n```"} />);
    expect(container.querySelector(".code-block .lang")?.textContent).toBe("rust");
    expect(container.querySelector(".hljs-keyword")?.textContent).toBe("fn");
  });
});
