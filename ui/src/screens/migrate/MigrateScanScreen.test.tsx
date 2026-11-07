import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { ToastProvider } from "../../overlays/toasts";
import { MigrateScanScreen } from "./MigrateScanScreen";

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

const events = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  emit(name: string, payload: unknown) {
    this.listeners.get(name)?.({ payload });
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
    events.listeners.set(name, handler);
    return () => {
      events.listeners.delete(name);
    };
  },
}));

const appState = createAppState();
appState.restore("prj_fixture", "project");

const item = (
  overrides: Partial<{
    kind: string;
    name: string;
    agents: string[];
    path: string;
    scope: string;
    content_hash: string;
    parse_error: string | null;
    store_link: boolean;
    state: string;
    ownership: string;
  }>,
) => ({
  kind: "skill",
  name: "pdf-extract",
  agents: ["claude_code"],
  path: "/sandbox/home/.claude/skills/pdf-extract",
  scope: "project",
  content_hash: "a".repeat(64),
  parse_error: null,
  store_link: false,
  state: "not_in_agentuna",
  ownership: "not_in_agentuna",
  ...overrides,
});

const scan = {
  items: [
    item({ name: "pdf-extract", path: "/sandbox/home/.claude/skills/pdf-extract" }),
    item({
      kind: "mcp_server",
      name: "context7",
      agents: ["codex", "omp"],
      path: "/sandbox/home/.codex/config.toml",
      scope: "user",
      content_hash: "b".repeat(64),
      state: "conflict",
      ownership: "conflict",
    }),
    item({
      name: "in-sync",
      path: "/sandbox/home/.claude/skills/in-sync",
      scope: "user",
      content_hash: "c".repeat(64),
      store_link: true,
      state: "in_agentuna",
      ownership: "managed",
    }),
  ],
  partial: false,
  skipped_roots: ["/sandbox/home/.cursor/skills"],
  warnings: [] as { path: string; kind: string; agent: string; scope: string; message: string }[],
  roots: [
    { path: "/sandbox/home/.claude/skills", kind: "skill", agents: ["claude_code"], found_items: 2, new_items: 1, skipped_items: 0, in_sync: 1, status: "scanned", unreadable_reason: null },
    { path: "/sandbox/home/.codex/config.toml", kind: "mcp_server", agents: ["codex", "omp"], found_items: 1, new_items: 0, skipped_items: 0, in_sync: 1, status: "scanned", unreadable_reason: null },
  ],
};
const conflict = {
  kind: "skill",
  name: "review-helper",
  copies: [
    { agent: "claude_code", origin: "native", path: "/sandbox/home/.claude/skills/review-helper", scope: "user", raw_hash: "a".repeat(64), normalized_hash: "a".repeat(64) },
    { agent: "codex", origin: "native", path: "/sandbox/home/.codex/skills/review-helper", scope: "user", raw_hash: "b".repeat(64), normalized_hash: "b".repeat(64) },
  ],
  formatting_only: false,
  unified_diff: "--- A\n+++ B\n",
  proposed_resolution: "needschoice",
};
const formattingConflict = {
  ...conflict,
  name: "notes",
  formatting_only: true,
  unified_diff: "",
  proposed_resolution: "keepa",
};
const plan = {
  plan_hash: "d".repeat(64),
  backup_dir: "/sandbox/support/backups/migration",
  strategy: "replace_with_links",
  counts: { items: 2, files_created: 4, files_backed_up: 1, links_written: 1, files_left_in_place: 0 },
  scopes: [
    {
      scope: "project",
      store_path: "/sandbox/sandbox-project/.agentuna",
      counts: { items: 2, files_created: 4, files_backed_up: 1, links_written: 1, files_left_in_place: 0 },
      steps: [
        { seq: 1, action: "backuporiginal", scope: "project", item_name: "pdf-extract", source: "/sandbox/home/.claude/skills/pdf-extract", destination: null, backup_path: "/sandbox/backups/migration/pdf-extract", bytes: 12, agents: [] },
        { seq: 2, action: "createstoreitem", scope: "project", item_name: "pdf-extract", source: "/sandbox/home/.claude/skills/pdf-extract", destination: "/sandbox/sandbox-project/.agentuna/skills/pdf-extract", backup_path: null, bytes: 12, agents: ["claude_code"] },
      ],
    },
  ],
  files_checked: 2,
};

/** The reviewed result the command layer retains for a finished apply job. */
const applyResult = {
  job_id: "job_apply",
  operation_id: "01M6TC30WZNDRNXKSJ4TA5BJV4",
  completed: 4n,
  touched_paths: ["/sandbox/sandbox-project/.agentuna/store.json"],
  warnings: [],
  backup_dir: "/sandbox/support/backups/migration",
  scopes: [{ scope: "project", store_path: "/sandbox/sandbox-project/.agentuna", counts: plan.counts }],
  strategy: "replace_with_links",
  plan_hash: "d".repeat(64),
  counts: plan.counts,
  steps: plan.scopes.flatMap((entry) => entry.steps),
};

/** Undo readiness the done screen reads before it offers the undo button. */
const undoReady = {
  operation_id: "01M6TC30WZNDRNXKSJ4TA5BJV4",
  known: true,
  already_undone: false,
  expired: false,
  window_days: 30,
  expires_at_unix_ms: BigInt(Date.now() + 30 * 86_400_000),
  conflicted_paths: [] as string[],
};

/** Replies for the cancellable scan job plus the supplied command outside the scan. */
function scanReply(view: unknown, extra: (command: string) => unknown = () => undefined) {
  return (command: string): Promise<unknown> => {
    if (command === "migrate_scan_start") return Promise.resolve("job_scan_fixture");
    if (command === "migrate_scan_result") return Promise.resolve(view);
    if (command === "jobs_cancel") return Promise.resolve(true);
    return Promise.resolve(extra(command));
  };
}

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
  events.listeners.clear();
});

function renderScreen() {
  return render(
    <AppStateProvider state={appState}>
      <ToastProvider>
        <MigrateScanScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

async function openResolve() {
  await screen.findByText("Found 3 items outside AgenTuna");
  fireEvent.click(screen.getByRole("checkbox", { name: "Select all items" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue with 3" }));
  await screen.findByRole("heading", { name: "Resolve conflicts" });
}

/** Drives the wizard to the plan step when the fixture has no conflicts to resolve. */
async function reachPlan() {
  await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" });
  fireEvent.click(screen.getByRole("checkbox", { name: "Select all items" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue with 3" }));
  await screen.findByRole("heading", { name: "Resolve conflicts" });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await screen.findByRole("heading", { name: "Plan the move" });
}

/** Confirms the reviewed plan through the confirmation modal. */
async function confirmApply() {
  fireEvent.click(screen.getByRole("button", { name: "Apply 2 items" }));
  await screen.findByRole("heading", { name: "Confirm migration" });
  fireEvent.click(screen.getByRole("checkbox", { name: /I understand/ }));
  fireEvent.click(screen.getByRole("button", { name: "Apply migration" }));
}

describe("MigrateScanScreen", () => {
  test("scans through the job and renders folders, rows and authoritative state labels", async () => {
    ipc.reply = scanReply(scan);
    renderScreen();
    expect(await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" })).toBeTruthy();
    const steps = screen.getByRole("navigation", { name: "Migration steps" });
    for (const label of ["Scan", "Resolve", "Plan", "Done"]) {
      expect(steps.textContent).toContain(label);
    }
    expect(ipc.calls.find((call) => call.command === "migrate_scan_start")).toBeTruthy();
    expect(screen.getByRole("complementary", { name: "Scanned folders" }).textContent).toContain(
      "/sandbox/home/.claude/skills",
    );
    expect(screen.getAllByText("1 new").length).toBe(1);
    expect(screen.getByText("1 in sync")).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "Select pdf-extract" })).toBeTruthy();
    // Ownership labels come from the backend `state`, not from agent counts.
    expect(screen.getByText("In AgenTuna")).toBeTruthy();
    expect(screen.getByText("Conflict")).toBeTruthy();
    expect(screen.getAllByText("Not in AgenTuna").length).toBe(1);
    expect(screen.getByText("Symlink")).toBeTruthy();
  });

  test("lists empty, missing and unreadable roots alongside found items", async () => {
    ipc.reply = scanReply({
      ...scan,
      partial: true,
      roots: [
        ...scan.roots,
        { path: "/sandbox/zero", kind: "skill", agents: ["codex", "omp"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "empty", unreadable_reason: null },
        { path: "/sandbox/missing", kind: "rule", agents: ["claude_code"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "not_present", unreadable_reason: null },
        { path: "/sandbox/unreadable", kind: "skill", agents: ["cursor"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "unreadable", unreadable_reason: "permission denied" },
        { path: "/sandbox/escaping", kind: "skill", agents: ["omp"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "unreadable", unreadable_reason: "symbolic link target is outside injected roots" },
        { path: "/sandbox/partial", kind: "skill", agents: ["codex"], found_items: 2, new_items: 1, skipped_items: 1, in_sync: 0, status: "scanned", unreadable_reason: null },
      ],
    });
    renderScreen();
    await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" });
    const folders = within(screen.getByRole("complementary", { name: "Scanned folders" }));
    for (const [path, status] of [
      ["/sandbox/zero", "0 new"],
      ["/sandbox/missing", "not present"],
      ["/sandbox/unreadable", "permission denied"],
      ["/sandbox/escaping", "symbolic link target is outside injected roots"],
      ["/sandbox/partial", "2 found · 1 skipped"],
    ] as const) {
      const row = folders.getByText(path).closest("li");
      expect(row?.textContent).toContain(status);
    }
    const emptyRow = folders.getByText("/sandbox/zero").closest("li")?.textContent ?? "";
    expect(emptyRow).toContain("empty");
    const missingRow = folders.getByText("/sandbox/missing").closest("li")?.textContent ?? "";
    expect(missingRow).toContain("not present");
    expect(missingRow).not.toContain("empty");
    expect(emptyRow).not.toContain("not present");
    expect(folders.getByText("/sandbox/zero").closest("li")?.textContent).toContain("codex, omp");
    expect(folders.getByText("/sandbox/zero").closest("li")?.textContent).toContain("skills");
    fireEvent.click(screen.getByRole("checkbox", { name: "Select pdf-extract" }));
    expect(screen.getByRole("button", { name: "Continue with 1" })).toBeTruthy();
  });

  test("shows per-step progress and cancels the running scan job", async () => {
    let polls = 0;
    ipc.reply = (command) => {
      if (command === "migrate_scan_start") return Promise.resolve("job_scan_live");
      if (command === "migrate_scan_result") {
        polls += 1;
        return Promise.resolve(polls < 4 ? null : scan);
      }
      if (command === "jobs_cancel") return Promise.resolve(true);
      return Promise.resolve(undefined);
    };
    renderScreen();
    await waitFor(() => expect(events.listeners.has("agentuna:job-progress")).toBe(true));
    events.emit("agentuna:job-progress", {
      id: "job_scan_live",
      name: "Migration scan",
      fraction: 0.5,
      label: "Reading claude_code user skill (3/9)",
    });
    expect(await screen.findByText("Reading claude_code user skill (3/9)")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel scan" }));
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "jobs_cancel")).toBe(true),
    );
    const cancel = ipc.calls.find((call) => call.command === "jobs_cancel");
    expect(cancel?.args).toEqual({ jobId: "job_scan_live" });
  });

  test("names every root that could not be read", async () => {
    ipc.reply = scanReply({
      ...scan,
      partial: true,
      warnings: [
        {
          path: "/sandbox/home/.claude.json",
          kind: "mcp_server",
          agent: "claude_code",
          scope: "user",
          message: "permission denied",
        },
      ],
    });
    renderScreen();
    expect(await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" })).toBeTruthy();
    const warnings = screen.getByRole("list", { name: "Scan warnings" });
    expect(warnings.textContent).toContain("/sandbox/home/.claude.json");
    expect(warnings.textContent).toContain("permission denied");
    expect(screen.getByText(/Some agent folders could not be read/)).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "Select pdf-extract" })).toBeTruthy();
  });

  test("an empty scan keeps the folders and offers a rescan", async () => {
    ipc.reply = scanReply({
      items: [],
      partial: false,
      skipped_roots: [],
      warnings: [],
      roots: [
        { path: "/sandbox/home/.claude/skills", kind: "skill", agents: ["claude_code"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "empty", unreadable_reason: null },
      ],
    });
    renderScreen();
    expect(await screen.findByText("Nothing to migrate")).toBeTruthy();
    expect(screen.getByRole("complementary", { name: "Scanned folders" }).textContent).toContain(
      "/sandbox/home/.claude/skills",
    );
    const folders = screen.getByRole("complementary", { name: "Scanned folders" }).textContent ?? "";
    expect(folders).toContain("0 new");
    expect(folders).toContain("empty");
    expect(folders).not.toContain("not present");
    expect(screen.getAllByRole("button", { name: "Rescan" }).length).toBeGreaterThan(0);
  });

  test("the never-scanned state lists no folders and still offers a rescan", async () => {
    ipc.reply = scanReply({
      items: [],
      partial: false,
      skipped_roots: [],
      warnings: [],
      roots: [],
    });
    renderScreen();
    expect(await screen.findByText("Nothing scanned yet.")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Rescan" }).length).toBeGreaterThan(0);
  });

  test("a rescan keeps unchanged selections and drops stale ones", async () => {
    let result: unknown = scan;
    ipc.reply = (command) => {
      if (command === "migrate_scan_start") return Promise.resolve("job_scan_rescan");
      if (command === "migrate_scan_result") return Promise.resolve(result);
      return Promise.resolve(undefined);
    };
    renderScreen();
    await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" });
    fireEvent.click(screen.getByRole("checkbox", { name: "Select all items" }));
    expect(screen.getByRole("button", { name: "Continue with 3" })).toBeTruthy();
    // The pdf-extract copy changed, so its selection no longer matches (kind, path, hash).
    result = {
      ...scan,
      items: [
        item({ name: "pdf-extract", path: "/sandbox/home/.claude/skills/pdf-extract", content_hash: "e".repeat(64) }),
        scan.items[1],
        scan.items[2],
      ],
    };
    fireEvent.click(screen.getByRole("button", { name: "Rescan" }));
    expect(
      await screen.findByText("1 selection changed or disappeared and were cleared."),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Continue with 2" })).toBeTruthy();
  });

  test("Keep both validates names before the plan can continue", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [conflict] };
      return undefined;
    });
    renderScreen();
    await openResolve();
    fireEvent.click(screen.getByRole("radio", { name: "Keep both" }));
    expect((screen.getByRole("textbox", { name: "Copy A name" }) as HTMLInputElement).value).toBe(
      "review-helper-claude-code",
    );
    fireEvent.change(screen.getByRole("textbox", { name: "Copy A name" }), {
      target: { value: "bad name!" },
    });
    expect(await screen.findByText(/Copy A name must be/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByRole("textbox", { name: "Copy A name" }), {
      target: { value: "review-helper-a" },
    });
    await waitFor(() =>
      expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(false),
    );
  });

  test("formatting-only conflicts default to Keep A and bulk keep-A leaves explicit choices alone", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [conflict, formattingConflict] };
      return undefined;
    });
    renderScreen();
    await openResolve();
    expect(screen.getByText("Equal content after formatting")).toBeTruthy();
    fireEvent.click(screen.getAllByRole("radio", { name: "Keep B" })[0] as HTMLInputElement);
    fireEvent.click(screen.getByRole("button", { name: "Keep A for unresolved" }));
    const radios = screen.getAllByRole("radio", { name: "Keep B" });
    expect((radios[0] as HTMLInputElement).checked).toBe(true);
    expect((radios[1] as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(false);
  });

  test("continuing hands the reviewed choices to migrate_plan and shows the dry run", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [conflict] };
      if (command === "migrate_plan") return plan;
      return undefined;
    });
    renderScreen();
    await openResolve();
    fireEvent.click(screen.getByRole("button", { name: "Keep A for unresolved" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("heading", { name: "Plan the move" })).toBeTruthy();
    await waitFor(() => expect(ipc.calls.some((call) => call.command === "migrate_plan")).toBe(true));
    const args = ipc.calls.find((call) => call.command === "migrate_plan")?.args;
    if (typeof args !== "object" || args === null || !("request" in args)) {
      throw new Error("migrate_plan was called without a request payload");
    }
    const request = args.request;
    if (typeof request !== "object" || request === null) {
      throw new Error("migrate_plan request payload is not an object");
    }
    expect("selected_paths" in request ? request.selected_paths : undefined).toHaveLength(3);
    expect("resolutions" in request ? request.resolutions : undefined).toEqual([
      { name: "review-helper", choice: "keep_a", names: [] },
    ]);
    expect(await screen.findByText("2 items copied into the store")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show file list" }));
    expect(await screen.findByText(/Back up original/)).toBeTruthy();
  });

  test("a finished apply is read from the retained job result and lands on the done step", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [] };
      if (command === "migrate_plan") return plan;
      if (command === "migrate_apply") return "job_apply";
      // The job finished before the screen read anything: the registry still answers.
      if (command === "migrate_apply_result") return applyResult;
      if (command === "operation_undo_check") return undoReady;
      return undefined;
    });
    renderScreen();
    await reachPlan();
    await confirmApply();

    expect(await screen.findByText(/now live in AgenTuna/)).toBeTruthy();
    expect(screen.getByText(/2 items copied into the Project store/)).toBeTruthy();
    expect(screen.getByText(/\/sandbox\/support\/backups\/migration/)).toBeTruthy();
    expect(screen.getByText(/operation 01M6TC30WZNDRNXKSJ4TA5BJV4/)).toBeTruthy();
    expect(await screen.findByRole("button", { name: /Undo migration/ })).toBeTruthy();
    expect(ipc.calls.filter((call) => call.command === "migrate_apply_result")).toHaveLength(1);
  });

  test("a still-running apply is polled until the retained result lands", async () => {
    let reads = 0;
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [] };
      if (command === "migrate_plan") return plan;
      if (command === "migrate_apply") return "job_apply";
      if (command === "migrate_apply_result") {
        reads += 1;
        return reads < 3
          ? Promise.reject({ code: "conflict", message: "The migration apply job is still running." })
          : applyResult;
      }
      if (command === "operation_undo_check") return undoReady;
      return undefined;
    });
    renderScreen();
    await reachPlan();
    await confirmApply();

    expect(await screen.findByText(/now live in AgenTuna/)).toBeTruthy();
    expect(reads).toBe(3);
  });

  test("a failed apply reports the backend's typed error and its remedy", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [] };
      if (command === "migrate_plan") return plan;
      if (command === "migrate_apply") return "job_apply";
      if (command === "migrate_apply_result") {
        return Promise.reject({ code: "not_found", message: "No completed migration apply was recorded for this job." });
      }
      return undefined;
    });
    renderScreen();
    await reachPlan();
    await confirmApply();
    await waitFor(() => expect(events.listeners.has("agentuna:job-finished")).toBe(true));
    events.emit("agentuna:job-finished", {
      id: "job_apply",
      name: "Migration apply",
      state: "failed",
      error: {
        kind: "migration",
        failure: {
          code: "stale_plan",
          message: "the files changed since the plan was reviewed",
          path: null,
          remedy: "Go back to the scan, rebuild the plan and review it again.",
        },
      },
    });

    expect(await screen.findByRole("heading", { name: "Migration rolled back" })).toBeTruthy();
    expect(screen.getByText(/the files changed since the plan was reviewed/)).toBeTruthy();
    expect(screen.getByText(/Go back to the scan/)).toBeTruthy();
  });

  test("a cancelled apply reports the cancellation instead of a timeout", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [] };
      if (command === "migrate_plan") return plan;
      if (command === "migrate_apply") return "job_apply";
      if (command === "migrate_apply_result") {
        return Promise.reject({ code: "not_found", message: "No completed migration apply was recorded for this job." });
      }
      return undefined;
    });
    renderScreen();
    await reachPlan();
    await confirmApply();
    await waitFor(() => expect(events.listeners.has("agentuna:job-finished")).toBe(true));
    events.emit("agentuna:job-finished", {
      id: "job_apply",
      name: "Migration apply",
      state: "cancelled",
      error: null,
    });

    expect(await screen.findByRole("heading", { name: "Migration rolled back" })).toBeTruthy();
    expect(screen.getByText(/cancelled before it finished/i)).toBeTruthy();
    expect(screen.queryByText(/timed out/i)).toBeNull();
  });

  test("an apply with no recorded result and no event reports the backend's own words", async () => {
    ipc.reply = scanReply(scan, (command) => {
      if (command === "migrate_conflicts") return { conflicts: [] };
      if (command === "migrate_plan") return plan;
      if (command === "migrate_apply") return "job_apply";
      if (command === "migrate_apply_result") {
        return Promise.reject({ code: "not_found", message: "No completed migration apply was recorded for this job." });
      }
      return undefined;
    });
    renderScreen();
    await reachPlan();
    await confirmApply();

    expect(await screen.findByRole("heading", { name: "Migration rolled back" })).toBeTruthy();
    expect(screen.getByText(/No completed migration apply was recorded/)).toBeTruthy();
    expect(screen.queryByText(/timed out/i)).toBeNull();
  });
});
