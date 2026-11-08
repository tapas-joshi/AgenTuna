import type { Locator, Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

interface KeyboardStep {
  readonly step: string;
  readonly keystrokes: string;
  readonly expectedFocus: string;
  readonly observed: string;
  readonly result: "pass";
}

async function tabTo(page: Page, target: Locator, description: string): Promise<number> {
  for (let presses = 1; presses <= 120; presses += 1) {
    await page.keyboard.press("Tab");
    if (await target.evaluate((element) => element === document.activeElement)) return presses;
  }
  throw new Error(`Could not reach ${description} with Tab after 120 key presses`);
}

/**
 * Open the app and wait until the shell that owns the shortcut dispatcher is mounted.
 *
 * `App` renders a "Loading AgenTuna…" status until the Tauri bootstrap resolves, and the
 * keydown listener that serves the screen shortcuts is registered when the overlays host
 * mounts after that. A shortcut sent between first paint and shell mount is dropped, so
 * every keyboard flow waits for the visible workspace navigation before pressing keys.
 */
async function gotoShell(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
}

const managedRows = [
  {
    item_id: "itm_visual_skill",
    name: "code-review",
    scope: "user",
    state: "active",
    shadowed_by: null,
    description: "Automated review guidance for pull requests",
    native: false,
    native_path: null,
    update_available: false,
    updated_at: "2026-11-06T12:00:00Z",
    agents: [
      { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/home/.codex/skills/code-review" },
      { agent: "omp", state: "linked", enabled: true, config_path: "/sandbox/home/.omp/skills/code-review" },
    ],
  },
];

const managedAgents = [
  { id: "codex", label: "Codex", presence: "installed" },
  { id: "omp", label: "omp", presence: "installed" },
];

test("keyboard-only project and scope switching, skill bulk actions, and dialogs", async ({ page }) => {
  const steps: KeyboardStep[] = [];
  await gotoShell(page);

  await page.keyboard.press("Meta+p");
  const picker = page.getByRole("dialog", { name: "Switch project" });
  const search = picker.getByRole("combobox", { name: "Search projects" });
  await expect(search).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(picker).toBeHidden();
  const projectTrigger = page.getByRole("button", { name: /Project: AgenTuna/ });
  await expect(projectTrigger).toBeFocused();
  steps.push({ step: "Dismiss project picker", keystrokes: "Cmd+P; Escape", expectedFocus: "project picker closes and focus returns to its opener", observed: "Project picker trigger focused", result: "pass" });
  await page.keyboard.press("Meta+p");
  await expect(search).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(picker).toBeHidden();
  await expect(page.getByRole("button", { name: /Project: AgenTuna/ })).toBeVisible();
  steps.push({ step: "Switch project", keystrokes: "Cmd+P; ArrowDown; Enter", expectedFocus: "Project picker closes after activating the highlighted project", observed: "Project: AgenTuna shown", result: "pass" });

  const scope = page.getByRole("radiogroup", { name: "Scope" });
  const userScope = scope.getByRole("radio", { name: "User" });
  const projectScope = scope.getByRole("radio", { name: "Project" });
  await tabTo(page, userScope, "the selected User scope radio");
  await page.keyboard.press("ArrowRight");
  await expect(projectScope).toBeFocused();
  await expect(projectScope).toHaveAttribute("aria-checked", "true");
  steps.push({ step: "Switch scope", keystrokes: "Tab to selected scope; ArrowRight", expectedFocus: "Project radio is focused and selected", observed: "Project radio focused and selected", result: "pass" });

  await page.evaluate(({ rows, agents }) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skills_list = {
      rows,
      agents,
      project_id: "prj_visual_fixture",
      project_name: "AgenTuna",
    };
  }, { rows: managedRows, agents: managedAgents });
  await page.keyboard.press("Meta+2");
  const skills = page.getByRole("main", { name: "Skills" });
  await expect(skills.getByText("code-review", { exact: true })).toBeVisible();
  const selectSkill = skills.getByRole("checkbox", { name: "Select code-review" });
  const checkboxTabs = await tabTo(page, selectSkill, "the code-review selection checkbox");
  await page.keyboard.press("Space");
  const bulkBar = page.getByRole("region", { name: "Bulk skill actions" });
  await expect(bulkBar).toContainText("1 selected");
  steps.push({ step: "Select a skill for bulk action", keystrokes: `Cmd+2; Tab × ${checkboxTabs}; Space`, expectedFocus: "code-review row checkbox toggled and bulk bar appears", observed: "1 selected", result: "pass" });

  const enableForAgent = bulkBar.getByRole("button", { name: "Enable for agent" });
  const bulkTabs = await tabTo(page, enableForAgent, "the bulk Enable for agent button");
  await page.keyboard.press("Enter");
  const bulkDialog = bulkBar.getByRole("dialog", { name: "Enable skills for agents" });
  const codex = bulkDialog.getByRole("checkbox", { name: "Codex" });
  const agentTabs = await tabTo(page, codex, "the Codex checkbox in the bulk action panel");
  await page.keyboard.press("Space");
  const applyBulk = bulkDialog.getByRole("button", { name: "Enable for selected agents" });
  const applyTabs = await tabTo(page, applyBulk, "the bulk apply button");
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("bulk_skill_apply");
  steps.push({ step: "Apply a bulk agent change", keystrokes: `Tab × ${bulkTabs}; Enter; Tab × ${agentTabs}; Space; Tab × ${applyTabs}; Enter`, expectedFocus: "reviewed bulk action submits without pointer input", observed: "bulk_skill_apply command dispatched", result: "pass" });

  const rowMenu = skills.getByRole("button", { name: "Actions for code-review" });
  await expect(rowMenu).not.toHaveAttribute("aria-disabled", "true");
  const rowMenuTabs = await tabTo(page, rowMenu, "the code-review row actions button");
  await page.keyboard.press("Enter");
  const firstItem = page.getByRole("menuitem", { name: "Open" });
  const promoteItem = page.getByRole("menuitem", { name: "Promote" });
  await expect(firstItem).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await expect(promoteItem).toBeFocused();
  await page.keyboard.press("Enter");
  const promoteDialog = page.getByRole("dialog", { name: "Promote skill" });
  await expect(promoteDialog).toBeVisible();
  await expect.poll(() => promoteDialog.evaluate((element) => element.contains(document.activeElement))).toBe(true);
  for (let index = 0; index < 10; index += 1) {
    await page.keyboard.press("Tab");
    await expect.poll(() => promoteDialog.evaluate((element) => element.contains(document.activeElement))).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(promoteDialog).toBeHidden();
  await expect(rowMenu).toBeFocused();
  steps.push({ step: "Open and dismiss a skill dialog", keystrokes: `Tab × ${rowMenuTabs}; Enter; ArrowDown × 2; Enter; Tab × 10; Escape`, expectedFocus: "menu arrows reach Promote; dialog traps Tab; Escape restores row-action focus", observed: "dialog closed and row-action trigger focused", result: "pass" });

  await test.info().attach("keyboard-flow-results", { body: JSON.stringify(steps, null, 2), contentType: "application/json" });
});

test("keyboard-only MCP list, detail and form editing", async ({ page }) => {
  const steps: KeyboardStep[] = [];
  await gotoShell(page);
  await page.keyboard.press("Meta+3");
  const list = page.getByRole("main", { name: "MCP Servers" });
  await expect(list.getByText("Visual server", { exact: true })).toBeVisible();
  const rowActions = page.getByRole("button", { name: "Actions for Visual server" });
  const actionTabs = await tabTo(page, rowActions, "the Visual server actions button");
  await page.keyboard.press("Enter");
  const openItem = page.getByRole("menuitem", { name: "Open" });
  await expect(openItem).toBeFocused();
  await page.keyboard.press("Enter");
  const detail = page.getByRole("main", { name: "MCP Server Detail" });
  await expect(detail.getByRole("heading", { name: "Visual server" })).toBeVisible();
  steps.push({ step: "Open MCP server details", keystrokes: `Cmd+3; Tab × ${actionTabs}; Enter; Enter`, expectedFocus: "Open menu item activates the Visual server detail screen", observed: "Visual server detail heading visible", result: "pass" });

  const editButton = detail.getByRole("button", { name: "Edit" });
  const editTabs = await tabTo(page, editButton, "the MCP Edit button");
  await page.keyboard.press("Enter");
  const form = page.locator('main[aria-labelledby="mcp-add-title"]');
  const name = form.getByRole("textbox", { name: "Name", exact: true });
  await expect(name).toBeVisible();
  const nameTabs = await tabTo(page, name, "the MCP server Name field");
  await page.keyboard.press("Meta+a");
  await page.keyboard.type("Keyboard server");
  const review = form.getByRole("button", { name: "Review edit" });
  const reviewTabs = await tabTo(page, review, "the Review edit button");
  await page.keyboard.press("Enter");
  await expect(form.getByRole("region", { name: "Edit summary" })).toBeVisible();
  steps.push({ step: "Edit MCP configuration", keystrokes: `Tab × ${editTabs}; Enter; Tab × ${nameTabs}; Cmd+A; type Keyboard server; Tab × ${reviewTabs}; Enter`, expectedFocus: "MCP edit enters reviewed summary without applying a write", observed: "Edit summary displayed", result: "pass" });

  await test.info().attach("keyboard-flow-results", { body: JSON.stringify(steps, null, 2), contentType: "application/json" });
});

test("keyboard-only drift review and four-step migration", async ({ page }) => {
  const steps: KeyboardStep[] = [];
  await gotoShell(page);
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.drift_snapshot = {
      rows: [{ event_id: 17n, kind: "drifted", item_id: "itm_visual_skill", item_name: "Code Review", agent: "codex", scope: "project", path: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md", detected_at: "2026-11-06T10:00:00Z", changed_file_count: 1, can_adopt: true, can_restore: true, link_target: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review" }],
      in_sync_count: 41,
      watched_count: 42,
      watcher: { status: "active", path: null, error: null },
    };
  });
  const driftNav = page.getByRole("navigation", { name: "Workspace" }).getByRole("button", { name: "Sync & drift", exact: true });
  const driftNavTabs = await tabTo(page, driftNav, "the Sync & drift navigation button");
  await page.keyboard.press("Enter");
  const drift = page.getByRole("main", { name: "Sync and drift" });
  const changedItem = drift.getByRole("button", { name: /Code Review/ }).first();
  await expect(changedItem).toBeVisible();
  const changedItemTabs = await tabTo(page, changedItem, "the Code Review drift item");
  await page.keyboard.press("Enter");
  await expect(drift.getByRole("button", { name: "Adopt on-disk" })).toBeVisible();
  const adopt = drift.getByRole("button", { name: "Adopt on-disk" });
  const adoptTabs = await tabTo(page, adopt, "the Adopt on-disk action");
  await page.keyboard.press("Enter");
  const adoptDialog = page.getByRole("dialog", { name: "Adopt on-disk changes?" });
  await expect(adoptDialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(adoptDialog).toBeHidden();
  await expect(adopt).toBeFocused();
  steps.push({ step: "Review drift using keyboard", keystrokes: `Tab × ${driftNavTabs}; Enter; Tab × ${changedItemTabs}; Enter; Tab × ${adoptTabs}; Enter; Escape`, expectedFocus: "drift confirmation closes and returns focus to Adopt on-disk", observed: "dialog closed; Adopt on-disk focused", result: "pass" });

  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  const commandSearch = palette.getByRole("combobox", { name: "Search commands" });
  await expect(commandSearch).toBeFocused();
  await page.keyboard.type("Import native items");
  await page.keyboard.press("Enter");
  const migrate = page.getByRole("main", { name: "Move into AgenTuna" });
  await expect(migrate.getByRole("heading", { name: /items outside AgenTuna/ })).toBeVisible();
  steps.push({ step: "Open migration scan", keystrokes: "Cmd+K; type Import native items; Enter", expectedFocus: "migration scan screen opens from the command palette", observed: "Move into AgenTuna screen visible", result: "pass" });

  const selectAll = migrate.getByRole("checkbox", { name: "Select all items" });
  const selectTabs = await tabTo(page, selectAll, "the migration Select all items checkbox");
  await page.keyboard.press("Space");
  const continueScan = migrate.getByRole("button", { name: "Continue with 3" });
  const scanContinueTabs = await tabTo(page, continueScan, "the migration Continue button");
  await page.keyboard.press("Enter");
  const resolve = page.getByRole("heading", { name: "Resolve conflicts" });
  await expect(resolve).toBeVisible();
  steps.push({ step: "Select migration items and continue", keystrokes: `Tab × ${selectTabs}; Space; Tab × ${scanContinueTabs}; Enter`, expectedFocus: "Resolve conflicts step opens", observed: "Resolve conflicts heading visible", result: "pass" });

  const keepA = page.getByLabel("Keep A");
  const keepBoth = page.getByLabel("Keep both");
  const resolveTabs = await tabTo(page, keepA, "the conflict resolution radio options");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect(keepBoth).toBeFocused();
  const continueResolve = page.getByRole("button", { name: "Continue" });
  const resolveContinueTabs = await tabTo(page, continueResolve, "the Resolve Continue button");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "Preview the exact changes" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Planned files" })).toBeVisible();
  steps.push({ step: "Resolve migration conflict and continue", keystrokes: `Tab × ${resolveTabs}; ArrowRight × 2; Tab × ${resolveContinueTabs}; Enter`, expectedFocus: "Preview step opens with the reviewed file list", observed: "Preview the exact changes heading and Planned files region visible", result: "pass" });

  const apply = page.getByRole("button", { name: "Apply 3 items" });
  const applyTabs = await tabTo(page, apply, "the Apply migration button");
  await page.keyboard.press("Enter");
  const confirmation = page.getByRole("dialog", { name: "Confirm migration" });
  await expect(confirmation).toBeVisible();
  const confirmCheckbox = confirmation.getByRole("checkbox");
  const confirmTabs = await tabTo(page, confirmCheckbox, "the migration confirmation checkbox");
  await page.keyboard.press("Space");
  const confirmButton = confirmation.getByRole("button", { name: "Apply migration" });
  const finalTabs = await tabTo(page, confirmButton, "the final Apply migration button");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: /3 items now live in AgenTuna/ })).toBeVisible();
  steps.push({ step: "Review and apply the migration plan", keystrokes: `Tab × ${applyTabs}; Enter; Tab × ${confirmTabs}; Space; Tab × ${finalTabs}; Enter`, expectedFocus: "migration completes after keyboard confirmation", observed: "migration Done heading visible", result: "pass" });

  await test.info().attach("keyboard-flow-results", { body: JSON.stringify(steps, null, 2), contentType: "application/json" });
});
