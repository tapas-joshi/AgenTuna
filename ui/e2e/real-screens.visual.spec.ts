import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

async function openMigration(page: Page) {
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
  await expect(page.getByRole("heading", { name: "Move into AgenTuna" })).toBeVisible();
}

test("Effective config shows populated agent state and why-trace dialog", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  const main = page.getByRole("main", { name: "Effective config" });
  await expect(main.getByText("Code Review", { exact: true })).toBeVisible();
  await expect(main.getByText("2 agents", { exact: true })).toBeVisible();
  await expect(main.getByText("claude_code: linked")).toBeVisible();
  await expect(page).toHaveScreenshot("effective-config-populated.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("button", { name: "Why is this on?" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Why is this on?" });
  await expect(dialog.getByText("This User copy wins for Claude Code.")).toBeVisible();
  await expect(page).toHaveScreenshot("why-trace-dialog.png", { animations: "disabled", caret: "hide" });
});

test("Migrate scan renders several discovered items", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);
  await page.getByRole("button", { name: "Rescan" }).click();
  await expect(page.getByText("3 found · 0 selected")).toBeVisible();
  await expect(page.getByText("Visual migration skill")).toBeVisible();
  await expect(page.getByText("Local docs server")).toBeVisible();
  await expect(page.getByText("Project guidance")).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-scan-populated.png", { animations: "disabled", caret: "hide" });
});

test("Remote-backup preferences never display the stored token", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  const section = page.getByRole("region", { name: "Remote backup" });
  await expect(section.getByText("Connected token saved")).toBeVisible();
  const token = section.getByRole("textbox", { name: "GitHub token" });
  await expect(token).toHaveValue("");
  await expect(token).toHaveAttribute("type", "password");
  await expect(section).not.toContainText("ghp-visual-stored-secret");
  await section.scrollIntoViewIfNeeded();
  await expect(section).toHaveScreenshot("preferences-remote-backup.png", { animations: "disabled", caret: "hide" });
});

test("Skill detail compares the project and user copies", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("deep-link", "agentuna://item/itm_visual_skill");
  });
  await expect(page.getByRole("heading", { name: "Code Review", exact: true, level: 1 }).first()).toBeVisible();
  await page.getByRole("button", { name: "Compare copies" }).click();
  const dialog = page.getByRole("dialog", { name: "Compare copies" });
  await expect(dialog.getByText("user compared with winning project")).toBeVisible();
  await expect(dialog.getByText("project · winner")).toBeVisible();
  await expect(dialog.getByText("user · shadow")).toBeVisible();
  await expect(page).toHaveScreenshot("skill-compare-copies.png", { animations: "disabled", caret: "hide" });
});
