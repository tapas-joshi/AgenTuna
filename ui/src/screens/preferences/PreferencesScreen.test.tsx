import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { axe } from "jest-axe";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
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
const remoteConflicts = [{ id: "travel-laptop_20261106T100000Z", device_name: "travel-laptop", timestamp: "20261106T100000Z", files: [{ path: "skills/review-helper/SKILL.md", local_path: "/sandbox/user/skills/review-helper/SKILL.md", remote_path: "/sandbox/user/.state/remote-conflicts/travel-laptop/20261106T100000Z/skills/review-helper/SKILL.md", local_exists: true, remote_size: 142, local_size: 136 }] }];
const remoteCommits = [{ commit: "a".repeat(40), committed_at: "2026-11-06T10:00:00Z", message: "Backup from laptop" }];
const remoteConfirmation = { confirmation_token: "one-use-test-grant", device_name: "workstation", repo_url: remoteSettings.repo_url, commit: remoteCommits[0]?.commit ?? "" };
const remotePreview = { preview_id: "preview-test-conflict", summary: "Keep the remote copy.", files: [{ path: "skills/review-helper/SKILL.md", action: "write", diff: "--- local\\n+++ remote\\n", before_hash: "1".repeat(64), after_hash: "2".repeat(64), backup_required: true }], archived_remote_path: null };

afterEach(() => { cleanup(); ipc.calls.length = 0; tauriEvents.handlers.clear(); });
function renderPreferences(props?: React.ComponentProps<typeof PreferencesScreen>) {
  return render(<ToastProvider><PreferencesScreen {...props} /></ToastProvider>);
}

describe("PreferencesScreen", () => {
  test("loads first-run defaults and the resolved Global store path", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    expect((await screen.findByLabelText("Health interval") as HTMLSelectElement).value).toBe("5");
    expect((screen.getByLabelText("Background health") as HTMLSelectElement).value).toBe("local");
    expect((screen.getByLabelText("Backup retention days") as HTMLInputElement).value).toBe("30");
    expect((screen.getByLabelText("Default link mode") as HTMLSelectElement).value).toBe("link");
    expect(screen.getByText("/sandbox/global")).toBeTruthy();
  });
  test("recent network events show only their metadata and Clear empties them", async () => {
    let current = { ...policy, events: [{ at: "2026-11-06T10:00:00Z", action: "git_fetch", host: "github.com", bytes: 1200 }] };
    ipc.reply = command => {
      if (command === "settings_load") return Promise.resolve(settings);
      if (command === "logs_tail") return Promise.resolve(emptyLogs);
      if (command === "app_paths") return Promise.resolve(paths);
      if (command === "network_policy") return Promise.resolve(current);
      if (command === "network_clear_events") { current = { ...current, events: [] }; return Promise.resolve(current); }
      return Promise.resolve(paths);
    };
    renderPreferences();
    const events = await screen.findByRole("region", { name: "Recent network events" });
    expect(events.textContent).toContain("git_fetch · github.com");
    expect(events.textContent).toContain("1200 bytes");
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Recent network events" })).toBeNull());
    expect(ipc.calls.some(({ command }) => command === "network_clear_events")).toBe(true);
  });
  test("auto-update is disabled by default and persists through typed settings", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    const toggle = await screen.findByRole("switch", { name: "Automatically update skills" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle);
    expect(ipc.calls.some(call => call.command === "settings_set" && JSON.stringify(call.args).includes("auto_update_skills"))).toBe(true);
  });

  test("rejects retention below 30 without a settings write", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
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
    renderPreferences();
    const control = await screen.findByLabelText("Marketplace access");
    fireEvent.click(control);
    expect((await screen.findAllByText("read-only settings database")).length).toBeGreaterThan(0);
    expect(control.getAttribute("aria-checked")).toBe("false");
    expect(screen.getAllByRole("button", { name: "Retry" }).length).toBeGreaterThan(0);
  });
  test("remote backup clears the token field after saving and never loads token contents", async () => {
    const remote = { device_name: "workstation", repo_url: "https://github.com/acme/backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false };
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : command === "remote_backup_settings" || command === "remote_backup_configure" ? remote : paths);
    renderPreferences();
    const token = await screen.findByLabelText("GitHub token") as HTMLInputElement;
    fireEvent.change(token, { target: { value: "ghp-secret-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    await waitFor(() => expect(screen.getByText("Last backup:").parentElement?.textContent).toBe("Last backup: Never"));
    expect(token.value).toBe("");
    const writes = ipc.calls.filter(item => JSON.stringify(item.args ?? null).includes("ghp-secret-sentinel"));
    expect(writes).toEqual([{ command: "remote_backup_configure", args: { deviceName: "workstation", repoUrl: "https://github.com/acme/backup.git", token: "ghp-secret-sentinel" } }]);
    expect(screen.queryByText("ghp-secret-sentinel")).toBeNull();
  });

  test("private repository creation uses only the name, supports cancellation and reports success", async () => {
    ipc.reply = command => Promise.resolve(command === "remote_backup_create_repo" ? { job_id: "job_test_create_repo" } : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Create private repo" }));
    const creation = await screen.findByRole("region", { name: "Private repository creation" });
    const name = screen.getByRole("textbox", { name: "Private repository name" });
    fireEvent.change(name, { target: { value: "agentuna-backup" } });
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    await screen.findByRole("button", { name: "Cancel creation" });
    const call = ipc.calls.find(item => item.command === "remote_backup_create_repo");
    expect(call).toEqual({ command: "remote_backup_create_repo", args: { request: { repo_name: "agentuna-backup" } } });
    expect(JSON.stringify(call?.args)).not.toContain("token");
    fireEvent.click(screen.getByRole("button", { name: "Cancel creation" }));
    await waitFor(() => expect(ipc.calls.some(item => item.command === "jobs_cancel")).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_create_repo", name: "Create private repository", state: "cancelled", error: null }));
    expect(await screen.findByText("Private repository creation was cancelled.")).toBeTruthy();
    expect(creation).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    await waitFor(() => expect(ipc.calls.filter(item => item.command === "remote_backup_create_repo").length).toBe(2));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_create_repo", name: "Create private repository", state: "succeeded", error: null }));
    expect(await screen.findByText("Private repository created and connected.")).toBeTruthy();
  });

  test("private repository creation exposes scope refusal, offline and authentication failures", async () => {
    let rejection: unknown = { code: "repo_scope_missing", message: "The saved token needs the repo scope.", remedy: "add_repo_scope" };
    ipc.reply = command => command === "remote_backup_create_repo"
      ? Promise.reject(rejection)
      : Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Create private repo" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Private repository name" }), { target: { value: "agentuna-backup" } });
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    expect(await screen.findByText("Scope refused: the saved token needs GitHub’s repo scope.")).toBeTruthy();
    expect(await screen.findByText("The saved token needs the repo scope.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Focus token field" }));
    expect(document.activeElement).toBe(screen.getByLabelText("GitHub token"));
    rejection = { code: "offline", message: "Offline mode is on.", remedy: "turn_network_on" };
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    expect(await screen.findByText("Offline: turn off offline mode before contacting GitHub.")).toBeTruthy();
    rejection = { code: "token_rejected", message: "GitHub rejected the saved token.", remedy: "replace_token" };
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    expect(await screen.findByText("Authentication failed: replace or save a valid GitHub token.")).toBeTruthy();
    expect(ipc.calls.filter(item => item.command === "remote_backup_create_repo").every(item => JSON.stringify(item.args).includes("repo_name") && !JSON.stringify(item.args).includes("token"))).toBe(true);
  });
  test("device name validation blocks slashes and empty values before writing", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : command === "remote_backup_settings" ? remoteSettings : paths);
    renderPreferences();
    const name = await screen.findByLabelText("Remote backup device name") as HTMLInputElement;
    fireEvent.change(name, { target: { value: "lab/desk" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    expect(await screen.findByText("Device names cannot contain slashes.")).toBeTruthy();
    expect(name.getAttribute("aria-invalid")).toBe("true");
    fireEvent.change(name, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    expect(await screen.findByText("Enter a device name before saving.")).toBeTruthy();
    expect(ipc.calls.some(call => call.command === "remote_backup_configure")).toBe(false);
  });
  test("runs backup as a cancellable job and displays progress events", async () => {
    ipc.reply = (command) => Promise.resolve(command === "backup_now" ? "job_test_backup" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    const backup = await screen.findByRole("button", { name: "Backup now" });
    await waitFor(() => expect(tauriEvents.handlers.get("agentuna:job-progress")?.size).toBe(1));
    fireEvent.click(backup);
    await waitFor(() => expect(ipc.calls.some(call => call.command === "backup_now")).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-progress", { id: "job_test_backup", name: "Backup remote store", fraction: 0.5, label: "Uploading snapshot" }));
    expect(await screen.findByText("Uploading snapshot")).toBeTruthy();
    expect((screen.getByRole("progressbar", { name: "Remote backup progress" }) as HTMLProgressElement).value).toBe(0.5);
    fireEvent.click(screen.getByRole("button", { name: "Cancel backup remote store" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "jobs_cancel" && JSON.stringify(call.args).includes("job_test_backup"))).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_backup", name: "Backup remote store", state: "cancelled", error: null }));
    expect(await screen.findByText("Remote backup action was cancelled.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Backup now" }));
    await waitFor(() => expect(ipc.calls.filter(call => call.command === "backup_now").length).toBe(2));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_backup", name: "Backup remote store", state: "succeeded", error: null }));
    expect(await screen.findByText("Remote backup action completed.")).toBeTruthy();
  });

  test("sync deletion refusal presents a second explicit confirmation", async () => {
    ipc.reply = command => Promise.resolve(command === "sync_now" ? "job_test_sync" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Sync" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "sync_now")).toBe(true));
    act(() => tauriEvents.emit("agentuna:job-finished", { id: "job_test_sync", name: "Sync remote backup", state: "failed", error: { kind: "remote_backup", failure: { code: "confirm_deletions", message: "Remote deletions need confirmation.", remedy: "confirm_deletions" } } }));
    fireEvent.click(await screen.findByRole("button", { name: "Confirm remote deletions" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "sync_now" && JSON.stringify(call.args).includes('"confirmDeletions":true'))).toBe(true));
  });

  test("restore requires a commit selection and an explicit confirmation token", async () => {
    ipc.reply = command => Promise.resolve(command === "restore" ? "job_test_restore" : command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Restore" }));
    expect(await screen.findByLabelText("Restore commit")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Review restore" }));
    expect(await screen.findByRole("dialog", { name: "Confirm restore" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm restore" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "restore")).toBe(true));
    expect(ipc.calls.find(call => call.command === "restore")?.args).toEqual({ commit: remoteCommits[0]?.commit, confirmationToken: "one-use-test-grant" });
  });

  test("conflict resolution previews a reviewed change set before applying it", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Resolve conflicts (1)" }));
    expect(await screen.findByText("travel-laptop · 20261106T100000Z")).toBeTruthy();
    expect(screen.getByText("/sandbox/user/skills/review-helper/SKILL.md · 136 B")).toBeTruthy();
    expect(screen.getByText("/sandbox/user/.state/remote-conflicts/travel-laptop/20261106T100000Z/skills/review-helper/SKILL.md · 142 B")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /overwrite/i })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open local" }));
    await waitFor(() => expect(ipc.calls.some(call => call.command === "conflicts_reveal")).toBe(true));
    expect(ipc.calls.find(call => call.command === "conflicts_reveal")?.args).toEqual({ groupId: "travel-laptop_20261106T100000Z", relativePath: "skills/review-helper/SKILL.md", side: "local" });
    fireEvent.click(screen.getByRole("button", { name: "Keep remote" }));
    expect(await screen.findByText("Keep the remote copy.")).toBeTruthy();
    expect(ipc.calls.find(call => call.command === "conflicts_resolve_preview")?.args).toEqual({ groupId: "travel-laptop_20261106T100000Z", relativePath: "skills/review-helper/SKILL.md", resolution: "keep_remote" });
    fireEvent.click(screen.getByRole("button", { name: "Apply reviewed resolution" }));
    expect(await screen.findByText("Conflict resolution applied (1 file changes).")).toBeTruthy();
    expect(ipc.calls.find(call => call.command === "conflicts_resolve_apply")?.args).toEqual({ previewId: "preview-test-conflict" });
  });

  test("offline status is visible and typed failures offer their remedy", async () => {
    ipc.reply = command => command === "backup_now"
      ? Promise.reject({ code: "token_missing", message: "No GitHub token is saved.", remedy: "save_token" })
      : Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : command === "status" ? { ...remoteStatus, offline: true } : paths);
    renderPreferences();
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
    renderPreferences();
    const token = await screen.findByLabelText("GitHub token") as HTMLInputElement;
    fireEvent.change(token, { target: { value: "ghp-secret-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save remote backup settings" }));
    expect(await screen.findByText("Rejected [redacted]")).toBeTruthy();
    expect(screen.queryByText("ghp-secret-sentinel")).toBeNull();
  });

  test("offline mode switch stores through the typed command and follows only the backend state", async () => {
    const firstToggle = Promise.withResolvers<unknown>();
    let toggles = 0;
    ipc.reply = command => {
      if (command === "settings_load") return Promise.resolve(settings);
      if (command === "network_policy") return Promise.resolve(policy);
      if (command === "logs_tail") return Promise.resolve(emptyLogs);
      if (command === "app_paths") return Promise.resolve(paths);
      if (command === "network_set_offline") {
        toggles += 1;
        return toggles === 1 ? firstToggle.promise : Promise.resolve({ ...policy, offline: false });
      }
      return Promise.resolve(paths);
    };
    renderPreferences();
    const toggle = await screen.findByRole("switch", { name: "Offline mode" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(screen.queryByText("Offline", { selector: "strong" })).toBeNull();
    fireEvent.click(toggle);
    // No optimistic flip: the stored value stays until the backend answers.
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect((toggle as HTMLButtonElement).disabled).toBe(true);
    expect(ipc.calls.filter(call => call.command === "network_set_offline")).toEqual([{ command: "network_set_offline", args: { offline: true } }]);
    act(() => firstToggle.resolve({ ...policy, offline: true }));
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));
    expect(toggle.textContent).toBe("On");
    expect(screen.getByText("Offline", { selector: "strong" })).toBeTruthy();
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
    expect(ipc.calls.filter(call => call.command === "network_set_offline").map(call => call.args)).toEqual([{ offline: true }, { offline: false }]);
    expect(screen.queryByText("Offline", { selector: "strong" })).toBeNull();
  });

  test("a failed offline switch keeps the stored value and retries the same request", async () => {
    let attempts = 0;
    ipc.reply = command => {
      if (command === "settings_load") return Promise.resolve(settings);
      if (command === "network_policy") return Promise.resolve(policy);
      if (command === "logs_tail") return Promise.resolve(emptyLogs);
      if (command === "app_paths") return Promise.resolve(paths);
      if (command === "network_set_offline") {
        attempts += 1;
        return attempts === 1 ? Promise.reject("policy store is read-only") : Promise.resolve({ ...policy, offline: true });
      }
      return Promise.resolve(paths);
    };
    renderPreferences();
    const toggle = await screen.findByRole("switch", { name: "Offline mode" });
    fireEvent.click(toggle);
    expect(await screen.findByText(/Offline mode was not changed: policy store is read-only/)).toBeTruthy();
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("true"));
    expect(ipc.calls.filter(call => call.command === "network_set_offline").map(call => call.args)).toEqual([{ offline: true }, { offline: true }]);
    expect(screen.queryByText(/Offline mode was not changed:/)).toBeNull();
  });

  test("offline remedies link to the switch instead of leaving the user to find it", async () => {
    ipc.reply = command => command === "remote_backup_create_repo"
      ? Promise.reject({ code: "offline", message: "Offline mode is on.", remedy: "turn_network_on" })
      : Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    fireEvent.click(await screen.findByRole("button", { name: "Create private repo" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Private repository name" }), { target: { value: "agentuna-backup" } });
    fireEvent.click(screen.getByRole("button", { name: "Create repository" }));
    expect(await screen.findByText("Offline: turn off offline mode before contacting GitHub.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show offline mode" }));
    expect(document.activeElement).toBe(screen.getByRole("switch", { name: "Offline mode" }));
    fireEvent.click(screen.getByRole("button", { name: "Open Network settings" }));
    expect(document.activeElement).toBe(screen.getByRole("switch", { name: "Offline mode" }));
  });

  test("TU-019 AC1: every section can render four states (loading, empty, error, populated)", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    const { rerender } = renderPreferences({
      sectionStates: {
        general: "loading",
        health: "empty",
        backups: "error",
        network: "populated",
      },
    });
    expect(await screen.findByRole("heading", { name: "Loading General…" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "No health settings" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Couldn't load backups" })).toBeTruthy();
    expect(screen.getByLabelText("Marketplace access")).toBeTruthy();

    rerender(
      <ToastProvider>
        <PreferencesScreen
          sectionStates={{
            general: "populated",
            health: "populated",
            backups: "populated",
            storage: "loading",
            reset: "error",
          }}
        />
      </ToastProvider>,
    );
    expect(await screen.findByLabelText("Default link mode")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Loading Storage…" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Couldn't load reset" })).toBeTruthy();
  });

  test("TU-019 AC2/AC4: marketplace toggle persists and updates network policy", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    const toggle = await screen.findByTestId("marketplace-toggle");
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(toggle.textContent).toBe("Off");
    fireEvent.click(toggle);
    expect(
      ipc.calls.some(
        call =>
          call.command === "settings_set" &&
          call.args &&
          (call.args as Record<string, unknown>).key === "network.marketplace_enabled" &&
          (call.args as Record<string, unknown>).value === true,
      ),
    ).toBe(true);
    expect(await screen.findByText(/No automatic network call occurs/)).toBeTruthy();
    expect(screen.getByText(/Offline mode disables network actions/)).toBeTruthy();
  });

  test("TU-019 AC2: native banner un-hide restores dismissed banner per kind", async () => {
    const settingsWithHiddenBanner = {
      schema_version: 1,
      values: {
        "native_banner_hidden.skill": true,
        "native_banner_hidden.mcp_server": false,
      },
    };
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settingsWithHiddenBanner : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences();
    const unhideButton = await screen.findByRole("button", { name: "Unhide Skills banner" });
    expect(unhideButton).toBeTruthy();
    fireEvent.click(unhideButton);
    expect(
      ipc.calls.some(
        call =>
          call.command === "settings_set" &&
          call.args &&
          (call.args as Record<string, unknown>).key === "native_banner_hidden.skill" &&
          (call.args as Record<string, unknown>).value === false,
      ),
    ).toBe(true);
  });

  test("TU-019 AC2: link controls are disabled with reason when symlinks unsupported and copy stays selected", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    renderPreferences({
      symlinksSupported: false,
      agentSymlinkSupport: { codex: false },
    });
    const defaultModeSelect = (await screen.findByLabelText("Default link mode")) as HTMLSelectElement;
    const linkOption = defaultModeSelect.querySelector('option[value="link"]') as HTMLOptionElement;
    expect(linkOption.disabled).toBe(true);
    expect(linkOption.textContent).toContain("symlinks unsupported on target");
    expect(defaultModeSelect.value).toBe("copy");
    expect(screen.getAllByText("Target cannot create symlinks; copy stays selected.").length).toBeGreaterThanOrEqual(1);

    // Now check agent mode
    const agentInput = screen.getByLabelText("Agent id");
    fireEvent.change(agentInput, { target: { value: "codex" } });
    const agentModeSelect = screen.getByLabelText("Link mode for agent") as HTMLSelectElement;
    const agentLinkOption = agentModeSelect.querySelector('option[value="link"]') as HTMLOptionElement;
    expect(agentLinkOption.disabled).toBe(true);
    expect(agentModeSelect.value).toBe("copy");
  });

  test("populated Preferences and embedded Logs have no axe violations", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    const { container } = renderPreferences();
    await screen.findByLabelText("Health interval");
    expect((await axe(container)).violations).toEqual([]);
  });
});
