import { expect, test } from "./support/tauri-mock";

test("account menu is visible and exposes app information", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
  const accountTrigger = page.getByRole("button", { name: "Open account menu" });
  await expect(accountTrigger).toBeVisible();
  await accountTrigger.click();
  await expect(page.getByRole("menuitem", { name: "About AgenTuna" })).toBeVisible();
  await expect(page.getByRole("menu")).toBeInViewport();
  await expect(page).toHaveScreenshot("account-menu.png", { animations: "disabled", caret: "hide" });
});
