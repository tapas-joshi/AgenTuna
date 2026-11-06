import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import type { EffectiveQueryView } from "../../generated/EffectiveQueryView";
import { EffectiveScreen } from "./EffectiveScreen";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command);
  },
}));

const appState = createAppState();
appState.restore("project-id", "global");
const unlocked: EffectiveQueryView = {
  items: [{ logical_id: "item:itm_lock", item_id: "itm_lock", kind: "skill", name: "Example", winner_scope: "global", source_path: "/global/example", agents: [], loads_in_agents: 0, lock: null }],
  disabled_here: [], diagnostics: [], load_diagnostics: [],
};
const locked: EffectiveQueryView = {
  ...unlocked,
  items: [{ ...unlocked.items[0]!, lock: { id: "lck_1", reason: "Approved version" } }],
};
afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("EffectiveScreen Global lock actions", () => {
  test("requires a reason, applies the lock, refreshes the query and shows success feedback", async () => {
    let queryCount = 0;
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve(++queryCount === 1 ? unlocked : locked);
      if (command === "overview_load") return Promise.resolve({ global_writable: true, global_reason: null });
      return Promise.resolve({ locked: true, reason: "Approved version" });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);
    const lockButton = await screen.findByRole("button", { name: "Lock" });
    fireEvent.click(lockButton);
    const reason = screen.getByRole("textbox", { name: "Lock reason" });
    expect((screen.getByRole("button", { name: "Lock item" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(reason, { target: { value: "Approved version" } });
    fireEvent.click(screen.getByRole("button", { name: "Lock item" }));
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "effective_lock")).toBe(true));
    expect(await screen.findByText("🔒 Locked: Approved version")).toBeTruthy();
    expect(screen.getByText("Locked Example")).toBeTruthy();
    expect(ipc.calls.filter((call) => call.command === "effective_query")).toHaveLength(2);
  });

  test("shows unlock only for locked rows and removes the lock after confirmation", async () => {
    let queryCount = 0;
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve(++queryCount === 1 ? locked : unlocked);
      if (command === "overview_load") return Promise.resolve({ global_writable: true, global_reason: null });
      return Promise.resolve({ locked: false, reason: "Approved version" });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "Unlock" }));
    const dialog = await screen.findByRole("dialog", { name: "Unlock Example?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Unlock" }));
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "effective_unlock")).toBe(true));
    expect(await screen.findByRole("button", { name: "Lock" })).toBeTruthy();
  });
});
