import { describe, expect, test } from "vitest";
import { blockFor, declarations, readTokenCss } from "./test-support";
import { ThemeTokens, tokenVariables } from "./tokens";
import type { ThemeMode, ThemeTokens as ThemeTokenTable } from "./tokens";

const CSS = readTokenCss();

/**
 * Effective token values for a mode: `:root` holds the dark defaults, the
 * light block overrides the mode-dependent ones.
 */
function effectiveVariables(mode: ThemeMode): Map<string, string> {
  const merged = declarations(blockFor(CSS, ":root"));
  if (mode === "light") {
    for (const [name, value] of declarations(blockFor(CSS, '[data-theme="light"]'))) {
      merged.set(name, value);
    }
  }
  return merged;
}

function expectedVariables(tokens: ThemeTokenTable): Map<string, string> {
  return new Map(tokenVariables(tokens).map(({ name, value }) => [name, value]));
}

function differences(actual: Map<string, string>, expected: Map<string, string>): string[] {
  const rows: string[] = [];
  for (const [name, value] of expected) {
    const found = actual.get(name);
    if (found === undefined) {
      rows.push(`missing ${name}`);
    } else if (found.toLowerCase() !== value.toLowerCase()) {
      rows.push(`${name}: css ${found}, tokens ${value}`);
    }
  }
  for (const name of actual.keys()) {
    if (!expected.has(name)) {
      rows.push(`unexpected ${name}`);
    }
  }
  return rows;
}

describe("tokens.css mirrors the typed token table", () => {
  test.each<ThemeMode>(["dark", "light"])("%s values match ThemeTokens", (mode) => {
    expect(differences(effectiveVariables(mode), expectedVariables(ThemeTokens[mode]))).toEqual([]);
  });

  test("the light block only overrides variables that exist in :root", () => {
    const root = declarations(blockFor(CSS, ":root"));
    const light = declarations(blockFor(CSS, '[data-theme="light"]'));
    const unknown = [...light.keys()].filter((name) => !root.has(name));
    expect(unknown).toEqual([]);
    // Light overrides are exactly the colour tokens.
    expect([...light.keys()].every((name) => name.startsWith("--at-color-"))).toBe(true);
    expect(light.size).toBeGreaterThan(0);
  });

  test(":root paints dark before JavaScript runs", () => {
    const root = blockFor(CSS, ":root");
    expect(root).toContain("color-scheme: dark");
    expect(CSS).toMatch(/\[data-theme="light"\]\s*\{[^}]*color-scheme:\s*light/);
  });

  test("every custom property is exposed to Tailwind through @theme inline", () => {
    const theme = blockFor(CSS, "@theme inline");
    const missing = tokenVariables(ThemeTokens.dark)
      .map(({ name }) => name)
      .filter((name) => !theme.includes(`var(${name})`));
    expect(missing).toEqual([]);
  });

  test("the stylesheet contains no duplicate custom-property definitions in :root", () => {
    const raw = blockFor(CSS, ":root");
    const names = [...raw.matchAll(/(--at-[a-z0-9-]+)\s*:/gi)].map((match) => match[1]);
    expect([...new Set(names)]).toHaveLength(names.length);
  });
});
