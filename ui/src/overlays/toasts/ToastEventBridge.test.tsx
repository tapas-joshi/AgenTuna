import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { createToastStore, ToastProvider } from ".";
import { createFakeClock } from "./testing";
import { ToastEventBridge } from "./ToastEventBridge";

const eventBus = vi.hoisted(() => ({ listeners: new Map<string, (message: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, listener: (message: { payload: unknown }) => void) => {
    eventBus.listeners.set(event, listener);
    return () => eventBus.listeners.delete(event);
  },
}));

afterEach(() => { cleanup(); eventBus.listeners.clear(); vi.restoreAllMocks(); });

const notice = (event_id: number, toast_eligible: boolean) => ({
  event_id,
  kind: "drifted",
  item_id: "itm_shared_skill",
  agent: "codex",
  scope: "project",
  path: "/sandbox/project/.agents/skills/review/SKILL.md",
  expected_hash: "baseline",
  actual_hash: "changed",
  link_target: null,
  changed_file_count: 1,
  toast_eligible,
});

describe("ToastEventBridge", () => {
  test("skips non-toast watcher events and collapses one item for ten seconds into one Review action", async () => {
    const app = createAppState();
    const currentTime = { value: 1_000 };
    vi.spyOn(Date, "now").mockImplementation(() => currentTime.value);
    const clock = createFakeClock();
    const store = createToastStore({ clock });
    render(
      <AppStateProvider state={app}>
        <ToastProvider store={store}><ToastEventBridge /></ToastProvider>
      </AppStateProvider>,
    );
    await waitFor(() => expect(eventBus.listeners.has("agentuna:drift-detected")).toBe(true));
    const emit = (items: unknown[]) => eventBus.listeners.get("agentuna:drift-detected")?.({ payload: items });

    emit([notice(1, false)]);
    expect(store.getSnapshot().visible).toHaveLength(0);
    emit([notice(1, true)]);
    currentTime.value += 9_999;
    emit([notice(2, true)]);
    expect(store.getSnapshot().visible).toHaveLength(1);
    expect(await screen.findByText("review changed outside AgenTuna.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Review" })).toBeTruthy();
    expect(store.getSnapshot().visible[0]?.count).toBe(1);

    const action = store.getSnapshot().visible[0]?.action;
    action?.run();
    expect(app.getSnapshot().route).toBe("drift");
    expect(app.getSnapshot().selections.drift).toEqual(["1"]);

  });
});
