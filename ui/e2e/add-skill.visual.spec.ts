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

test("review previews exact paths and Done reflects the applied Git result", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Write a new skill/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Template").selectOption("blank");
  await page.getByLabel("Skill name").fill("Visual Skill");
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByLabel("Scope").selectOption("project");
  await page.getByLabel("omp").check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill", { exact: true })).toBeVisible();
  await page.screenshot({ path: "test-results/add-skill-review.png", animations: "disabled" });
  await page.getByLabel("Stage these project-scope paths with Git").check();
  await page.getByRole("button", { name: "Import skill" }).click();
  await expect(page.getByRole("heading", { name: "Visual Skill is ready" })).toBeVisible();
  await expect(page.getByText("Operation: op_visual_skill_add")).toBeVisible();
  await expect(page.getByText(/Git staging: staged/)).toBeVisible();
  await expect(page.getByRole("list", { name: "Written file paths" })).toContainText("/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill/SKILL.md");
  await expect(page.getByRole("list", { name: "Actual deployments" })).toContainText("/sandbox/Projects/AgenTuna/.agents/skills/visual-skill");
  await page.screenshot({ path: "test-results/add-skill-done.png", animations: "disabled" });
});
