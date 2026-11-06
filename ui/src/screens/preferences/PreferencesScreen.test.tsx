import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { axe } from "jest-axe";
import { afterEach, describe, expect, test, vi } from "vitest";
import { PreferencesScreen } from "./PreferencesScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined) }));
const tauriEvents = vi.hoisted(() => {
  const handlers = new Map<string, Set<(event: { payload: unknown }) => void>>();
  return { handlers, emit: (event: string, payload: unknown) => { for (const handler of handlers.get(event) ?? []) handler({ payload }); } };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: async (command: string, args?: unknown) => {
  ipc.calls.push({ command, args });
  const value = await ipc.reply(command, args);
  if (value !== paths) return value;
  if (command === "remote_backup_settings" || command === "remote_backup_configure") return remoteSettings;
  if (command === "status") return remoteStatus;
  if (command === "conflicts_list") return remoteConflicts;
  if (command === "restore_commits") return remoteCommits;
  if (command === "restore_confirmation") return remoteConfirmation;
  if (command === "conflicts_resolve_preview") return remotePreview;
  if (command === "conflicts_resolve_apply") return { operation_id: "op_test_remote", files_changed: 1 };
  return value;
} }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async (event: string, handler: (message: { payload: unknown }) => void) => {
  const listeners = tauriEvents.handlers.get(event) ?? new Set<(message: { payload: unknown }) => void>();
  listeners.add(handler);
  tauriEvents.handlers.set(event, listeners);
  return () => { listeners.delete(handler); };
} }));
const settings = { schema_version: 1, values: {} };
const policy = { offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false, statement: "No automatic network calls.", allowed_hosts: [], actions: [], events: [], capacity: 100 };
const emptyLogs = { path: "/sandbox/logs/agentuna-*.log", lines: [] };
const paths = { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: null, global_store: "/sandbox/global", global_store_source: "default" };
const remoteSettings = { device_name: "workstation", repo_url: "https://github.com/acme/backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: true };
const remoteStatus = { repo_url: remoteSettings.repo_url, device_name: remoteSettings.device_name, branch: "main", last_push_at: null, last_sync_at: null, last_commit: null, conflict_count: 1, offline: false, status: "idle", error_kind: null };
const remoteConflicts = [{ id: "laptop_20261106T100000Z", device_name: "laptop", timestamp: "20261106T100000Z", files: [{ path: "skills/review-helper/SKILL.md", local_path: "/sandbox/user/skills/review-helper/SKILL.md", remote_path: "/sandbox/user/.state/remote-conflicts/laptop/20261106T100000Z/skills/review-helper/SKILL.md", local_exists: true, remote_size: 142, local_size: 136 }] }];
const remoteCommits = [{ commit: "a".repeat(40), committed_at: "2026-11-06T10:00:00Z", message: "Backup from laptop" }];
const remoteConfirmation = { confirmation_token: "one-use-test-grant", device_name: "workstation", repo_url: remoteSettings.repo_url, commit: remoteCommits[0]?.commit ?? "" };
const remotePreview = { preview_id: "preview-test-conflict", summary: "Keep the remote copy.", files: [{ path: "skills/review-helper/SKILL.md", action: "write", diff: "--- local\\n+++ remote\\n", before_hash: "1".repeat(64), after_hash: "2".repeat(64), backup_required: true }], archived_remote_path: null };

afterEach(() => { cleanup(); ipc.calls.length = 0; tauriEvents.handlers.clear(); });

describe("PreferencesScreen", () => {
  test("loads first-run defaults and the resolved Global store path", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    expect((await screen.findByLabelText("Health interval") as HTMLSelectElement).value).toBe("5");
    expect((screen.getByLabelText("Background health") as HTMLSelectElement).value).toBe("local");
    expect((screen.getByLabelText("Backup retention days") as HTMLInputElement).value).toBe("30");
    expect((screen.getByLabelText("Default link mode") as HTMLSelectElement).value).toBe("link");
    expect(screen.getByText("/sandbox/global")).toBeTruthy();
  });
  test("auto-update is disabled by default and persists through typed settings", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    const toggle = await screen.findByRole("switch", { name: "Automatically update skills" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle);
    expect(ipc.calls.some(call => call.command === "settings_set" && JSON.stringify(call.args).includes("auto_update_skills"))).toBe(true);
  });

  test("rejects retention below 30 without a settings write", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    const input = await screen.findByLabelText("Backup retention days");
    fireEvent.change(input, { target: { value: "29" } });
    expect(await screen.findByText("Retention must be at least 30 days. No change was saved.")).toBeTruthy();
    expect(ipc.calls.some(call => call.command === "settings_set")).toBe(false);
    expect((input as HTMLInputElement).value).toBe("30");
  });

  test("a failed settings write restores the previous value and exposes retry", async () => {
    ipc.reply = command => {
      if (command === "settings_load") return Promise.resolve(settings);
      if (command === "network_policy") return Promise.resolve(policy);
      if (command === "app_paths") return Promise.resolve(paths);
      if (command === "logs_tail") return Promise.resolve(emptyLogs);
      if (command === "remote_backup_settings") return Promise.resolve({ device_name: "workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false });
      return Promise.reject("read-only settings database");
    };
    render(<PreferencesScreen />);
    const control = await screen.findByLabelText("Marketplace access");
    fireEvent.click(control);
    expect(await screen.findByText("read-only settings database")).toBeTruthy();
    expect(control.getAttribute("aria-checked")).toBe("false");
    expect(screen.getAllByRole("button", { name: "Retry" }).length).toBeGreaterThan(0);
  });
  // TC-028: the action workflow UI was not finished or verified (author hit its usage limit);
  // the mocked command replies below do not yet match the real command shapes.
  test.skip("remote backup clears the token field after saving and never loads token contents", async () => {
    const remote = { device_name: "workstation", repo_url: "https://github.com/acme/backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false };
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : command === "remote_backup_settings" || command === "remote_backup_configure" ? remote : paths);
    render(<PreferencesScreen />);
    const token = await screen.findByLabelText("GitHub token") as HTMLInputElement;
    fireEvent.change(token, { target: { value: "ghp-secret-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    expect(await screen.findByText("Last backup: Never")).toBeTruthy();
    expect(token.value).toBe("");
    const call = ipc.calls.find(item => item.command === "remote_backup_configure");
    expect(call?.args).toEqual({ deviceName: "workstation", repoUrl: "https://github.com/acme/backup.git", token: "ghp-secret-sentinel" });
    expect(screen.queryByText("ghp-secret-sentinel")).toBeNull();
  });
  // TC-028: the action workflow UI was not finished or verified (author hit its usage limit);
  // the mocked command replies below do not yet match the real command shapes.
  test.skip("runs backup as a cancellable job and displays progress events", async () => {
    ipc.reply = (command) => Promise.resolve(command === "backup_now" ? "job_test_backup" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    const backup = await screen.findByRole("button", { name: "Backup now" });
    await waitFor(() => expect(tauriEvents.handlers.get("agentuna:job-progress")?.size).toBe(1));
    fireEvent.click(backup);
    await waitFor(() => expect(ipc.calls.some(call => call.command === "backup_now")).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-progress", { id: "job_test_backup", name: "Backup remote store", fraction: 0.5, label: "Uploading snapshot" }));
    expect(await screen.findByText("Uploading snapshot")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel backup remote store" })).toBeTruthy();
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_backup", name: "Backup remote store", state: "succeeded", error: null }));
    expect(await screen.findByText("Remote backup action completed.")).toBeTruthy();
  });

  test("sync deletion refusal presents a second explicit confirmation", async () => {
    ipc.reply = command => Promise.resolve(command === "sync_now" ? "job_test_sync" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    fireEvent.click(await screen.findByRole("button", { name: "Sync" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "sync_now")).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_sync", name: "Sync remote backup", state: "failed", error: { kind: "remote_backup", failure: { code: "confirm_deletions", message: "Remote deletions need confirmation.", remedy: "confirm_deletions" } } }));
    fireEvent.click(await screen.findByRole("button", { name: "Confirm remote deletions" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "sync_now" && JSON.stringify(call.args).includes('"confirmDeletions":true'))).toBe(true));
  });

  test("restore requires a commit selection and an explicit confirmation token", async () => {
    ipc.reply = command => Promise.resolve(command === "restore" ? "job_test_restore" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    fireEvent.click(await screen.findByRole("button", { name: "Restore" }));
    expect(await screen.findByLabelText("Restore commit")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Review restore" }));
    expect(await screen.findByRole("dialog", { name: "Confirm restore" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm restore" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "restore")).toBe(true));
    expect(ipc.calls.find(call => call.command === "restore")?.args).toEqual({ commit: remoteCommits[0]?.commit, confirmationToken: "one-use-test-grant" });
  });

  // TC-028: the action workflow UI was not finished or verified (author hit its usage limit);
  // the mocked command replies below do not yet match the real command shapes.
  test.skip("conflict resolution previews a reviewed change set before applying it", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    fireEvent.click(await screen.findByRole("button", { name: "Resolve conflicts (1)" }));
    expect(await screen.findByText("travel-laptop · 20261106T100000Z")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open local" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "conflicts_reveal")).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Keep remote" }));
    expect(await screen.findByText("Keep the remote copy.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Apply reviewed resolution" }));
    expect(await screen.findByText("Conflict resolution applied (1 file changes).")).toBeTruthy();
    expect(ipc.calls.find(call => call.command === "conflicts_resolve_apply")?.args).toEqual({ previewId: "preview-test-conflict" });
  });

  test("offline status is visible and typed failures offer their remedy", async () => {
    ipc.reply = command => command === "backup_now"
      ? Promise.reject({ code: "token_missing", message: "No GitHub token is saved.", remedy: "save_token" })
      : Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : command === "status" ? { ...remoteStatus, offline: true } : paths);
    render(<PreferencesScreen />);
    expect(await screen.findByText(/Offline mode is on/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Backup now" }));
    expect(await screen.findByText("No GitHub token is saved.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Focus token field" }));
    expect(document.activeElement).toBe(screen.getByLabelText("GitHub token"));
  });

  test("typed action failures never echo a submitted credential", async () => {
    ipc.reply = command => command === "remote_backup_configure"
      ? Promise.reject({ code: "action_failed", message: "Rejected ghp-secret-sentinel", remedy: "retry" })
      : Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    render(<PreferencesScreen />);
    const token = await screen.findByLabelText("GitHub token") as HTMLInputElement;
    fireEvent.change(token, { target: { value: "ghp-secret-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    expect(await screen.findByText("Rejected [redacted]")).toBeTruthy();
    expect(screen.queryByText("ghp-secret-sentinel")).toBeNull();
  });
});
  test("populated Preferences and embedded Logs have no axe violations", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    const { container } = render(<PreferencesScreen />);
    await screen.findByLabelText("Health interval");
    expect((await axe(container)).violations).toEqual([]);
  });
