import { describe, expect, test } from "vitest";
import { blockFor, readTokenCss } from "./test-support";

/**
 * Automated guards for the manual zoom/clipping check documented in
 * `MANUAL-CHECKS.md`. jsdom cannot lay out text, so these tests pin the
 * structural rules that keep labels from being clipped when the UI is scaled.
 */

const CSS = readTokenCss();

describe("base styles keep text unclipped", () => {
  test("body text has a unitless line-height of at least 1.3", () => {
    const body = blockFor(CSS, "body");
    const match = /line-height:\s*([\d.]+)\s*;/.exec(body);
    expect(match, body).not.toBeNull();
    expect(Number(match?.[1])).toBeGreaterThanOrEqual(1.3);
  });

  test("body type comes from the token scale, not a literal", () => {
    expect(blockFor(CSS, "body")).toMatch(/font-size:\s*var\(--at-text-\d+\)/);
  });

  test("the app fills the window without globally hidden overflow", () => {
    expect(CSS).toMatch(/html,\s*\n?\s*body,\s*\n?\s*#root\s*\{[^}]*height:\s*100%/);
    expect(CSS).not.toMatch(/overflow\s*:\s*hidden/);
  });

  test("focus outlines use the focus token and never rely on colour alone", () => {
    expect(blockFor(CSS, ":focus-visible")).toMatch(/outline:[^;]*var\(--at-color-focus\)/);
  });
});
