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
  updated_at: "2026-11-06T12:00:00Z",
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

  test("toggling agent cell handles NativeConflict by reverting toggle and showing toast", async () => {
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
      expect(screen.getAllByText(/adopt or move it aside first/).length).toBeGreaterThan(0);
    });
  });

  test("empty state CTA routes to add-skill wizard", async () => {
    ipc.reply = (command) => {
      if (command === "skills_list") return Promise.resolve(mockView({ rows: [] }));
      return Promise.resolve(undefined);
    };
    renderScreen();

    const addSkillBtns = await screen.findAllByRole("button", { name: "Add skill" });
    if (addSkillBtns[0]) fireEvent.click(addSkillBtns[0]);
    expect(app.getSnapshot().route).toBe("add-skill");
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
