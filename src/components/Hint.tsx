import { type ReactNode, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "./Icon";

/** Where the note stands: at the mark's left edge or held inside the window, under the mark or over it. */
interface Place {
  left: number;
  top?: number;
  bottom?: number;
}

/** The gap between the mark and its note, and between the note and the window's edge. */
const GAP = 8;
const EDGE = 12;

/**
 * What a label or a heading means, for whoever wants to know: an ⓘ beside it, and the words in a note that shows
 * while the mark is pointed at or has the keyboard, and stays when it is pressed. What must be seen to use the
 * page (how something stands, what to do next) is not put here.
 */
export function Info({ children, label = "说明" }: { children: ReactNode; label?: string }) {
  const mark = useRef<HTMLButtonElement>(null);
  const note = useRef<HTMLSpanElement>(null);
  const id = useId();
  /** Shown because the mark is pointed at or focused; kept because it was pressed. */
  const [shown, setShown] = useState(false);
  const [kept, setKept] = useState(false);
  const [place, setPlace] = useState<Place | null>(null);
  const leaving = useRef<number | undefined>(undefined);
  const open = shown || kept;
  const show = () => {
    window.clearTimeout(leaving.current);
    setShown(true);
  };
  // A moment's grace, so the pointer can cross from the mark to the note to read or select it.
  const hide = () => {
    window.clearTimeout(leaving.current);
    leaving.current = window.setTimeout(() => setShown(false), 120);
  };
  const shut = () => {
    window.clearTimeout(leaving.current);
    setShown(false);
    setKept(false);
  };
  // Placed once it has a size: under the mark, or over it where there is no room under; never off the window's side.
  useLayoutEffect(() => {
    if (!open || !mark.current || !note.current) return setPlace(null);
    const at = mark.current.getBoundingClientRect();
    const size = note.current.getBoundingClientRect();
    const left = Math.max(EDGE, Math.min(at.left - EDGE, window.innerWidth - size.width - EDGE));
    const under = at.bottom + GAP + size.height + EDGE <= window.innerHeight;
    setPlace(under || at.top - GAP - size.height < EDGE ? { left, top: at.bottom + GAP } : { left, bottom: window.innerHeight - at.top + GAP });
  }, [open, children]);
  // It stands where the mark was when it opened: when the page moves under it, or something else is pressed, it goes.
  useEffect(() => {
    if (!open) return;
    const away = (event: Event) => {
      if (event.target instanceof Node && (mark.current?.contains(event.target) || note.current?.contains(event.target))) return;
      shut();
    };
    window.addEventListener("scroll", away, true);
    window.addEventListener("pointerdown", away, true);
    window.addEventListener("resize", shut);
    return () => {
      window.removeEventListener("scroll", away, true);
      window.removeEventListener("pointerdown", away, true);
      window.removeEventListener("resize", shut);
    };
  }, [open]);
  useEffect(() => () => window.clearTimeout(leaving.current), []);
  return (
    <span className="info" onMouseEnter={show} onMouseLeave={hide}>
      <button
        ref={mark}
        type="button"
        className="info-mark"
        aria-label={label}
        aria-expanded={open}
        aria-describedby={open ? id : undefined}
        onFocus={show}
        onBlur={shut}
        onClick={(event) => {
          // Beside a label or in a form, pressing it is asking what the thing is, never the form's own action.
          event.preventDefault();
          event.stopPropagation();
          setKept((now) => !now);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape" && open) {
            // The note is what Escape puts away: not the window the mark may be in.
            event.preventDefault();
            event.stopPropagation();
            shut();
          }
        }}
      >
        <Icon name="info-circle" size={14} />
      </button>
      {open && (
        <span ref={note} id={id} role="tooltip" className="info-note" style={place ?? { left: 0, top: 0, visibility: "hidden" }}>
          {children}
        </span>
      )}
    </span>
  );
}
