import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, test } from "vitest";
import { TOAST_KINDS } from "./types";

/** Reads the shipped stylesheet from `ui/` whether tests start there or at the repo root. */
function readStylesheet(): string {
  for (const base of [process.cwd(), resolve(process.cwd(), "..")]) {
    const path = join(base, "src", "overlays", "toasts", "toasts.css");
    if (existsSync(path)) {
      return readFileSync(path, "utf8");
    }
  }
  throw new Error(`toasts.css not found from ${process.cwd()}`);
}

const CSS = readStylesheet();

function ruleFor(selector: string): string {
  const start = CSS.indexOf(`${selector} {`);
  expect(start, `missing rule ${selector}`).toBeGreaterThanOrEqual(0);
  return CSS.slice(start, CSS.indexOf("}", start) + 1);
}

describe("toast stylesheet", () => {
  test("pins the stack to the bottom-right, above the status bar", () => {
    const container = ruleFor(".at-toasts");
    expect(container).toContain("position: fixed");
    expect(container).toContain("inset-block-end");
    expect(container).toContain("--at-space-32");
    expect(container).toContain("--at-status-bar-height");
    expect(container).toContain("inset-inline-end");
    expect(container).toContain("flex-direction: column");
    expect(container).toMatch(/z-index:\s*\d+/);
  });

  test("the container lets clicks through and only cards take input", () => {
    expect(ruleFor(".at-toasts")).toContain("pointer-events: none");
    expect(ruleFor(".at-toasts__item")).toContain("pointer-events: auto");
  });

  test("live regions are visually hidden but never display:none", () => {
    const live = ruleFor(".at-toasts__live");
    expect(live).toContain("clip-path: inset(50%)");
    expect(live).not.toContain("display: none");
  });

  test("each kind has a text label and its own accent border", () => {
    for (const kind of TOAST_KINDS) {
      expect(CSS).toContain(`[data-kind="${kind}"]`);
    }
    expect(ruleFor(".at-toasts__kind")).toContain("text-transform: uppercase");
    expect(CSS).toContain("var(--at-color-success)");
    expect(CSS).toContain("var(--at-color-warning)");
    expect(CSS).toContain("var(--at-color-danger)");
  });

  test("uses theme tokens instead of raw colours and font sizes", () => {
    expect(CSS).not.toMatch(/#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})\b/i);
    const fontSizes = CSS.split("\n")
      .map((line) => line.trim())
      .filter((line) => line.startsWith("font-size"))
      .map((line) => line.slice(line.indexOf(":") + 1, -1).trim());
    expect(fontSizes.length).toBeGreaterThan(0);
    for (const value of fontSizes) {
      expect(value?.trim().startsWith("var(--at-text-")).toBe(true);
    }
  });

  test("motion is optional", () => {
    expect(CSS).toContain("@media (prefers-reduced-motion: reduce)");
    expect(CSS).toContain("animation: none");
  });
});
