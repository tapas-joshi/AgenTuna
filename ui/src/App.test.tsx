import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "./App";

// A plain function, not vi.fn(): the spy wrapper reports rejected results as unhandled.
const tauriEvents = vi.hoisted(() => ({ handlers: new Map<string, (event: { payload: unknown }) => void>() }));
const ipc = vi.hoisted(() => ({
  calls: [] as string[],
  reply: (_cmd: string): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    ipc.calls.push(cmd);
    return ipc.reply(cmd);
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (event: { payload: unknown }) => void) => {
    tauriEvents.handlers.set(event, handler);
    return Promise.resolve(() => { tauriEvents.handlers.delete(event); });
  },
}));

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
  tauriEvents.handlers.clear();
});

test("shows name and version returned by the app_info command", async () => {
  ipc.reply = (cmd) =>
    Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "1.2.3" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "remote_backup_settings" ? { device_name: "Visual workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false } :
    { values: {} },
  );
  render(<App />);
  expect(await screen.findByText("v1.2.3 · local")).toBeTruthy();
  // Startup restores settings and project state and checks for pending recovery.
  expect(new Set(ipc.calls)).toEqual(new Set(["app_info", "settings_load", "projects_list", "overview_load", "recovery_sheet"]));
});

test("shows the backend error instead of a blank screen", async () => {
  ipc.reply = () => Promise.reject("boom");
  render(<App />);
  expect(await screen.findByText("Backend error: boom")).toBeTruthy();
});
test("native Preferences menu action is handed to the UI dispatcher", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "remote_backup_settings" ? { device_name: "Visual workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false } :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "preferences" }));
  expect(await screen.findByRole("heading", { name: "Preferences" })).toBeTruthy();
});
