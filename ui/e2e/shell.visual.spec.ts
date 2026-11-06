import { expect, test } from "./support/tauri-mock";

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
});

test("every routed screen has a meaningful page heading", async ({ page }) => {
  const routes = [
    ["Overview", "Overview"], ["Skills", "Skills"], ["MCP Servers", "MCP Servers"],
    ["Instructions", "Instructions"], ["Agents", "Agents"], ["Effective config", "Effective config"],
    ["Sync & drift", "Sync & drift"],
  ] as const;
  for (const [label, heading] of routes) {
    await page.getByRole("button", { name: label, exact: true }).click();
    await expect(page.getByRole("heading", { name: heading, level: 1 })).toBeVisible();
    await expect(page.locator("main")).toContainText(heading);
    await expect(page.locator("body")).toHaveScreenshot(`shell-${label.toLowerCase().replaceAll(" ", "-").replaceAll("&", "-")}.png`, {
      animations: "disabled", caret: "hide", maxDiffPixelRatio: 0.001,
    });
  }
});
test("deep-linked skill detail renders package content", async ({ page }) => {
  await page.evaluate(() => {
    (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("deep-link", "agentuna://item/itm_visual_skill");
  });
  await expect(page.getByRole("heading", { name: "Code Review", exact: true, level: 1 }).first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Skill document" })).toContainText("A deterministic visual fixture.");
  await expect(page).toHaveScreenshot("skill-detail.png", { animations: "disabled", caret: "hide" });
});
test("MCP detail, MCP form and migration routes render meaningful states", async ({ page }) => {
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown>; emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.item_lookup = { kind: "mcp_server", scope: "user", project_id: null };
    mocks.emit("deep-link", "agentuna://item/srv_visual_fixture");
  });
  await expect(page.getByRole("heading", { name: "MCP Servers", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "This server has no tools" })).toBeVisible();
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByRole("menuitem", { name: "Add MCP server" }).click();
  await expect(page.getByRole("heading", { name: "Add MCP server", exact: true })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Name" })).toBeVisible();
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  const search = palette.getByRole("combobox", { name: "Search commands" });
  await page.keyboard.type("Import native items");
  await page.keyboard.press("Enter");
  await expect(palette).toBeHidden();
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Nothing to migrate" })).toBeVisible();
});

test("Keychain secrets and add-skill routes render real screen content", async ({ page }) => {
  await page.getByRole("button", { name: "Open account menu" }).click();
  await page.getByRole("menuitem", { name: "Keychain secrets" }).click();
  await expect(page.getByRole("heading", { name: "Keychain secrets" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "No secrets yet" })).toBeVisible();
  await expect(page).toHaveScreenshot("secrets-empty.png", { animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await page.getByRole("button", { name: "Add to this repo" }).click();
  await expect(page.getByRole("heading", { name: "Add a skill" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Where does the skill come from?" })).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-source.png", { animations: "disabled", caret: "hide" });
});
