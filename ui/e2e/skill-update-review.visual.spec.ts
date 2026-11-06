import { expect, test } from "./support/tauri-mock";

test("skill source updates require explicit check, review, and apply", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_detail_load = {
      item_id: "itm_visual_skill", name: "Code Review", slug: "code-review", scope: "user",
      path: "/fixtures/user/skills/code-review", markdown: "# Code Review\n\nA deterministic visual fixture.",
      description: "Review code changes.", hash: "fixture-hash", updated_at: "2026-11-06T10:00:00Z",
      source: "git", mode: "copy", shadowed_by: null, shadow_chain: ["user"], deployments: [], history: [],
      editable: true, missing: false, update_available: true, source_commit_sha: "installed-sha",
      source_remote_sha: "remote-sha", source_last_checked_at: "2026-11-06T11:00:00Z", auto_update: "inherit",
    };
    mocks.commandOverrides.skill_source_check = { status: "update_available", remote_sha: "remote-sha", local_modified: false };
    mocks.commandOverrides.skill_update_preview = {
      preview_id: "preview-update", files: [{ path: "skills/code-review/SKILL.md", diff: "--- old\n+++ new\n@@ -1 +1 @@\n-old\n+new", backup_required: true }],
    };
    mocks.commandOverrides.skill_update_apply = { operation_id: "op-update", files_changed: 1 };
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeVisible();
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__;
    mocks.emit("deep-link", "agentuna://item/itm_visual_skill");
  });
  await expect(page.getByRole("button", { name: "Check now" })).toBeVisible();
  await expect(page.getByText("Update available")).toBeVisible();
  await expect(page).toHaveScreenshot("skill-update-badge.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: "Check now" }).click();
  await expect(page.getByRole("region", { name: "Skill source updates" }).getByText("Update available")).toBeVisible();
  await page.getByRole("button", { name: "Update now" }).click();
  await expect(page.getByRole("dialog", { name: "Review skill update" })).toBeVisible();
  await page.getByRole("button", { name: "Apply update" }).click();
  await expect(page.getByText("Skill updated", { exact: true })).toBeVisible();
});
