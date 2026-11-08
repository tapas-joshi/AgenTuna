/**
 * Remote backup refresh error rendering (wave-16 follow-up).
 *
 * The refresh path used `String(reason)` on rejected command objects, which
 * renders `[object Object]` in the alert boxes. These tests pin the shared
 * `commandErrorMessage` rendering: message text only, never object coercion.
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { RemoteBackupSection } from "./RemoteBackupSection";

const ipc = vi.hoisted(() => ({ reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) => ipc.reply(command),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => undefined),
}));

const okReply = (command: string): Promise<unknown> => {
  if (command === "remote_backup_settings") return Promise.resolve({ device_name: "Mac", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false });
  if (command === "status") return Promise.resolve({ status: "idle", error_kind: null, last_push_at: null, last_pull_at: null, last_commit: null });
  return Promise.resolve([]);
};

afterEach(cleanup);

test("a rejected settings command shows its message instead of object coercion", async () => {
  ipc.reply = (command) => (command === "remote_backup_settings" ? Promise.reject({ code: "unavailable", message: "Backup service offline." }) : okReply(command));
  render(<ToastProvider><RemoteBackupSection /></ToastProvider>);

  expect(await screen.findByText("Backup service offline.")).toBeTruthy();
  expect(document.body.textContent ?? "").not.toContain("[object Object]");
});

test("a rejected status command prefixes the status alert with its message", async () => {
  ipc.reply = (command) => (command === "status" ? Promise.reject({ code: "unavailable", message: "Status endpoint unreachable." }) : okReply(command));
  render(<ToastProvider><RemoteBackupSection /></ToastProvider>);

  expect(await screen.findByText(/Status unavailable: Status endpoint unreachable\./)).toBeTruthy();
  expect(document.body.textContent ?? "").not.toContain("[object Object]");
});
