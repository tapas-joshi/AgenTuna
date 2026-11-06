import { beforeEach, describe, expect, test } from "vitest";
import {
  applyTheme,
  DEFAULT_THEME_MODE,
  getThemeTokens,
  ThemeTokens,
  tokenVariables,
} from "./tokens";

beforeEach(() => {
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.removeAttribute("style");
});

describe("applyTheme", () => {
  test("dark is the default mode", () => {
    expect(DEFAULT_THEME_MODE).toBe("dark");
    expect(getThemeTokens(DEFAULT_THEME_MODE)).toBe(ThemeTokens.dark);
  });

  test("writes every light token to the root element", () => {
    applyTheme(document.documentElement, "light");

    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    for (const { name, value } of tokenVariables(ThemeTokens.light)) {
      expect(document.documentElement.style.getPropertyValue(name), name).toBe(value);
    }
  });

  test("switching back to dark restores every value without restarting", () => {
    applyTheme(document.documentElement, "light");
    applyTheme(document.documentElement, "dark");

    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    for (const { name, value } of tokenVariables(ThemeTokens.dark)) {
      expect(document.documentElement.style.getPropertyValue(name), name).toBe(value);
    }
    expect(
      document.documentElement.style.getPropertyValue("--at-color-surface"),
    ).toBe(ThemeTokens.dark.color.surface);
  });

  test("a switch leaves no stale variables behind", () => {
    applyTheme(document.documentElement, "light");
    applyTheme(document.documentElement, "dark");

    const names = new Set(tokenVariables(ThemeTokens.dark).map(({ name }) => name));
    const inline = document.documentElement.getAttribute("style") ?? "";
    const written = [...inline.matchAll(/(--at-[a-z0-9-]+)\s*:/gi)].map((match) => match[1]);
    expect(written.filter((name) => name !== undefined && !names.has(name))).toEqual([]);
  });

  test("can theme an isolated subtree root (portals, overlays)", () => {
    const host = document.createElement("div");
    applyTheme(host, "light");

    expect(host.getAttribute("data-theme")).toBe("light");
    expect(host.style.getPropertyValue("--at-color-raised")).toBe(
      ThemeTokens.light.color.raised,
    );
    // The document root is untouched by a subtree call.
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });

  test("re-applying the same mode is idempotent", () => {
    applyTheme(document.documentElement, "light");
    const first = document.documentElement.getAttribute("style");
    applyTheme(document.documentElement, "light");
    expect(document.documentElement.getAttribute("style")).toBe(first);
  });

  test("token sets are mode-complete", () => {
    // A mode switch must not leave a property undefined: every variable gets
    // a value in both tables.
    const names = tokenVariables(ThemeTokens.dark).map(({ name }) => name);
    const lightNames = tokenVariables(ThemeTokens.light).map(({ name }) => name);
    expect(lightNames).toEqual(names);
    expect(names.length).toBeGreaterThan(0);
  });
});
