/**
 * Route chunk failure guard (TF-035 companion).
 *
 * Route-level code splitting introduces a failure mode that static imports
 * did not have: a lazy chunk can fail to fetch or a screen can crash while
 * rendering. Without a boundary React unmounts the whole tree — a blank
 * window with no shell. These tests pin the shell-surviving error state.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "./App";
import { RouteErrorBoundary } from "./shell/RouteErrorBoundary";

const ipc = vi.hoisted(() => ({ reply: (_cmd: string): Promise<unknown> => Promise.resolve(null) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    void args;
    return ipc.reply(cmd);
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => undefined),
}));

// The chunk itself fails to fetch: the exact failure mode code splitting adds.
// Skills needs no active project, so the click cannot be intercepted by
// the project-picker guard.
vi.mock("./screens/skills/SkillsScreen", () => {
  throw new Error("Failed to fetch dynamically imported module");
});

afterEach(cleanup);

function startupReply(cmd: string): Promise<unknown> {
  if (cmd === "app_info") return Promise.resolve({ name: "AgenTuna", version: "1.0.0" });
  if (cmd === "overview_load") {
    return Promise.resolve({
      scopes: [],
      attention: [],
      attention_total: 0,
      changes: [],
      user_path: "/sandbox/user",
      global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] },
      startup_warnings: [],
    });
  }
  if (cmd === "conflicts_list" || cmd === "projects_list" || cmd === "agents_list") return Promise.resolve([]);
  if (cmd === "settings_load") return Promise.resolve({ schema_version: 1, values: {} });
  return Promise.resolve(null);
}

test("a route chunk that fails to fetch keeps the shell and shows a reloadable error", async () => {
  ipc.reply = startupReply;
  render(<App />);

  await screen.findByRole("navigation", { name: "Workspace" });
  fireEvent.click(screen.getByRole("button", { name: "Skills" }));

  // The shell (sidebar, top bar) must survive the failed chunk…
  expect(screen.getByRole("navigation", { name: "Workspace" })).toBeTruthy();
  // …and the routed region must show the screen-level error state, not nothing.
  expect(await screen.findByRole("heading", { name: "This screen could not load" })).toBeTruthy();
  expect(screen.getByRole("main", { name: "Screen error" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
});

test("RouteErrorBoundary offers the injected retry for a screen render crash", () => {
  function Bomb(): never {
    throw new Error("render crash");
  }
  const onRetry = vi.fn();
  render(
    <RouteErrorBoundary onRetry={onRetry}>
      <Bomb />
    </RouteErrorBoundary>,
  );

  expect(screen.getByRole("heading", { name: "This screen could not load" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(onRetry).toHaveBeenCalledTimes(1);
});
