import { describe, expect, test } from "vitest";
import { rowMenuPlacement } from "./row-menu";

/** A trigger box at `top` with the given height, spanning x 700..740. */
function anchor(top: number, height = 24): DOMRect {
  return {
    top,
    bottom: top + height,
    left: 700,
    right: 740,
    width: 40,
    height,
    x: 700,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

describe("rowMenuPlacement", () => {
  test("opens below when the container has room under the trigger", () => {
    expect(rowMenuPlacement(anchor(100), { top: 40, bottom: 600 })).toEqual({
      side: "below",
      maxHeight: 320,
    });
  });

  test("flips above when the trigger sits near the bottom edge", () => {
    // 476px of room above, 116 below: the menu must not run past the scroller.
    expect(rowMenuPlacement(anchor(480), { top: 40, bottom: 600 })).toEqual({
      side: "above",
      maxHeight: 320,
    });
  });

  test("clamps the menu to the roomier side so every item stays reachable", () => {
    // 86px below, 26 above: the menu scrolls its own items inside 86px.
    expect(rowMenuPlacement(anchor(200), { top: 170, bottom: 314 })).toEqual({
      side: "below",
      maxHeight: 86,
    });
  });
});
