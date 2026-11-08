import { expect, test } from "./support/tauri-mock";
import type { Locator, Page } from "@playwright/test";


async function tabTo(page: Page, target: Locator): Promise<void> {
  for (let step = 0; step < 80; step += 1) {
    if (await target.evaluate((element) => element === document.activeElement)) return;
    await page.keyboard.press("Tab");
  }
  throw new Error("Keyboard tab order did not reach the requested control");
}
async function openMigration(page: Page) {
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.keyboard.press("Meta+k");

  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
}

test("Instructions loads, edits and previews a sanitized rule", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Instructions", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Instructions", level: 1 })).toBeVisible();
  await expect(page.getByRole("button", { name: "AGENTS.md", exact: true })).toBeVisible();
  await page.getByRole("radio", { name: "Preview" }).click();
  await expect(page.getByRole("paragraph").filter({ hasText: "Review changes carefully." })).toBeVisible();
  await expect(page).toHaveScreenshot("instructions-populated.png", { animations: "disabled", caret: "hide" });
});

test("keyboard-only Instructions flow creates, edits, saves and previews a rule file", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & {
      __AT_TEST_MOCKS__: { settings: { values: Record<string, unknown> }; sampleProject: { id: string } };
    }).__AT_TEST_MOCKS__;
    mocks.settings.values[`last_scope.${mocks.sampleProject.id}`] = "project";
  });
  await page.goto("/");
  await page.keyboard.press("Meta+k");
  await page.keyboard.type("Instructions");
  await page.keyboard.press("Enter");
  const main = page.getByRole("main", { name: "Instructions" });
  await expect(main).toBeVisible();

  const addRule = main.getByRole("button", { name: "New rule file", exact: true });
  await tabTo(page, addRule);
  await expect(addRule).toBeFocused();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "New rule file" });
  await expect(dialog).toBeVisible();
  const fileType = dialog.getByRole("radiogroup", { name: "File type" }).getByRole("radio", { name: "CLAUDE.md" });
  const scope = dialog.getByRole("radiogroup", { name: "Scope" }).getByRole("radio", { name: "Project" });
  const create = dialog.getByRole("button", { name: "Create CLAUDE.md in project scope" });
  await tabTo(page, fileType);
  await expect(fileType).toBeFocused();
  await tabTo(page, scope);
  await expect(scope).toBeFocused();
  await tabTo(page, create);
  await expect(create).toBeFocused();
  await page.keyboard.press("Enter");

  const editor = main.getByRole("textbox", { name: "Instruction content" });
  await expect(editor).toBeVisible();
  await tabTo(page, editor);
  await expect(editor).toBeFocused();
  await page.keyboard.press("Meta+ArrowDown");
  await page.keyboard.type("\nKeyboard-created guidance.");
  const save = main.getByRole("button", { name: "Save", exact: true });
  await tabTo(page, save);
  await expect(save).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(main.getByText("No unsaved changes")).toBeVisible();

  const edit = main.getByRole("radio", { name: "Edit" });
  const preview = main.getByRole("radio", { name: "Preview" });
  await tabTo(page, edit);
  await expect(edit).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(preview).toBeFocused();
  await page.keyboard.press("Space");
  await expect(main.getByRole("paragraph").filter({ hasText: "Keyboard-created guidance." })).toBeVisible();
  await page.keyboard.press("Tab");
  expect(await page.evaluate(() => document.activeElement?.closest('[data-slot="segmented-control"]') === null)).toBe(true);
});

test("Agents exposes persisted manage and deployment settings", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Agents", level: 1 })).toBeVisible();
  await expect(page.getByText("~/.claude/skills · 2 / 3 managed · 2 in sync")).toBeVisible();
  await expect(page.getByRole("article", { name: "Claude Code" }).getByRole("switch", { name: "Let AgenTuna manage this agent" })).toBeChecked();
  await page.getByRole("radio", { name: "Copy" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "agent_link_mode_set").length)).toBe(1);
  await expect(page).toHaveScreenshot("agents-populated.png", { animations: "disabled", caret: "hide" });
});

test("Presets lists database ratios and edits member state", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("tab", { name: "Presets" }).click();
  await expect(page.getByRole("heading", { name: "Presets", level: 1 })).toBeVisible();
  await expect(page.getByLabel("Preset list").getByText("Code review", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("presets-populated.png", { animations: "disabled", caret: "hide" });
});

test("Migrate resolve compares selected copies and supports Keep both names", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);
  await page.getByRole("checkbox", { name: "Select all items" }).check();
  await page.getByRole("button", { name: "Continue with 3" }).click();
  await expect(page.getByRole("heading", { name: "Resolve conflicts", level: 1 })).toBeVisible();
  await expect(page.getByRole("heading", { name: "skill · review-helper" })).toBeVisible();
  await page.getByLabel("Keep both").check();
  await expect(page.getByLabel("Copy A name")).toHaveValue("review-helper-claude-code");
  await expect(page.getByLabel("Copy B name")).toHaveValue("review-helper-codex");
  await expect(page.getByRole("main", { name: "Move into AgenTuna" }).getByRole("button", { name: "Back", exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-resolve-populated.png", { animations: "disabled", caret: "hide" });
});
