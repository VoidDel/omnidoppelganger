import { useLayoutEffect, useRef } from "react";

/**
 * Light going round the edge of the thing it is put in: on a seal while its act is under way, and round the box an
 * order is written in while the 分身 works. The edge alone, unless `aura` asks for the same light out of focus about
 * it too, which only the box does: on a seal a glow spills over what lies around it. Layers that lie under the
 * thing's own words, so the thing must stack its own (`isolation`, or a `z-index`). See "light going round an edge"
 * in the stylesheet for why they are layers that turn, not a gradient that is redrawn.
 */
export function Light({ aura = false }: { aura?: boolean }) {
  const edge = useRef<HTMLSpanElement>(null);
  const glow = useRef<HTMLSpanElement>(null);

  // Size the turning square to the box's diagonal in pixels so it can be centred by `left`/`top` without a
  // percentage `translate`: WebKit keeps a stale pixel offset for `translate: -50% -50%` on a running rotation
  // animation when the box changes size (such as opening the side panel), shifting the light off one side.
  useLayoutEffect(() => {
    if (!aura) return;
    const frame = edge.current;
    if (!frame) return;
    let last = "";
    const fit = () => {
      const { width, height } = frame.getBoundingClientRect();
      if (width <= 0 && height <= 0) return;
      // `.aura` extends 3px outside `.edge` on every side; cover its full diagonal at every angle.
      const side = `${Math.ceil(Math.hypot(width + 6, height + 6)) + 4}px`;
      if (side === last) return;
      last = side;
      frame.style.setProperty("--light-side", side);
      glow.current?.style.setProperty("--light-side", side);
    };
    fit();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(fit);
    observer.observe(frame);
    return () => observer.disconnect();
  }, [aura]);

  return (
    <>
      {aura && <span ref={glow} className="aura" aria-hidden="true" />}
      <span ref={edge} className="edge" aria-hidden="true" />
    </>
  );
}
