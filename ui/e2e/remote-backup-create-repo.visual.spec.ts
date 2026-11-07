import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

type TestMocks = { commandOverrides: Record<string, unknown>; calls: string[]; emit: (event: string, payload: unknown) => void };

async function openRemoteBackup(page: Page) {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  return page.getByRole("region", { name: "Remote backup" });
}


async function setCreateResult(page: Page, result: unknown) {
  await page.evaluate((value) => {
    const target = (window as typeof window & { __AT_TEST_MOCKS__: TestMocks }).__AT_TEST_MOCKS__;
    target.commandOverrides["remote_backup_create_repo"] = value;
  }, result);
}

async function setCreateFailure(page: Page, failure: unknown) {
  await page.evaluate((value) => {
    const target = (window as typeof window & { __AT_TEST_MOCKS__: TestMocks }).__AT_TEST_MOCKS__;
    const rejected = Promise.reject(value);
    void rejected.catch(() => undefined);
    target.commandOverrides["remote_backup_create_repo"] = rejected;
  }, failure);
}

test("private repository creation confirms, can be cancelled, and reports success", async ({ page }) => {
  const section = await openRemoteBackup(page);
  await section.getByRole("button", { name: "Create private repo" }).click();
  const panel = page.getByRole("region", { name: "Private repository creation" });
  await expect(panel.getByRole("heading", { name: "Create a private GitHub repository" })).toBeVisible();
  await panel.getByRole("textbox", { name: "Private repository name" }).fill("agentuna-backup");
  await panel.scrollIntoViewIfNeeded();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-confirm.png", { animations: "disabled", caret: "hide" });

  await setCreateResult(page, { job_id: "job_visual_repo_create" });
  await panel.getByRole("button", { name: "Create repository" }).click();
  await expect(panel.getByRole("button", { name: "Cancel creation" })).toBeVisible();
  await expect(panel.getByText("Creating private repository…")).toBeVisible();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-creating.png", { animations: "disabled", caret: "hide" });
  await panel.getByRole("button", { name: "Cancel creation" }).click();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: TestMocks }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_repo_create", name: "Create private repository", state: "cancelled", error: null }));
  await expect(section.getByText("Private repository creation was cancelled.")).toBeVisible();

  await panel.getByRole("button", { name: "Create repository" }).click();
  await expect(panel.getByRole("button", { name: "Cancel creation" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: TestMocks }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_repo_create", name: "Create private repository", state: "succeeded", error: null }));
  await expect(section.getByText("Private repository created and connected.")).toBeVisible();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-success.png", { animations: "disabled", caret: "hide" });
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: TestMocks }).__AT_TEST_MOCKS__.calls)).toContain("remote_backup_create_repo");
});

test("scope refusal returns a typed remedy and never submits token contents", async ({ page }) => {
  const section = await openRemoteBackup(page);
  await section.getByRole("button", { name: "Create private repo" }).click();
  const panel = page.getByRole("region", { name: "Private repository creation" });
  await panel.getByRole("textbox", { name: "Private repository name" }).fill("agentuna-backup");
  await setCreateFailure(page, { code: "repo_scope_missing", message: "The saved token needs the repo scope.", remedy: "add_repo_scope" });
  await panel.getByRole("button", { name: "Create repository" }).click();
  await expect(panel.getByText("Scope refused: the saved token needs GitHub’s repo scope.")).toBeVisible();
  await expect(section.getByRole("alert")).toContainText("The saved token needs the repo scope.");
  await section.getByRole("alert").scrollIntoViewIfNeeded();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-scope-refused.png", { animations: "disabled", caret: "hide" });
  await section.getByRole("alert").getByRole("button", { name: "Focus token field" }).click();
  await expect(section.getByRole("textbox", { name: "GitHub token" })).toBeFocused();
});

test("private repository creation distinguishes offline and authentication failures", async ({ page }) => {
  const section = await openRemoteBackup(page);
  await section.getByRole("button", { name: "Create private repo" }).click();
  const panel = page.getByRole("region", { name: "Private repository creation" });
  await panel.getByRole("textbox", { name: "Private repository name" }).fill("agentuna-backup");
  await setCreateFailure(page, { code: "offline", message: "Offline mode is on.", remedy: "turn_network_on" });
  await panel.getByRole("button", { name: "Create repository" }).click();
  await expect(panel.getByText("Offline: turn off offline mode before contacting GitHub.")).toBeVisible();
  await section.getByRole("alert").scrollIntoViewIfNeeded();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-offline.png", { animations: "disabled", caret: "hide" });

  await setCreateFailure(page, { code: "token_rejected", message: "GitHub rejected the saved token.", remedy: "replace_token" });
  await panel.getByRole("button", { name: "Create repository" }).click();
  await expect(panel.getByText("Authentication failed: replace or save a valid GitHub token.")).toBeVisible();
  await section.getByRole("alert").scrollIntoViewIfNeeded();
  await expect.soft(page).toHaveScreenshot("preferences-remote-repo-auth-failed.png", { animations: "disabled", caret: "hide" });
});
