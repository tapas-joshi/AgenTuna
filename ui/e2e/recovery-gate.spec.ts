import type { Locator, Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

/**
 * Startup recovery gate (TU-004 / TF-020).
 *
 * `recovery_sheet` answers with a view after every unclean exit, and the common answer is an
 * empty one: nothing was half-written. The sheet is a full-window surface, so rendering it for
 * an empty view covered the app and swallowed every pointer event until Continue was pressed —
 * which read as "the app ignores mouse clicks" in the real build.
 */

/** The view the backend returns when an unclean exit interrupted nothing. */
const emptyRecovery = { operations: [], files: [], manual_review: [], temp_files: [] };

/** A view that really does need a person to look at it. */
const pendingRecovery = {
  operations: [{ id: "op-1", kind: "write", paths: ["/sandbox/user/skills/demo/SKILL.md"] }],
  files: [{ operation_id: "op-1", path: "/sandbox/user/skills/demo/SKILL.md", state: "after", can_restore: true, before_hash: "abc" }],
  manual_review: [],
  temp_files: [],
};

/** Answers `recovery_sheet` with `view` for this page; the mock global is installed by the fixture. */
async function useRecoveryView(page: Page, view: unknown): Promise<void> {
  await page.addInitScript((fixture) => {
    // Test-only global installed by e2e/support/tauri-mock.ts; e2e/ sits outside the tsc project.
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.recovery_sheet = fixture;
  }, view);
}

/** Clicks the centre of `locator` with a real mouse event, the way a person would. */
async function mouseClick(page: Page, locator: Locator): Promise<void> {
  const box = await locator.boundingBox();
  if (box === null) throw new Error("element has no layout box");
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
}

test("an empty recovery view never covers the app", async ({ page }) => {
  await useRecoveryView(page, emptyRecovery);
  await page.goto("/");
  const workspace = page.getByRole("navigation", { name: "Workspace" });
  await expect(workspace).toBeVisible();
  await expect(page.getByRole("dialog", { name: "Recover interrupted changes" })).toHaveCount(0);

  // A real mouse click at the centre of a sidebar item has to reach the app.
  await mouseClick(page, workspace.getByRole("button", { name: "Skills", exact: true }));
  await expect(page.getByRole("heading", { name: "Skills" })).toBeVisible();
});

test("a recovery view with work to review still gates until Continue", async ({ page }) => {
  await useRecoveryView(page, pendingRecovery);
  await page.goto("/");
  const gate = page.getByRole("dialog", { name: "Recover interrupted changes" });
  await expect(gate).toBeVisible();
  await expect(gate.getByText("/sandbox/user/skills/demo/SKILL.md").first()).toBeVisible();

  await gate.getByRole("button", { name: "Continue" }).click();
  await expect(gate).toBeHidden();

  const workspace = page.getByRole("navigation", { name: "Workspace" });
  await mouseClick(page, workspace.getByRole("button", { name: "Skills", exact: true }));
  await expect(page.getByRole("heading", { name: "Skills" })).toBeVisible();
});
