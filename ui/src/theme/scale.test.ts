import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { blockFor, readTokenCss, sourceRoot } from "./test-support";
import { ThemeTokens } from "./tokens";

/**
 * Guards the numeric utility convention: the number in a Tailwind utility is
 * the pixel value (`gap-6` is 6px, `size-20` is 20px, `rounded-8` is 8px).
 *
 * Tailwind's own base is 0.25rem per step, which silently turned `h-40` into
 * 160px and made `rounded-8` emit no CSS at all. These tests fail if the base
 * drifts back or if a component uses a step the theme does not define.
 * `e2e/scale.visual.spec.ts` checks the same convention against the computed
 * styles of the built app.
 */

const CSS = readTokenCss();
const THEME = blockFor(CSS, "@theme inline");
const SOURCE = sourceRoot();

function sourceFiles(directory: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...sourceFiles(path));
    else if (/\.(tsx?|css)$/.test(entry.name)) files.push(path);
  }
  return files;
}

const SOURCE_TEXT = sourceFiles(SOURCE)
  .map((file) => readFileSync(file, "utf8"))
  .join("\n");

/** Numbers used by `rounded-<n>` in the source. */
const USED_RADII = [
  ...SOURCE_TEXT.matchAll(/(?<![\w-])rounded(?:-[tblrse]{1,2})?-(\d+)(?![\w./[-])/g),
].map((match) => Number(match[1]));

/** Numbers used by `text-<n>` in the source. */
const USED_TEXT = [...SOURCE_TEXT.matchAll(/(?<![\w-])text-(\d+)(?![\w.])/g)].map((match) =>
  Number(match[1]),
);

/** Numeric token keys and their pixel value, from the typed table. */
function pixels(scale: Readonly<Record<number, string>>): Map<number, string> {
  return new Map(Object.entries(scale).map(([key, value]) => [Number(key), value]));
}

const SPACING = pixels(ThemeTokens.dark.space);
const RADIUS = pixels(ThemeTokens.dark.radius);
const FONT_SIZE = pixels(ThemeTokens.dark.fontSize);

describe("numeric utilities are pixels", () => {
  test("--spacing is 1px, so gap-6 is 6px and h-40 is 40px", () => {
    const declared = /--spacing:\s*([^;]+);/.exec(THEME);
    expect(declared?.[1]?.trim(), "@theme inline must pin --spacing: 1px").toBe("1px");
    expect(CSS).not.toMatch(/--spacing:\s*0?\.25rem/);
  });

  test("every spacing token is its own pixel value", () => {
    expect(SPACING.size).toBeGreaterThan(0);
    for (const [step, value] of SPACING) expect(value, `space ${step}`).toBe(`${step}px`);
  });

  test("every radius token is its own pixel value", () => {
    expect([...RADIUS.keys()]).toEqual([2, 3, 4, 6, 8, 10, 12]);
    for (const [step, value] of RADIUS) expect(value, `radius ${step}`).toBe(`${step}px`);
  });

  test("the theme exposes every radius step as a Tailwind utility", () => {
    for (const step of RADIUS.keys()) expect(THEME).toContain(`--radius-${step}:`);
  });

  test("every rounded-<n> used in the source has a radius token", () => {
    const missing = [...new Set(USED_RADII)]
      .filter((step) => !RADIUS.has(step))
      .sort((a, b) => a - b);
    expect(missing, "rounded-<n> would emit no CSS").toEqual([]);
  });

  test("every text-<n> used in the source has a font-size token", () => {
    const missing = [...new Set(USED_TEXT)]
      .filter((step) => !FONT_SIZE.has(step))
      .sort((a, b) => a - b);
    expect(missing, "text-<n> would emit no CSS").toEqual([]);
  });
});
