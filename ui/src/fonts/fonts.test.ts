import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, expect, test } from "vitest";

/** Resolves a path under `ui/src/fonts` whether tests start in `ui/` or the repo root. */
function fontPath(...segments: string[]): string {
  for (const base of [process.cwd(), resolve(process.cwd(), "..")]) {
    const path = join(base, "src", "fonts", ...segments);
    if (existsSync(path)) {
      return path;
    }
  }
  throw new Error(`ui/src/fonts/${segments.join("/")} not found from ${process.cwd()}`);
}

const CSS = readFileSync(fontPath("fonts.css"), "utf8");
const UI_ROOT = resolve(dirname(fontPath("fonts.css")), "..", "..");
const FACES = [...CSS.matchAll(/@font-face\s*\{([^}]*)\}/g)].map((match) => match[1] ?? "");

function face(family: string): string {
  const block = FACES.find((candidate) => candidate.includes(`font-family: "${family}"`));
  expect(block, `missing @font-face for ${family}`).toBeDefined();
  return block ?? "";
}

describe("bundled font declarations", () => {
  test("declares Inter for UI text and JetBrains Mono for code", () => {
    expect(FACES).toHaveLength(2);
    const inter = face("Inter");
    const mono = face("JetBrains Mono");

    expect(inter).toContain("font-weight: 100 900");
    expect(mono).toContain("font-weight: 100 800");
    for (const block of [inter, mono]) {
      expect(block).toContain("font-style: normal");
      expect(block).toContain("font-display: swap");
      expect(block).toContain('format("woff2")');
    }
  });

  test("loads only local woff2 files, never a remote URL", () => {
    const urls = [...CSS.matchAll(/url\("([^"]+)"\)/g)].map((match) => match[1] ?? "");
    expect(urls).toEqual([
      "./files/inter-latin-wght-normal.woff2",
      "./files/jetbrains-mono-latin-wght-normal.woff2",
    ]);
    expect(CSS).not.toMatch(/https?:/);
  });
});

describe("vendored font files", () => {
  test.each([
    "inter-latin-wght-normal.woff2",
    "jetbrains-mono-latin-wght-normal.woff2",
  ])("%s is a real woff2 file", (file) => {
    const bytes = readFileSync(fontPath("files", file));
    expect(bytes.subarray(0, 4).toString("ascii")).toBe("wOF2");
    expect(bytes.byteLength).toBeGreaterThan(10_000);
  });

  test("ships the SIL Open Font Licence for both families", () => {
    for (const file of ["LICENSE-Inter.txt", "LICENSE-JetBrainsMono.txt"]) {
      expect(readFileSync(fontPath("files", file), "utf8")).toContain("SIL Open Font License");
    }
  });

  test("vendored files still match the pinned fontsource release", () => {
    const pairs: ReadonlyArray<readonly [string, string]> = [
      ["@fontsource-variable/inter/files/inter-latin-wght-normal.woff2", "inter-latin-wght-normal.woff2"],
      [
        "@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2",
        "jetbrains-mono-latin-wght-normal.woff2",
      ],
    ];
    for (const [packagePath, vendored] of pairs) {
      const source = join(UI_ROOT, "node_modules", packagePath);
      expect(existsSync(source), `missing dependency file ${packagePath}`).toBe(true);
      expect(statSync(fontPath("files", vendored)).size).toBe(statSync(source).size);
    }
  });
});

describe("density and monospace wiring", () => {
  test("derives a one-device-pixel hairline from the pixel ratio", () => {
    expect(CSS).toContain("--at-pixel-ratio: 1");
    expect(CSS).toContain("--at-hairline: calc(1px / var(--at-pixel-ratio))");
  });

  test("code surfaces use the mono stack with tabular figures", () => {
    expect(CSS).toMatch(/code,\s*pre,\s*kbd,\s*samp\s*\{[^}]*font-family:\s*var\(--at-font-mono\)/);
    expect(CSS).toContain("font-variant-numeric: tabular-nums");
  });
});
