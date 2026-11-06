import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AgentsScreen } from "./AgentsScreen";

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

const claude = {
  id: "claude_code",
  name: "Claude Code",
  installed: true,
  version: "1.2.0",
  managed: true,
  paths: ["/sandbox/home/.claude/skills"],
  health: "~/.claude/skills · 3 / 3 managed · 3 in sync · copy fallback",
  link_mode: "link",
  presence: "installed",
  format: "JSON + Markdown skills",
  managed_skills: 3,
  managed_servers: 0,
  native_skills: 2,
  native_servers: 0,
  copy_fallback: true,
  primary_path: "/sandbox/home/.claude/skills",
  needs_path: false,
  in_sync: 3,
  total: 3,
  linked_deployments: 2,
};
const cursor = {
  ...claude,
  id: "cursor",
  name: "Cursor",
  installed: false,
  version: null,
  managed: false,
  paths: [],
  health: "!<path> · 0 / 0 managed · 0 in sync",
  presence: "not_installed",
  format: "—",
  managed_skills: 0,
  managed_servers: 0,
  native_skills: 0,
  native_servers: 0,
  copy_fallback: false,
  primary_path: null,
  needs_path: true,
  in_sync: 0,
  total: 0,
  linked_deployments: 0,
};

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

function renderScreen() {
  return render(
    <ToastProvider>
      <AgentsScreen />
    </ToastProvider>,
  );
}

describe("AgentsScreen", () => {
  test("renders the health line with the copy fallback and managed counts", async () => {
    ipc.reply = (command) => Promise.resolve(command === "agents_list" ? [claude, cursor] : undefined);
    renderScreen();
    expect(
      await screen.findByText("~/.claude/skills · 3 / 3 managed · 3 in sync · copy fallback"),
    ).toBeTruthy();
    expect(screen.getByText("3 skills · 0 servers")).toBeTruthy();
    expect(screen.getByText("+2 skills outside AgenTuna")).toBeTruthy();
    expect(screen.getByText("Missing")).toBeTruthy();
    expect(screen.getByText("Not installed")).toBeTruthy();
  });

  test("Set path sends the chosen folder to agent_path_set", async () => {
    ipc.reply = (command) => {
      if (command === "agents_list") return Promise.resolve([cursor]);
      if (command === "agent_path_candidates") return Promise.resolve([]);
      if (command === "agent_path_validate") {
        return Promise.resolve({
          exists: true,
          is_directory: true,
          structure_warning: null,
          reason: null,
        });
      }
      if (command === "agent_path_choose") return Promise.resolve("/sandbox/Applications/Cursor.app");
      if (command === "agent_path_set") {
        return Promise.resolve({ ...cursor, presence: "installed", primary_path: "/sandbox/Applications/Cursor.app" });
      }
      if (command === "agents_rescan") {
        return Promise.resolve([{ ...cursor, presence: "installed", primary_path: "/sandbox/Applications/Cursor.app" }]);
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: "Set path…" }));
    fireEvent.click(await screen.findByRole("button", { name: /Choose folder|Browse/ }));
    await waitFor(() =>
      expect((screen.getByRole("textbox", { name: "Agent configuration path" }) as HTMLInputElement).value).toBe(
        "/sandbox/Applications/Cursor.app",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "agent_path_set")).toBe(true),
    );
    expect(ipc.calls.find((call) => call.command === "agent_path_set")?.args).toMatchObject({
      agentId: "cursor",
      path: "/sandbox/Applications/Cursor.app",
    });
  });

  test("turning management off persists the change and reports affected deployments on", async () => {
    ipc.reply = (command) => {
      if (command === "agents_list" || command === "agents_rescan") return Promise.resolve([claude]);
      return Promise.resolve(undefined);
    };
    renderScreen();
    const toggle = await screen.findByRole("switch", { name: "Manage Claude Code" });
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "agent_manage_set")).toBe(true),
    );
    expect(ipc.calls.find((call) => call.command === "agent_manage_set")?.args).toMatchObject({
      agentId: "claude_code",
      managed: false,
    });
    expect((await screen.findAllByText(/stopped managing Claude Code/)).length).toBeGreaterThan(0);
  });

  test("a failed rescan shows an inline error with Retry", async () => {
    let scans = 0;
    ipc.reply = (command) => {
      if (command === "agents_list") return Promise.resolve([claude]);
      if (command === "agents_rescan") {
        scans += 1;
        return scans === 1 ? Promise.reject(new Error("detection failed")) : Promise.resolve([claude]);
      }
      return Promise.resolve(undefined);
    };
    renderScreen();
    await screen.findByRole("switch", { name: "Manage Claude Code" });
    fireEvent.click(screen.getByRole("button", { name: "Rescan all agents" }));
    expect(await screen.findByText(/Rescan failed: Error: detection failed/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(scans).toBe(2));
  });
});
