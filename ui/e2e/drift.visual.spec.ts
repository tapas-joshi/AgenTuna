import { expect, test } from "./support/tauri-mock";

test("drift review and backup restore use typed commands and show their reviewed diffs", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.drift_snapshot = {
      rows: [{
        event_id: 17n,
        kind: "drifted",
        item_id: "itm_visual_skill",
        item_name: "Code Review",
        agent: "codex",
        scope: "project",
        path: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md",
        detected_at: "2026-11-06T10:00:00Z",
        changed_file_count: 1,
        can_adopt: true,
        can_restore: true,
        link_target: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review",
      }],
      in_sync_count: 41,
      watched_count: 42,
      watcher: { status: "active", path: null, error: null },
    };
  });
  await page.getByRole("button", { name: "Sync & drift" }).click();
  const screen = page.getByRole("main", { name: "Sync and drift" });
  await expect(screen.getByRole("heading", { name: "Sync & drift", level: 1 })).toBeVisible();
  await expect(screen.getByText("Watching", { exact: true })).toBeVisible();
  await expect(screen.getByText("41", { selector: "strong" })).toBeVisible();
  await expect(screen.getByRole("list", { name: "Drifted items" })).toContainText("Changed on disk");
  await expect(screen.getByText("# Code Review", { exact: true })).toBeVisible();
  await expect(screen.getByText("# Code Review checklist", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("drift-review.png", { animations: "disabled", caret: "hide" });
  await screen.getByRole("button", { name: "Open in editor" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("drift_open_in_editor");

  await screen.getByRole("button", { name: "Restore AgenTuna version" }).click();
  const restoreDriftDialog = page.getByRole("dialog", { name: "Restore AgenTuna version?" });
  await expect(restoreDriftDialog.getByText("A backup of this path will be created before the change is applied.")).toBeVisible();
  await expect(restoreDriftDialog.getByRole("paragraph").filter({ hasText: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md" })).toBeVisible();
  await expect(page).toHaveScreenshot("drift-restore-confirmation.png", { animations: "disabled", caret: "hide" });
  await restoreDriftDialog.getByRole("button", { name: "Restore AgenTuna version" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("drift_restore");

  await screen.getByRole("button", { name: "Adopt on-disk" }).click();
  const adoptDialog = page.getByRole("dialog", { name: "Adopt on-disk changes?" });
  await expect(adoptDialog.getByText("A backup of this path will be created before the change is applied.")).toBeVisible();
  await adoptDialog.getByRole("button", { name: "Adopt on-disk" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("drift_adopt");

  await screen.getByRole("tab", { name: "Backups" }).click();
  const backupRow = screen.getByRole("row", { name: /op-visual-drift/ });
  await expect(backupRow).toBeVisible();
  await expect(screen.getByText("# Code Review checklist", { exact: true })).toBeVisible();
  await expect(screen.getByText("# Code Review", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("drift-backups.png", { animations: "disabled", caret: "hide" });

  await backupRow.getByRole("button", { name: "Restore" }).click();
  const restoreDialog = page.getByRole("dialog", { name: "Restore from backup" });
  await expect(restoreDialog.getByText("# Code Review", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("drift-backup-restore-confirmation.png", { animations: "disabled", caret: "hide" });
  const restoreButton = restoreDialog.getByRole("button", { name: "Restore backup" });
  await expect(restoreButton).toBeEnabled();
  await restoreButton.click();
  await expect(page.getByText("Restored /sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md from backup.", { exact: true })).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("backup_restore");
  await expect(page).toHaveScreenshot("drift-backup-restored-toast.png", { animations: "disabled", caret: "hide" });
  await screen.getByRole("tab", { name: /Drift/ }).click();
  await screen.getByRole("button", { name: "Pause watching" }).click();
  await expect(screen.getByText("Paused", { exact: true })).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("drift_watcher_set_paused");
  await screen.getByRole("button", { name: "Resume watching" }).click();
  await expect(screen.getByText("Watching", { exact: true })).toBeVisible();
  await screen.getByRole("button", { name: "Rescan" }).click();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).toContain("drift_rescan");
});

test("new drift toast routes to the selected item for review", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void; commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    const row = {
      event_id: 17n,
      kind: "drifted",
      item_id: "itm_visual_skill",
      item_name: "Code Review",
      agent: "codex",
      scope: "project",
      path: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md",
      detected_at: "2026-11-06T10:00:00Z",
      changed_file_count: 1,
      can_adopt: true,
      can_restore: true,
      link_target: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review",
    };
    mocks.commandOverrides.drift_snapshot = {
      rows: [row],
      in_sync_count: 41,
      watched_count: 42,
      watcher: { status: "active", path: null, error: null },
    };
    mocks.emit("agentuna:drift-detected", [{
      event_id: row.event_id,
      kind: row.kind,
      item_id: row.item_id,
      agent: row.agent,
      scope: row.scope,
      path: row.path,
      expected_hash: "baseline",
      actual_hash: "changed",
      link_target: row.link_target,
      changed_file_count: 1,
      toast_eligible: true,
    }]);
  });
  await expect(page.getByText("code-review changed outside AgenTuna.", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("drift-detected-toast.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: "Review", exact: true }).click();
  const screen = page.getByRole("main", { name: "Sync and drift" });
  await expect(screen.getByRole("heading", { name: "Sync & drift", level: 1 })).toBeVisible();
  await expect(screen.getByRole("list", { name: "Drifted items" }).getByRole("button", { name: /Code Review/ })).toHaveAttribute("aria-current", "true");
});
