import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

async function openMigration(page: Page) {
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
  await expect(page.getByRole("heading", { name: /items outside AgenTuna/ })).toBeVisible();
}

test("Effective config shows populated agent state and why-trace dialog", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  const main = page.getByRole("main", { name: "Effective config" });
  await expect(main.getByText("Code Review", { exact: true }).first()).toBeVisible();
  await expect(main.getByText("2/2", { exact: true }).first()).toBeVisible();
  await expect(main.getByRole("region", { name: "Effective item detail" })).toContainText("Loaded for Claude Code, Codex");
  await expect(page).toHaveScreenshot("effective-config-populated.png", { animations: "disabled", caret: "hide" });

  await main.getByRole("button", { name: "Why is this on?" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Why is this on?" });
  await expect(dialog.getByText("This User copy wins for Claude Code.")).toBeVisible();
  await expect(page).toHaveScreenshot("why-trace-dialog.png", { animations: "disabled", caret: "hide" });
});

test("Migrate scan renders several discovered items", async ({ page }) => {
  await page.goto("/");
  await openMigration(page);
  await expect(page.getByText("3 found · 0 selected")).toBeVisible();
  await expect(page.getByText("Visual migration skill")).toBeVisible();
  await expect(page.getByText("Local docs server")).toBeVisible();
  await expect(page.getByText("Project guidance")).toBeVisible();
  await expect(page).toHaveScreenshot("migrate-scan-populated.png", { animations: "disabled", caret: "hide" });
});

test("Remote-backup preferences show status and reviewed conflict actions without exposing the stored token", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  const section = page.getByRole("region", { name: "Remote backup" });
  await expect(section.getByText("Connected token saved")).toBeVisible();
  const token = section.getByRole("textbox", { name: "GitHub token" });
  await expect(token).toHaveValue("");
  await expect(token).toHaveAttribute("type", "password");
  await expect(section).not.toContainText("ghp-visual-stored-secret");
  await expect(section.getByRole("button", { name: "Backup now" })).toBeVisible();
  await expect(section.getByRole("button", { name: "Sync", exact: true })).toBeVisible();
  await expect(section.getByRole("button", { name: "Restore", exact: true })).toBeVisible();
  await expect(section).toContainText("Last sync: Never");
  await expect(section).toContainText("Last commit: None");

  // A device name with a slash is refused before any command is sent.
  const device = section.getByRole("textbox", { name: "Remote backup device name" });
  await device.fill("lab/desk");
  await section.getByRole("button", { name: "Save remote backup settings" }).click();
  await expect(section.getByRole("alert")).toContainText("Device names cannot contain slashes.");
  await expect(device).toHaveAttribute("aria-invalid", "true");
  await device.scrollIntoViewIfNeeded();
  await expect(page).toHaveScreenshot("preferences-remote-backup-validation.png", { animations: "disabled", caret: "hide" });
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).not.toContain("remote_backup_configure");
  await device.fill("Visual workstation");
  await expect(section.getByRole("alert")).toHaveCount(0);

  // Restore names the device and repository in its confirmation.
  await section.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(section.getByLabel("Restore commit")).toBeVisible();
  await section.getByRole("button", { name: "Review restore" }).click();
  const confirm = page.getByRole("dialog", { name: "Confirm restore" });
  await expect(confirm).toContainText("Visual workstation");
  await expect(confirm).toContainText("https://example.invalid/agentuna-backup.git");
  await confirm.scrollIntoViewIfNeeded();
  await expect(page).toHaveScreenshot("preferences-remote-backup-restore.png", { animations: "disabled", caret: "hide" });
  await section.getByRole("button", { name: "Cancel restore" }).click();
  await section.getByRole("button", { name: "Close", exact: true }).first().click();

  // Backup now runs as a cancellable job that reports progress and a final status.
  await section.getByRole("button", { name: "Backup now" }).click();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-progress", { id: "job_visual_remote_backup", name: "Backup remote store", fraction: 0.5, label: "Uploading snapshot" }));
  await expect(section.getByText("Uploading snapshot")).toBeVisible();
  await expect(section.getByRole("progressbar", { name: "Remote backup progress" })).toBeVisible();
  await expect(section.getByRole("button", { name: "Cancel backup remote store" })).toBeVisible();
  await section.getByRole("button", { name: "Cancel backup remote store" }).scrollIntoViewIfNeeded();
  await expect(page).toHaveScreenshot("preferences-remote-backup-progress.png", { animations: "disabled", caret: "hide" });
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_remote_backup", name: "Backup remote store", state: "succeeded", error: null }));
  await expect(section.getByText("Remote backup action completed.")).toBeVisible();

  // The conflict list shows both copies and only offers a reviewed resolution.
  await section.getByRole("button", { name: /Resolve conflicts/ }).click();
  await expect(section.getByText("skills/review-helper/SKILL.md", { exact: true })).toBeVisible();
  await expect(section).toContainText("/sandbox/user/skills/review-helper/SKILL.md");
  await expect(section).toContainText(".state/remote-conflicts/travel-laptop");
  await expect(section.getByRole("button", { name: /overwrite/i })).toHaveCount(0);
  await section.getByRole("button", { name: "Keep remote" }).click();
  await expect(section.getByRole("button", { name: "Apply reviewed resolution" })).toBeVisible();
  await section.getByRole("button", { name: "Apply reviewed resolution" }).scrollIntoViewIfNeeded();
  await expect(page).toHaveScreenshot("preferences-remote-backup.png", { animations: "disabled", caret: "hide" });
});

test("Skill detail compares the project and user copies", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("deep-link", "agentuna://item/itm_visual_skill");
  });
  await expect(page.getByRole("heading", { name: "Code Review", exact: true, level: 1 }).first()).toBeVisible();
  await page.getByRole("button", { name: "Compare copies" }).click();
  const dialog = page.getByRole("dialog", { name: "Compare copies" });
  await expect(dialog.getByText("user compared with winning project")).toBeVisible();
  await expect(dialog.getByText("project · winner")).toBeVisible();
  await expect(dialog.getByText("user · shadow")).toBeVisible();
  await expect(page).toHaveScreenshot("skill-compare-copies.png", { animations: "disabled", caret: "hide" });
});

test("Skill detail enables an agent through a reviewed TS-010 plan", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("deep-link", "agentuna://item/itm_visual_skill");
  });
  await expect(page.getByRole("heading", { name: "Code Review", exact: true, level: 1 }).first()).toBeVisible();
  await page.getByRole("button", { name: "Manage agents" }).click();
  const dialog = page.getByRole("dialog", { name: "Enable for agents" });
  await dialog.getByRole("checkbox", { name: "Claude Code" }).check();
  await dialog.getByRole("button", { name: "Apply" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "skill_agent_apply").length)).toBe(1);
});

test("Why trace explains all scopes, exposes diagnostics, and restores focus on dismissal", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.why_trace = {
      trace: {
        item_id: "itm_visual_skill",
        active_scope: "user",
        agent: "claude_code",
        steps: [
          { scope: "global", status: "Absent", location: null, reason: "No Global definition." },
          { scope: "user", status: "Winning", location: { path: "/sandbox/user/skills/code-review/SKILL.md", byte_span: null, line: null, rendering_path: null }, reason: "The User copy wins." },
          { scope: "project", status: { Unreadable: { os_error: "permission denied" } }, location: null, reason: "The Project entry could not be read." },
          { scope: "local", status: "Absent", location: null, reason: "No Local override." },
        ],
        verdict: { on: true, decisive_step: 1 },
      },
      load_diagnostics: [{ scope: "project", path: "/sandbox/Projects/AgenTuna/.config-studio/skills/broken/SKILL.md", code: "invalid_skill", message: "The skill document could not be parsed." }],
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  const opener = page.getByRole("button", { name: "Why is this on?" }).first();
  await opener.click();
  const dialog = page.getByRole("dialog", { name: "Why is this on?" });
  for (const scope of ["global", "user", "project", "local"]) {
    await expect(dialog.getByText(new RegExp(`^${scope} —`))).toBeVisible();
  }
  await expect(dialog.getByText("The Project entry could not be read.")).toBeVisible();
  const diagnostics = dialog.getByRole("region", { name: "Load diagnostics" });
  await expect(diagnostics.getByText("The skill document could not be parsed.")).toBeVisible();

  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (value: string) => { (window as typeof window & { __AT_TEST_CLIPBOARD__?: string }).__AT_TEST_CLIPBOARD__ = value; } } });
  });
  await dialog.getByRole("button", { name: "Open file" }).click();
  const calledCommands = await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls);
  expect(calledCommands).toContain("reveal_item");
  await dialog.getByRole("button", { name: "Copy path" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Path copied.");
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_CLIPBOARD__?: string }).__AT_TEST_CLIPBOARD__)).toBe("/sandbox/user/skills/code-review/SKILL.md");

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(opener).toBeFocused();
  await opener.click();
  await expect(dialog).toBeVisible();
  await page.locator(".at-modal__backdrop").click({ position: { x: 2, y: 2 } });
  await expect(dialog).toBeHidden();
  await expect(opener).toBeFocused();
});
