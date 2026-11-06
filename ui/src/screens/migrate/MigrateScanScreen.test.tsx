import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
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

const appState = createAppState();
appState.restore("prj_fixture", "project");

const scan = {
  items: [
    { kind: "skill", name: "pdf-extract", agents: ["claude_code"], path: "/sandbox/home/.claude/skills/pdf-extract", scope: "project", content_hash: "a".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" },
    { kind: "mcp_server", name: "context7", agents: ["codex", "omp"], path: "/sandbox/home/.codex/config.toml", scope: "user", content_hash: "b".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" },
    { kind: "skill", name: "in-sync", agents: ["claude_code"], path: "/sandbox/home/.claude/skills/in-sync", scope: "user", content_hash: "c".repeat(64), parse_error: null, store_link: true, ownership: "managed" },
  ],
  partial: false,
  skipped_roots: ["/sandbox/home/.cursor/skills"],
  roots: [
    { path: "/sandbox/home/.claude/skills", agents: ["claude_code"], new_items: 1, in_sync: 1 },
    { path: "/sandbox/home/.codex/config.toml", agents: ["codex", "omp"], new_items: 0, in_sync: 1 },
  ],
};
const conflict = {
  kind: "skill",
  name: "review-helper",
  copies: [
    { agent: "claude_code", path: "/sandbox/home/.claude/skills/review-helper", scope: "user", raw_hash: "a".repeat(64), normalized_hash: "a".repeat(64) },
    { agent: "codex", path: "/sandbox/home/.codex/skills/review-helper", scope: "user", raw_hash: "b".repeat(64), normalized_hash: "b".repeat(64) },
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
  plan_id: "itm_plan",
  plan_hash: "d".repeat(64),
  scope: "project",
  strategy: "replace_with_links",
  counts: { items: 2, files_created: 4, files_backed_up: 1, links_written: 1, files_left_in_place: 0 },
  steps: [
    { seq: 1, action: "backuporiginal", item_name: "pdf-extract", source: "/sandbox/home/.claude/skills/pdf-extract", destination: null, backup_path: "/sandbox/backups/migration/pdf-extract", bytes: 12, agents: [] },
    { seq: 2, action: "createstoreitem", item_name: "pdf-extract", source: "/sandbox/home/.claude/skills/pdf-extract", destination: "/sandbox/Projects/AgenTuna/.config-studio/skills/pdf-extract", backup_path: null, bytes: 12, agents: ["claude_code"] },
  ],
  files_checked: 2,
};

afterEach(() => {
  cleanup();
  ipc.calls.length = 0;
});

function renderScreen() {
  return render(
    <AppStateProvider state={appState}>
      <MigrateScanScreen />
    </AppStateProvider>,
  );
}

async function openResolve() {
  await screen.findByText("Found 3 items outside AgenTuna");
  fireEvent.click(screen.getByRole("checkbox", { name: "Select all items" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue with 3" }));
  await screen.findByRole("heading", { name: "Resolve conflicts" });
}

describe("MigrateScanScreen", () => {
  test("renders the scan stepper, scanned folders and the item table", async () => {
    ipc.reply = (command) => Promise.resolve(command === "migrate_scan" ? scan : undefined);
    renderScreen();
    expect(await screen.findByRole("heading", { name: "Found 3 items outside AgenTuna" })).toBeTruthy();
    const steps = screen.getByRole("navigation", { name: "Migration steps" });
    for (const label of ["Scan", "Resolve", "Plan", "Done"]) {
      expect(steps.textContent).toContain(label);
    }
    expect(screen.getByRole("complementary", { name: "Scanned folders" }).textContent).toContain(
      "/sandbox/home/.claude/skills",
    );
    expect(screen.getAllByText("1 new").length).toBe(1);
    expect(screen.getByText("1 in sync")).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "Select pdf-extract" })).toBeTruthy();
    expect(screen.getAllByText("native only").length).toBe(3);
  });

  test("Keep both validates names before the plan can continue", async () => {
    ipc.reply = (command) => {
      if (command === "migrate_scan") return Promise.resolve(scan);
      if (command === "migrate_conflicts") return Promise.resolve({ conflicts: [conflict] });
      return Promise.resolve(undefined);
    };
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
    ipc.reply = (command) => {
      if (command === "migrate_scan") return Promise.resolve(scan);
      if (command === "migrate_conflicts") return Promise.resolve({ conflicts: [conflict, formattingConflict] });
      return Promise.resolve(undefined);
    };
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
    ipc.reply = (command) => {
      if (command === "migrate_scan") return Promise.resolve(scan);
      if (command === "migrate_conflicts") return Promise.resolve({ conflicts: [conflict] });
      if (command === "migrate_plan") return Promise.resolve(plan);
      return Promise.resolve(undefined);
    };
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
});
