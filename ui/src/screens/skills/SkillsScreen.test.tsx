import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ToastProvider } from "../../overlays/toasts";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import type { AppState } from "../../state/AppState";
import type { SkillsListView } from "../../generated/SkillsListView";
import { SkillsScreen } from "./SkillsScreen";

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

const mockSkill = (overrides = {}) => ({
  item_id: "itm_1",
  name: "code-review",
  scope: "user",
  state: "active",
  shadowed_by: null,
  description: "Code review helpers for pull requests",
  native: false,
  native_path: null,
  update_available: false,
  source: null,
  source_commit_sha: null,
  source_remote_sha: null,
  source_last_checked_at: null,
  updated_at: "2026-11-06T12:00:00Z",
  locked: false,
  lock_scope: null,
  lock_reason: null,
  agents: [
    { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/codex/skills/code-review" },
    { agent: "omp", state: "off", enabled: false, config_path: null },
  ],
  ...overrides,
});

const mockView = (overrides = {}): SkillsListView => ({
  rows: [mockSkill()],
  agents: [
    { id: "codex", label: "Codex", presence: "installed" },
    { id: "omp", label: "omp", presence: "installed" },
  ],
  project_id: "prj_test",
  project_name: "Test Project",
  ...overrides,
});

let app: AppState;
beforeEach(() => {
  app = createAppState();
  app.restore("prj_test", "user");
  ipc.calls.length = 0;
});
afterEach(() => cleanup());

function renderScreen() {
  return render(
    <AppStateProvider state={app}>
      <ToastProvider>
        <SkillsScreen />
      </ToastProvider>
    </AppStateProvider>,
  );
}

describe("SkillsScreen (TS-028)", () => {
  test("renders 500 rows fixture and virtualizes", async () => {
    const fiveHundredRows = Array.from({ length: 500 }, (_, i) =>
      mockSkill({
        item_id: `itm_${i}`,
        name: `skill-${i.toString().padStart(3, "0")}`,
        description: `Description for skill ${i}`,
      })
    );
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows: fiveHundredRows }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    const table = await screen.findByRole("table", { name: "Skills" });
    expect(table).toBeTruthy();
    expect(table.getAttribute("data-total-rows")).toBe("500");
    // Builds fewer than 50 cell widgets per frame (virtualization assertion)
    const renderedRows = Number(table.getAttribute("data-rendered-rows"));
    expect(renderedRows).toBeLessThan(50);
  });

  test("each chip filters as labelled and live counts match rows", async () => {
    const mixedRows = [
      mockSkill({ item_id: "1", name: "Global Skill", scope: "global" }),
      mockSkill({ item_id: "2", name: "User Skill", scope: "user" }),
      mockSkill({ item_id: "3", name: "Project Skill", scope: "project" }),
      mockSkill({ item_id: "", name: "Native Skill", scope: "user", native: true, native_path: "/sandbox/omp/skills/native" }),
      mockSkill({ item_id: "5", name: "Shadowed Skill", scope: "global", shadowed_by: "user" }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows: mixedRows }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    await screen.findByText("Global Skill");

    const filterGroup = screen.getByRole("group", { name: "Filter chips" });
    const allChip = within(filterGroup).getByText("All").closest("button")!;
    expect(within(allChip).getByText("5")).toBeTruthy();
    // Global chip
    const globalChip = within(filterGroup).getByText("Global").closest("button")!;
    expect(within(globalChip).getByText("2")).toBeTruthy();
    // Shadowed chip
    const shadowedChip = within(filterGroup).getByText("Shadowed").closest("button")!;
    expect(within(shadowedChip).getByText("1")).toBeTruthy();

    // Click Shadowed chip: only shadowed row is displayed and has dimmed tone
    fireEvent.click(shadowedChip);
    expect(screen.queryByText("User Skill")).toBeNull();
    const shadowedRow = screen.getByText("Shadowed Skill").closest("[data-row-id]");
    expect(shadowedRow?.getAttribute("data-tone")).toBe("dimmed");
    expect(shadowedRow?.getAttribute("title")).toBe("Shadowed by user");
  });
  test("keeps filtered-out selections and reports a refused atomic bulk update", async () => {
    const rows = [
      mockSkill({ item_id: "itm_user", name: "User Skill", scope: "user" }),
      mockSkill({ item_id: "itm_global", name: "Global Skill", scope: "global" }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows }));
      if (command === "skill_agent_preview") {
        return Promise.resolve({ preview_id: "preview-user-skill", enabled: true, mode: "copy", paths: [], coupled_agents: [], warnings: [], no_op: false });
      }
      if (command === "bulk_skill_apply") {
        return Promise.resolve({
          batch_id: "batch-refused",
          events: [],
          operation_id: "",
          files_changed: 0,
          applied: false,
          refusal: "A native skill conflict blocked the entire batch.",
        });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("checkbox", { name: "Select User Skill" }));
    const filterGroup = screen.getByRole("group", { name: "Filter chips" });
    fireEvent.click(within(filterGroup).getByText("Global").closest("button")!);
    const bulkActions = screen.getByRole("region", { name: "Bulk skill actions" });
    expect(within(bulkActions).getByText("1 selected")).toBeTruthy();
    expect(within(bulkActions).getByText("1 hidden by filter")).toBeTruthy();

    fireEvent.click(within(bulkActions).getByRole("button", { name: "Enable for agent…" }));
    const picker = screen.getByRole("dialog", { name: "Enable skills for agents" });
    fireEvent.click(within(picker).getByRole("checkbox", { name: "Codex" }));
    fireEvent.click(within(picker).getByRole("button", { name: "Enable for selected agents" }));

    expect(await screen.findByText("No changes applied")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Bulk skill failures" }).textContent).toContain(
      "A native skill conflict blocked the entire batch.",
    );
    const applyCall = ipc.calls.find((call) => call.command === "bulk_skill_apply");
    expect(applyCall?.args).toEqual({
      request: { preview_ids: ["preview-user-skill"], label: "Enable 1 skills for 1 agent(s)" },
    });
  });


  test("toggling agent cell turns a native conflict into an adoption action", async () => {
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView());
      if (command === "skill_agent_preview") {
        return Promise.reject({
          code: "refused_overwrite",
          message: "/sandbox/omp/skills/code-review holds an unmanaged skill folder; adopt it or move it aside first.",
        });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const switchBtn = await screen.findByRole("switch", { name: "Codex: Linked" });
    fireEvent.click(switchBtn);

    await waitFor(() => {
      expect(ipc.calls.some((c) => c.command === "skill_agent_preview")).toBe(true);
      expect(screen.getByText("Adopt this unmanaged native item into AgenTuna before enabling it.")).toBeTruthy();
      expect(screen.getByRole("button", { name: "Adopt" })).toBeTruthy();
    });
  });

  test("opens the selected managed skill detail from the row menu", async () => {
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView());
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "Actions for code-review" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Open" }));

    expect(app.getSnapshot().route).toBe("skill-detail");
    expect(app.getSnapshot().screenState["detail:itemId"]).toBe("itm_1");
  });

  test("empty state CTA routes to add-skill wizard", async () => {
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows: [] }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    const addSkillBtns = await screen.findAllByRole("button", { name: "+ New skill" });
    if (addSkillBtns[0]) fireEvent.click(addSkillBtns[0]);
    expect(app.getSnapshot().route).toBe("add-skill");
    expect(app.getSnapshot().screenState["add-skill:source"]).toBe("new");
  });

  test("error state renders retry and re-runs query", async () => {
    let failedOnce = true;
    ipc.reply = (command) => {
      if (command === "skills_list") {
        if (failedOnce) {
          failedOnce = false;
          return Promise.reject({ code: "unavailable", message: "Database offline" });
        }
        return Promise.resolve(mockView());
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    await screen.findByText("Database offline");
    const retryBtn = screen.getByRole("button", { name: "Retry" });
    fireEvent.click(retryBtn);

    await screen.findByText("code-review");
  });
});

describe("SkillsScreen update badge (TS-040)", () => {
  const sourced = {
    item_id: "itm_1",
    source: "git",
    source_commit_sha: "a".repeat(40),
    source_remote_sha: "b".repeat(40),
    source_last_checked_at: "2026-11-06T11:00:00Z",
    update_available: true,
  };

  test("badge tooltip names both commits and the last check; ignored and gone sources never show it", async () => {
    ipc.reply = (command) => {
      if (command === "skills_list") {
        return Promise.resolve(mockView({
          rows: [
            mockSkill({ ...sourced }),
            mockSkill({
              item_id: "itm_2",
              name: "gone-skill",
              source: "git",
              source_commit_sha: "c".repeat(40),
              source_remote_sha: null,
              source_last_checked_at: "2026-11-06T09:00:00Z",
            }),
            mockSkill({
              item_id: "itm_3",
              name: "unchecked-skill",
              source: "git",
              source_commit_sha: null,
              source_remote_sha: null,
              source_last_checked_at: null,
            }),
          ],
        }));
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    const badge = await screen.findByRole("button", { name: "Update available for code-review" });
    expect(badge.getAttribute("title")).toBe(
      `Installed ${"a".repeat(40)} · Upstream ${"b".repeat(40)} · Last checked 2026-11-06T11:00:00Z`,
    );
    fireEvent.click(badge);
    const menu = await screen.findByRole("menu", { name: "Update actions for code-review" });
    expect(menu.textContent).toContain(`Installed ${"a".repeat(40)}`);
    expect(within(menu).getByRole("menuitem", { name: "Update now" })).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: "Ignore this version" })).toBeTruthy();

    const gone = screen.getByText("Source unavailable");
    expect(gone.getAttribute("title")).toContain("no update badge is shown");
    expect(screen.getByText("Source unchecked")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Update available for gone-skill" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Update available for unchecked-skill" })).toBeNull();
  });

  test("Update now reviews the exact diff, applies once with a backup, and clears the badge", async () => {
    let listed = 0;
    ipc.reply = (command) => {
      if (command === "skills_list") {
        listed += 1;
        return Promise.resolve(mockView({
          rows: [mockSkill(listed === 1 ? sourced : { ...sourced, update_available: false, source_remote_sha: sourced.source_commit_sha })],
        }));
      }
      if (command === "skill_update_preview") {
        return Promise.resolve({
          preview_id: "pv_update",
          files: [{ path: "skills/code-review/SKILL.md", diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1 +1 @@\n-old\n+new", backup_required: true }],
        });
      }
      if (command === "skill_update_apply") {
        return Promise.resolve({ operation_id: "op_update", files_changed: 1 });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "Update available for code-review" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Update now" }));

    const dialog = await screen.findByRole("dialog", { name: "Review skill update" });
    expect(within(dialog).getAllByText("skills/code-review/SKILL.md").length).toBeGreaterThan(0);
    expect(dialog.textContent).toContain("+new");
    expect(dialog.textContent).toContain("-old");
    expect(within(dialog).getByText("Will be backed up")).toBeTruthy();

    fireEvent.click(within(dialog).getByRole("button", { name: "Apply update" }));

    await waitFor(() => {
      expect(ipc.calls).toContainEqual({
        command: "skill_update_preview",
        args: { itemId: "itm_1", confirmed: true },
      });
      expect(ipc.calls).toContainEqual({
        command: "skill_update_apply",
        args: { previewId: "pv_update", confirmed: true },
      });
    });
    expect(await screen.findByText("1 file backed up and updated.")).toBeTruthy();
    await waitFor(() => expect(listed).toBeGreaterThanOrEqual(2));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Update available for code-review" })).toBeNull());
  });

  test("Ignore records the checked upstream sha and reloads the list", async () => {
    let listed = 0;
    ipc.reply = (command) => {
      if (command === "skills_list") {
        listed += 1;
        return Promise.resolve(mockView({ rows: [mockSkill(listed === 1 ? sourced : { ...sourced, update_available: false })] }));
      }
      if (command === "skill_update_ignore") return Promise.resolve(null);
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "Update available for code-review" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Ignore this version" }));

    await waitFor(() => {
      expect(ipc.calls).toContainEqual({ command: "skill_update_ignore", args: { itemId: "itm_1" } });
    });
    expect((await screen.findAllByText(/Ignored upstream bbbbbbbbbb for code-review/)).length).toBeGreaterThan(0);
    await waitFor(() => expect(listed).toBeGreaterThanOrEqual(2));
    expect(screen.queryByRole("button", { name: "Update available for code-review" })).toBeNull();
  });
});

describe("SkillsScreen STATUS column (design 02-skills)", () => {
  test("names lock, shadow, override and native copy states as text", async () => {
    const rows = [
      mockSkill({
        item_id: "itm_locked",
        name: "code-review",
        scope: "global",
        locked: true,
        lock_scope: "global",
        lock_reason: "Approved by the team",
      }),
      mockSkill({ item_id: "itm_override", name: "tdd", scope: "project" }),
      mockSkill({ item_id: "itm_shadowed", name: "tdd", scope: "user", shadowed_by: "project" }),
      mockSkill({ item_id: "itm_active", name: "research", scope: "user" }),
      mockSkill({
        item_id: "",
        name: "pdf-extract",
        scope: "user",
        native: true,
        native_path: "/sandbox/home/.omp/agent/skills/pdf-extract",
        agents: [
          {
            agent: "omp",
            state: "native",
            enabled: true,
            config_path: "/sandbox/home/.omp/agent/skills/pdf-extract",
          },
        ],
      }),
      mockSkill({
        item_id: "",
        name: "release-notes",
        scope: "project",
        native: true,
        native_path: "/sandbox/project/.claude/skills/release-notes",
        agents: [
          {
            agent: "claude_code",
            state: "native",
            enabled: true,
            config_path: "/sandbox/project/.claude/skills/release-notes",
          },
        ],
      }),
      mockSkill({
        item_id: "",
        name: "release-notes",
        scope: "project",
        native: true,
        native_path: "/sandbox/project/.agents/skills/release-notes",
        agents: [
          { agent: "codex", state: "native", enabled: true, config_path: "/sandbox/project/.agents/skills/release-notes" },
          { agent: "omp", state: "native", enabled: true, config_path: "/sandbox/project/.agents/skills/release-notes" },
        ],
      }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    // Locked: the lock label and the reason tooltip are both real data.
    const locked = await screen.findByText("Locked by Global");
    expect(locked.closest('[data-slot="badge"]')?.getAttribute("title")).toBe("Approved by the team");
    // Scope cell states the same fact in text, not colour alone.
    expect(screen.getAllByText(/global · locked/i).length).toBeGreaterThan(0);

    expect(await screen.findByText("Overrides User copy")).toBeTruthy();
    expect(screen.getAllByText("Shadowed by Project").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Active").length).toBeGreaterThan(0);

    // Native-only rows: the not-in-AgenTuna badge plus a named copy state.
    expect(screen.getAllByText("not in AgenTuna").length).toBe(3);
    expect(await screen.findByText("omp only")).toBeTruthy();
    expect((await screen.findAllByText("2 copies · 1 shared root")).length).toBe(2);
  });

  test("a native row with one shared folder states the shared root", async () => {
    const rows = [
      mockSkill({
        item_id: "",
        name: "shared-helper",
        scope: "project",
        native: true,
        native_path: "/sandbox/project/.agents/skills/shared-helper",
        agents: [
          {
            agent: "codex",
            state: "native",
            enabled: true,
            config_path: "/sandbox/project/.agents/skills/shared-helper",
          },
          {
            agent: "omp",
            state: "native",
            enabled: true,
            config_path: "/sandbox/project/.agents/skills/shared-helper",
          },
        ],
      }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    expect(await screen.findByText("2 agents · 1 shared root")).toBeTruthy();
    expect(screen.getAllByText("1 folder · 2 agents").length).toBeGreaterThan(0);
    // One folder is one block: the second agent column is merged, not a second cell.
    const blocks = document.querySelectorAll('[data-slot="shared-folder"]');
    expect(blocks.length).toBe(1);
    expect(blocks[0]?.getAttribute("data-state")).toBe("native");
    expect(blocks[0]?.getAttribute("title")).toContain("/sandbox/project/.agents/skills/shared-helper");
    expect(screen.queryAllByRole("switch").length).toBe(0);
  });

  test("a coupled managed toggle asks before writing the shared folder", async () => {
    const rows = [
      mockSkill({
        agents: [
          {
            agent: "codex",
            state: "linked",
            enabled: true,
            config_path: "/sandbox/project/.agents/skills/code-review",
          },
          {
            agent: "omp",
            state: "linked",
            enabled: true,
            config_path: "/sandbox/project/.agents/skills/code-review",
          },
        ],
      }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows }));
      if (command === "skill_agent_preview") {
        return Promise.resolve({
          preview_id: "pv_coupled",
          enabled: false,
          mode: "link",
          paths: ["/sandbox/project/.agents/skills/code-review"],
          coupled_agents: ["omp"],
          warnings: ["Codex and omp share .agents/skills, so this changes both agents."],
          no_op: false,
        });
      }
      if (command === "skill_agent_apply") {
        return Promise.resolve({ operation_id: "op_coupled", enabled: false, changed: true });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(
      await screen.findByRole("switch", { name: "Codex + omp: Linked, one shared folder" }),
    );
    const dialog = await screen.findByRole("dialog", { name: "Shared discovery root" });
    expect(within(dialog).getByText(/Codex and omp share/)).toBeTruthy();
    // Review first: nothing is written until the coupling is confirmed.
    expect(ipc.calls.some((call) => call.command === "skill_agent_apply")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Disable for both agents" }));
    await waitFor(() =>
      expect(ipc.calls).toContainEqual({
        command: "skill_agent_apply",
        args: { previewId: "pv_coupled" },
      }),
    );
  });

  test("TC-025: clicking Adopt on a native row opens AdoptModal naming store target and backup directory", async () => {
    const rows = [
      mockSkill({
        id: "",
        item_id: "",
        name: "unmanaged-tool",
        scope: "native",
        state: "native",
        native: true,
        native_path: "/sandbox/home/.claude/skills/unmanaged-tool",
      }),
    ];
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows }));
      if (command === "adopt_preview") {
        return Promise.resolve({
          source: "/sandbox/home/.claude/skills/unmanaged-tool",
          store_root: "/sandbox/user",
          store_destination: "/sandbox/user/skills/unmanaged-tool",
          backup_dir: "/sandbox/support/backups",
          slug: "unmanaged-tool",
          native_name: "unmanaged-tool",
          kind: "skill",
          agents: ["claude_code"],
          handling: "link",
          renamed: false,
          current: "present",
        });
      }
      if (command === "adopt_apply") {
        return Promise.resolve({
          summary: "Adopted unmanaged-tool (link)",
          operation_id: "op_test_adopt",
          store_destination: "/sandbox/user/skills/unmanaged-tool",
          slug: "unmanaged-tool",
          handling: "link",
        });
      }
      return Promise.resolve(undefined);
    };
    renderScreen();

    fireEvent.click(await screen.findByRole("button", { name: "Adopt" }));
    const modal = await screen.findByRole("dialog", { name: /Adopt unmanaged-tool/i });
    expect(modal).toBeTruthy();
    expect(screen.getByText("/sandbox/user/skills/unmanaged-tool")).toBeTruthy();
    expect(screen.getByText("/sandbox/support/backups")).toBeTruthy();

    const confirmBtn = within(modal).getByRole("button", { name: "Adopt" });
    fireEvent.click(confirmBtn);
    await waitFor(() =>
      expect(ipc.calls.some((call) => call.command === "adopt_apply")).toBe(true),
    );
    expect(
      ipc.calls.some(
        (call) =>
          call.command === "adopt_apply" &&
          (call.args as Record<string, unknown>).path === "/sandbox/home/.claude/skills/unmanaged-tool",
      ),
    ).toBe(true);
  });
});
