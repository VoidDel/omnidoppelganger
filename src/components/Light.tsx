/**
 * Light going round the edge of the thing it is put in, with its glow about it: on a seal while its act is under
 * way, and round the box an order is written in while the 分身 works. Two layers that lie under the thing's own
 * words, so the thing must stack its own (`isolation`, or a `z-index`). See "light going round an edge" in the
 * stylesheet for why it is made of layers that turn, not of a gradient that is redrawn.
 */
export function Light() {
  return (
    <>
      <span className="aura" aria-hidden="true" />
      <span className="edge" aria-hidden="true" />
    </>
  );
}
