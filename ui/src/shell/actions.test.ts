import { describe, expect, it } from "vitest";
import { ACTION_IDS, routeForAction, SHORTCUTS, shortcutHint } from "./actions";

describe("shared action registry", () => {
  it("uses the same stable action identifiers for palette, picker and native routes", () => {
    expect(ACTION_IDS.projectPicker).toBe("project-picker");
    expect(SHORTCUTS.find(({ key, modifier }) => key === "p" && modifier === "primary")?.action).toBe(ACTION_IDS.projectPicker);
    expect(SHORTCUTS.find(({ key, modifier }) => key === "k" && modifier === "primary")?.action).toBe(ACTION_IDS.palette);
    expect(routeForAction("navigate:overview")).toBe("overview");
    expect(routeForAction(ACTION_IDS.preferences)).toBe("settings");
    expect(routeForAction(ACTION_IDS.projectPicker)).toBeNull();
  });

  it("renders help hints from the registered shortcuts", () => {
    expect(shortcutHint(ACTION_IDS.projectPicker)).toBe("⌘P");
    expect(shortcutHint(ACTION_IDS.palette, "other")).toBe("Ctrl+K");
    expect(shortcutHint("overlay:close")).toBe("Escape");
  });

  it("maps every documented screen, scope and action shortcut exactly once", () => {
    const routeKeys = ["overview", "skills", "mcp-list", "effective", "drift", "instructions", "agents"] as const;
    for (const [index, route] of routeKeys.entries()) {
      const shortcut = SHORTCUTS.find((item) => item.action === `navigate:${route}`);
      expect(shortcut).toMatchObject({ key: String(index + 1), modifier: "primary" });
    }
    for (const [index, scope] of (["global", "user", "project", "local"] as const).entries()) {
      expect(SHORTCUTS.find((item) => item.action === `scope:${scope}`)).toMatchObject({ key: String(index + 1), modifier: "primary-shift" });
    }
    expect(SHORTCUTS.find((item) => item.action === ACTION_IDS.save)).toMatchObject({ key: "s", modifier: "primary" });
    expect(SHORTCUTS.find((item) => item.action === ACTION_IDS.addMenu)).toMatchObject({ key: "n", modifier: "primary" });
    expect(SHORTCUTS.find((item) => item.action === ACTION_IDS.preferences)).toMatchObject({ key: ",", modifier: "primary" });
    expect(SHORTCUTS.find((item) => item.action === "overlay:close")).toMatchObject({ key: "Escape", modifier: "none" });
    const scopeKeys = SHORTCUTS.filter((item) => item.action.startsWith("scope:")).map((item) => `${item.modifier}:${item.key}`);
    expect(new Set(scopeKeys).size).toBe(scopeKeys.length);
  });
});
