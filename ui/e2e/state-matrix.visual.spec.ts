import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

type State = "loading" | "empty" | "error";
type Screen = "Skills" | "MCP Servers" | "Effective config" | "Sync & drift" | "Instructions" | "Agents";

const cases: ReadonlyArray<{
  readonly screen: Screen;
  readonly command: string;
  readonly emptyValue: unknown;
  readonly emptyText: string;
}> = [
  { screen: "Skills", command: "skills_list", emptyValue: { rows: [], agents: [], project_id: null, project_name: null }, emptyText: "No skills found" },
  { screen: "MCP Servers", command: "mcp_server_list", emptyValue: { servers: [], project_id: null, project_name: null, project_agents: [], global_writable: false, global_reason: null }, emptyText: "No MCP servers yet" },
  { screen: "Effective config", command: "effective_query", emptyValue: { items: [], disabled_here: [], diagnostics: [], load_diagnostics: [] }, emptyText: "Nothing configured at this scope" },
  { screen: "Sync & drift", command: "drift_snapshot", emptyValue: { rows: [], in_sync_count: 0, watched_count: 0, watcher: { status: "active", path: null, error: null } }, emptyText: "Everything is in sync" },
  { screen: "Instructions", command: "instructions_list", emptyValue: [], emptyText: "No rule files found" },
  { screen: "Agents", command: "agents_list", emptyValue: [], emptyText: "No agents detected" },
];

function mainName(screen: Screen): string {
  return screen === "Sync & drift" ? "Sync and drift" : screen;
}

async function openState(page: Page, screen: Screen, command: string, state: State, emptyValue: unknown): Promise<void> {
  await page.addInitScript(({ targetCommand, targetState, value }) => {
    const mocks = (window as typeof window & {
      __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> };
    }).__AT_TEST_MOCKS__;
    mocks.commandOverrides[targetCommand] = targetState === "loading"
      ? () => Promise.withResolvers<never>().promise
      : targetState === "error"
        ? () => { throw new Error("E_OFFLINE: /sandbox/fixture\nOS error: disconnected"); }
        : value;
  }, { targetCommand: command, targetState: state, value: emptyValue });
  await page.goto("/");
  await page.getByRole("button", { name: screen, exact: true }).click();
  await expect(page.getByRole("main", { name: mainName(screen) })).toBeVisible();
}

for (const entry of cases) {
  for (const state of ["loading", "empty", "error"] as const) {
    test(`${entry.screen} real widget ${state} state`, async ({ page }) => {
      await openState(page, entry.screen, entry.command, state, entry.emptyValue);
      const main = page.getByRole("main", { name: mainName(entry.screen) });
      if (state === "loading") {
        await expect(main.getByRole("status").first()).toBeVisible();
      } else if (state === "empty") {
        await expect(main.getByRole("heading", { name: entry.emptyText })).toBeVisible();
      } else {
        await expect(main.getByText(/E_OFFLINE|disconnected/)).toBeVisible();
      }
      await expect(page).toHaveScreenshot(`state-${entry.screen.toLowerCase().replaceAll(" ", "-").replaceAll("&", "and")}-${state}.png`, {
        animations: "disabled",
        caret: "hide",
      });
    });
  }
}

for (const entry of cases) {
  test(`${entry.screen} populated data reaches the real screen widget`, async ({ page }) => {
    if (entry.screen === "Sync & drift") {
      await page.addInitScript(() => {
        const mocks = (window as typeof window & {
          __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> };
        }).__AT_TEST_MOCKS__;
        mocks.commandOverrides.drift_snapshot = {
          rows: [{
            event_id: 17,
            kind: "drifted",
            item_id: "itm_visual_skill",
            item_name: "Code Review",
            agent: "codex",
            scope: "project",
            path: "/sandbox/project/.agents/skills/code-review/SKILL.md",
            detected_at: "2026-11-06T10:00:00Z",
            changed_file_count: 1,
            can_adopt: true,
            can_restore: true,
            link_target: "/sandbox/project/.config-studio/skills/code-review",
          }],
          in_sync_count: 41,
          watched_count: 42,
          watcher: { status: "active", path: null, error: null },
        };
      });
    }
    await page.goto("/");
    await page.getByRole("button", { name: entry.screen, exact: true }).click();
    const main = page.getByRole("main", { name: mainName(entry.screen) });
    switch (entry.screen) {
      case "Skills":
        await expect(main.getByText("code-review", { exact: true })).toBeVisible();
        break;
      case "MCP Servers":
        await expect(main.getByText("Visual server", { exact: true })).toBeVisible();
        break;
      case "Effective config":
        await expect(main.getByRole("heading", { name: "Code Review" })).toBeVisible();
        break;
      case "Sync & drift":
        await expect(main.getByRole("heading", { name: "Code Review" })).toBeVisible();
        break;
      case "Instructions":
        await expect(main.getByRole("button", { name: "AGENTS.md", exact: true })).toBeVisible();
        break;
      case "Agents":
        await expect(main.getByRole("article", { name: "Claude Code" })).toBeVisible();
        break;
    }
  });
}
