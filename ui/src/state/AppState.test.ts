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
  test("back pops one route and requests a dirty-navigation decision", () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    state.navigate("skills");
    state.navigate("mcp-list");
    state.setEditorBuffer("mcp-list", { route: "mcp-list", value: "edited", savedValue: "saved" });
    expect(state.back()).toBe(false);
    expect(state.getSnapshot().route).toBe("mcp-list");
    expect(effects.at(-1)).toEqual({ type: "dirty-navigation", from: "mcp-list", to: "skills" });
    expect(state.back("discard")).toBe(true);
    expect(state.getSnapshot().route).toBe("skills");
    expect(state.getSnapshot().routeStack).toEqual(["overview", "skills"]);
  });
  test("Save invokes the active buffer handler before leaving", async () => {
    const save = vi.fn(async () => undefined);
    const state = createAppState();
    state.setEditorBuffer("overview", {
      route: "overview",
      value: "updated",
      savedValue: "old",
      onSave: save,
    });
    expect(state.navigate("skills")).toBe(false);
    expect(state.navigate("skills", "save")).toBe(false);
    await vi.waitFor(() => expect(state.getSnapshot().route).toBe("skills"));
    expect(save).toHaveBeenCalledOnce();
  });
  test("restores project and scope before the app shell is shown", () => {
    const state = createAppState();
    state.restore("project-42", "project");
    expect(state.getSnapshot().activeProject).toBe("project-42");
    expect(state.getSnapshot().scope).toBe("project");
    expect(state.getSnapshot().route).toBe("overview");
  });
  test("persists changed scope for active project", async () => {
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    await state.selectProject("project-7");
    state.receiveScope("local");
    expect(effects).toContainEqual({ type: "persist-last-scope", projectId: "project-7", scope: "local" });
    expect(state.getSnapshot().route).toBe("effective");
  });
  test("item deep links route by kind and unknown items toast without navigation", () => {
    const state = createAppState();
    state.openItem("itm_bad", null);
    expect(state.getSnapshot().route).toBe("overview");
    expect(state.toasts.getSnapshot().visible[0]?.message).toBe("Item not found");
    state.openItem("itm_mcp", { kind: "mcp_server", scope: "user", project_id: null });
    expect(state.getSnapshot().route).toBe("mcp-detail");
    expect(state.getSnapshot().screenState["detail:itemId"]).toBe("itm_mcp");
    state.openItem("itm_missing", null);
    expect(state.getSnapshot().route).toBe("mcp-detail");
    expect(state.getSnapshot().screenState["detail:itemId"]).toBe("itm_mcp");
  });
  test("visited items are deduplicated, newest-first and bounded to eight", () => {
    const state = createAppState();
    for (let index = 0; index < 10; index += 1) state.recordVisitedItem(`skill:${index}`);
    state.recordVisitedItem("skill:5");
    expect(state.getSnapshot().recentItemIds).toEqual([
      "skill:5", "skill:9", "skill:8", "skill:7", "skill:6", "skill:4", "skill:3", "skill:2",
    ]);
  });

  test("a burst of external changes produces one trailing-edge reload", () => {
    vi.useFakeTimers();
    const effects: AppEffect[] = [];
    const state = createAppState({ effects: (effect) => effects.push(effect) });
    for (let index = 0; index < 200; index += 1) state.requestExternalReload();
    vi.advanceTimersByTime(100);
    expect(reloadEffects(effects)).toEqual([{ type: "reload-route", route: "overview", reason: "external" }]);
    expect(state.getSnapshot().externalRefreshVersion).toBe(1);
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
    state.restore("project-1", "user");
    for (const route of ["skills", "mcp-list", "instructions", "agents", "effective", "drift"] as const) {
      state.navigate(route);
    }
    state.setJob("job-1", true);
    expect(state.getSnapshot().routeStack.at(-1)).toBe("drift");
    expect(state.getSnapshot().backgroundJobs).toEqual(["job-1"]);
    state.setJob("job-1", false);
    expect(state.getSnapshot().backgroundJobs).toEqual([]);
  });
  test("route history is bounded to the newest 50 entries", () => {
    const state = createAppState();
    state.restore("project-1", "user");
    const routes = ["overview", "skills", "mcp-list", "agents", "settings"] as const;
    for (let index = 0; index < 60; index += 1) {
      const route = routes[index % routes.length];
      if (route) state.navigate(route);
    }
    expect(state.getSnapshot().routeStack).toHaveLength(50);
    expect(state.getSnapshot().routeStack.at(-1)).toBe(state.getSnapshot().route);
  });
});
