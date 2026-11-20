import { type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";

/** The room kept between the bubble and the edge of what holds it. */
const EDGE = 12;

/**
 * What asks before something is taken away for good: a bubble from the button that asked, its point at that button,
 * saying what will be lost, with 算了 and the way on. Put it in a `.bubble-anchor` right after that button. It hangs
 * under the button, or over it where there is no room under (at the foot of a dialog). It goes on Escape or a press
 * anywhere outside the anchor, as 算了 does; the keyboard starts on 算了, the choice that loses nothing, so Enter at
 * once keeps everything.
 */
export function ConfirmBubble({ label, confirm, onConfirm, onCancel, children }: { label: string; confirm: string; onConfirm: () => void; onCancel: () => void; children: ReactNode }) {
  const bubble = useRef<HTMLDivElement>(null);
  const keep = useRef<HTMLButtonElement>(null);
  const [up, setUp] = useState(false);
  // The latest way back, so the page drawing again neither re-listens nor takes the keys back to 算了.
  const cancel = useRef(onCancel);
  cancel.current = onCancel;
  // Before it is seen: its point over the middle of the button, whatever its width, and which way it has room.
  useLayoutEffect(() => {
    const element = bubble.current;
    const trigger = element?.parentElement?.firstElementChild;
    if (!element || !(trigger instanceof HTMLElement)) return;
    element.style.setProperty("--point", `${trigger.offsetWidth / 2}px`);
    const room = element.closest("dialog")?.getBoundingClientRect().bottom ?? window.innerHeight;
    setUp(element.getBoundingClientRect().bottom > room - EDGE);
  }, []);
  useEffect(() => {
    keep.current?.focus();
    const away = (event: PointerEvent) => {
      const anchor = bubble.current?.parentElement;
      if (event.target instanceof Node && !anchor?.contains(event.target)) cancel.current();
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      // In a dialog, Escape puts the bubble away, not the dialog.
      event.preventDefault();
      event.stopPropagation();
      cancel.current();
    };
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", escape, true);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", escape, true);
    };
  }, []);
  return (
    <div ref={bubble} className={`confirm-bubble ${up ? "up" : ""}`} role="alertdialog" aria-label={label}>
      {children}
      <div className="confirm-bubble-acts">
        <button ref={keep} type="button" className="button text small" onClick={onCancel}>
          算了
        </button>
        <button type="button" className="button danger-fill small" onClick={onConfirm}>
          {confirm}
        </button>
      </div>
    </div>
  );
}
