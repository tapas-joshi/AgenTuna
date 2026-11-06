/**
 * Helpers shared by the theme tests.
 */

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

/** Returns every leaf path in `value`, using `.` as the separator. */
export function flattenKeys(value: object, prefix = ""): string[] {
  const keys: string[] = [];
  for (const [key, child] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (child !== null && typeof child === "object") {
      keys.push(...flattenKeys(child, path));
    } else {
      keys.push(path);
    }
  }
  return keys.sort();
}

function channel(value: number): number {
  const scaled = value / 255;
  return scaled <= 0.04045 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
}

/** Relative luminance of a `#rrggbb` colour, per WCAG 2.1. */
export function relativeLuminance(hex: string): number {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!match) {
    throw new Error(`expected a #rrggbb colour, got ${hex}`);
  }
  const [, red = "00", green = "00", blue = "00"] = match;
  return (
    0.2126 * channel(Number.parseInt(red, 16)) +
    0.7152 * channel(Number.parseInt(green, 16)) +
    0.0722 * channel(Number.parseInt(blue, 16))
  );
}

/** WCAG 2.1 contrast ratio between two `#rrggbb` colours. */
export function contrastRatio(foreground: string, background: string): number {
  const first = relativeLuminance(foreground);
  const second = relativeLuminance(background);
  const lighter = Math.max(first, second);
  const darker = Math.min(first, second);
  return (lighter + 0.05) / (darker + 0.05);
}

/**
 * Locates `ui/src` from `start`, whether the tests run from `ui/` or from the
 * repository root. jsdom gives `import.meta.url` a non-file scheme, so the
 * filesystem is the reliable anchor here.
 */
export function sourceRoot(start = process.cwd()): string {
  let directory = resolve(start);
  for (;;) {
    for (const candidate of [join(directory, "src"), join(directory, "ui", "src")]) {
      if (existsSync(join(candidate, "theme", "tokens.css"))) {
        return candidate;
      }
    }
    const parent = dirname(directory);
    if (parent === directory) {
      throw new Error(`could not locate ui/src above ${start}`);
    }
    directory = parent;
  }
}

/** Reads the stylesheet that mirrors the token table. */
export function readTokenCss(): string {
  return readFileSync(join(sourceRoot(), "theme", "tokens.css"), "utf8");
}

/** Removes comments so selectors can be matched reliably. */
export function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

function escapeSelector(selector: string): string {
  return selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Extracts the declaration block that follows `selector`.
 *
 * The selector must start a line, so `body` matches the standalone rule and
 * not the `html, body, #root` group.
 */
export function blockFor(css: string, selector: string): string {
  const source = stripComments(css);
  const pattern = new RegExp(`(?:^|\\n)[ \\t]*${escapeSelector(selector)}[ \\t]*\\{([^}]*)\\}`, "m");
  const match = pattern.exec(source);
  if (match?.[1] === undefined) {
    throw new Error(`selector ${selector} not found in tokens.css`);
  }
  return match[1];
}

/** Parses `--at-*: value;` declarations into a map. */
export function declarations(block: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const match of block.matchAll(/(--at-[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    const name = match[1];
    const value = match[2];
    if (name === undefined || value === undefined) {
      continue;
    }
    found.set(name, value.trim());
  }
  return found;
}
