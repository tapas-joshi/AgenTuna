import { expect, test } from "./support/tauri-mock";

test("Help opens the registered keyboard-shortcuts dialog", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("agentuna:menu-action", { detail: "keyboard-shortcuts" })));
  const dialog = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("Command palette")).toBeVisible();
  await expect(page).toHaveScreenshot("keyboard-shortcuts.png", { animations: "disabled", caret: "hide" });
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
});
