import { type CSSProperties, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { type AnsiStyle, detectOutputKind, parseTerminalLines, stripAnsi } from "../lib/terminalOutput";
import { Icon } from "./Icon";

const INITIAL_LINES = 800;
const LOAD_STEP = 1000;
const EXPAND_THRESHOLD = 14;

export interface CommandOutputProps {
  output: string;
  command?: string;
  cwd?: string;
  onOpenPath?: (path: string, line?: number) => void;
}

function parsePathToken(token: string, cwd: string): { path: string; line?: number } {
  const match = /^(.*?)(?::(\d+)(?::\d+)?)?$/.exec(token);
  const rawPath = (match?.[1] ?? token).replace(/^\.\//, "");
  const line = match?.[2] ? Number.parseInt(match[2], 10) : undefined;
  const isAbs = rawPath.startsWith("/") || rawPath.startsWith("~/") || /^[A-Za-z]:[/\\]/.test(rawPath);
  const isRelCwd = cwd && !cwd.startsWith("/") && !cwd.startsWith("~/") && !cwd.startsWith(".");
  const resolved = !isAbs && isRelCwd && !rawPath.startsWith(`${cwd}/`) ? `${cwd}/${rawPath}` : rawPath;
  return { path: resolved, line };
}

function segmentStyle(style?: AnsiStyle): { className?: string; inline?: CSSProperties } {
  if (!style) return {};
  const classes: string[] = [];
  const inline: CSSProperties = {};
  if (style.bold) classes.push("ansi-bold");
  if (style.dim) classes.push("ansi-dim");
  if (style.italic) classes.push("ansi-italic");
  if (style.underline) classes.push("ansi-underline");
  if (typeof style.fg === "number") {
    classes.push(`ansi-fg-${style.fg}`);
  } else if (typeof style.fg === "string") {
    inline.color = style.fg;
  }
  if (typeof style.bg === "number") {
    classes.push(`ansi-bg-${style.bg}`);
  } else if (typeof style.bg === "string") {
    inline.backgroundColor = style.bg;
  }
  return {
    className: classes.length > 0 ? classes.join(" ") : undefined,
    inline: Object.keys(inline).length > 0 ? inline : undefined,
  };
}

/**
 * Paper-and-ink styled terminal output block for shell command tool cards.
 * Displays the full command bar, copy/wrap/expand controls, ANSI colors,
 * and automatic diff/log semantic highlighting.
 */
export function CommandOutput({ output, command, cwd, onOpenPath }: CommandOutputProps) {
  const { t } = useI18n();
  const rawLines = useMemo(() => {
    const normalized = output.endsWith("\n") ? output.slice(0, -1) : output;
    return normalized.split("\n");
  }, [output]);

  const kind = useMemo(() => detectOutputKind(rawLines), [rawLines]);
  const isDiff = kind === "unified-diff" || kind === "normal-diff";

  const [limit, setLimit] = useState(INITIAL_LINES);
  const [wrap, setWrap] = useState(!isDiff);
  const [showLineNumbers, setShowLineNumbers] = useState(true);
  const [expanded, setExpanded] = useState(false);
  const [copiedWhich, setCopiedWhich] = useState<"output" | "command" | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      const canScrollY = el.scrollHeight > el.clientHeight + 1;
      const canScrollX = el.scrollWidth > el.clientWidth + 1;
      if (!canScrollY && !canScrollX) return;

      const vertical = Math.abs(event.deltaY) >= Math.abs(event.deltaX);
      if (vertical && canScrollY) {
        event.stopPropagation();
        const atTop = el.scrollTop <= 0;
        const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 1;
        if ((event.deltaY < 0 && atTop) || (event.deltaY > 0 && atBottom)) {
          event.preventDefault();
        }
      } else if (!vertical && canScrollX) {
        event.stopPropagation();
        const atLeft = el.scrollLeft <= 0;
        const atRight = el.scrollLeft + el.clientWidth >= el.scrollWidth - 1;
        if ((event.deltaX < 0 && atLeft) || (event.deltaX > 0 && atRight)) {
          event.preventDefault();
        }
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  useEffect(() => {
    setLimit(INITIAL_LINES);
    setWrap(!isDiff);
  }, [output, isDiff]);

  useEffect(() => {
    if (!copiedWhich) return;
    const timer = window.setTimeout(() => setCopiedWhich(null), 1600);
    return () => window.clearTimeout(timer);
  }, [copiedWhich]);

  const shownCount = Math.min(limit, rawLines.length);
  const parsedLines = useMemo(
    () => parseTerminalLines(rawLines.slice(0, shownCount), kind),
    [rawLines, shownCount, kind],
  );

  const copyText = (text: string, which: "output" | "command") => {
    navigator.clipboard?.writeText(text).then(
      () => setCopiedWhich(which),
      () => {},
    );
  };

  const cleanCommand = command?.trim() || "";
  const cleanCwd = cwd && cwd !== "." ? cwd.trim() : "";

  return (
    <div
      className={`cmd-out ${isDiff ? "is-diff" : ""} ${wrap ? "is-wrap" : "is-scroll"} ${expanded ? "is-expanded" : ""} ${showLineNumbers ? "" : "hide-line-no"}`}
    >
      <div className="cmd-out-head">
        <div className="cmd-out-cmd" title={cleanCommand || undefined}>
          <span className="cmd-out-prompt" aria-hidden="true">
            $
          </span>
          {cleanCwd && <span className="cmd-out-cwd">{cleanCwd}</span>}
          <code className="cmd-out-text">{cleanCommand || t("quote.terminalLabel")}</code>
        </div>
        <div className="cmd-out-tools">
          {isDiff && <span className="cmd-out-badge">Diff</span>}
          <span className="cmd-out-meta">{t("cmdOut.lines", { count: rawLines.length })}</span>
          <button
            type="button"
            className={`cmd-out-btn ${showLineNumbers ? "on" : ""}`}
            aria-pressed={showLineNumbers}
            title={t("cmdOut.lineNumbers")}
            onClick={() => setShowLineNumbers((prev) => !prev)}
          >
            {t("cmdOut.lineNumbers")}
          </button>
          <button
            type="button"
            className={`cmd-out-btn ${wrap ? "on" : ""}`}
            aria-pressed={wrap}
            title={wrap ? t("cmdOut.nowrap") : t("cmdOut.wrap")}
            onClick={() => setWrap((prev) => !prev)}
          >
            {wrap ? t("cmdOut.wrap") : t("cmdOut.nowrap")}
          </button>
          {rawLines.length > EXPAND_THRESHOLD && (
            <button
              type="button"
              className="cmd-out-btn"
              onClick={() => setExpanded((prev) => !prev)}
            >
              {expanded ? t("cmdOut.collapse") : t("cmdOut.expand")}
            </button>
          )}
          {cleanCommand && (
            <button
              type="button"
              className="cmd-out-btn"
              title={t("cmdOut.copyCommand")}
              onClick={() => copyText(cleanCommand, "command")}
            >
              <Icon name={copiedWhich === "command" ? "check-lg" : "terminal"} size={12} />
              <span>{copiedWhich === "command" ? t("cmdOut.copied") : t("cmdOut.copyCommand")}</span>
            </button>
          )}
          <button
            type="button"
            className="cmd-out-btn"
            title={t("cmdOut.copyOutput")}
            onClick={() => copyText(stripAnsi(output), "output")}
          >
            <Icon name={copiedWhich === "output" ? "check-lg" : "copy"} size={12} />
            <span>{copiedWhich === "output" ? t("cmdOut.copied") : t("cmdOut.copyOutput")}</span>
          </button>
        </div>
      </div>

      <div ref={scrollRef} className="cmd-out-scroll" role="region" aria-label={t("quote.terminalLabel")}>
        <div className="cmd-out-lines">
          {parsedLines.map((line, idx) =>
            line.kind === "diff-sep" ? null : (
              <div key={idx} className={`cmd-line kind-${line.kind}`}>
                <span className="cmd-line-no" aria-hidden="true">
                  {idx + 1}
                </span>
                <span className="cmd-line-text">
                  {line.segments.map((seg, segIdx) => {
                    const styled = segmentStyle(seg.style);
                    if (seg.token === "path" && onOpenPath) {
                      const target = parsePathToken(seg.text, cleanCwd);
                      return (
                        <button
                          key={segIdx}
                          type="button"
                          className={[styled.className, "cmd-path", "cmd-path-btn"].filter(Boolean).join(" ")}
                          style={styled.inline}
                          title={
                            target.line
                              ? t("cmdOut.openPathLine", { path: target.path, line: target.line })
                              : t("cmdOut.openPath", { path: target.path })
                          }
                          onClick={() => onOpenPath(target.path, target.line)}
                        >
                          {seg.text}
                        </button>
                      );
                    }
                    const cls = [styled.className, seg.token === "path" ? "cmd-path" : undefined]
                      .filter(Boolean)
                      .join(" ");
                    if (!cls && !styled.inline) {
                      return seg.text;
                    }
                    return (
                      <span key={segIdx} className={cls || undefined} style={styled.inline}>
                        {seg.text}
                      </span>
                    );
                  })}
                  {line.segments.length === 1 && line.segments[0].text === "" ? "\n" : null}
                </span>
              </div>
            ),
          )}
        </div>
      </div>

      {rawLines.length > limit && (
        <div className="tool-more-bar">
          <button
            type="button"
            className="button outline small"
            onClick={() => setLimit((cur) => Math.min(rawLines.length, cur + LOAD_STEP))}
          >
            {t("workbench.loadMoreLines", { shown: shownCount, total: rawLines.length })}
          </button>
        </div>
      )}
    </div>
  );
}
