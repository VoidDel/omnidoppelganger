// An order's trip from the composer to the conversation: the 令牌 on the send
// button flies up to where the message lands, and there the message unrolls
// from it like a scroll, right to left.
// The composer says which token left; the transcript, once the message is on
// the page, says where it lands.

/** The token that left with the last message sent: a copy of it to fly, and the one on the button. */
let launch: { copy: SVGElement; button: SVGElement; at: number } | null = null;

/** A launch older than this found nothing to land on: the send failed, or the message is shown elsewhere. */
const STALE_MS = 3000;
/** The flight, and then the turning into words, in milliseconds. */
const FLY_MS = 440;
const TURN_MS = 420;

/** Note that a message is leaving: `button` is the send button, with the token drawn on it. */
export function launchFrom(button: HTMLElement | null) {
  const token = button?.querySelector("svg");
  // Copied now: by the time the message lands the button may read 音 for a 分身 at work.
  if (token) launch = { copy: token.cloneNode(true) as SVGElement, button: token, at: performance.now() };
}

/** Whether a message has left the composer and not landed yet. */
export function inFlight(): boolean {
  if (launch && performance.now() - launch.at > STALE_MS) launch = null;
  return launch !== null;
}

/**
 * Fly the token to `target`, the element that now shows the message, and turn
 * it into that element. The token flies above everything else, since the
 * conversation clips what scrolls in it; the message stays unseen until the
 * token arrives at its right-hand end, then unrolls from there to the left.
 */
export function land(target: HTMLElement | null) {
  const start = inFlight() ? launch : null;
  launch = null;
  if (!start || !target || typeof target.animate !== "function") return;
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const from = start.button.getBoundingClientRect();
  const to = target.getBoundingClientRect();
  // It comes down on the message's right-hand end, where a scroll starts.
  const spot = { x: to.width - Math.min(to.width / 2, 20), y: Math.min(to.height / 2, 60) };
  const dx = to.left + spot.x - (from.left + from.width / 2);
  const dy = to.top + spot.y - (from.top + from.height / 2);

  // Three boxes, one inside the next, so that going across, going up, and the token's own
  // turning each keep their own pace: up fast and across late makes the path an arc.
  const across = document.createElement("div");
  const up = document.createElement("div");
  across.className = "token-flight";
  across.setAttribute("aria-hidden", "true");
  Object.assign(across.style, { left: `${from.left}px`, top: `${from.top}px`, width: `${from.width}px`, height: `${from.height}px` });
  up.append(start.copy);
  across.append(up);
  document.body.append(across);

  const total = FLY_MS + TURN_MS;
  const arrive = FLY_MS / total;
  across.animate([{ transform: "translateX(0)" }, { transform: `translateX(${dx}px)` }], { duration: FLY_MS, easing: "cubic-bezier(0.5, 0, 0.75, 0.6)", fill: "forwards" });
  up.animate([{ transform: "translateY(0)" }, { transform: `translateY(${dy}px)` }], { duration: FLY_MS, easing: "cubic-bezier(0.15, 0.7, 0.3, 1)", fill: "forwards" });
  const flight = start.copy.animate(
    [
      { transform: "rotate(0deg) scale(1)", opacity: 1 },
      // It grows faint as it climbs, and is gone as the message it carried comes out of it.
      { transform: "rotate(-24deg) scale(1.15)", opacity: 0.85, offset: arrive * 0.45 },
      { transform: "rotate(0deg) scale(1)", opacity: 0.4, offset: arrive },
      { transform: "rotate(0deg) scale(1.5)", opacity: 0 },
    ],
    { duration: total, fill: "forwards" },
  );
  // The message, unseen while the token is in the air, unrolls from its right-hand end. (What is cut
  // away reaches past the box on the other sides, to leave the paper showing under it whole.)
  const unroll: KeyframeAnimationOptions = { delay: FLY_MS - 40, duration: TURN_MS, easing: "cubic-bezier(0.3, 0.7, 0.2, 1)", fill: "backwards" };
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
  // The button is without its token while it is away, and has a new one once the order is delivered.
  start.button.animate([{ opacity: 0 }, { opacity: 0, offset: 0.8 }, { opacity: 1 }], { duration: total + 200 });

  const arrived = () => across.remove();
  flight.finished.then(arrived, arrived);
}
