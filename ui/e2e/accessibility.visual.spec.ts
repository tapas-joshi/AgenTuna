import { expect, test } from "./support/tauri-mock";

const project = {
  id: "prj_visual_fixture",
  name: "AgenTuna",
  path: "/sandbox/Projects/AgenTuna",
  last_used_at: "2026-11-06T10:00:00Z",
  skill_count: 5,
  server_count: 2,
};

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
});

test("picker, palette, Preferences and state-view baselines", async ({ page }) => {
  await page.evaluate((fixture) => { (window as typeof window & { __AT_TEST_MOCKS__: { projects: typeof fixture[] } }).__AT_TEST_MOCKS__.projects = [fixture]; }, project);

  await page.keyboard.press("Meta+p");
  await expect(page.getByRole("dialog", { name: "Switch project" })).toBeVisible();
  await expect(page).toHaveScreenshot("picker-populated.png", { animations: "disabled", caret: "hide" });
  await page.keyboard.press("Escape");

  await page.keyboard.press("Meta+k");
  await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();
  await expect(page).toHaveScreenshot("palette-idle.png", { animations: "disabled", caret: "hide" });
  await page.keyboard.press("Escape");

  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  await expect(page).toHaveScreenshot("preferences-populated.png", { animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Skills", exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("state-empty.png", { animations: "disabled", caret: "hide" });
});
test("Preferences loading and error state-view baselines", async ({ page }) => {
  await page.evaluate(() => { (window as typeof window & { __AT_TEST_MOCKS__: { policyMode: string } }).__AT_TEST_MOCKS__.policyMode = "pending"; });
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Loading preferences" })).toBeVisible();
  await expect(page).toHaveScreenshot("state-loading.png", { animations: "disabled", caret: "hide" });

  await page.keyboard.press("Meta+1");
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
  await page.evaluate(() => { (window as typeof window & { __AT_TEST_MOCKS__: { policyMode: string } }).__AT_TEST_MOCKS__.policyMode = "error"; });
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Couldn't load preferences" })).toBeVisible();
  await expect(page).toHaveScreenshot("state-error.png", { animations: "disabled", caret: "hide" });
});

test("keyboard-only project, scope, palette, Add menu and focus behavior", async ({ page }) => {
  const steps: { step: string; keystrokes: string; expectedFocus: string; observed: string; result: "pass" }[] = [];
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
  await page.evaluate((fixture) => { (window as typeof window & { __AT_TEST_MOCKS__: { projects: typeof fixture[] } }).__AT_TEST_MOCKS__.projects = [fixture]; }, project);

  await page.keyboard.press("Meta+p");
  const picker = page.getByRole("dialog", { name: "Switch project" });
  await expect(picker).toBeVisible();
  const projectSearch = picker.getByRole("combobox", { name: "Search projects" });
  await expect(projectSearch).toBeFocused();
  steps.push({ step: "Open project picker", keystrokes: "Cmd+P", expectedFocus: "Search projects", observed: await projectSearch.getAttribute("aria-label") ?? "missing", result: "pass" });
  await picker.getByRole("option", { name: /AgenTuna/ }).click();
  await expect(picker).toBeHidden();
  await expect(page.getByRole("button", { name: /Project: AgenTuna/ })).toBeVisible();
  steps.push({ step: "Switch project", keystrokes: "Click project row", expectedFocus: "Picker closes and project name is shown", observed: await page.getByRole("button", { name: /Project: AgenTuna/ }).getAttribute("aria-label") ?? "missing", result: "pass" });

  const scope = page.getByRole("radiogroup", { name: "Scope" });
  await expect(scope).toBeVisible();
  const globalScope = scope.getByRole("radio", { name: "Global" });
  for (let tab = 0; tab < 16 && !(await globalScope.evaluate((element) => element === document.activeElement)); tab += 1) {
    await page.keyboard.press("Tab");
  }
  await expect(globalScope).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(scope.getByRole("radio", { name: "User" })).toBeFocused();
  steps.push({ step: "Switch scope", keystrokes: "Tab; ArrowRight", expectedFocus: "User radio", observed: "User radio focused", result: "pass" });
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  const commandSearch = palette.getByRole("combobox", { name: "Search commands" });
  await expect(commandSearch).toBeFocused();
  await page.keyboard.type("Preferences");
  await page.keyboard.press("Enter");
  await expect(palette).toBeHidden();
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  steps.push({ step: "Open palette and run Preferences action", keystrokes: "Cmd+K; type Preferences; Enter", expectedFocus: "Search commands, then Preferences screen", observed: "Preferences heading visible", result: "pass" });

  await page.keyboard.press("Meta+n");
  await expect(page.getByRole("menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toBeHidden();
  await expect(page.getByRole("button", { name: "Add" })).toBeFocused();
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  steps.push({ step: "Dismiss Add menu", keystrokes: "Cmd+N; Esc", expectedFocus: "Add button; route unchanged", observed: "Add button focused; Preferences route remains", result: "pass" });
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  steps.push({ step: "Escape without overlay", keystrokes: "Esc", expectedFocus: "Route unchanged (TU-016)", observed: "Preferences route remains", result: "pass" });
  await test.info().attach("keyboard-flow-results", { body: JSON.stringify(steps, null, 2), contentType: "application/json" });
});
