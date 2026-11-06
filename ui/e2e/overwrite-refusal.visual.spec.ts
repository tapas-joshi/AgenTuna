import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

async function mountApp(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
}

async function showRefusal(page: Page): Promise<void> {
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("agentuna:refused-overwrite", { detail: {
    code: "refused_overwrite",
    message: "AgenTuna refused to overwrite an unmanaged item.",
    refusal: { path: "/fixtures/user/agents/skill", kind: "directory", existing_hash: null, remedies: ["adopt", "move_aside", "cancel"] },
  } })));
}

test("typed RefusedOverwrite dialog screenshot", async ({ page }) => {
  await mountApp(page);
  await showRefusal(page);
  const dialog = page.getByRole("dialog", { name: "AgenTuna won't overwrite this item" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Adopt" })).toBeDisabled();
  await expect(dialog.getByText(/TC-011/)).toBeVisible();
  await expect(page).toHaveScreenshot("overwrite-refusal.png", { animations: "disabled", caret: "hide" });
});

test("Move aside invokes the typed at-fs command; Cancel does not", async ({ page }) => {
  await mountApp(page);
  await showRefusal(page);
  const dialog = page.getByRole("dialog", { name: "AgenTuna won't overwrite this item" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Move aside" }).click();
  const resolved = page.getByRole("dialog", { name: "Overwrite resolved" });
  await expect(resolved.getByText("/fixtures/user/agents/skill.agentuna-backup-20261106T100000Z")).toBeVisible();
  await expect(resolved.getByRole("button", { name: "Undo" })).toHaveCount(0);
  await resolved.getByRole("button", { name: "Done" }).click();
  await expect(resolved).toBeHidden();
});

test("Cancel closes without invoking a file command", async ({ page }) => {
  await mountApp(page);
  await showRefusal(page);
  const dialog = page.getByRole("dialog", { name: "AgenTuna won't overwrite this item" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
});
