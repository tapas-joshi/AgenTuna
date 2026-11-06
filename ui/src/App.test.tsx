import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "./App";

// A plain function, not vi.fn(): the spy wrapper reports rejected results as unhandled.
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
  // Outside a Tauri runtime `listen` rejects; the app must carry on without native events.
  listen: () => Promise.reject(new Error("no tauri runtime")),
}));

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

test("shows name and version returned by the app_info command", async () => {
  ipc.reply = (cmd) =>
    Promise.resolve(
      cmd === "app_info" ? { name: "AgenTuna", version: "1.2.3" } : cmd === "projects_list" ? [] : { values: {} },
    );
  render(<App />);
  expect(await screen.findByText("v1.2.3 · local")).toBeTruthy();
  // Startup reads the app info and restores persisted settings and projects; nothing writes.
  expect(new Set(ipc.calls)).toEqual(new Set(["app_info", "settings_load", "projects_list"]));
});

test("shows the backend error instead of a blank screen", async () => {
  ipc.reply = () => Promise.reject("boom");
  render(<App />);
  expect(await screen.findByText("Backend error: boom")).toBeTruthy();
});
