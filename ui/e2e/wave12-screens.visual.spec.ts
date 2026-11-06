import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

async function openMigration(page: Page) {
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
  await expect(page.getByRole("heading", { name: /items outside AgenTuna/ })).toBeVisible();
}

test("Instructions shows the designed list, editor footer and the conflict dialog", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Instructions", exact: true }).click();
  const main = page.getByRole("main", { name: "Instructions" });
  await expect(main.getByRole("button", { name: "AGENTS.md", exact: true })).toBeVisible();
  await expect(main.getByText("No unsaved changes")).toBeVisible();
  await expect(page).toHaveScreenshot("instructions-list-editor.png", { animations: "disabled", caret: "hide" });

  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.instruction_save = {
      status: "conflict",
      conflict: {
        path: "/sandbox/Projects/AgenTuna/AGENTS.md",
        diff: "--- disk\n+++ buffer\n@@ -1,2 +1,2 @@\n-# Repository guidance\n+# Repository guidance edited\n \n Review changes carefully.\n",
        disk_content: "# Repository guidance\n\nReview changes carefully.\n",
        disk_hash: "a".repeat(64),
        buffer_content: "# Repository guidance edited\n\nReview changes carefully.\n",
      },
      backup_path: null,
      hash: null,
      unreadable_path: null,
    };
  });
  const editor = main.getByRole("textbox", { name: "Instruction content" });
  await editor.click();
  await page.keyboard.press("Meta+ArrowDown");
  await page.keyboard.type(" Edited.");
  await main.getByRole("button", { name: "Save" }).click();
  const dialog = page.getByRole("dialog", { name: "File changed on disk" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText(/Repository guidance edited/)).toBeVisible();
  await expect(page).toHaveScreenshot("instructions-conflict.png", { animations: "disabled", caret: "hide" });
  await dialog.getByRole("button", { name: "Reload" }).click();
  await expect(dialog).toBeHidden();
});

test("Agents shows copy fallback, missing Set path and the path dialog", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.agents_list = [
      { id: "claude_code", name: "Claude Code", installed: true, version: "1.2.0", managed: true, paths: ["/sandbox/home/.claude/skills"], health: "~/.claude/skills · 2 / 3 managed · 2 in sync · copy fallback", link_mode: "link", presence: "installed", format: "JSON + Markdown skills", managed_skills: 2, managed_servers: 1, native_skills: 1, native_servers: 1, copy_fallback: true, primary_path: "/sandbox/home/.claude/skills", needs_path: false, in_sync: 2, total: 3, linked_deployments: 2 },
      { id: "cursor", name: "Cursor", installed: false, version: null, managed: false, paths: [], health: "!<path> · 0 / 0 managed · 0 in sync", link_mode: "link", presence: "not_installed", format: "—", managed_skills: 0, managed_servers: 0, native_skills: 0, native_servers: 0, copy_fallback: false, primary_path: null, needs_path: true, in_sync: 0, total: 0, linked_deployments: 0 },
    ];
    mocks.commandOverrides.agents_rescan = mocks.commandOverrides.agents_list;
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  const main = page.getByRole("main", { name: "Agents" });
  await expect(main.getByText("copy fallback", { exact: false }).first()).toBeVisible();
  await expect(main.getByText("+1 skill, 1 server outside AgenTuna")).toBeVisible();
  await expect(page).toHaveScreenshot("agents-copy-fallback.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("button", { name: "Set path…" }).click();
  const dialog = page.getByRole("dialog", { name: "Set path for Cursor" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Choose folder…" }).click();
  await expect(dialog.getByRole("textbox", { name: "Agent configuration path" })).toHaveValue("/sandbox/Applications/Cursor.app");
  await expect(page).toHaveScreenshot("agents-set-path.png", { animations: "disabled", caret: "hide" });
});

test("Presets applies and unapplies a reviewed preset with ratio chips", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("tab", { name: "Presets" }).click();
  const main = page.getByRole("main", { name: "Presets" });
  const list = page.getByLabel("Preset list");
  await expect(list.getByText("claude code 2/3")).toBeVisible();
  await expect(main.getByText("1 missing")).toBeVisible();
  await expect(page).toHaveScreenshot("presets-ratios.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("button", { name: "Apply to agent" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "preset_apply").length)).toBe(1);
  await expect(main.getByRole("button", { name: /Applied/ }).first()).toBeVisible();
  await main.getByRole("button", { name: "Unapply" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "preset_unapply").length)).toBe(1);
  await expect(page).toHaveScreenshot("presets-applied.png", { animations: "disabled", caret: "hide" });
});

test("Migrate scan, resolve and plan steps render the designed stepper and panels", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);
  const main = page.getByRole("main", { name: "Move into AgenTuna" });
  await expect(main.getByRole("complementary", { name: "Scanned folders" })).toBeVisible();
  await expect(main.getByRole("checkbox", { name: "Select Visual migration skill" })).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-scan-roots.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("checkbox", { name: "Select all items" }).check();
  await main.getByRole("button", { name: "Continue with 3" }).click();
  await expect(page.getByRole("heading", { name: "Resolve conflicts" })).toBeVisible();
  await page.getByLabel("Keep both").check();
  await expect(page).toHaveScreenshot("migrate-resolve-design.png", { animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("heading", { name: "Plan the move" })).toBeVisible();
  await expect(page.getByText("3 items copied into the store")).toBeVisible();
  await page.getByRole("button", { name: "Show file list" }).click();
  await expect(page.getByText(/Back up original/)).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-plan-design.png", { animations: "disabled", caret: "hide" });
});

test("Effective config renders the table, filter and detail timeline", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  const main = page.getByRole("main", { name: "Effective config" });
  await expect(main.getByRole("tab", { name: /Skills/ })).toBeVisible();
  await expect(main.getByRole("textbox", { name: "Filter effective items" })).toBeVisible();
  await expect(main.getByRole("region", { name: "Effective item detail" })).toContainText("Why is this on?");
  await expect(page).toHaveScreenshot("effective-config-design.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("textbox", { name: "Filter effective items" }).fill("missing");
  await expect(main.getByText(/No items match/)).toBeVisible();
  await main.getByRole("textbox", { name: "Filter effective items" }).fill("");
  await main.getByRole("tab", { name: /MCP servers/ }).click();
  await expect(main.getByRole("heading", { name: "Postgres" })).toBeVisible();
  await expect(page).toHaveScreenshot("effective-config-mcp.png", { animations: "disabled", caret: "hide" });
});
