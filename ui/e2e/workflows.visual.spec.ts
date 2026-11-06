import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

async function openMigration(page: Page) {
  await page.keyboard.press("Meta+k");

  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
}

test("Instructions loads, edits and previews a sanitized rule", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Instructions", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Instructions", level: 1 })).toBeVisible();
  await expect(page.getByRole("button", { name: /AGENTS.md/ })).toBeVisible();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByRole("article")).toContainText("Review changes carefully.");
  await expect(page).toHaveScreenshot("instructions-populated.png", { animations: "disabled", caret: "hide" });
});

test("Agents exposes persisted manage and deployment settings", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Agents", level: 1 })).toBeVisible();
  await expect(page.getByText("~/.claude/skills · 2 / 3 managed · 2 in sync")).toBeVisible();
  await expect(page.getByRole("checkbox", { name: "Manage Claude Code" })).toBeChecked();
  await page.getByRole("combobox", { name: "Claude Code deployment mode" }).selectOption("copy");
  await expect(page).toHaveScreenshot("agents-populated.png", { animations: "disabled", caret: "hide" });
});

test("Presets lists database ratios and edits member state", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Presets", level: 1 })).toBeVisible();
  await expect(page.getByText("Code review", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("presets-populated.png", { animations: "disabled", caret: "hide" });
});

test("Migrate resolve compares selected copies and supports Keep both names", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);
  await page.getByRole("button", { name: "Rescan" }).click();
  await page.getByRole("checkbox", { name: "Select all skill" }).check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByRole("heading", { name: "Resolve conflicts", level: 1 })).toBeVisible();
  await expect(page.getByRole("heading", { name: "skill · review-helper" })).toBeVisible();
  await page.getByLabel("Keep both").check();
  await expect(page.getByLabel("Copy A name")).toHaveValue("review-helper-a");
  await expect(page.getByLabel("Copy B name")).toHaveValue("review-helper-b");
  await expect(page.getByRole("button", { name: "Back to scan" })).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-resolve-populated.png", { animations: "disabled", caret: "hide" });
});
