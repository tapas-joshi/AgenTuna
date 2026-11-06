import { afterEach, describe, expect, test, vi } from "vitest";
import { createAppState, type AppEffect } from "./AppState";

afterEach(() => vi.useRealTimers());

function reloadEffects(effects: AppEffect[]) {
  return effects.filter((effect) => effect.type === "reload-route");
}

describe("AppState", () => {
  test("switching project clears project-bound views and emits one reload", async () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    state.setScreenState("project:skills", { rows: [1] });
    state.setSelection("project:skills", ["item-1"]);
    await state.selectProject("project-1");
    expect(state.getSnapshot().screenState).toEqual({});
    expect(state.getSnapshot().selections).toEqual({});
    expect(reloadEffects(effects)).toEqual([{ type: "reload-route", route: "overview", reason: "project" }]);
    expect(effects).toContainEqual({ type: "persist-last-project", projectId: "project-1" });
  });

  test("a scope event reloads once and duplicate current scope events do nothing", () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    state.receiveScope("project");
    state.receiveScope("project");
    expect(state.getSnapshot().scope).toBe("project");
    expect(reloadEffects(effects)).toEqual([{ type: "reload-route", route: "overview", reason: "scope" }]);
  });

  test("cancel preserves dirty route and editor buffer; discard clears it", () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    state.setEditorBuffer("overview", { route: "overview", value: "edited", savedValue: "saved" });
    expect(state.navigate("skills")).toBe(false);
    expect(state.getSnapshot().route).toBe("overview");
    expect(state.getSnapshot().editors.overview?.value).toBe("edited");
    expect(effects.at(-1)).toEqual({ type: "dirty-navigation", from: "overview", to: "skills" });
    expect(state.navigate("skills", "discard")).toBe(true);
    expect(state.getSnapshot().route).toBe("skills");
    expect(state.getSnapshot().editors.overview).toBeUndefined();
  });

  test("a burst of external changes produces one trailing-edge reload", () => {
    vi.useFakeTimers();
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    for (let index = 0; index < 200; index += 1) state.requestExternalReload();
    vi.advanceTimersByTime(100);
    expect(reloadEffects(effects)).toEqual([{ type: "reload-route", route: "overview", reason: "external" }]);
  });

  test("external changes protect dirty editors until explicitly resolved", () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    state.setEditorBuffer("overview", { route: "overview", value: "mine", savedValue: "old" });
    state.markExternalChange("overview", "new-hash");
    expect(state.getSnapshot().externalChanges.overview).toEqual({ contentHash: "new-hash", kept: false });
    state.resolveExternalChange("overview", "keep");
    expect(state.getSnapshot().editors.overview?.value).toBe("mine");
    expect(state.getSnapshot().externalChanges.overview?.kept).toBe(true);
    state.resolveExternalChange("overview", "reload", "theirs");
    expect(state.getSnapshot().editors.overview).toEqual({
      route: "overview",
      value: "theirs",
      savedValue: "theirs",
    });
    expect(state.getSnapshot().externalChanges.overview).toBeUndefined();
  });

  test("tracks route changes and background job handles", () => {
    const state = createAppState();
    for (const route of ["skills", "mcp-list", "instructions", "agents", "effective", "drift"] as const) {
      state.navigate(route);
    }
    state.setJob("job-1", true);
    expect(state.getSnapshot().routeStack.at(-1)).toBe("drift");
    expect(state.getSnapshot().backgroundJobs).toEqual(["job-1"]);
    state.setJob("job-1", false);
    expect(state.getSnapshot().backgroundJobs).toEqual([]);
  });
});
