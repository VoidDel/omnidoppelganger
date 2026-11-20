/**
 * Light going round the edge of the thing it is put in: on a seal while its act is under way, and round the box an
 * order is written in while the 分身 works. The edge alone, unless `aura` asks for the same light out of focus about
 * it too, which only the box does: on a seal a glow spills over what lies around it. Layers that lie under the
 * thing's own words, so the thing must stack its own (`isolation`, or a `z-index`). See "light going round an edge"
 * in the stylesheet for why they are layers that turn, not a gradient that is redrawn.
 */
export function Light({ aura = false }: { aura?: boolean }) {
  return (
    <>
      {aura && <span className="aura" aria-hidden="true" />}
      <span className="edge" aria-hidden="true" />
    </>
  );
}
