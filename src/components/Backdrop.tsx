import { type CSSProperties, useState } from "react";

/** Points of light that rise from among the mountains: where each starts, how large, how long it takes, how far it sways. */
const MOTES: [x: number, y: number, size: number, seconds: number, delay: number, sway: number, gold: boolean][] = [
  [6, 10, 4, 13, -2, 14, false],
  [14, 22, 3, 17, -9, -10, true],
  [23, 8, 5, 15, -5, 8, false],
  [31, 30, 3, 19, -13, -16, false],
  [39, 14, 4, 14, -7, 12, true],
  [47, 26, 3, 18, -1, -8, false],
  [55, 9, 5, 16, -11, 18, false],
  [63, 20, 3, 13, -4, -12, true],
  [71, 33, 4, 20, -15, 10, false],
  [80, 12, 3, 15, -8, -14, false],
  [88, 24, 5, 17, -3, 9, true],
  [95, 15, 3, 14, -10, -6, false],
];

/** The stars of the night sky: where each is, how large, how long it takes to brighten, and when it starts. */
const STARS: [x: number, y: number, size: number, seconds: number, delay: number][] = [
  [4, 12, 2, 3.4, -1],
  [11, 30, 1.5, 4.6, -3],
  [19, 7, 2.5, 5.2, -2],
  [27, 21, 1.5, 3.8, -4],
  [36, 40, 2, 4.4, -1.5],
  [44, 10, 1.5, 5.6, -3.5],
  [52, 27, 2, 3.2, -0.5],
  [59, 5, 2.5, 4.8, -2.5],
  [66, 34, 1.5, 4.2, -4.5],
  [72, 16, 2, 5.4, -1],
  [91, 38, 1.5, 3.6, -3],
  [96, 9, 2, 4.9, -2],
];

/**
 * The eight trigrams in the order of the 先天八卦, clockwise from the top (which is south): each as its three
 * lines from the innermost out, true for a whole line (阳) and false for a broken one (阴).
 */
const TRIGRAMS: [name: string, lines: [boolean, boolean, boolean]][] = [
  ["乾", [true, true, true]],
  ["巽", [false, true, true]],
  ["坎", [false, true, false]],
  ["艮", [false, false, true]],
  ["坤", [false, false, false]],
  ["震", [true, false, false]],
  ["离", [true, false, true]],
  ["兑", [true, true, false]],
];

/** The eight trigrams in a ring, drawn round the origin: each line a bar across its radius, the innermost first. */
function Bagua() {
  const [wide, thick, gap] = [30, 5, 7];
  return (
    <svg viewBox="-100 -100 200 200">
      {TRIGRAMS.map(([name, lines], at) => (
        <g key={name} transform={`rotate(${at * 45})`}>
          {lines.map((whole, row) => {
            const y = -(62 + row * (thick + 5)) - thick;
            return whole ? (
              <rect key={row} x={-wide / 2} y={y} width={wide} height={thick} rx="1" />
            ) : (
              <g key={row}>
                <rect x={-wide / 2} y={y} width={(wide - gap) / 2} height={thick} rx="1" />
                <rect x={gap / 2} y={y} width={(wide - gap) / 2} height={thick} rx="1" />
              </g>
            );
          })}
        </g>
      ))}
    </svg>
  );
}

/** How a range of mountains is drawn, in the 1200×420 frame the backdrop's drawings share: y counts down from its top. */
export interface RangeShape {
  /** How far apart a peak and the pass beside it stand. */
  step: [number, number];
  /** How high a peak and a pass reach. */
  peak: [number, number];
  pass: [number, number];
  /** One peak raised above the others by this much, to stand for the range: the far range has one. */
  crown?: [number, number];
}

export const RANGES: Record<"far" | "mid" | "near", RangeShape> = {
  far: { step: [80, 140], peak: [160, 220], pass: [235, 290], crown: [30, 55] },
  mid: { step: [80, 125], peak: [250, 292], pass: [305, 338] },
  near: { step: [105, 150], peak: [322, 350], pass: [368, 394] },
};

/**
 * A range drawn afresh: peaks and passes in turn across the frame, each as high and as far apart as chance has it
 * within what the range allows, and now and then a shoulder part way up a slope, as real ridges have. Straight
 * strokes from point to point, as the brush draws them.
 */
export function ridge({ step, peak, pass, crown }: RangeShape, random: () => number = Math.random): string {
  const between = ([low, high]: [number, number]) => low + (high - low) * random();
  const points: [number, number][] = [];
  let up = random() < 0.5;
  points.push([0, between(up ? peak : pass)]);
  for (let x = 0; x < 1200; ) {
    x += between(step);
    // Close enough to the edge, it is the edge: a sliver of a step there would round onto it.
    if (x > 1200 - step[0] / 2) x = 1200;
    up = !up;
    const [fromX, fromY] = points[points.length - 1];
    const y = between(up ? peak : pass);
    if (x - fromX > 70 && random() < 0.35) {
      // A shoulder: a point part way along, set a little higher than the straight line would put it.
      const t = 0.4 + 0.2 * random();
      points.push([fromX + (x - fromX) * t, fromY + (y - fromY) * t - between([6, 16])]);
    }
    points.push([x, y]);
  }
  if (crown) {
    // The highest of the peaks, not at either edge, raised to stand over the rest.
    const peaks = points.map(([, y], at) => [y, at] as const).filter(([, at]) => at > 1 && at < points.length - 2);
    const [, top] = peaks.reduce((best, one) => (one[0] < best[0] ? one : best), peaks[0] ?? [0, -1]);
    if (top > 0) points[top] = [points[top][0], points[top][1] - between(crown)];
  }
  return `M${points.map(([x, y]) => `${Math.round(x)} ${Math.round(y)}`).join(" L")} L1200 420 L0 420 Z`;
}

// One circle split along an S into two halves, drawn around the origin with radius 10.
// SELF is 本尊's half, DOUBLE is the 分身's: together they close into the whole circle.
const SELF = "M0-10A10 10 0 0 1 0 10A5 5 0 0 0 0 0A5 5 0 0 1 0-10Z";
const DOUBLE = "M0-10A10 10 0 0 0 0 10A5 5 0 0 0 0 0A5 5 0 0 1 0-10Z";

/**
 * The home screen's backdrop: mountains in three washes of ink under an open sky. In the sky's upper corner
 * stands the 太极, with the eight trigrams in a ring about it, each turning slowly its own way: golden by day
 * as the sun is, and by night (the dark theme) pale as the moon, among stars. The
 * mountains rise into place when the screen opens, and the 太极 comes up from behind them to where it
 * stands, clear of what is written on the screen. Then mist drifts between the ranges, points of light rise
 * from the valleys, now and then a stroke of light crosses the sky, as a sword in flight does, and a pair of wild
 * geese flies across it. The mountains, the mist and the geese are drawn afresh each time the screen opens. Everything
 * that moves is a layer of its own, moved or faded whole.
 */
export function Mountains() {
  // Every drawing has the same frame, so they lie on one another exactly.
  const frame = { viewBox: "0 0 1200 420", preserveAspectRatio: "xMidYMax slice" } as const;
  // The ranges are drawn afresh each time the screen opens: no two openings show the same mountains.
  const [ranges] = useState(() => ({ far: ridge(RANGES.far), mid: ridge(RANGES.mid), near: ridge(RANGES.near) }));
  // A pair of wild geese crosses the sky, at a height, a pace and in a order of their own each time; and the mist
  // lies at its own heights and drifts at its own pace.
  const [sky] = useState(() => {
    const between = (low: number, high: number) => low + (high - low) * Math.random();
    const lead = between(28, 38);
    const geese = {
      // How high above the foot of the mountains, in the drawings' unit: along the middle range's slopes, about the
      // height of the near range's peaks.
      lift: between(85, 135),
      "--pace": `${between(52, 76)}s`,
      animationDelay: `${between(2, 6)}s`,
      lead: { "--size": `${lead}px` },
      // The second a little behind, above or below the first, and smaller, being farther off.
      follow: { "--size": `${lead * between(0.62, 0.8)}px`, "--behind": `${between(34, 58)}px`, "--aside": `${between(-16, 20)}px` },
    };
    const mist = (pace: number, span: number, deep: number) => {
      const drift = pace * between(0.75, 1.3);
      return { "--lift": between(-14, 14), "--span": `${span * between(0.8, 1.25)}cqw`, "--deep": deep * between(0.8, 1.25), "--pace": `${drift}s`, "--phase": `${-between(0, drift)}s` };
    };
    return { geese, mists: { slow: mist(64, 78, 86), fast: mist(41, 60, 68), low: mist(53, 68, 64) } };
  });
  return (
    <div className="scene" aria-hidden="true">
      {STARS.map(([x, y, size, seconds, delay], at) => (
        <span key={at} className="star" style={{ left: `${x}%`, top: `${y}%`, width: size, height: size, animationDuration: `${seconds}s`, animationDelay: `${delay}s` }} />
      ))}
      <span className="streak one" />
      <span className="streak two" />
      {/* Before the mountains, so behind them: it is seen to come up from behind the far range. */}
      <div className="sun">
        <span className="sun-halo" />
        <span className="bagua">
          <Bagua />
        </span>
        {/* The 太极: the circle cut along an S, as the app's own mark is, with a point of each half in the other. */}
        <span className="taiji">
          <svg viewBox="-10 -10 20 20">
            <circle className="yin" r="10" />
            <path className="yang" d={SELF} />
            <circle className="yin" cy="-5" r="1.5" />
            <circle className="yang" cy="5" r="1.5" />
          </svg>
        </span>
      </div>
      <div className="mountains">
        <div className="range far">
          <svg {...frame}>
            <path d={ranges.far} />
          </svg>
        </div>
        <span className="mist slow" style={sky.mists.slow as CSSProperties} />
        <div className="range mid">
          <svg {...frame}>
            <path d={ranges.mid} />
          </svg>
        </div>
        <span className="mist fast" style={sky.mists.fast as CSSProperties} />
        {/* Two wild geese, a stroke of the brush each, flying west together: the one ahead larger, nearer. Low among the
            mountains: in front of the middle range and behind the near one, whose higher peaks they pass behind. */}
        <div className="geese" style={{ "--lift": sky.geese.lift, "--pace": sky.geese["--pace"], animationDelay: sky.geese.animationDelay } as CSSProperties}>
          {(["lead", "follow"] as const).map((which) => (
            <span key={which} className={`goose ${which}`} style={sky.geese[which] as CSSProperties}>
              <svg viewBox="0 0 40 16">
                <path d="M2 11 Q11 2 20 10 Q29 2 38 11" />
              </svg>
            </span>
          ))}
        </div>
        <div className="range near">
          <svg {...frame}>
            <path d={ranges.near} />
          </svg>
        </div>
        <span className="mist low" style={sky.mists.low as CSSProperties} />
        {MOTES.map(([x, y, size, seconds, delay, sway, gold], at) => (
          <span
            key={at}
            className={`mote-rise ${gold ? "gold" : ""}`}
            style={{ left: `${x}%`, bottom: `${y}%`, width: size, height: size, "--sway": `${sway}px`, animationDuration: `${seconds}s`, animationDelay: `${delay}s` } as CSSProperties}
          />
        ))}
      </div>
    </div>
  );
}

/**
 * The 分身 mark as a status sign: the halves rest together when idle, the 分身's half
 * drifts out and back while it works, and it turns cinnabar and holds apart while it
 * waits for a decision.
 */
export function Mark({ state = "idle", size = 26 }: { state?: "idle" | "working" | "waiting"; size?: number }) {
  // Each half is a drawing of its own, one on the other, so that the 分身's can be moved whole.
  const half = (name: string, d: string, shift: number) => (
    // In a box of the page's own, which is what is moved: a drawing moved by itself is painted again at every step.
    <span className={name}>
      <svg width={size} height={size} viewBox="0 0 26 26">
        <g transform="translate(13 13) rotate(30)">
          <path d={d} transform={`translate(${shift} 0)`} />
        </g>
      </svg>
    </span>
  );
  return (
    <span className={`mark ${state}`} aria-hidden="true" style={{ width: size, height: size, "--unit": `${size / 26}px` } as CSSProperties}>
      {half("self", SELF, 0.5)}
      {half("double", DOUBLE, -0.5)}
    </span>
  );
}

/** The logo: the split circle stamped on a cinnabar seal. Used where the product names itself. */
export function Logo({ size = 26, alive = false }: { size?: number; alive?: boolean }) {
  if (alive) {
    // On the home screen the 分身's half parts from 本尊's and comes back, now and then: what the app does, in
    // its own mark. The plate and each half are drawings of their own, so the half is moved whole.
    const half = (name: string, d: string, shift: number) => (
      <span className={name}>
        <svg width={size} height={size} viewBox="0 0 26 26">
          <g transform="translate(13 13) rotate(30) scale(0.74)">
            <path d={d} transform={`translate(${shift} 0)`} />
          </g>
        </svg>
      </span>
    );
    return (
      <span className="logo alive" aria-hidden="true" style={{ width: size, height: size, "--unit": `${size / 26}px` } as CSSProperties}>
        <span>
          <svg width={size} height={size} viewBox="0 0 26 26">
            <rect className="plate" x="1" y="1" width="24" height="24" rx="5.5" />
          </svg>
        </span>
        {half("self", SELF, 0.7)}
        {half("double", DOUBLE, -0.7)}
      </span>
    );
  }
  return (
    <svg className="logo" aria-hidden="true" width={size} height={size} viewBox="0 0 26 26">
      <rect className="plate" x="1" y="1" width="24" height="24" rx="5.5" />
      <g transform="translate(13 13) rotate(30) scale(0.74)">
        <path className="self" d={SELF} transform="translate(0.7 0)" />
        <path className="double" d={DOUBLE} transform="translate(-0.7 0)" />
      </g>
    </svg>
  );
}
