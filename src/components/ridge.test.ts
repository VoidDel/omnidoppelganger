import { describe, expect, it } from "vitest";
import { RANGES, ridge } from "./Chrome";

/** The points of a drawn range, without the two that close it along the frame's foot. */
const points = (path: string) =>
  path
    .replace(/^M/, "")
    .replace(/ Z$/, "")
    .split(" L")
    .map((pair) => pair.split(" ").map(Number))
    .slice(0, -2);

describe("the mountains on the home screen", () => {
  it("are drawn afresh each time, across the whole frame, within each range's heights", () => {
    for (const [name, shape] of Object.entries(RANGES)) {
      for (let round = 0; round < 50; round++) {
        const drawn = points(ridge(shape));
        expect(drawn[0][0], name).toBe(0);
        expect(drawn[drawn.length - 1][0], name).toBe(1200);
        // Left to right, and never above the crown's reach or below the lowest pass.
        for (let at = 1; at < drawn.length; at++) expect(drawn[at][0]).toBeGreaterThan(drawn[at - 1][0]);
        const highest = shape.peak[0] - (shape.crown?.[1] ?? 0) - 16;
        expect(drawn.every(([, y]) => y >= highest - 1 && y <= shape.pass[1] + 1), name).toBe(true);
      }
      expect(ridge(shape)).not.toBe(ridge(shape));
    }
  });
});
