import { describe, expect, test } from "vitest";
import { contrastRatio } from "./test-support";
import { ThemeTokens } from "./tokens";
import type { ThemeMode, ThemeTokens as ThemeTokenTable } from "./tokens";

/** WCAG 2.1 AA threshold for normal-size body text. */
const MIN_TEXT_CONTRAST = 4.5;

const MODES: ThemeMode[] = ["dark", "light"];

/** Surfaces that can sit behind text. */
function surfaces(tokens: ThemeTokenTable): Array<[string, string]> {
  return [
    ["surface", tokens.color.surface],
    ["raised", tokens.color.raised],
    ["overlay", tokens.color.overlay],
    ["sidebar", tokens.color.sidebar],
  ];
}

/** Every token that is allowed to render as text. */
function inks(tokens: ThemeTokenTable): Array<[string, string]> {
  return [
    ["text.primary", tokens.color.text.primary],
    ["text.secondary", tokens.color.text.secondary],
    ["text.muted", tokens.color.text.muted],
    ["accent", tokens.color.accent],
    ["success", tokens.color.success],
    ["warning", tokens.color.warning],
    ["danger", tokens.color.danger],
    ["scope.global", tokens.color.scope.global],
    ["scope.user", tokens.color.scope.user],
    ["scope.project", tokens.color.scope.project],
    ["scope.local", tokens.color.scope.local],
  ];
}

function report(mode: ThemeMode): string {
  const tokens = ThemeTokens[mode];
  const rows: string[] = [];
  for (const [inkName, ink] of inks(tokens)) {
    for (const [surfaceName, surface] of surfaces(tokens)) {
      const ratio = contrastRatio(ink, surface);
      if (ratio < MIN_TEXT_CONTRAST) {
        rows.push(`${mode}: ${inkName} ${ink} on ${surfaceName} ${surface} = ${ratio.toFixed(2)}:1`);
      }
    }
  }
  return rows.join("\n");
}

describe("text contrast", () => {
  test.each(MODES)("%s: every text-on-surface pair reaches 4.5:1", (mode) => {
    expect(report(mode)).toBe("");
  });

  test("the contrast helper agrees with the WCAG reference values", () => {
    expect(contrastRatio("#000000", "#FFFFFF")).toBeCloseTo(21, 5);
    expect(contrastRatio("#FFFFFF", "#FFFFFF")).toBeCloseTo(1, 5);
    // #767676 is the canonical lightest grey passing 4.5:1 on white.
    expect(contrastRatio("#767676", "#FFFFFF")).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio("#777777", "#FFFFFF")).toBeLessThan(4.5);
  });

  test.each(MODES)("%s: the focus ring stands out from every surface", (mode) => {
    const tokens = ThemeTokens[mode];
    for (const [surfaceName, surface] of surfaces(tokens)) {
      // WCAG 1.4.11: non-text UI indicators need 3:1.
      expect(
        contrastRatio(tokens.color.focus, surface),
        `${mode}: focus on ${surfaceName}`,
      ).toBeGreaterThanOrEqual(3);
    }
  });
});
