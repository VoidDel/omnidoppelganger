/**
 * Prepare Markdown text for `remark-math` + `rehype-katex`:
 * - Leaves fenced code blocks and inline code spans untouched.
 * - Converts `\[ ... \]` display delimiters to `$$ ... $$` and `\( ... \)` inline delimiters to `$ ... $`.
 * - Escapes bare `%` after digits inside math spans (e.g. `$-1.18%$` -> `$-1.18\%$`) so KaTeX doesn't treat `%` as a comment.
 */
export function normalizeMath(text: string): string {
  if (!text.includes("$") && !text.includes("\\(") && !text.includes("\\[")) {
    return text;
  }
  // Split out fenced code blocks and inline code spans first so code is never touched.
  const segments = text.split(/(```[\s\S]*?```|~~~[\s\S]*?~~~|`[^`\n]+`)/g);
  return segments
    .map((seg, idx) => {
      if (idx % 2 === 1) return seg;
      let out = seg
        .replace(/\\\[([\s\S]*?)\\\]/g, (_, body: string) => `$$${body}$$`)
        .replace(/\\\(([^\n]*?)\\\)/g, (_, body: string) => `$${body}$`);
      // Escape unescaped `%` after a digit inside $...$ or $$...$$ so engineering percentages render properly.
      out = out.replace(/(\$\$[\s\S]*?\$\$|\$[^$\n]+\$)/g, (expr) => expr.replace(/(\d)(?<!\\)%/g, "$1\\%"));
      return out;
    })
    .join("");
}
