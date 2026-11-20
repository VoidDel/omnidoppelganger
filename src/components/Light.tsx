/**
 * Light going round the edge of the thing it is put in: on a seal while its act is under way, and round the box an
 * order is written in while the 分身 works. The edge alone, with no glow spilling about it. A layer that lies under
 * the thing's own words, so the thing must stack its own (`isolation`, or a `z-index`). See "light going round an
 * edge" in the stylesheet for why it is a layer that turns, not a gradient that is redrawn.
 */
export function Light() {
  return <span className="edge" aria-hidden="true" />;
}
