import { readdirSync, readFileSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";
import { describe, expect, test } from "vitest";
import { sourceRoot } from "./test-support";

/**
 * Enforces the TU-001 rule that `ui/src/theme` is the only place raw colours
 * and font sizes may appear. Any hit outside that directory fails the suite.
 */

const SOURCE_ROOT = sourceRoot();
const THEME_ROOT = join(SOURCE_ROOT, "theme");
const SCANNED_EXTENSIONS: Record<string, true> = {
  ".ts": true,
  ".tsx": true,
  ".css": true,
  ".js": true,
  ".jsx": true,
  ".html": true,
  ".svg": true,
};

interface Rule {
  readonly id: string;
  readonly pattern: RegExp;
  readonly hint: string;
}

const RULES: readonly Rule[] = [
  {
    id: "raw colour",
    pattern: /#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})\b/gi,
    hint: "use a theme token (bg-surface, text-text-muted, var(--at-color-…))",
  },
  {
    id: "font-size literal",
    pattern: /font-size\s*:\s*(?!var\()[^;}\n]+/gi,
    hint: "use a type token (text-13, var(--at-text-…))",
  },
  {
    id: "Tailwind arbitrary font size",
    pattern: /text-\[[^\]\n]*(?:px|rem|em|pt|%)[^\]\n]*\]/g,
    hint: "use a type token class such as text-13",
  },
  {
    id: "inline font size",
    pattern: /\bfontSize\s*[:=]\s*(?:["'`]\s*)?-?\d+(?:\.\d+)?\s*(?:px|rem|em|pt)?\b/g,
    hint: "use a type token via the theme table",
  },
];

/** Returns `file:line: rule: match` rows for every violation in `text`. */
export function violationsIn(relativePath: string, text: string): string[] {
  const rows: string[] = [];
  for (const rule of RULES) {
    for (const match of text.matchAll(rule.pattern)) {
      const line = text.slice(0, match.index).split("\n").length;
      rows.push(`${relativePath}:${line}: ${rule.id} "${match[0].trim()}" (${rule.hint})`);
    }
  }
  return rows;
}

function sourceFiles(directory: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const full = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (resolve(full) === THEME_ROOT) {
        continue;
      }
      files.push(...sourceFiles(full));
    } else if (SCANNED_EXTENSIONS[extname(entry.name)] === true) {
      files.push(full);
    }
  }
  return files;
}

describe("no raw colours or font sizes outside ui/src/theme", () => {
  test("the detector catches raw values and allows token references", () => {
    const relativePath = "ui/src/fixture.tsx";
    expect(violationsIn(relativePath, 'const ink = "#0f172a";')).toHaveLength(1);
    expect(violationsIn(relativePath, '<p className="text-[26px]" />')).toHaveLength(1);
    expect(violationsIn(relativePath, "<p style={{ fontSize: '13px' }} />")).toHaveLength(1);
    expect(violationsIn(relativePath, "<p style={{ fontSize: 13 }} />")).toHaveLength(1);
    expect(violationsIn(relativePath, "p { font-size: 14px; }")).toHaveLength(1);
    expect(
      violationsIn(
        relativePath,
        '<p className="text-13 text-text-muted" style={{ color: "var(--at-color-text)" }} />',
      ),
    ).toEqual([]);
  });

  test("ui/src outside ui/src/theme contains no raw values", () => {
    const violations = sourceFiles(SOURCE_ROOT).flatMap((file) =>
      violationsIn(relative(SOURCE_ROOT, file), readFileSync(file, "utf8")),
    );
    expect(violations, violations.join("\n")).toEqual([]);
  });

  test("the scan actually sees the app sources", () => {
    const files = sourceFiles(SOURCE_ROOT);
    expect(files.some((file) => file.endsWith("main.tsx"))).toBe(true);
    expect(files.some((file) => file.endsWith("App.tsx"))).toBe(true);
  });
});
