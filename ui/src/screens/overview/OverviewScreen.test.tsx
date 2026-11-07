import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { OverviewScreen } from "./OverviewScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string, _args?: unknown): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command, args); } }));
const appState = createAppState();
appState.restore("project-id", "project");
const payload = {
  scopes: [
    { scope: "global", skills: 1, mcp: 0, rules: 0, locks: 1, last_edited: "2026-11-05T10:00:00Z", last_edited_by: "ui", disabled: 0, store_ready: true, store_root: "/Library/Application Support/AgenTuna/global", native_roots: [], git_uncommitted: null },
    { scope: "user", skills: 2, mcp: 1, rules: 0, locks: 0, last_edited: "2026-11-06T10:00:00Z", last_edited_by: "ui", disabled: 0, store_ready: true, store_root: "/home/tester/.agentuna", native_roots: ["~/.claude", "~/.codex", "~/.omp", "~/.agents"], git_uncommitted: null },
    { scope: "project", skills: 3, mcp: 1, rules: 1, locks: 0, last_edited: null, last_edited_by: null, disabled: 1, store_ready: true, store_root: "./.agentuna", native_roots: ["./.claude", "./.agents", "./.mcp.json", "CLAUDE.md", "AGENTS.md"], git_uncommitted: 1 },
  ],
  effective: { skills: 6, mcp: 2, rules: 1, shadowed_skills: 2, disabled: 1 },
  attention: [{ item_id: "itm_1", title: "Skill shadowed", subtitle: "Project copy wins", kind: "shadowed_skill", screen: "skills", row_id: "itm_1" }],
  attention_total: 1,
  changes: [{ id: "op-1", kind: "skill_added", summary: "Added skill", created_at: "2026-11-06T10:00:00Z", file_count: 2, scope: "project", files: [{ path: "./.agentuna/skills/sample/SKILL.md", scope: "project" }, { path: "./.agentuna/store.json", scope: "project" }] }],
  user_path: "/home/tester/.agentuna",
  global_store: {
    path: "/Library/Application Support/AgenTuna/global",
    source: "settings",
    status: "available",
    reason: null,
    needs_elevation: false,
    locks: [{ item_id: "itm_1", scope: "global", reason: "Org policy" }],
  },
  startup_warnings: [],
};

beforeAll(() => {
  window.scrollTo = vi.fn();
});
afterEach(() => { cleanup(); ipc.calls.length = 0; });
describe("OverviewScreen", () => {
  test("renders real scope summaries, proportional segment labels and direct attention actions", async () => {
    ipc.reply = (command) => Promise.resolve(
      command === "project_sync_status" ? [] :
      command === "skill_detail_load" ? { item_id: "itm_1", name: "Skill shadowed", scope: "user", shadow_chain: ["project", "user"], shadowed_by: "project" } :
      command === "skill_compare_copies" ? { winner: { scope: "project" }, shadow: { scope: "user" }, identical: false, diff: "- old\n+ new" } :
      payload,
    );
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Effective in this repo")).toBeTruthy();
    expect(screen.getByText("6 skills · 2 MCP servers · 1 instruction file")).toBeTruthy();
    expect(screen.getByText("2 skills shadowed by a nearer scope · 1 item disabled in this repo")).toBeTruthy();
    expect(screen.getByLabelText("Global 1 items")).toBeTruthy();
    expect(screen.getByLabelText("User 3 items")).toBeTruthy();
    expect(screen.getByLabelText("Project 5 items")).toBeTruthy();
    expect(screen.getByText("1 locked")).toBeTruthy();
    expect(screen.getByText("/Library/Application Support/AgenTuna/global")).toBeTruthy();
    expect(screen.getByText("~/.claude · ~/.codex · ~/.omp · ~/.agents")).toBeTruthy();
    expect(screen.getByText("git: 1 uncommitted")).toBeTruthy();
    expect(document.querySelector('[aria-label="Scope stores"] article')?.className).toContain("min-h-[196px]");
    // The status word is text, not colour alone.
    expect(screen.getByText("Shadowed")).toBeTruthy();
    screen.getByRole("button", { name: /Added skill/ }).click();
    expect(await screen.findByText("./.agentuna/skills/sample/SKILL.md")).toBeTruthy();
    // The row action runs the same compare command the skill detail screen uses.
    screen.getByRole("button", { name: "Compare for Skill shadowed" }).click();
    const dialog = await screen.findByRole("dialog", { name: "Compare Skill shadowed copies" });
    expect(within(dialog).getByText(/- old/)).toBeTruthy();
    expect(ipc.calls.some((call) => call.command === "skill_compare_copies")).toBe(true);
    expect(ipc.calls[0]?.command).toBe("overview_load");
  });
  test("scope variants use the designed title, subtitle, CTA and summary heading", async () => {
    const variants = [
      { scope: "project" as const, title: "Overview", subtitle: "What your agents load in payments-service. Nearest scope wins: Project, then User, then Global.", cta: "+ Add to this repo", section: "Effective in this repo" },
      { scope: "user" as const, title: "Overview · User scope", subtitle: "What loads for you in every repo. A repo's Project files can still override these.", cta: "+ Add to User", section: "Effective for you, every repo" },
      { scope: "global" as const, title: "Overview · Global scope", subtitle: "What every user and agent on this Mac gets. Locked values can't be overridden below.", cta: "+ Add to Global", section: "Effective for everyone on this Mac" },
    ];
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : payload);
    for (const variant of variants) {
      const state = createAppState();
      state.restore("project-id", variant.scope);
      const view = render(<AppStateProvider state={state}><OverviewScreen projectName="payments-service" /></AppStateProvider>);
      expect(await screen.findByRole("heading", { level: 1, name: variant.title })).toBeTruthy();
      expect(screen.getByText(variant.subtitle)).toBeTruthy();
      expect(screen.getByRole("button", { name: variant.cta })).toBeTruthy();
      expect(screen.getByText(variant.section)).toBeTruthy();
      view.unmount();
      cleanup();
    }
  });
  test("renders the startup warning with its quarantine path", async () => {
    const warning = {
      kind: "database_quarantined",
      message: "The index was damaged and moved to /tmp/agentuna.db.corrupt-20261106T101500Z.",
      path: "/tmp/agentuna.db.corrupt-20261106T101500Z",
    };
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : { ...payload, startup_warnings: [warning] });
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByLabelText("Startup warning")).toBeTruthy();
    expect(screen.getByText(warning.message)).toBeTruthy();
    expect(screen.getByText(warning.path)).toBeTruthy();
  });
  test("shows retry after the database query fails", async () => {
    ipc.reply = () => Promise.reject(new Error("database unavailable"));
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Overview could not load")).toBeTruthy();
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : payload);
    screen.getByRole("button", { name: "Retry" }).click();
    await waitFor(() => expect(screen.getByText("Effective in this repo")).toBeTruthy());
  });
  test("caps attention at five rows per severity until expanded", async () => {
    const rows = [
      ...Array.from({ length: 6 }, (_, index) => ({ item_id: `health-${index}`, title: `Health ${index}`, subtitle: "health", kind: "failing_health", screen: "mcp", row_id: `health-${index}` })),
      ...Array.from({ length: 6 }, (_, index) => ({ item_id: `drift-${index}`, title: `Drift ${index}`, subtitle: "drift", kind: "drift", screen: "drift", row_id: `drift-${index}` })),
      ...Array.from({ length: 6 }, (_, index) => ({ item_id: `other-${index}`, title: `Other ${index}`, subtitle: "other", kind: "missing_agent", screen: "agents", row_id: `other-${index}` })),
    ];
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : { ...payload, attention: rows, attention_total: rows.length });
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Health 0")).toBeTruthy();
    expect(screen.queryByText("Health 5")).toBeNull();
    expect(screen.queryByText("Drift 5")).toBeNull();
    expect(screen.queryByText("Other 5")).toBeNull();
    screen.getByRole("button", { name: "Show all 18" }).click();
    expect(await screen.findByText("Health 5")).toBeTruthy();
    expect(screen.getByText("Drift 5")).toBeTruthy();
    expect(screen.getByText("Other 5")).toBeTruthy();
  });
  test("Review diff and Set path route through the shared dispatcher with the row selected", async () => {
    const rows = [
      { item_id: "itm_drift", title: "Drift detected: config.toml", subtitle: "12 min ago", kind: "drift_event", screen: "drift", row_id: "7" },
      { item_id: null, title: "Agent not managed: Cursor", subtitle: "1.0", kind: "missing_agent", screen: "agents", row_id: "cursor" },
    ];
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : { ...payload, attention: rows, attention_total: rows.length });
    const dispatched: string[] = [];
    const listener = (event: Event) => { dispatched.push((event as CustomEvent<string>).detail); };
    window.addEventListener("agentuna:menu-action", listener);
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);

    expect(await screen.findByText("Drift")).toBeTruthy();
    expect(screen.getByText("Missing")).toBeTruthy();
    screen.getByRole("button", { name: "Review diff for Drift detected: config.toml" }).click();
    expect(appState.getSnapshot().selections.drift).toEqual(["7"]);
    expect(dispatched).toContain("navigate:drift");
    screen.getByRole("button", { name: "Set path for Agent not managed: Cursor" }).click();
    expect(appState.getSnapshot().selections.agents).toEqual(["cursor"]);
    expect(dispatched).toContain("navigate:agents");
    window.removeEventListener("agentuna:menu-action", listener);
  });

  test("TA-051 AC1 & AC2: Global scope read-only banner and writable state", async () => {
    const state = createAppState();
    state.restore("project-id", "global");

    // AC1: Non-writable global store shows read-only banner + action, and lists locks
    const readonlyPayload = {
      ...payload,
      global_store: {
        ...payload.global_store,
        status: "readonly",
        reason: "Managed by IT policy",
      },
    };
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : readonlyPayload);
    render(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);

    expect(await screen.findByText(/Global store is read-only: Managed by IT policy/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Enable global writes" })).toBeTruthy();
    expect(screen.getByText("1 locked")).toBeTruthy();

    cleanup();

    // AC2: Writable global store hides the banner; lock count equals locks count
    const writablePayload = {
      ...payload,
      global_store: {
        ...payload.global_store,
        status: "available",
        reason: null,
      },
    };
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : writablePayload);
    render(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);

    expect(await screen.findByText("Effective for everyone on this Mac")).toBeTruthy();
    expect(screen.queryByText(/Global store is read-only/)).toBeNull();
    expect(screen.getByText("1 locked")).toBeTruthy();
  });

  test("TA-051 AC3: User variant omits admin banner and shows ~/.agentuna path with Copy button", async () => {
    const state = createAppState();
    state.restore("project-id", "user");

    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : payload);
    render(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);

    expect(await screen.findByText("Effective for you, every repo")).toBeTruthy();
    // Admin banner omitted
    expect(screen.queryByText(/Global store is read-only/)).toBeNull();
    // Shows resolved user path
    expect(screen.getByText(/\/home\/tester\/\.agentuna/)).toBeTruthy();

    // Copy path button copies to clipboard
    const copyBtn = screen.getByRole("button", { name: "Copy path" });
    copyBtn.click();
    expect(writeText).toHaveBeenCalledWith("/home/tester/.agentuna");
  });

  test("TA-051 AC4: switching scope remounts variant and preserves scroll position per scope", async () => {
    const state = createAppState();
    state.restore("project-id", "user");

    // Set up a mock scroll container
    const scrollContainer = document.createElement("div");
    scrollContainer.id = "shell-main";
    document.body.appendChild(scrollContainer);

    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : payload);
    const { rerender } = render(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Effective for you, every repo")).toBeTruthy();

    // Simulate scrolling on user scope
    scrollContainer.scrollTop = 180;
    scrollContainer.dispatchEvent(new Event("scroll"));

    // Switch scope to global
    state.receiveScope("global");
    rerender(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);
    await screen.findByText("Effective for everyone on this Mac");

    // Global starts at 0 scroll
    expect(scrollContainer.scrollTop).toBe(0);

    // Scroll on global scope
    scrollContainer.scrollTop = 320;
    scrollContainer.dispatchEvent(new Event("scroll"));

    // Switch back to user scope
    state.receiveScope("user");
    rerender(<AppStateProvider state={state}><OverviewScreen /></AppStateProvider>);
    await screen.findByText("Effective for you, every repo");

    // User scroll position is restored
    expect(scrollContainer.scrollTop).toBe(180);

    scrollContainer.remove();
  });

  test("TA-051 AC5: lock with missing item appears in card's lock count and as Needs attention row", async () => {
    const attentionPayload = {
      ...payload,
      attention: [
        {
          item_id: "itm_missing",
          title: "Locked item not found",
          subtitle: "Org policy lock references missing item",
          kind: "locked_item_not_found",
          screen: "effective",
          row_id: "itm_missing",
        },
      ],
      attention_total: 1,
    };
    ipc.reply = (command) => Promise.resolve(command === "project_sync_status" ? [] : attentionPayload);
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);

    expect(await screen.findByText("Locked item not found")).toBeTruthy();
    expect(screen.getByText("Org policy lock references missing item")).toBeTruthy();
    expect(screen.getByText("Locked")).toBeTruthy();
    expect(screen.getByText("1 locked")).toBeTruthy();
  });
});

test("sets up a missing scope store through preview and apply", async () => {
  const empty = { ...payload, scopes: payload.scopes.map((scope) => ({ ...scope, store_ready: scope.scope !== "project" })) };
  const previewGate = Promise.withResolvers<unknown>();
  const argsFor = (command: string): Record<string, unknown> | null => {
    const args: unknown = ipc.calls.find((call) => call.command === command)?.args;
    // Test harness stores already-parsed invoke arguments; only shape we read is checked below.
    return args !== null && typeof args === "object" ? (args as Record<string, unknown>) : null;
  };
  ipc.reply = (command) => {
    if (command === "overview_setup_preview") return previewGate.promise;
    if (command === "overview_setup_apply") return Promise.resolve({ operation_id: "op-setup", scope: "project", path: "/sandbox/Projects/AgenTuna/.agentuna", files_changed: 2 });
    if (command === "project_sync_status") return Promise.resolve([]);
    return Promise.resolve(empty);
  };
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findByText("This scope has no AgenTuna store yet.")).toBeTruthy();
  const setup = screen.getByRole("button", { name: "Set up the Project scope" });
  setup.click();
  await waitFor(() => expect(setup.getAttribute("aria-busy")).toBe("true"));
  expect(argsFor("overview_setup_preview")?.scope).toBe("project");
  previewGate.resolve({ preview_id: "preview-setup", scope: "project", path: "/sandbox/Projects/AgenTuna/.agentuna", files: [{ path: "skills", action: "mkdir", is_new: true }, { path: "store.json", action: "write", is_new: true }], foreign: [], already_ready: false });
  expect(await screen.findByRole("dialog", { name: "Set up the Project store" })).toBeTruthy();
  expect(screen.getByText("store.json")).toBeTruthy();
  screen.getByRole("button", { name: "Create store" }).click();
  await waitFor(() => expect(argsFor("overview_setup_apply")?.confirmed).toBe(true));
  expect(argsFor("overview_setup_apply")?.previewId).toBe("preview-setup");
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Set up the Project store" })).toBeNull());
});

test("shows an inline error when the scope store cannot be prepared", async () => {
  const empty = { ...payload, scopes: payload.scopes.map((scope) => ({ ...scope, store_ready: false })) };
  ipc.reply = (command) => command === "overview_setup_preview" ? Promise.reject(new Error("not writable")) : Promise.resolve(command === "project_sync_status" ? [] : empty);
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findAllByText("This scope has no AgenTuna store yet.")).toBeTruthy();
  screen.getByRole("button", { name: "Set up the Project scope" }).click();
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByText(/Project store could not be prepared/)).toBeTruthy();
});

test("renders loading skeletons and the scope empty state", async () => {
  ipc.reply = () => new Promise(() => {});
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findByText("Loading overview")).toBeTruthy();
  cleanup();
  ipc.reply = () => Promise.resolve({ scopes: [], attention: [], attention_total: 0, changes: [] });
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findByText("No scope data")).toBeTruthy();
});

describe("ProjectSyncPanel (TC-027)", () => {
  const syncItems = [
    { item_id: "itm_sync", scope: "project", name: "In Sync Skill", label: "in_sync", action: "none" },
    { item_id: "itm_proj_only", scope: "project", name: "Project Only Skill", label: "project_only", action: "push" },
    { item_id: "itm_lib_newer", scope: "project", name: "Library Newer Skill", label: "library_newer", action: "pull" },
    { item_id: "itm_proj_newer", scope: "project", name: "Project Newer Skill", label: "project_newer", action: "push" },
    { item_id: "itm_conflict", scope: "project", name: "Conflicted Skill", label: "conflict", action: "compare" },
  ];

  test("TC-027 AC0: Each of the four labels plus Conflict renders the correct enabled action and no action for In sync", async () => {
    ipc.reply = (command) => {
      if (command === "project_sync_status") return Promise.resolve(syncItems);
      return Promise.resolve(payload);
    };
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);

    const panel = await screen.findByRole("region", { name: "Project and User sync" });
    expect(within(panel).getByText("Project sync")).toBeTruthy();

    // In sync row: label chip exists, but no button
    const inSyncRow = within(panel).getByText("In Sync Skill").closest("li")!;
    expect(within(inSyncRow).getByText("In sync")).toBeTruthy();
    expect(within(inSyncRow).queryByRole("button")).toBeNull();

    // Project only: push action
    const projOnlyRow = within(panel).getByText("Project Only Skill").closest("li")!;
    expect(within(projOnlyRow).getByText("Project only")).toBeTruthy();
    const pushBtn = within(projOnlyRow).getByRole("button", { name: "Push to User library" }) as HTMLButtonElement;
    expect(pushBtn.disabled).toBe(false);

    // Library newer: pull action
    const libNewerRow = within(panel).getByText("Library Newer Skill").closest("li")!;
    expect(within(libNewerRow).getByText("Library newer")).toBeTruthy();
    const pullBtn = within(libNewerRow).getByRole("button", { name: "Pull to Project" }) as HTMLButtonElement;
    expect(pullBtn.disabled).toBe(false);

    // Project newer: push action
    const projNewerRow = within(panel).getByText("Project Newer Skill").closest("li")!;
    expect(within(projNewerRow).getByText("Project newer")).toBeTruthy();
    expect((within(projNewerRow).getByRole("button", { name: "Push to User library" }) as HTMLButtonElement).disabled).toBe(false);

    // Conflict: compare action
    const conflictRow = within(panel).getByText("Conflicted Skill").closest("li")!;
    expect(within(conflictRow).getByText("Conflict")).toBeTruthy();
    const compareBtn = within(conflictRow).getByRole("button", { name: "Compare copies" }) as HTMLButtonElement;
    expect(compareBtn.disabled).toBe(false);
  });

  test("TC-027 AC1: Push and pull confirmations show the diff and the destination backup before writing", async () => {
    const preview = {
      preview_id: "prev_push_1",
      item_id: "itm_proj_only",
      name: "Project Only Skill",
      direction: "push",
      files: [
        { path: "skills/project-only/SKILL.md", action: "write", diff: "+ # Project Only\n+ Description", backup_required: true },
      ],
      missing_source_paths: [],
    };
    ipc.reply = (command) => {
      if (command === "project_sync_status") return Promise.resolve(syncItems);
      if (command === "project_sync_preview") return Promise.resolve(preview);
      if (command === "project_sync_apply") return Promise.resolve({ operation_id: "op-1", files_changed: 1 });
      return Promise.resolve(payload);
    };
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);

    const panel = await screen.findByRole("region", { name: "Project and User sync" });
    const pushBtn = within(panel).getAllByRole("button", { name: "Push to User library" })[0]!;
    fireEvent.click(pushBtn);

    const dialog = await screen.findByRole("dialog", { name: "Review push for Project Only Skill" });
    expect(within(dialog).getByText("1 file changes will be backed up before applying.")).toBeTruthy();
    expect(within(dialog).getByText("skills/project-only/SKILL.md")).toBeTruthy();
    expect(within(dialog).getByText(/\+ # Project Only/)).toBeTruthy();

    const confirmBtn = within(dialog).getByRole("button", { name: "Confirm sync" });
    fireEvent.click(confirmBtn);

    await waitFor(() => expect(ipc.calls.some((c) => c.command === "project_sync_apply")).toBe(true));
    expect(ipc.calls.find((c) => c.command === "project_sync_apply")?.args).toMatchObject({
      previewId: "prev_push_1",
      confirmed: true,
    });
  });

  test("TC-027 AC2: TargetChanged renders in the modal and force re-diffs instead of silently overwriting", async () => {
    let previewCount = 0;
    const preview = {
      preview_id: "prev_pull_1",
      item_id: "itm_lib_newer",
      name: "Library Newer Skill",
      direction: "pull",
      files: [
        { path: "skills/lib-newer/SKILL.md", action: "write", diff: "- old line\n+ new line", backup_required: true },
      ],
      missing_source_paths: [],
    };
    ipc.reply = (command) => {
      if (command === "project_sync_status") return Promise.resolve(syncItems);
      if (command === "project_sync_preview") {
        previewCount += 1;
        return Promise.resolve(preview);
      }
      if (command === "project_sync_apply") {
        return Promise.reject(new Error("TargetChanged: file on disk was modified externally"));
      }
      return Promise.resolve(payload);
    };
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);

    const panel = await screen.findByRole("region", { name: "Project and User sync" });
    const pullBtn = within(panel).getByRole("button", { name: "Pull to Project" });
    fireEvent.click(pullBtn);

    const dialog = await screen.findByRole("dialog", { name: "Review pull for Library Newer Skill" });
    const confirmBtn = within(dialog).getByRole("button", { name: "Confirm sync" });
    fireEvent.click(confirmBtn);

    // TargetChanged error is displayed in the modal
    expect(
      await within(dialog).findByText("The files changed after this preview. Build a fresh diff before continuing."),
    ).toBeTruthy();

    // Confirm sync is gone; Re-diff and review forces re-evaluating the diff
    expect(within(dialog).queryByRole("button", { name: "Confirm sync" })).toBeNull();
    const rediffBtn = within(dialog).getByRole("button", { name: "Re-diff and review" });
    expect(previewCount).toBe(1);
    fireEvent.click(rediffBtn);

    await waitFor(() => expect(previewCount).toBe(2));
    expect(ipc.calls.filter((c) => c.command === "project_sync_preview").length).toBe(2);
  });

  test("TC-027 AC3: Chips recompute after project switch without a restart", async () => {
    const testState = createAppState();
    testState.restore("proj-alpha", "project");
    const alphaItems = [
      { item_id: "itm_alpha", scope: "project", name: "Alpha Skill", label: "project_only", action: "push" },
    ];
    const betaItems = [
      { item_id: "itm_beta", scope: "project", name: "Beta Skill", label: "library_only", action: "pull" },
    ];
    ipc.reply = (command, args) => {
      if (command === "project_sync_status") {
        const pid = typeof args === "object" && args !== null && "projectId" in args && typeof args.projectId === "string" ? args.projectId : null;
        if (pid === "proj-beta") return Promise.resolve(betaItems);
        return Promise.resolve(alphaItems);
      }
      return Promise.resolve(payload);
    };
    const { rerender } = render(<AppStateProvider state={testState}><OverviewScreen /></AppStateProvider>);

    const panel = await screen.findByRole("region", { name: "Project and User sync" });
    expect(await within(panel).findByText("Alpha Skill")).toBeTruthy();
    expect(within(panel).getByText("Project only")).toBeTruthy();

    // Switch project on testState; key={projectId} remounts ProjectSyncPanel
    testState.restore("proj-beta", "project");
    rerender(<AppStateProvider state={testState}><OverviewScreen /></AppStateProvider>);

    const nextPanel = await screen.findByRole("region", { name: "Project and User sync" });
    expect(await within(nextPanel).findByText("Beta Skill")).toBeTruthy();
    expect(within(nextPanel).getByText("Library only")).toBeTruthy();
    expect(within(nextPanel).queryByText("Alpha Skill")).toBeNull();
  });
});
