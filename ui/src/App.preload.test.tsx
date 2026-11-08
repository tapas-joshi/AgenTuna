import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { App } from "./App";

// Counts module evaluations of the two list routes. The production code fetches them ahead of
// the first click; a counter is the only observable side effect of the chunk request.
const loads = vi.hoisted(() => ({ skills: 0, mcp: 0 }));

vi.mock("./screens/skills/SkillsScreen", () => {
  loads.skills += 1;
  return { SkillsScreen: () => null };
});
vi.mock("./screens/mcp_list/McpListScreen", () => {
  loads.mcp += 1;
  return { McpListScreen: () => null };
});

const idle = vi.hoisted(() => ({ scheduled: 0 }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) =>
    Promise.resolve(
      command === "app_info"
        ? { name: "AgenTuna", version: "0.1.0" }
        : command === "projects_list"
          ? []
          : command === "settings_load"
            ? { schema_version: 1, values: {} }
            : command === "conflicts_list"
              ? []
              : null,
    ),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => undefined),
}));

beforeEach(() => {
  loads.skills = 0;
  loads.mcp = 0;
  idle.scheduled = 0;
});

afterEach(() => {
  cleanup();
  Reflect.deleteProperty(window, "requestIdleCallback");
  Reflect.deleteProperty(window, "cancelIdleCallback");
});

test("prefetches the list routes without waiting for an idle callback", async () => {
  // A webview main thread that never idles is the real case this guards: the previous
  // implementation scheduled the prefetch through requestIdleCallback, which fires only after
  // boot completes, i.e. at or after the first click (TF-035).
  Object.defineProperty(window, "requestIdleCallback", {
    configurable: true,
    value: () => {
      idle.scheduled += 1;
      return 1;
    },
  });
  Object.defineProperty(window, "cancelIdleCallback", {
    configurable: true,
    value: () => undefined,
  });

  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(loads.skills).toBe(1));
  expect(loads.mcp).toBe(1);
  expect(idle.scheduled).toBe(0);
});
