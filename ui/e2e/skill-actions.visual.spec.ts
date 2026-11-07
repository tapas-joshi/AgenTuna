import { expect, test } from "./support/tauri-mock";

const managedRows = [
  {
    item_id: "itm_visual_skill",
    name: "code-review",
    scope: "user",
    state: "active",
    shadowed_by: null,
    description: "Automated review guidance for pull requests",
    native: false,
    native_path: null,
    update_available: false,
    updated_at: "2026-11-06T12:00:00Z",
    agents: [
      { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/home/.codex/skills/code-review" },
      { agent: "omp", state: "linked", enabled: true, config_path: "/sandbox/home/.omp/skills/code-review" },
    ],
  },
  {
    item_id: "itm_visual_release_notes",
    name: "release-notes",
    scope: "project",
    state: "active",
    shadowed_by: null,
    description: "Draft release notes from git history",
    native: false,
    native_path: null,
    update_available: false,
    updated_at: "2026-11-06T12:00:00Z",
    agents: [
      { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/Projects/AgenTuna/.agents/skills/release-notes" },
      { agent: "omp", state: "linked", enabled: true, config_path: "/sandbox/Projects/AgenTuna/.agents/skills/release-notes" },
    ],
  },
];

test("Skills multi-select, transfer, delete and repo-disable actions match reviewed states", async ({ page }) => {
  await page.addInitScript((rows) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skills_list = {
      rows,
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed" },
        { id: "codex", label: "Codex", presence: "installed" },
        { id: "omp", label: "omp", presence: "installed" },
      ],
      project_id: "prj_visual_fixture",
      project_name: "AgenTuna",
    };
  }, managedRows);
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });

  await main.getByRole("checkbox", { name: "Select code-review" }).click();
  await main.getByRole("checkbox", { name: "Select release-notes" }).click();
  await expect(page.getByRole("region", { name: "Bulk skill actions" })).toContainText("2 selected");
  await expect(page).toHaveScreenshot("skills-bulk-action-bar.png", { animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: "Actions for code-review" }).click();
  await page.getByRole("menuitem", { name: "Promote" }).click();
  await expect(page.getByRole("dialog", { name: "Promote skill" })).toBeVisible();
  await page.getByRole("button", { name: "Review transfer" }).click();
  await expect(page.getByRole("region", { name: "Destination overwrite warning" })).toBeVisible();
  await expect(page).toHaveScreenshot("skills-promote-overwrite.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: "Replace with backup" }).click();
  await expect(page.getByRole("region", { name: "Reviewed transfer" })).toBeVisible();
  await page.getByRole("button", { name: "Promote skill" }).click();
  await expect(page.getByRole("status")).toContainText("Skill transfer complete");
  await page.getByRole("button", { name: /Dismiss notification: Skill transfer complete/ }).click();

  await page.getByRole("button", { name: "Actions for code-review" }).click();
  await page.getByRole("menuitem", { name: "Delete" }).click();
  await expect(page.getByRole("dialog", { name: "Delete skill" })).toBeVisible();
  await expect(page).toHaveScreenshot("skills-delete-confirmation.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("textbox", { name: "Type skill name to confirm deletion" }).fill("Code Review");
  await page.getByRole("button", { name: "Delete skill" }).click();
  await expect(page.getByRole("status")).toContainText("Skill deleted");
  await page.getByRole("button", { name: /Dismiss notification: Skill deleted/ }).click();

  await page.getByRole("button", { name: "Actions for code-review" }).click();
  await page.getByRole("menuitem", { name: "Disable in this repo" }).click();
  await expect(page.getByRole("dialog", { name: "Disable in this repo" })).toBeVisible();
  await expect(page).toHaveScreenshot("skills-disable-in-repo.png", { animations: "disabled", caret: "hide" });
  await expect(page.getByText(/Backup location:/)).toBeVisible();
  await page.getByRole("button", { name: "Disable in repo" }).click();
  await expect(page.getByRole("status")).toContainText("Disabled in this repo");
});

test("Skill detail exposes transfer, delete and repository disable actions", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("button", { name: "Actions for code-review" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const detail = page;
  await expect(page.getByRole("main").getByRole("heading", { name: "Code Review", exact: true }).first()).toBeVisible();
  await expect(detail.getByRole("button", { name: "Promote" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Move" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Duplicate" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Delete" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Disable in this repo" })).toBeVisible();

  await detail.getByRole("button", { name: "Move" }).click();
  await expect(page.getByRole("dialog", { name: "Move skill to scope" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "Move skill to scope" })).toBeHidden();
  await expect(detail.getByRole("main").getByRole("heading", { name: "Code Review", exact: true }).first()).toBeVisible();
});
