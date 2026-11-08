import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppUpdatesSection } from "./AppUpdatesSection";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command, args);
  },
}));
const events = vi.hoisted(() => ({ handlers: new Map<string, Set<(message: { payload: unknown }) => void>>() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: (message: { payload: unknown }) => void) => {
    const listeners = events.handlers.get(event) ?? new Set<(message: { payload: unknown }) => void>();
    listeners.add(handler);
    events.handlers.set(event, listeners);
    return () => { listeners.delete(handler); };
  },
}));

const idle = { status: { state: "idle" }, can_install: false, defer_reason: null, can_cancel: false };
const ready = { status: { state: "ready", version: "0.2.0" }, can_install: true, defer_reason: null, can_cancel: false };
const queued = { status: { state: "queued", version: "0.2.0", reason: "job_running", requested_at: "2026-11-07T10:00:00Z" }, can_install: false, defer_reason: "job_running", can_cancel: true };
const installing = { status: { state: "installing", version: "0.2.0" }, can_install: false, defer_reason: null, can_cancel: false };

afterEach(() => { cleanup(); ipc.calls.length = 0; events.handlers.clear(); });

describe("AppUpdatesSection", () => {

  test("unsupported platforms show the release link and no in-place update controls", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve({
        status: {
          state: "unsupported",
          reason: "in-place install is macOS-only; download the new release from GitHub",
        },
        can_install: false,
        defer_reason: null,
        can_cancel: false,
      });
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    expect(await screen.findByText(/in-place install is macOS-only/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Check for updates" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Install update" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Download release from GitHub" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "open_external_link")).toBe(true));
    expect(ipc.calls.find(call => call.command === "open_external_link")?.args).toEqual({
      url: "https://github.com/tapas-joshi/AgenTuna/releases/latest",
    });
    expect(ipc.calls.some(call => call.command === "app_update_check")).toBe(false);
  });
  test("a check is explicit and a ready update can be installed", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve(idle);
      if (command === "app_update_check") return Promise.resolve(ready);
      if (command === "app_update_install_request") return Promise.resolve(installing);
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    expect(await screen.findByText(/Current version 0\.1\.0/)).toBeTruthy();
    expect(ipc.calls.filter(call => call.command === "app_update_check")).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "Check for updates" }));
    const install = await screen.findByRole("button", { name: "Install 0.2.0" });
    expect(screen.getByText(/Version 0\.2\.0 is verified and ready to install/)).toBeTruthy();
    fireEvent.click(install);
    await waitFor(() => expect(ipc.calls.some(call => call.command === "app_update_install_request")).toBe(true));
    const request = ipc.calls.find(call => call.command === "app_update_install_request");
    expect(request?.args).toEqual({ version: "0.2.0", editorDirty: false, modalOpen: false });
    expect(await screen.findByText(/Installing version 0\.2\.0/)).toBeTruthy();
  });

  test("a queued install shows the reason and can be cancelled", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve(queued);
      if (command === "app_update_install_cancel") return Promise.resolve(idle);
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    expect(await screen.findByText(/Install waits for the running job to finish/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Install update" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Cancel queued install" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "app_update_install_cancel")).toBe(true));
    expect(await screen.findByText(/No update is staged/)).toBeTruthy();
  });

  test("a failed install shows its message and remedy", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve({
        status: { state: "failed", version: "0.2.0", message: "the update archive could not be read", remedy: "Delete the downloaded update and run Check for Updates again.", rolled_back: false },
        can_install: false, defer_reason: null, can_cancel: false,
      });
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    expect(await screen.findByText(/Version 0\.2\.0 could not be installed/)).toBeTruthy();
    expect(screen.getByText(/Delete the downloaded update/)).toBeTruthy();
  });

  test("a rolled-back install is labelled and explains the remedy", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve({
        status: { state: "rolled_back", version: "0.2.0", reason: "The new version is installed but could not be relaunched: open exited with 1", remedy: "The previous version was restored. Quit and reopen AgenTuna, then retry the update." },
        can_install: false, defer_reason: null, can_cancel: false,
      });
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    expect(await screen.findByText(/Version 0\.2\.0 did not start/)).toBeTruthy();
    expect(screen.getByText(/The previous version was restored/)).toBeTruthy();
  });

  test("the check button is disabled while an install runs", async () => {
    ipc.reply = command => {
      if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
      if (command === "app_update_status") return Promise.resolve(installing);
      return Promise.resolve(undefined);
    };
    render(<AppUpdatesSection />);
    const check = await screen.findByRole("button", { name: "Check for updates" });
    expect((check as HTMLButtonElement).disabled).toBe(true);
  });

  test("a queued install is retried once the app reports idle", async () => {
    vi.useFakeTimers();
    try {
      ipc.reply = command => {
        if (command === "app_info") return Promise.resolve({ name: "AgenTuna", version: "0.1.0" });
        if (command === "app_update_status") return Promise.resolve(queued);
        if (command === "app_update_install_resume") return Promise.resolve(installing);
        return Promise.resolve(undefined);
      };
      render(<AppUpdatesSection />);
      await act(async () => { await Promise.resolve(); });
      await act(async () => { vi.advanceTimersByTime(2100); await Promise.resolve(); });
      expect(ipc.calls.some(call => call.command === "app_update_install_resume")).toBe(true);
      const resume = ipc.calls.find(call => call.command === "app_update_install_resume");
      expect(resume?.args).toEqual({ editorDirty: false, modalOpen: false });
    } finally {
      vi.useRealTimers();
    }
  });
});
