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
});
