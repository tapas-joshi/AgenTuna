import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

async function openMigration(page: Page) {
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
  await expect(page.getByRole("heading", { name: /items outside AgenTuna/ })).toBeVisible();
}

test("Migrate complete flow: scan -> resolve -> plan -> apply -> done -> undo", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);

  // 1. Scan step
  const main = page.getByRole("main", { name: "Move into AgenTuna" });
  await expect(main.getByRole("complementary", { name: "Scanned folders" })).toBeVisible();
  await main.getByRole("checkbox", { name: "Select all items" }).check();
  await main.getByRole("button", { name: "Continue with 3" }).click();

  // 2. Resolve step
  await expect(page.getByRole("heading", { name: "Resolve conflicts" })).toBeVisible();
  await page.getByLabel("Keep both").check();
  await page.getByRole("button", { name: "Continue" }).click();

  // 3. Plan step
  await expect(page.getByRole("heading", { name: "Plan the move" })).toBeVisible();
  await expect(page.getByText("3 items copied into the store")).toBeVisible();
  await page.getByRole("button", { name: "Show file list" }).click();
  await expect(page.getByText(/Back up original/)).toBeVisible();

  // 4. Apply confirmation modal
  await page.getByRole("button", { name: "Apply 3 items" }).click();
  const confirmModal = page.getByRole("dialog", { name: "Confirm migration" });
  await expect(confirmModal).toBeVisible();
  await expect(confirmModal.getByText("Backup location")).toBeVisible();

  const applyBtn = confirmModal.getByRole("button", { name: "Apply migration" });
  await expect(applyBtn).toBeDisabled();

  // Check confirmation checkbox
  await confirmModal.getByRole("checkbox").check();
  await expect(applyBtn).toBeEnabled();
  await applyBtn.click();

  // 5. Done step
  await expect(page.getByRole("heading", { name: /3 items now live in AgenTuna/ })).toBeVisible();
  await expect(page.getByText(/3 items copied into the Project store/)).toBeVisible();
  const undoBtn = page.getByRole("button", { name: /Undo migration/ });
  await expect(undoBtn).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-done-success.png", { animations: "disabled", caret: "hide" });

  // 6. Undo flow (success)
  await undoBtn.click();
  const undoModal = page.getByRole("dialog", { name: "Undo migration" });
  await expect(undoModal).toBeVisible();
  await undoModal.getByRole("button", { name: "Confirm undo" }).click();
  await expect(page.getByText(/Migration was successfully undone/)).toBeVisible();
});

test("Migrate undo refusal with external edits shows changed paths and force option", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);

  const main = page.getByRole("main", { name: "Move into AgenTuna" });
  await main.getByRole("checkbox", { name: "Select all items" }).check();
  await main.getByRole("button", { name: "Continue with 3" }).click();
  await page.getByLabel("Keep both").check();
  await page.getByRole("button", { name: "Continue" }).click();

  // Apply
  await page.getByRole("button", { name: "Apply 3 items" }).click();
  const confirmModal = page.getByRole("dialog", { name: "Confirm migration" });
  await confirmModal.getByRole("checkbox").check();
  await confirmModal.getByRole("button", { name: "Apply migration" }).click();

  await expect(page.getByRole("heading", { name: /3 items now live in AgenTuna/ })).toBeVisible();

  // Simulate external edits that block undo
  await page.evaluate(() => {
    const mocks = (window as typeof window & {
      __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> };
    }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.operation_undo_check = {
      operation_id: "op_visual_migration",
      known: true,
      already_undone: false,
      expired: false,
      window_days: 30,
      expires_at_unix_ms: BigInt(Date.now() + 30 * 86400 * 1000),
      conflicted_paths: ["/sandbox/Projects/AgenTuna/skills/visual-migration/SKILL.md"],
    };
  });

  // Click Undo
  await page.getByRole("button", { name: /Undo migration/ }).click();
  const undoModal = page.getByRole("dialog", { name: "Undo migration" });
  await expect(undoModal).toBeVisible();
  await expect(undoModal.getByText(/Undo blocked: the following files changed since migration/)).toBeVisible();
  await expect(undoModal.getByText("/sandbox/Projects/AgenTuna/skills/visual-migration/SKILL.md")).toBeVisible();

  // Force button is offered
  const forceBtn = undoModal.getByRole("button", { name: "Force undo and overwrite changes" });
  await expect(forceBtn).toBeVisible();
  await forceBtn.click();
  await expect(page.getByText(/Migration was successfully undone/)).toBeVisible();
});
