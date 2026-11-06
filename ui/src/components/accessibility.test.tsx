import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { getThemeTokens } from "../theme";
import { contrastRatio, sourceRoot } from "../theme/test-support";

/**
 * A focus ring is only visible if it exists, uses the focus token and has
 * enough contrast against the surfaces behind it. These checks hold for the
 * dark theme, which is the default; the ring is expressed in CSS pixels, so a
 * 1x and a 2x display both draw a 2px ring with a 2px offset.
 */

const INTERACTIVE_COMPONENTS = [
  "button.tsx",
  "chip.tsx",
  "toggle.tsx",
  "checkbox.tsx",
  "input.tsx",
  "segmented-control.tsx",
  "radio-card.tsx",
] as const;

describe("focus ring contract", () => {
  test.each(INTERACTIVE_COMPONENTS)("%s declares an offset, token-coloured ring", (file) => {
    const source = readFileSync(join(sourceRoot(), "components", file), "utf8");

    expect(source).toContain("outline-2");
    expect(source).toContain("outline-offset-2");
    expect(source).toContain("outline-focus");
  });

  test.each(INTERACTIVE_COMPONENTS)("%s never removes the ring without replacing it", (file) => {
    const source = readFileSync(join(sourceRoot(), "components", file), "utf8");
    // The only allowed ring removal is on the inner input, where the wrapper
    // draws the ring with `has-[:focus-visible]`.
    const removals = source.match(/focus-visible:outline-none/g) ?? [];
    expect(removals.length).toBe(file === "input.tsx" ? 1 : 0);
  });
});

describe("dark theme focus contrast", () => {
  const { color } = getThemeTokens("dark");

  test.each([color.surface, color.raised, color.overlay, color.sidebar])(
    "focus colour is distinguishable from %s",
    (background) => {
      expect(contrastRatio(color.focus, background)).toBeGreaterThanOrEqual(3);
    },
  );

  test("focus ring is 2 CSS px, so 1x and 2x displays both keep it visible", () => {
    const source = readFileSync(join(sourceRoot(), "components", "button.tsx"), "utf8");
    // `outline-2` is 2 CSS pixels: the device pixel ratio scales the size but
    // never removes the ring.
    expect(source).toContain("outline-2");
    expect(source).not.toMatch(/outline-\[[^\]]*px\]/);
  });
});
