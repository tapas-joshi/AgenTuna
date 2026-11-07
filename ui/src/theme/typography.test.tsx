import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";
import { Chip } from "../components/chip";
import { AgentCell } from "../components/table/agent-cell";
import { ThemeTokens } from "./tokens";
import { blockFor, readTokenCss } from "./test-support";

/**
 * TU-024 typography guards.
 *
 * jsdom cannot lay text out, so the scalable parts are pinned structurally: the
 * fallback order inside both stacks, the Tailwind aliases that make `font-mono`
 * resolve, the mono stack and tabular figures on editor, table and count
 * surfaces, and the shorthands the browser needs to render glyphs unclipped.
 * Pixel-level clipping at 1x, 1.5x and 2x is asserted by
 * `ui/e2e/typography.visual.spec.ts`.
 */

function sourceFile(...segments: string[]): string {
  for (const base of [process.cwd(), resolve(process.cwd(), "..")]) {
    const path = join(base, "src", ...segments);
    if (existsSync(path)) {
      return readFileSync(path, "utf8");
    }
  }
  throw new Error(`ui/src/${segments.join("/")} not found from ${process.cwd()}`);
}

const CSS = readTokenCss();

afterEach(cleanup);

describe("font stacks", () => {
  test.each(["dark", "light"] as const)(
    "%s mode keeps bundled families first and generic fallbacks last",
    (mode) => {
      const { ui, mono } = ThemeTokens[mode].font;
      let previous = -1;
      for (const family of ['"Inter"', '"Helvetica Neue"', "system-ui", "-apple-system", "sans-serif"]) {
        const index = ui.indexOf(family);
        expect(index, `${family} is in the UI stack: ${ui}`).toBeGreaterThan(previous);
        previous = index;
      }
      expect(ui.trimEnd().endsWith("sans-serif")).toBe(true);
      previous = -1;
      for (const family of ['"JetBrains Mono"', '"Menlo"', "ui-monospace", '"SFMono-Regular"', ", monospace"]) {
        const index = mono.indexOf(family);
        expect(index, `${family} is in the mono stack: ${mono}`).toBeGreaterThan(previous);
        previous = index;
      }
      expect(mono.trimEnd().endsWith("monospace")).toBe(true);
    },
  );

  test("Tailwind aliases resolve the token stacks for font-ui and font-mono", () => {
    expect(CSS).toContain("--font-ui: var(--at-font-ui);");
    expect(CSS).toContain("--font-mono: var(--at-font-mono);");
    expect(blockFor(CSS, "body")).toContain("font-family: var(--at-font-ui);");
  });
});

describe("monospace and tabular rendering", () => {
  test("native table paths render in the mono stack", () => {
    const { container } = render(
      <AgentCell
        agent={{ id: "claude-code", label: "Claude Code" }}
        state={{ kind: "native", path: "/sandbox/home/.claude/skills", readOnly: false }}
      />,
    );
    const path = container.querySelector("[data-slot='agent-path']");
    expect(path?.textContent).toBe("/sandbox/home/.claude/skills");
    expect(path?.className).toContain("font-mono");
  });

  test("count figures use tabular numerals so columns keep their width", () => {
    const { container } = render(
      <Chip pressed={false} onPressedChange={() => undefined} count={128}>
        Agents
      </Chip>,
    );
    expect(container.textContent).toContain("128");
    const count = container.querySelector(".tabular-nums");
    expect(count?.textContent).toBe("128");
  });

  test("the editor pins the mono stack and a fixed line height", () => {
    const theme = sourceFile("components", "editor", "cm-theme.ts");
    const match = /"\.cm-scroller":\s*\{([^}]*)\}/.exec(theme);
    expect(match?.[1], "cm-scroller theme block").toBeDefined();
    expect(match?.[1]).toContain('fontFamily: "var(--at-font-mono)"');
    expect(match?.[1]).toContain('lineHeight: "1.5"');
  });

  test("code surfaces request tabular figures explicitly", () => {
    const fonts = sourceFile("fonts", "fonts.css");
    expect(fonts).toMatch(/code,\s*pre,\s*kbd,\s*samp\s*\{[^}]*font-variant-numeric:\s*tabular-nums/);
  });
});
