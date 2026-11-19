import { type ReactNode, useLayoutEffect, useRef, useState } from "react";

/**
 * A field's explanation, held to its first line until it is opened: the line says what the field is for, and
 * the rest is there for whoever wants it. The way to open it shows only when there is more to see.
 */
export function Hint({ children }: { children: ReactNode }) {
  const body = useRef<HTMLParagraphElement>(null);
  const [open, setOpen] = useState(false);
  const [more, setMore] = useState(false);
  // Measured while held: whether its words go past the one line they are held to.
  useLayoutEffect(() => {
    if (!open && body.current) setMore(body.current.scrollHeight > body.current.clientHeight + 1);
  });
  return (
    <div className="hint-wrap">
      <p ref={body} className={`hint ${open ? "" : "held"}`}>
        {children}
      </p>
      {(more || open) && (
        <button type="button" className="hint-more" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "收起" : "展开"}
        </button>
      )}
    </div>
  );
}
