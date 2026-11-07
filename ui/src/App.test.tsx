import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import "./components/editor/codemirror-test-setup";
import { App } from "./App";

// A plain function, not vi.fn(): the spy wrapper reports rejected results as unhandled.
const tauriEvents = vi.hoisted(() => ({ handlers: new Map<string, (event: { payload: unknown }) => void>() }));
const ipc = vi.hoisted(() => ({
  calls: [] as string[],
  arguments: [] as { readonly command: string; readonly args?: unknown }[],
  reply: (_cmd: string): Promise<unknown> => Promise.resolve(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    ipc.calls.push(cmd);
    ipc.arguments.push({ command: cmd, args });
    return ipc.reply(cmd);
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: (event: { payload: unknown }) => void) => {
    tauriEvents.handlers.set(event, handler);
    return Promise.resolve(() => { tauriEvents.handlers.delete(event); });
  },
}));

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
  ipc.arguments.length = 0;
  tauriEvents.handlers.clear();
});

test("shows name and version returned by the app_info command", async () => {
  ipc.reply = (cmd) =>
    Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "1.2.3" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "remote_backup_settings" ? { device_name: "Visual workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false } :
    { values: {} },
  );
  render(<App />);
  expect(await screen.findByText("v1.2.3 · local")).toBeTruthy();
  // Startup restores settings and project state and checks for pending recovery.
  expect(new Set(ipc.calls)).toEqual(new Set(["app_info", "deep_link_take", "app_menu_set_add_skill_enabled", "settings_load", "projects_list", "overview_load", "recovery_sheet", "agents_list"]));
});

test("shows the backend error instead of a blank screen", async () => {
  ipc.reply = () => Promise.reject("boom");
  render(<App />);
  expect(await screen.findByText("Backend error: boom")).toBeTruthy();
});
test("native Preferences menu action is handed to the UI dispatcher", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "conflicts_list" ? [] :
    cmd === "remote_backup_settings" ? { device_name: "Visual workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false } :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "preferences" }));
  expect(await screen.findByRole("heading", { name: "Preferences" })).toBeTruthy();
});
test("native non-navigation actions use the UI dispatcher and modal state updates native enablement", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "diagnostics_report_copy" ? "Diagnostics report" :
    cmd === "conflicts_list" ? [] :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "run-doctor" }));
  expect(await screen.findByRole("dialog", { name: "Doctor report" })).toBeTruthy();
  expect(screen.getByText("Diagnostics report")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Copy report" })).toBeTruthy();
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "keyboard-shortcuts" }));
  expect(await screen.findByRole("dialog", { name: "Keyboard shortcuts" })).toBeTruthy();
  await waitFor(() => expect(ipc.arguments.some(({ command, args }) =>
    command === "app_menu_set_add_skill_enabled" &&
    typeof args === "object" && args !== null && "enabled" in args && args.enabled === false,
  )).toBe(true));
});
test("native About action shows the version and local-only privacy message", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "2.4.6" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "about" }));
  expect(await screen.findByRole("dialog", { name: "About AgenTuna" })).toBeTruthy();
  expect(screen.getByText("AgenTuna 2.4.6 · No account, no telemetry.")).toBeTruthy();
});
test("About labels an ad-hoc build as a local unsigned build", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "app_signing_status" ? "ad_hoc" :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "about" }));
  expect(await screen.findByText("AgenTuna 0.1.0 · Local build (unsigned) · No account, no telemetry.")).toBeTruthy();
});
test("command palette loads live skill inventory and dispatches the selected item", async () => {
  const detail = {
    item_id: "itm_skill", name: "Nimbus", slug: "nimbus", scope: "user",
    path: "/sandbox/.agentuna/skills/nimbus", markdown: "# Nimbus\n\nLive skill detail.",
    description: "Searchable fixture", hash: "abc123", updated_at: "2026-11-06T10:00:00Z",
    source: null, mode: "copy", shadowed_by: null, shadow_chain: [], deployments: [], history: [],
    missing: false, editable: true, update_available: false, source_commit_sha: null,
    source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
  };
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "skills_list" ? { rows: [{
      item_id: "itm_skill", name: "Nimbus", scope: "user", state: "active", shadowed_by: null,
      description: "Searchable fixture", native: false, native_path: null, update_available: false,
      updated_at: "2026-11-06T10:00:00Z", locked: false, lock_scope: null, lock_reason: null, agents: [],
    }], agents: [], project_id: null, project_name: null } :
    cmd === "mcp_server_list" ? { servers: [], project_id: null, project_name: null, project_agents: [], global_writable: false, global_reason: null } :
    cmd === "instructions_list" || cmd === "agents_list" ? [] :
    cmd === "skill_detail_load" ? detail :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  fireEvent(window, new CustomEvent("agentuna:menu-action", { detail: "command-palette" }));
  const dialog = await screen.findByRole("dialog", { name: "Command palette" });
  await waitFor(() => expect(ipc.calls).toEqual(expect.arrayContaining(["skills_list", "mcp_server_list", "instructions_list", "agents_list"])));
  fireEvent.change(within(dialog).getByRole("combobox", { name: "Search commands" }), { target: { value: "Nimbus" } });
  const option = await within(dialog).findByRole("option", { name: /Nimbus/ });
  fireEvent.click(option);
  expect(await screen.findByText("Live skill detail.")).toBeTruthy();
  expect(ipc.arguments).toContainEqual({
    command: "skill_detail_load",
    args: { itemId: "itm_skill" },
  });
});
test("restores the last project, persists a changed project, and prompts before dirty navigation", async () => {
  const projects = [
    { id: "project-alpha", name: "AgenTuna alpha", path: "/sandbox/alpha", last_used_at: null, skill_count: 0, server_count: 0 },
    { id: "project-beta", name: "AgenTuna beta", path: "/sandbox/beta", last_used_at: null, skill_count: 0, server_count: 0 },
  ];
  const rule = {
    item_id: "itm_rule", scope: "user", path: "/sandbox/user/AGENTS.md", managed: false,
    content: "# Personal rules\n", hash: "c".repeat(64), agents: ["codex"], read_error: null,
  };
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? projects :
    cmd === "settings_load" ? { schema_version: 1, values: { last_project_id: "project-alpha", "last_scope.project-alpha": "project" } } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "instructions_list" ? [rule] :
    null,
  );
  render(<App />);
  expect(await screen.findByRole("button", { name: "Project: AgenTuna alpha. Choose a different project" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Project: AgenTuna alpha. Choose a different project" }));
  const picker = await screen.findByRole("dialog", { name: "Switch project" });
  fireEvent.click(await within(picker).findByRole("option", { name: /AgenTuna beta/ }));
  await screen.findByRole("button", { name: "Project: AgenTuna beta. Choose a different project" });
  await waitFor(() => expect(ipc.arguments).toContainEqual({
    command: "settings_set",
    args: { key: "last_project_id", value: "project-beta" },
  }));

  act(() => window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: "navigate:instructions" })));
  const editor = await screen.findByRole("textbox", { name: "Instruction content" });
  await userEvent.click(editor);
  await userEvent.type(editor, "updated");
  expect(await screen.findByText(/Unsaved changes/)).toBeTruthy();
  act(() => window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: "navigate:overview" })));
  const prompt = await screen.findByRole("dialog", { name: "Unsaved changes" });
  expect(within(prompt).getByRole("button", { name: "Cancel" })).toBeTruthy();
  expect(within(prompt).getByRole("button", { name: "Discard" })).toBeTruthy();
  expect(within(prompt).getByRole("button", { name: "Save" })).toBeTruthy();
});
test("external watcher events coalesce and refresh the active skills snapshot", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "skills_list" ? { rows: [], agents: [], project_id: null, project_name: null } :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("agentuna:external-refresh")).toBe(true));
  act(() => window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: "navigate:skills" })));
  await waitFor(() => expect(ipc.calls.filter((call) => call === "skills_list")).toHaveLength(1));
  act(() => {
    for (let index = 0; index < 200; index += 1) {
      tauriEvents.handlers.get("agentuna:external-refresh")?.({
        payload: { paths: ["/sandbox/user/.agentuna/skills/nimbus/SKILL.md"], full_rescan: false },
      });
    }
  });
  await waitFor(() => expect(ipc.calls.filter((call) => call === "skills_list")).toHaveLength(2));
  expect(ipc.calls.filter((call) => call === "skills_list").length).toBeLessThanOrEqual(3);
});
test("deep links resolve known items and keep the current screen when an item is missing", async () => {
  const detail = {
    item_id: "itm_linked", name: "Linked skill", slug: "linked-skill", scope: "user",
    path: "/sandbox/user/skills/linked-skill", markdown: "# Linked skill\n\nResolved deep link.",
    description: "Deep link fixture", hash: "abc123", updated_at: "2026-11-06T10:00:00Z",
    source: null, mode: "copy", shadowed_by: null, shadow_chain: [], deployments: [], history: [],
    missing: false, editable: true, update_available: false, source_commit_sha: null,
    source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
  };
  let lookupFound = false;
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "item_lookup" ? lookupFound ? { kind: "skill", scope: "user", project_id: null } : null :
    cmd === "skill_detail_load" ? detail :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("deep-link")).toBe(true));
  act(() => tauriEvents.handlers.get("deep-link")?.({ payload: "agentuna://item/missing" }));
  expect(await screen.findAllByText("Item not found")).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Overview" }).getAttribute("aria-current")).toBe("page");
  lookupFound = true;
  act(() => tauriEvents.handlers.get("deep-link")?.({ payload: "agentuna://item/itm_linked" }));
  expect(await screen.findByText("Resolved deep link.")).toBeTruthy();
  expect(ipc.arguments).toContainEqual({ command: "item_lookup", args: { itemId: "itm_linked" } });
});

test("a cold-start URL opens from the buffered deep link take", async () => {
  const detail = {
    item_id: "itm_cold", name: "Cold start skill", slug: "cold-start-skill", scope: "user",
    path: "/sandbox/user/skills/cold-start-skill", markdown: "# Cold start skill\n\nResolved cold start deep link.",
    description: "Cold start fixture", hash: "abc123", updated_at: "2026-11-06T10:00:00Z",
    source: null, mode: "copy", shadowed_by: null, shadow_chain: [], deployments: [], history: [],
    missing: false, editable: true, update_available: false, source_commit_sha: null,
    source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
  };
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "deep_link_take" ? ["agentuna://item/itm_cold"] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "item_lookup" ? { kind: "skill", scope: "user", project_id: null } :
    cmd === "skill_detail_load" ? detail :
    null,
  );
  render(<App />);
  expect(await screen.findByText("Resolved cold start deep link.")).toBeTruthy();
  expect(ipc.arguments).toContainEqual({ command: "item_lookup", args: { itemId: "itm_cold" } });
});
test("project-scoped deep link opens the picker and resumes the requested item after selection", async () => {
  const detail = {
    item_id: "itm_project", name: "Project skill", slug: "project-skill", scope: "project",
    path: "/sandbox/project/.agents/skills/project-skill", markdown: "# Project skill\n\nSelected after picker.",
    description: "Project fixture", hash: "abc123", updated_at: "2026-11-06T10:00:00Z",
    source: null, mode: "copy", shadowed_by: null, shadow_chain: [], deployments: [], history: [],
    missing: false, editable: true, update_available: false, source_commit_sha: null,
    source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
  };
  const projects = [{ id: "project-alpha", name: "AgenTuna alpha", path: "/sandbox/alpha", last_used_at: null, skill_count: 0, server_count: 0 }];
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? projects :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "item_lookup" ? { kind: "skill", scope: "project", project_id: null } :
    cmd === "project_sync_status" ? [] :
    cmd === "skill_detail_load" ? detail :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("deep-link")).toBe(true));
  act(() => tauriEvents.handlers.get("deep-link")?.({ payload: "agentuna://item/itm_project" }));
  const picker = await screen.findByRole("dialog", { name: "Switch project" });
  expect(ipc.calls).not.toContain("skill_detail_load");
  fireEvent.click(await within(picker).findByRole("option", { name: /AgenTuna alpha/ }));
  expect(await screen.findByText("Selected after picker.")).toBeTruthy();
  expect(ipc.arguments).toContainEqual({ command: "skill_detail_load", args: { itemId: "itm_project" } });
});
test("Add menu disables agent-dependent actions but opens the rule flow in the active scope", async () => {
  const projects = [{ id: "project-alpha", name: "AgenTuna alpha", path: "/sandbox/alpha", last_used_at: null, skill_count: 0, server_count: 0 }];
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? projects :
    cmd === "settings_load" ? { schema_version: 1, values: { last_project_id: "project-alpha", "last_scope.project-alpha": "project" } } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: "Managed by your organization", needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "agents_list" ? [] :
    cmd === "instructions_list" ? [] :
    cmd === "instruction_create_options" ? { global_writable: false, global_reason: "Managed centrally", local_available: true, local_reason: null, has_project: true } :
    cmd === "instruction_create_preview" ? { path: "/sandbox/alpha/AGENTS.md", exists: false, reason: null } :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  const addButton = await screen.findByRole("button", { name: "Add" });
  await userEvent.click(addButton);
  const skillItem = await screen.findByRole("menuitem", { name: /New skill/ });
  const serverItem = screen.getByRole("menuitem", { name: /Add MCP server/ });
  await waitFor(() => {
    expect(skillItem.getAttribute("aria-disabled")).toBe("true");
    expect(serverItem.getAttribute("aria-disabled")).toBe("true");
  });
  expect(screen.getByRole("menuitem", { name: /New rule file/ }).getAttribute("aria-disabled")).not.toBe("true");
  await userEvent.click(screen.getByRole("menuitem", { name: /New rule file/ }));
  const dialog = await screen.findByRole("dialog", { name: "New rule file" });
  await waitFor(() => expect(ipc.arguments).toContainEqual({
    command: "instruction_create_preview",
    args: { fileType: "CLAUDE.md", scope: "project", projectId: "project-alpha" },
  }));
  expect(within(dialog).getByRole("button", { name: "Create CLAUDE.md in project scope" })).toBeTruthy();
});

test("Add MCP server form starts in the active scope", async () => {
  const projects = [{ id: "project-alpha", name: "AgenTuna alpha", path: "/sandbox/alpha", last_used_at: null, skill_count: 0, server_count: 0 }];
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? projects :
    cmd === "settings_load" ? { schema_version: 1, values: { last_project_id: "project-alpha", "last_scope.project-alpha": "project" } } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] }, startup_warnings: [] } :
    cmd === "agents_list" ? [{ id: "codex", installed: true }] :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: /Add MCP server/ }));
  const form = screen.getByRole("main", { name: "Add MCP server" });
  const projectScope = within(form).getByRole("radio", { name: "Project" });
  expect(projectScope instanceof HTMLInputElement && projectScope.checked).toBe(true);
});

test("Global read-only state disables every Add action with the backend reason", async () => {
  const projects = [{ id: "project-alpha", name: "AgenTuna alpha", path: "/sandbox/alpha", last_used_at: null, skill_count: 0, server_count: 0 }];
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? projects :
    cmd === "settings_load" ? { schema_version: 1, values: { last_project_id: "project-alpha", "last_scope.project-alpha": "global" } } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: "Managed by your organization", needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "agents_list" ? [{ id: "codex", installed: true }] :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  await waitFor(() => expect(ipc.arguments).toContainEqual({
    command: "overview_load",
    args: { activeScope: "global" },
  }));
  for (const label of [/New skill/, /Add MCP server/, /New rule file/]) {
    expect(screen.getByRole("menuitem", { name: label }).getAttribute("aria-disabled")).toBe("true");
  }
  expect(screen.getAllByText("Unavailable: Managed by your organization")).toHaveLength(3);
});
test("CLI Help action reviews exact install paths, reports install, and warns on foreign uninstall", async () => {
  const bundledBinary = "/Applications/AgenTuna.app/Contents/Resources/agentuna";
  const fallbackPath = "/Users/test/.local/bin/agentuna";
  const cliStatus = {
    bundled_version: "1.2.3",
    bundled_binary: bundledBinary,
    primary: { directory: "/usr/local/bin", link_path: "/usr/local/bin/agentuna", writable: false, on_path: false, path_export: null },
    fallback: { directory: "/Users/test/.local/bin", link_path: fallbackPath, writable: true, on_path: false, path_export: 'export PATH="$HOME/.local/bin:$PATH"' },
    installed: null,
  };
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "1.2.3" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
    cmd === "cli_install_status" ? cliStatus :
    cmd === "cli_install" ? { link_path: fallbackPath, target: bundledBinary, created: true, reported_version: "agentuna 1.2.3", matches_app_version: true } :
    cmd === "cli_uninstall" ? { link_path: "/usr/local/bin/agentuna", removed: false, warning: "The foreign symlink was left untouched." } :
    null,
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "install-cli" }));
  const dialog = await screen.findByRole("dialog", { name: "Install command-line tool" });
  expect(within(dialog).getByText("Not writable; use the fallback location.")).toBeTruthy();
  expect(within(dialog).getByText('export PATH="$HOME/.local/bin:$PATH"')).toBeTruthy();
  const installButtons = within(dialog).getAllByRole("button", { name: "Install" });
  expect((installButtons[0] as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(installButtons[1]!);
  const confirmation = await screen.findByRole("dialog", { name: "Confirm CLI installation" });
  expect(within(confirmation).getByText(fallbackPath)).toBeTruthy();
  expect(within(confirmation).getByText(bundledBinary)).toBeTruthy();
  expect(within(confirmation).getByText('export PATH="$HOME/.local/bin:$PATH"')).toBeTruthy();
  fireEvent.click(within(confirmation).getByRole("button", { name: "Install" }));
  expect(await within(dialog).findByText("CLI installed")).toBeTruthy();
  expect(within(dialog).getByText(/Version: agentuna 1\.2\.3 \(matches this app\)/)).toBeTruthy();
  expect(ipc.arguments).toContainEqual({ command: "cli_install", args: { location: "fallback" } });
  fireEvent.click(within(dialog).getAllByRole("button", { name: "Uninstall" })[0]!);
  const uninstallConfirmation = await screen.findByRole("dialog", { name: "Confirm CLI uninstall" });
  expect(within(uninstallConfirmation).getByText("/usr/local/bin/agentuna")).toBeTruthy();
  expect(within(uninstallConfirmation).getByText(/Only a symlink owned by AgenTuna is removed/)).toBeTruthy();
  fireEvent.click(within(uninstallConfirmation).getByRole("button", { name: "Uninstall" }));
  expect(await within(dialog).findByText("The foreign symlink was left untouched.")).toBeTruthy();
  expect(ipc.arguments).toContainEqual({ command: "cli_uninstall", args: { location: "primary" } });
});

test("CLI install failures stay visible and return to the alternate location chooser", async () => {
  const cliStatus = {
    bundled_version: "1.2.3",
    bundled_binary: "/Applications/AgenTuna.app/Contents/Resources/agentuna",
    primary: { directory: "/usr/local/bin", link_path: "/usr/local/bin/agentuna", writable: true, on_path: true, path_export: null },
    fallback: { directory: "/Users/test/.local/bin", link_path: "/Users/test/.local/bin/agentuna", writable: true, on_path: false, path_export: 'export PATH="$HOME/.local/bin:$PATH"' },
    installed: null,
  };
  ipc.reply = (cmd) => cmd === "cli_install"
    ? Promise.reject({ message: "A foreign file already exists at /usr/local/bin/agentuna." })
    : Promise.resolve(
      cmd === "app_info" ? { name: "AgenTuna", version: "1.2.3" } :
      cmd === "projects_list" ? [] :
      cmd === "settings_load" ? { schema_version: 1, values: {} } :
      cmd === "recovery_sheet" ? null :
      cmd === "conflicts_list" ? [] :
      cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "read_only", reason: null, needs_elevation: true, locks: [] }, startup_warnings: [] } :
      cmd === "cli_install_status" ? cliStatus :
      null,
    );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(tauriEvents.handlers.has("menu-action")).toBe(true));
  act(() => tauriEvents.handlers.get("menu-action")?.({ payload: "install-cli" }));
  const dialog = await screen.findByRole("dialog", { name: "Install command-line tool" });
  fireEvent.click(within(dialog).getAllByRole("button", { name: "Install" })[0]!);
  fireEvent.click(within(await screen.findByRole("dialog", { name: "Confirm CLI installation" })).getByRole("button", { name: "Install" }));
  expect((await within(dialog).findByRole("alert")).textContent).toContain("A foreign file already exists at /usr/local/bin/agentuna.");
  expect(within(dialog).getAllByRole("button", { name: "Install" })).toHaveLength(2);
  expect(within(dialog).getByText("/Users/test/.local/bin/agentuna")).toBeTruthy();
});

const keyboardShell = (cmd: string): unknown => (
  cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
  cmd === "projects_list" ? [] :
  cmd === "settings_load" ? { schema_version: 1, values: {} } :
  cmd === "recovery_sheet" ? null :
  cmd === "conflicts_list" ? [] :
  cmd === "skills_list" ? { project_id: null, rows: [] } :
  cmd === "mcp_server_list" ? { project_id: null, servers: [] } :
  cmd === "instructions_list" ? [] :
  cmd === "agents_list" ? [] :
  cmd === "remote_backup_settings" ? { device_name: "Visual workstation", repo_url: null, branch: null, last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: false } :
  null
);

test("Cmd shortcuts work over a focused field, Esc closes one overlay per press, and Cmd+S reports nothing to save", async () => {
  ipc.reply = (cmd) => Promise.resolve(keyboardShell(cmd));
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });

  fireEvent.keyDown(window, { key: ",", metaKey: true });
  expect(await screen.findByRole("heading", { name: "Preferences" })).toBeTruthy();

  screen.getByRole("spinbutton", { name: "Backup retention days" }).focus();
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeTruthy();

  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull());
  expect(screen.getByRole("heading", { name: "Preferences" })).toBeTruthy();

  // A second Escape must not pop the screen stack or close anything else.
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.getByRole("heading", { name: "Preferences" })).toBeTruthy();

  fireEvent.keyDown(window, { key: "s", metaKey: true });
  expect(await screen.findByText("Nothing to save")).toBeTruthy();
  expect(ipc.calls.some((command) => command.includes("save"))).toBe(false);
});

test("Cmd+Shift scope shortcuts select available scopes and refuse project or local without a project", async () => {
  ipc.reply = (cmd) => Promise.resolve(keyboardShell(cmd));
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });

  expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("radio", { name: "Project" }).getAttribute("aria-disabled")).toBe("true");

  fireEvent.keyDown(window, { key: "3", metaKey: true, shiftKey: true });
  fireEvent.keyDown(window, { key: "4", metaKey: true, shiftKey: true });
  expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true");

  fireEvent.keyDown(window, { key: "1", metaKey: true, shiftKey: true });
  await waitFor(() => expect(screen.getByRole("radio", { name: "Global" }).getAttribute("aria-checked")).toBe("true"));

  fireEvent.keyDown(window, { key: "2", metaKey: true, shiftKey: true });
  await waitFor(() => expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true"));
});

test("a locked Global store is disabled with the documented tooltip and its shortcut refuses", async () => {
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "projects_list" ? [{ id: "prj_locked", name: "Locked", path: "/sandbox/project", last_used_at: "2026-11-06T10:00:00Z", skill_count: 1, server_count: 1 }] :
    cmd === "settings_load" ? { schema_version: 1, values: { last_project_id: "prj_locked" } } :
    cmd === "instruction_create_options" ? { global_writable: false, global_reason: "The Global store needs administrator permission.", local_available: true, local_reason: null, has_project: true } :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "unavailable", reason: "The Global store needs administrator permission.", needs_elevation: true, locks: [] }, startup_warnings: [] } :
    keyboardShell(cmd),
  );
  render(<App />);
  await screen.findByRole("navigation", { name: "Workspace" });
  await waitFor(() => expect(screen.getByRole("radio", { name: "Global" }).getAttribute("aria-disabled")).toBe("true"));
  expect(screen.getByRole("radio", { name: "Global" }).getAttribute("title")).toBe("Global store not available");

  fireEvent.keyDown(window, { key: "1", metaKey: true, shiftKey: true });
  expect(screen.getByRole("radio", { name: "Global" }).getAttribute("aria-checked")).toBe("false");
  expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true");

  fireEvent.keyDown(window, { key: "3", metaKey: true, shiftKey: true });
  await waitFor(() => expect(screen.getByRole("radio", { name: "Project" }).getAttribute("aria-checked")).toBe("true"));
  fireEvent.keyDown(window, { key: "4", metaKey: true, shiftKey: true });
  await waitFor(() => expect(screen.getByRole("radio", { name: "Local (only you, this repo)" }).getAttribute("aria-checked")).toBe("true"));
});

test("sidebar lists agents from agents_list and refreshes after Rescan", async () => {
  const claudeCard = { id: "claude_code", name: "Claude Code", presence: "installed", installed: true };
  const cursorMissing = { id: "cursor", name: "Cursor", presence: "not_installed", installed: false };
  const cursorFound = { id: "cursor", name: "Cursor", presence: "installed", installed: true };
  ipc.reply = (cmd) => Promise.resolve(
    cmd === "app_info" ? { name: "AgenTuna", version: "0.1.0" } :
    cmd === "projects_list" ? [] :
    cmd === "settings_load" ? { schema_version: 1, values: {} } :
    cmd === "recovery_sheet" ? null :
    cmd === "conflicts_list" ? [] :
    cmd === "agents_list" ? [claudeCard, cursorMissing] :
    cmd === "agents_rescan" ? [claudeCard, cursorFound] :
    cmd === "overview_load" ? { scopes: [], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] }, startup_warnings: [] } :
    { values: {} },
  );
  render(<App />);

  expect(await screen.findByText("Agents on this Mac")).toBeTruthy();
  expect(await screen.findByText("Claude Code")).toBeTruthy();
  await waitFor(() => expect(screen.getAllByText("not found").length).toBeGreaterThan(0));
  // The shared Rescan action is the same one the menu and palette dispatch.
  fireEvent(window, new CustomEvent("agentuna:menu-action", { detail: "projects-rescan" }));
  await waitFor(() => expect(screen.queryByText("not found")).toBeNull());
  expect(ipc.calls).toContain("agents_rescan");
});
