import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { AgentsScreen } from "./AgentsScreen";

const ipc = vi.hoisted(() => ({
  calls: [] as { command: string; args: unknown }[],
  reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => {
    ipc.calls.push({ command, args });
    return ipc.reply(command, args);
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
    <AppStateProvider state={createAppState()}>
      <ToastProvider>
        <AgentsScreen />
      </ToastProvider>
    </AppStateProvider>,
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

  test("renders the nothing-deployed hint instead of zero counts", async () => {
    const fresh = {
      ...claude,
      id: "omp",
      name: "omp",
      health: "~/.agents/skills · 0 / 0 managed · 0 in sync",
      paths: [],
      managed_skills: 0,
      managed_servers: 0,
      native_skills: 0,
      native_servers: 0,
      copy_fallback: false,
      primary_path: "/sandbox/home/.agents/skills",
      in_sync: 0,
      total: 0,
      linked_deployments: 0,
    };
    ipc.reply = (command) => Promise.resolve(command === "agents_list" ? [fresh] : undefined);
    renderScreen();
    expect(await screen.findByText("· Nothing deployed yet")).toBeTruthy();
    expect(screen.queryByText("~/.agents/skills · 0 / 0 managed · 0 in sync")).toBeNull();
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
    const toggle = await screen.findByRole("switch", { name: "Let AgenTuna manage this agent" });
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
    await screen.findByRole("switch", { name: "Let AgenTuna manage this agent" });
    fireEvent.click(screen.getByRole("button", { name: "Rescan all agents" }));
    expect(await screen.findByText(/Rescan failed: detection failed/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(scans).toBe(2));
  });
  test("mounts the typed custom-agent dialog and inserts the created card", async () => {
    const custom = {
      ...cursor,
      id: "custom:windsurf",
      name: "WindSurf",
      installed: true,
      managed: true,
      paths: ["/sandbox/windsurf/skills"],
      health: "1 skill · 0 servers",
      format: "MCP not supported",
      presence: "installed",
      primary_path: "/sandbox/windsurf/skills",
      needs_path: false,
    };
    ipc.reply = (command, args) => {
      if (command === "agents_list") return Promise.resolve([claude]);
      if (command === "custom_agent_preview") {
        const request = args as { request: { display_name: string; root: string } };
        const valid = Boolean(request.request.display_name.trim() && request.request.root.startsWith("/"));
        return Promise.resolve({
          skills_path: valid ? `${request.request.root}/skills` : null,
          mcp_path: null,
          rule_paths: valid ? [`${request.request.root}/AGENTS.md`] : [],
          valid,
          reason: valid ? null : "Enter a name and absolute root folder.",
          existing_agent_id: null,
        });
      }
      if (command === "custom_agent_create") return Promise.resolve(custom);
      return Promise.resolve(undefined);
    };
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: "Add custom agent" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent name" }), { target: { value: "WindSurf" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Custom agent root" }), { target: { value: "/sandbox/windsurf" } });
    expect(await screen.findByText("/sandbox/windsurf/skills")).toBeTruthy();
    await waitFor(() => expect((screen.getByRole("button", { name: "Add agent" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Add agent" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add custom agent" })).toBeNull());
    expect(await screen.findByRole("article", { name: /WindSurf/ })).toBeTruthy();
    expect(ipc.calls.some(({ command }) => command === "custom_agent_preview")).toBe(true);
    expect(ipc.calls.some(({ command }) => command === "custom_agent_create")).toBe(true);
  });

  test("TA-058 AC3: Link->Copy switch persists via agent_link_mode_set and shows migration offer toast", async () => {
    let currentAgent = {
      ...claude,
      link_mode: "link",
      linked_deployments: 2,
    };
    ipc.reply = (command) => {
      if (command === "agents_list") return Promise.resolve([currentAgent]);
      if (command === "agent_link_mode_set") {
        currentAgent = { ...currentAgent, link_mode: "copy" };
        return Promise.resolve(undefined);
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const modeGroup = await screen.findByRole("radiogroup", { name: "Claude Code deployment mode" });
    const copyRadio = within(modeGroup).getByRole("radio", { name: "Copy" });
    fireEvent.click(copyRadio);

    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "agent_link_mode_set")).toBe(true),
    );
    expect(ipc.calls.find((call) => call.command === "agent_link_mode_set")?.args).toMatchObject({
      agentId: "claude_code",
      mode: "copy",
    });

    // Toast informs user of migrating existing links
    expect(
      await screen.findByText("2 existing links will migrate to copies on the next deploy."),
    ).toBeTruthy();
  });

  test("TA-058 AC3: singular migration offer toast when exactly 1 linked deployment exists", async () => {
    let currentAgent = {
      ...claude,
      id: "cursor",
      name: "Cursor",
      link_mode: "link",
      linked_deployments: 1,
    };
    ipc.reply = (command) => {
      if (command === "agents_list") return Promise.resolve([currentAgent]);
      if (command === "agent_link_mode_set") {
        currentAgent = { ...currentAgent, link_mode: "copy" };
        return Promise.resolve(undefined);
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const modeGroup = await screen.findByRole("radiogroup", { name: "Cursor deployment mode" });
    const copyRadio = within(modeGroup).getByRole("radio", { name: "Copy" });
    fireEvent.click(copyRadio);

    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "agent_link_mode_set")).toBe(true),
    );
    expect(
      await screen.findByText("1 existing link will migrate to copies on the next deploy."),
    ).toBeTruthy();
  });
});
