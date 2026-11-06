import { expect, test } from "./support/tauri-mock";

test("add-skill source matches the three-card design", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await expect(page.getByRole("heading", { name: "Add a skill" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Wizard progress" })).toBeVisible();
  await expect(page.locator('[data-source-card="true"]')).toHaveCount(3);
  await expect(page.getByRole("button", { name: /Write a new skill/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Import from git/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /From your library/ })).toBeVisible();
  await page.screenshot({ path: "test-results/add-skill-source.png", animations: "disabled" });
});
test("marketplace source respects disabled network policy", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Marketplace" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Marketplace browsing is unavailable because network policy disables it.", { exact: true })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Search marketplace" })).toHaveCount(0);
});

test("marketplace search runs only after explicit Search", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.network_policy = {
      offline: false, marketplace_enabled: true, background_health: "local", updates_opt_in: false,
      statement: "Network access is user controlled.", allowed_hosts: [], actions: [], events: [], capacity: 100,
    };
    mocks.commandOverrides.skill_marketplace_search = {
      results: [{ id: "example/visual-skill", name: "Visual Skill", author: "Example", installs: 12, source: "https://example.invalid/visual-skill", description: "A visual test fixture." }],
      offline: false, stale: false, provenance: "Marketplace results",
    };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Marketplace" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Marketplace search is enabled. Search runs only after you press Search.", { exact: true })).toBeVisible();
  const query = page.getByRole("textbox", { name: "Search marketplace" });
  await expect(page.getByText("Visual Skill", { exact: true })).toHaveCount(0);
  await query.fill("visual");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page.getByRole("button", { name: /Visual Skill/ })).toBeVisible();
});
test("Git import lists skills only after explicit Fetch and selecting a candidate fills the name", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Import from git/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Repository URL").fill("https://example.invalid/skills.git");
  await expect(page.getByRole("button", { name: "Fetch skills" })).toBeEnabled();
  await expect(page.getByRole("radio", { name: /Review checklist/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Fetch skills" }).click();
  const candidate = page.getByRole("radio", { name: /Review checklist/ });
  await expect(candidate).toBeVisible();
  await candidate.check();
  await expect(candidate).toBeChecked();
  await expect(page.getByLabel("Skill name").first()).toHaveValue("Review checklist");
});

test("review previews exact paths and Done reflects the applied Git result", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Write a new skill/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("radio", { name: /Blank/ }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByLabel("Skill name").first().fill("Visual Skill");
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByLabel("Scope").selectOption("project");
  await page.getByLabel("omp").check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("/fixtures/Projects/AgenTuna/.config-studio/skills/visual-skill", { exact: true })).toBeVisible();
  await page.screenshot({ path: "test-results/add-skill-review.png", animations: "disabled" });
  await page.getByLabel("Stage these project-scope paths with Git").check();
  await page.getByRole("button", { name: "Import skill" }).click();
  await expect(page.getByRole("heading", { name: "Visual Skill is ready" })).toBeVisible();
  await expect(page.getByText("Operation: op_visual_skill_add")).toBeVisible();
  await expect(page.getByText(/Git staging: staged/)).toBeVisible();
  await expect(page.getByRole("list", { name: "Written file paths" })).toContainText("/fixtures/Projects/AgenTuna/.config-studio/skills/visual-skill/SKILL.md");
  await expect(page.getByRole("list", { name: "Actual deployments" })).toContainText("/fixtures/Projects/AgenTuna/.agents/skills/visual-skill");
  await page.screenshot({ path: "test-results/add-skill-done.png", animations: "disabled" });
});
