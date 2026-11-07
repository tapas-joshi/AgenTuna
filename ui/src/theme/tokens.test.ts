import { describe, expect, test } from "vitest";
import { flattenKeys } from "./test-support";
import { ThemeTokens, tokenVariables } from "./tokens";
import type { ThemeMode, ThemeTokens as ThemeTokenTable } from "./tokens";
import { applyTheme as publicApplyTheme } from "./index";
import type { ThemeMode as PublicThemeMode, ThemeTokens as PublicThemeTokenTable } from "./index";

const MODES: ThemeMode[] = ["dark", "light"];

/** Token paths required by TU-001, independent of the design extras. */
const REQUIRED_PATHS = [
  "color.surface",
  "color.raised",
  "color.overlay",
  "color.border",
  "color.text.primary",
  "color.text.secondary",
  "color.text.muted",
  "color.accent",
  "color.accentFill",
  "color.onAccent",
  "color.success",
  "color.warning",
  "color.danger",
  "color.focus",
  "space.2",
  "space.4",
  "space.8",
  "space.12",
  "space.16",
  "space.24",
  "space.32",
  "radius.3",
  "radius.6",
  "radius.10",
  "fontSize.11",
  "fontSize.12",
  "fontSize.13",
  "fontSize.15",
  "fontSize.18",
  "fontSize.22",
  "font.ui",
  "font.mono",
];

function assertNonEmptyStrings(value: object, prefix = ""): void {
  for (const [key, child] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (child !== null && typeof child === "object") {
      assertNonEmptyStrings(child, path);
    } else {
      expect(typeof child, path).toBe("string");
      expect((child as string).trim(), path).not.toBe("");
    }
  }
}

describe("ThemeTokens", () => {
  test("dark and light expose identical key sets", () => {
    const darkKeys = flattenKeys(ThemeTokens.dark);
    const lightKeys = flattenKeys(ThemeTokens.light);
    expect(lightKeys).toEqual(darkKeys);
    // Guard against an empty slice on both sides looking "equal".
    expect(darkKeys.length).toBeGreaterThan(0);
  });

  test("every token in either mode is a non-empty string", () => {
    for (const mode of MODES) {
      assertNonEmptyStrings(ThemeTokens[mode]);
    }
  });

  test("every token required by TU-001 exists in both modes", () => {
    for (const mode of MODES) {
      const keys = new Set(flattenKeys(ThemeTokens[mode]));
      expect([...REQUIRED_PATHS].filter((path) => !keys.has(path)), mode).toEqual([]);
    }
  });

  test("colours are #rrggbb and lengths are pixel values", () => {
    for (const mode of MODES) {
      const tokens = ThemeTokens[mode];
      for (const color of Object.values(tokens.color).flatMap((value) =>
        typeof value === "object" ? Object.values(value) : [value],
      )) {
        expect(color, mode).toMatch(/^#[0-9a-f]{6}$/i);
      }
      for (const size of Object.values(tokens.space)) {
        expect(size, mode).toMatch(/^\d+px$/);
      }
      for (const radius of Object.values(tokens.radius)) {
        expect(radius, mode).toMatch(/^\d+px$/);
      }
      for (const size of Object.values(tokens.fontSize)) {
        expect(size, mode).toMatch(/^\d+px$/);
      }
    }
  });

  test("fonts name Inter for UI and Menlo for code", () => {
    for (const mode of MODES) {
      expect(ThemeTokens[mode].font.ui).toContain("Inter");
      expect(ThemeTokens[mode].font.mono).toContain("Menlo");
    }
  });

  test("maps every token to a unique --at-* custom property", () => {
    for (const mode of MODES) {
      const variables = tokenVariables(ThemeTokens[mode]);
      expect(variables).toHaveLength(flattenKeys(ThemeTokens[mode]).length);
      expect([...new Set(variables.map((variable) => variable.name))]).toHaveLength(variables.length);
      expect(variables.every((variable) => variable.name.startsWith("--at-"))).toBe(true);
      expect(variables.every((variable) => variable.value !== "")).toBe(true);
    }
    // Spot-check the naming rules other lanes rely on.
    const names = tokenVariables(ThemeTokens.dark).map((variable) => variable.name);
    expect(names).toContain("--at-color-text");
    expect(names).toContain("--at-color-text-muted");
    expect(names).toContain("--at-color-scope-project");
    expect(names).toContain("--at-space-16");
    expect(names).toContain("--at-radius-10");
    expect(names).toContain("--at-text-13");
    expect(names).toContain("--at-font-mono");
  });

  test("dark and light differ only where a mode override is expected", () => {
    const dark = ThemeTokens.dark;
    const light = ThemeTokens.light;
    expect(dark.color.surface).not.toBe(light.color.surface);
    expect(dark.color.text.primary).not.toBe(light.color.text.primary);
    // Type, spacing and radii are mode-independent.
    expect(light.space).toEqual(dark.space);
    expect(light.radius).toEqual(dark.radius);
    expect(light.fontSize).toEqual(dark.fontSize);
    expect(light.font).toEqual(dark.font);
  });

  test("typed access is stable for consumers", () => {
    const table: ThemeTokenTable = ThemeTokens.dark;
    expect(table.color.text.primary).toBe(ThemeTokens.dark.color.text.primary);
    expect(ThemeTokens.light.color.scope.user).toMatch(/^#[0-9a-f]{6}$/i);
  });

  test("the public barrel exports the same typed API", () => {
    const mode: PublicThemeMode = "light";
    const table: PublicThemeTokenTable = ThemeTokens[mode];
    expect(table.color.surface).toBe(ThemeTokens.light.color.surface);
    expect(publicApplyTheme).toBeTypeOf("function");
  });
});
