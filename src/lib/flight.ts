// An order's trip from the composer to the conversation: the 令牌 on the send
// button melts into a point of light, the light flies up to where the message
// lands with a tail behind it, and there the message unrolls from it like a
// scroll, right to left. The composer says which token left; the transcript,
// once the message is on the page, says where it lands.

/** The token that left with the last message sent: a copy of it to melt away, where on the screen it stood, and its colour. */
let launch: { copy: SVGElement; from: DOMRect; colour: string; at: number } | null = null;

/** A launch older than this found nothing to land on: the send failed, or the message is shown elsewhere. */
const STALE_MS = 3000;
/** The token's melting into light, the light's flight, and the message's unrolling, in milliseconds. */
const MELT_MS = 150;
const FLY_MS = 440;
const TURN_MS = 420;
/** The light and the tail behind it: how many points, and how long each sets off after the one before. */
const POINTS = 10;
const GAP_MS = 11;

/** Note that a message is leaving: `button` is the send button, with the token drawn on it. */
export function launchFrom(button: HTMLElement | null) {
  const token = button?.querySelector("svg");
  // Copied and measured now: by the time the message lands the button has given its place to the one that stops the 分身.
  if (!token) return;
  // The light it turns into is the colour of the token's own plate: jade for an order, gold for a word.
  const plate = token.querySelector(".plate");
  launch = { copy: token.cloneNode(true) as SVGElement, from: token.getBoundingClientRect(), colour: plate ? getComputedStyle(plate).fill : "", at: performance.now() };
}

/** Whether a message has left the composer and not landed yet. */
export function inFlight(): boolean {
  if (launch && performance.now() - launch.at > STALE_MS) launch = null;
  return launch !== null;
}

/** A box above everything else on the page, at a place in the window, that takes no part in the layout. */
function above(left: number, top: number): HTMLElement {
  const box = document.createElement("div");
  box.className = "token-flight";
  box.setAttribute("aria-hidden", "true");
  Object.assign(box.style, { left: `${left}px`, top: `${top}px` });
  document.body.append(box);
  return box;
}

/**
 * Fly the order to `target`, the element that now shows the message. The
 * light flies above everything else, since the conversation clips what
 * scrolls in it; the message stays unseen until the light arrives at its
 * right-hand end, then unrolls from there to the left.
 */
export function land(target: HTMLElement | null) {
  const start = inFlight() ? launch : null;
  launch = null;
  if (!start || !target || typeof target.animate !== "function") return;
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const { from } = start;
  const to = target.getBoundingClientRect();
  const origin = { x: from.left + from.width / 2, y: from.top + from.height / 2 };
  // It comes down on the message's right-hand end, where a scroll starts.
  const dx = to.right - Math.min(to.width / 2, 20) - origin.x;
  const dy = to.top + Math.min(to.height / 2, 60) - origin.y;
  const boxes: HTMLElement[] = [];
  const moves: Animation[] = [];

  // The token, where it stood on the button, draws in to a point and is gone.
  const melting = above(from.left, from.top);
  melting.append(start.copy);
  boxes.push(melting);
  moves.push(
    start.copy.animate(
      [
        { transform: "scale(1)", opacity: 1 },
        { transform: "scale(0.12)", opacity: 0 },
      ],
      { duration: MELT_MS, easing: "ease-in", fill: "forwards" },
    ),
  );

  // The light, and the fainter points that follow it. Each is three boxes, one inside the next, so
  // that going across, going up, and its own glow keep their own pace: up fast and across late
  // makes the path an arc, and the points setting off one after another draw the tail along it.
  const leave = MELT_MS * 0.6;
  for (let at = 0; at < POINTS; at++) {
    const across = above(origin.x, origin.y);
    if (start.colour) across.style.setProperty("--mote", start.colour);
    const up = document.createElement("div");
    const point = document.createElement("span");
    point.className = "mote";
    up.append(point);
    across.append(up);
    boxes.push(across);
    const delay = leave + at * GAP_MS;
    const size = 1 - at * 0.08;
    const glow = 1 - at * 0.09;
    moves.push(
      across.animate([{ transform: "translateX(0)" }, { transform: `translateX(${dx}px)` }], { delay, duration: FLY_MS, easing: "cubic-bezier(0.5, 0, 0.75, 0.6)", fill: "both" }),
      up.animate([{ transform: "translateY(0)" }, { transform: `translateY(${dy}px)` }], { delay, duration: FLY_MS, easing: "cubic-bezier(0.15, 0.7, 0.3, 1)", fill: "both" }),
    );
    if (at === 0) {
      // The light itself: it swells out of the token, and bursts where it lands.
      const burst = 220;
      const landed = FLY_MS / (FLY_MS + burst);
      moves.push(
        point.animate(
          [
            { transform: "scale(0)", opacity: 0 },
            { transform: "scale(1.3)", opacity: 1, offset: landed * 0.15 },
            { transform: "scale(1)", opacity: 1, offset: landed },
            { transform: "scale(3.4)", opacity: 0 },
          ],
          { delay, duration: FLY_MS + burst, fill: "both" },
        ),
      );
    } else {
      moves.push(
        point.animate(
          [
            { transform: "scale(0)", opacity: 0 },
            { transform: `scale(${size})`, opacity: glow * 0.8, offset: 0.2 },
            { transform: `scale(${size * 0.5})`, opacity: 0 },
          ],
          { delay, duration: FLY_MS, fill: "both" },
        ),
      );
    }
  }

  // The message, unseen while the light is in the air, unrolls from its right-hand end. (What is cut
  // away reaches past the box on the other sides, to leave the paper showing under it whole.)
  const unroll: KeyframeAnimationOptions = { delay: leave + FLY_MS - 40, duration: TURN_MS, easing: "cubic-bezier(0.3, 0.7, 0.2, 1)", fill: "backwards" };
  target.animate(
    [
      { clipPath: "inset(-12px -12px -12px 100%)", opacity: 0 },
      { opacity: 1, offset: 0.12 },
      { clipPath: "inset(-12px -12px -12px -12px)", opacity: 1 },
    ],
    unroll,
  );
  // Who said it and when, under the message, comes in with it.
  target.parentElement?.querySelector(".said-foot")?.animate([{ opacity: 0 }, { opacity: 1 }], unroll);

  const arrived = () => boxes.forEach((box) => box.remove());
  Promise.allSettled(moves.map((move) => move.finished)).then(arrived);
}
