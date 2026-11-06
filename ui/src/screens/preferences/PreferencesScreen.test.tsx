import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { axe } from "jest-axe";
import { afterEach, describe, expect, test, vi } from "vitest";
import { PreferencesScreen } from "./PreferencesScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command); } }));
const settings = { schema_version: 1, values: {} };
const policy = { offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false, statement: "No automatic network calls.", allowed_hosts: [], actions: [], events: [], capacity: 100 };
const emptyLogs = { path: "/sandbox/logs/agentuna-*.log", lines: [] };
const paths = { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: null, global_store: "/sandbox/global", global_store_source: "default" };

afterEach(() => { cleanup(); ipc.calls.length = 0; });

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
  test("remote backup clears the token field after saving and never loads token contents", async () => {
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
});
  test("populated Preferences and embedded Logs have no axe violations", async () => {
    ipc.reply = command => Promise.resolve(command === "settings_load" ? settings : command === "network_policy" ? policy : command === "logs_tail" ? emptyLogs : paths);
    const { container } = render(<PreferencesScreen />);
    await screen.findByLabelText("Health interval");
    expect((await axe(container)).violations).toEqual([]);
  });
