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
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
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
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
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

describe("EffectiveScreen change-it-for-this-repo controls", () => {
  test("disables an inherited skill in this repo through the reviewed dialog", async () => {
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve(unlocked);
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      if (command === "skill_disable_in_repo_preview") {
        return Promise.resolve({
          preview_id: "preview-disable",
          name: "Example",
          scope: "global",
          override_path: "/sandbox/project/.config-studio/disabled-skills.json",
          links: ["/sandbox/project/.agents/skills/example"],
          agents: ["Codex", "omp"],
          backup_location: "/sandbox/backups",
        });
      }
      if (command === "skill_disable_in_repo_apply") {
        return Promise.resolve({ operation_id: "op-1", backup_location: "/sandbox/backups", agents_disabled: 2 });
      }
      return Promise.resolve({ skills: 1, mcp_servers: 0, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const disable = within(section).getByRole("button", { name: "Disable in this repo" }) as HTMLButtonElement;
    expect(disable.disabled).toBe(false);
    fireEvent.click(disable);
    const dialog = await screen.findByRole("dialog", { name: "Disable in this repo" });
    fireEvent.click(await within(dialog).findByRole("button", { name: "Disable in repo" }));
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "skill_disable_in_repo_apply")).toBe(true));
  });

  test("copies an inherited MCP server into the project through the reviewed dialog", async () => {
    const mcpRow = {
      ...unlocked.items[0]!,
      logical_id: "item:itm_mcp",
      item_id: "itm_mcp",
      kind: "mcp_server",
      name: "Web Search",
      winner_scope: "user",
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [mcpRow] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      if (command === "mcp_server_copy_preview") {
        return Promise.resolve({
          preview_id: "preview-copy",
          name: "Web Search",
          source_scope: "user",
          target_scope: "project",
          source_slug: "web-search",
          target_slug: "web-search",
          target_item_id: "itm_copy",
          replaces_existing: false,
          backup_location: "/sandbox/backups",
          files: ["/sandbox/project/.agentuna/mcp.json", "/sandbox/project/mcp.json"],
          agents: [{ agent: "omp", path: "/sandbox/project/mcp.json", effect: "edit" }],
          refusals: [{ agent: "omp", reason: "omp: refused secret `API_TOKEN` for field `TOKEN`" }],
          warnings: [],
          collision: null,
        });
      }
      if (command === "mcp_server_copy_apply") {
        return Promise.resolve({
          operation_id: "op-copy",
          target_item_id: "itm_copy",
          target_slug: "web-search",
          files_changed: 2,
          backup_location: "/sandbox/backups",
        });
      }
      return Promise.resolve({ skills: 0, mcp_servers: 1, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const override = within(section).getByRole("button", { name: "Override in Project" }) as HTMLButtonElement;
    expect(override.disabled).toBe(false);
    fireEvent.click(override);

    const dialog = await screen.findByRole("dialog", { name: "Copy Web Search to project?" });
    const usable = await within(dialog).findByRole("button", { name: "Copy server" });
    expect(within(dialog).getByText("/sandbox/project/mcp.json")).toBeTruthy();
    expect(within(dialog).getByText(/refused secret `API_TOKEN`/)).toBeTruthy();
    fireEvent.click(usable);
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "mcp_server_copy_apply")).toBe(true));
  });

  test("a Global lock explains itself and disables both repo controls", async () => {
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve(locked);
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      return Promise.resolve({ skills: 1, mcp_servers: 0, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    expect(within(section).getByText("Locked: Approved version")).toBeTruthy();
    for (const label of ["Disable in this repo", "Override in Project", "Edit User definition"]) {
      const control = within(section).getByRole("button", { name: label }) as HTMLButtonElement;
      expect(control.disabled).toBe(true);
      expect(control.getAttribute("title")).toBe("Approved version");
    }
  });

  test("navigates to the owning skill editor in User scope when clicking Edit User definition", async () => {
    const userSkill = {
      ...unlocked.items[0]!,
      logical_id: "item:itm_user_skill",
      item_id: "itm_user_skill",
      kind: "skill",
      name: "Triage Skill",
      winner_scope: "user",
      source_path: "/sandbox/user/.agentuna/skills/triage/SKILL.md",
      lock: null,
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [userSkill] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      return Promise.resolve({ skills: 1, mcp_servers: 0, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const editBtn = within(section).getByRole("button", { name: "Edit User definition" }) as HTMLButtonElement;
    expect(editBtn.disabled).toBe(false);
    fireEvent.click(editBtn);

    expect(appState.getSnapshot().route).toBe("skill-detail");
    expect(appState.getSnapshot().scope).toBe("user");
    expect(appState.getSnapshot().screenState["detail:itemId"]).toBe("itm_user_skill");
  });

  test("navigates to the owning MCP server editor in User scope when clicking Edit User definition", async () => {
    const userMcp = {
      ...unlocked.items[0]!,
      logical_id: "item:itm_user_mcp",
      item_id: "itm_user_mcp",
      kind: "mcp_server",
      name: "User DB",
      winner_scope: "user",
      source_path: "/sandbox/user/.agentuna/mcp.json",
      lock: null,
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [userMcp] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      return Promise.resolve({ skills: 0, mcp_servers: 1, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const editBtn = within(section).getByRole("button", { name: "Edit User definition" }) as HTMLButtonElement;
    expect(editBtn.disabled).toBe(false);
    fireEvent.click(editBtn);

    expect(appState.getSnapshot().route).toBe("mcp-detail");
    expect(appState.getSnapshot().scope).toBe("user");
    expect(appState.getSnapshot().screenState["detail:itemId"]).toBe("itm_user_mcp");
  });

  test("navigates to instructions for an inherited rule in User scope when clicking Edit User definition", async () => {
    const userRule = {
      ...unlocked.items[0]!,
      logical_id: "item:itm_user_rule",
      item_id: "itm_user_rule",
      kind: "rule",
      name: "User CLAUDE.md",
      winner_scope: "user",
      source_path: "/sandbox/user/.claude/CLAUDE.md",
      lock: null,
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [userRule] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      return Promise.resolve({ skills: 0, mcp_servers: 0, rules: 1 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const editBtn = within(section).getByRole("button", { name: "Edit User definition" }) as HTMLButtonElement;
    expect(editBtn.disabled).toBe(false);
    // Rule does not offer disable or override in repo
    expect(within(section).queryByRole("button", { name: "Disable in this repo" })).toBeNull();
    expect(within(section).queryByRole("button", { name: "Override in Project" })).toBeNull();

    fireEvent.click(editBtn);
    expect(appState.getSnapshot().route).toBe("instructions");
    expect(appState.getSnapshot().scope).toBe("user");
    expect(appState.getSnapshot().screenState["detail:itemId"]).toBe("itm_user_rule");
  });

  test("disables Edit User definition with reason when read-only", async () => {
    const globalSkill = {
      ...unlocked.items[0]!,
      logical_id: "item:itm_global_ro",
      item_id: "itm_global_ro",
      kind: "skill",
      name: "Global Security",
      winner_scope: "global",
      source_path: "/etc/agentuna/skills/sec",
      lock: null,
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [globalSkill] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "read_only", reason: "Global policy managed by IT" } });
      return Promise.resolve({ skills: 1, mcp_servers: 0, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    const section = await screen.findByRole("region", { name: "Change it for this repo" });
    const editBtn = within(section).getByRole("button", { name: "Edit User definition" }) as HTMLButtonElement;
    expect(editBtn.disabled).toBe(true);
    expect(editBtn.getAttribute("title")).toBe("Global policy managed by IT");
  });

  test("shows loaded agent cards with native path and state matching design", async () => {
    const loadedItem = {
      logical_id: "item:itm_github",
      item_id: "itm_github",
      kind: "mcp_server",
      name: "github",
      winner_scope: "user",
      source_path: "~/.claude.json · ~/.codex/config.toml · ~/.omp/agent/mcp.json",
      agents: [
        { agent: "claude_code", state: "native", loads: true, path: "~/.claude.json" },
        { agent: "codex", state: "native", loads: true, path: "~/.codex/config.toml" },
        { agent: "omp", state: "native", loads: true, path: "~/.omp/agent/mcp.json" },
      ],
      loads_in_agents: 3,
      lock: null,
    };
    ipc.reply = (command) => {
      if (command === "effective_query") return Promise.resolve({ ...unlocked, items: [loadedItem] });
      if (command === "overview_load") return Promise.resolve({ global_store: { status: "available", reason: null } });
      return Promise.resolve({ skills: 0, mcp_servers: 1, rules: 0 });
    };
    render(<AppStateProvider state={appState}><ToastProvider><EffectiveScreen /></ToastProvider></AppStateProvider>);

    expect(await screen.findByText("Loaded for Claude Code, Codex, omp")).toBeTruthy();
    expect(screen.getByText("~/.claude.json")).toBeTruthy();
    expect(screen.getByText("~/.codex/config.toml")).toBeTruthy();
    expect(screen.getByText("~/.omp/agent/mcp.json")).toBeTruthy();
  });
});
