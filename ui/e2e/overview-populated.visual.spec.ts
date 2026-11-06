import { expect, test } from "./support/tauri-mock";

test("populated Overview uses compact scope cards and usable attention and change rows", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 12).toISOString();
    const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 12).toISOString();
    mocks.commandOverrides.overview_load = {
      scopes: [
        { scope: "global", skills: 5, mcp: 2, rules: 0, locks: 2, last_edited: "2026-11-03T10:00:00Z", disabled: 0 },
        { scope: "user", skills: 11, mcp: 4, rules: 1, locks: 0, last_edited: "2026-11-06T08:00:00Z", disabled: 0 },
        { scope: "project", skills: 5, mcp: 1, rules: 1, locks: 0, last_edited: "2026-11-06T09:48:00Z", disabled: 1 },
      ],
      attention: [
        { item_id: "itm_health", title: "MCP server fails health check", subtitle: "User · 401 Unauthorized", kind: "failing_health", screen: "mcp", row_id: "itm_health" },
        { item_id: "itm_shadow", title: "Skill is shadowed", subtitle: "Project copy wins", kind: "shadowed_skill", screen: "skills", row_id: "itm_shadow" },
        { item_id: "itm_drift", title: "Configuration changed outside AgenTuna", subtitle: "Review the file diff", kind: "drift", screen: "drift", row_id: "itm_drift" },
        { item_id: null, title: "Cursor not found on this Mac", subtitle: "3 skills target Cursor", kind: "missing_agent", screen: "agents", row_id: "cursor" },
        { item_id: "itm_orphan_lock", title: "Locked item not found", subtitle: "Policy lock reason", kind: "locked_item_not_found", screen: "effective", row_id: "itm_orphan_lock" },
      ],
      attention_total: 5,
      changes: [
        { id: "op_today", kind: "skill_added", summary: "Added MCP server 'postgres'", created_at: today, file_count: 2, scope: "project", files: [{ path: "./.agentuna/mcp.json", scope: "project" }, { path: "./.agentuna/store.json", scope: "project" }] },
        { id: "op_yesterday", kind: "skill_enabled", summary: "Enabled 'ui-ux-pro-max' for omp", created_at: yesterday, file_count: 1, scope: "user", files: [{ path: "~/.agentuna/skills/ui-ux-pro-max", scope: "user" }] },
      ],
      user_path: "/sandbox/user/.agentuna", global_path: "/sandbox/global", global_writable: false, global_reason: "read only",
    };
    mocks.commandOverrides.project_sync_status = [
      { item_id: "itm_project", scope: "project", name: "Search Skill", label: "project_newer", action: "push" },
      { item_id: "itm_project_only", scope: "project", name: "Project-only Skill", label: "project_only", action: "pull" },
      { item_id: "itm_library_newer", scope: "project", name: "Library-newer Skill", label: "library_newer", action: "pull" },
      { item_id: "itm_library_only", scope: "user", name: "Library-only Skill", label: "library_only", action: "push" },
      { item_id: "itm_conflict", scope: "project", name: "Conflicted Skill", label: "conflict", action: "compare" },
      { item_id: "itm_in_sync", scope: "project", name: "In-sync Skill", label: "in_sync", action: "none" },
    ];
    mocks.commandOverrides.project_sync_preview = { preview_id: "preview-sync", item_id: "itm_project", name: "Search Skill", direction: "push", files: [{ path: "skills/search/SKILL.md", action: "write", diff: "--- old\n+++ new\n", backup_required: true }], missing_source_paths: [] };
    mocks.commandOverrides.project_sync_apply = { operation_id: "op_sync", files_changed: 1 };
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeVisible();
  await expect(page.getByText("MCP server fails health check")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Today" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Yesterday" })).toBeVisible();
  await page.getByRole("radio", { name: "Project" }).click();
  await page.getByRole("button", { name: /Added MCP server/ }).click();
  await expect(page.getByText("./.agentuna/mcp.json")).toBeVisible();
  await page.getByRole("main").evaluate((element) => { element.scrollTop = 0; });
  await expect(page).toHaveScreenshot("overview-populated.png", { animations: "disabled", caret: "hide" });
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("Search Skill");
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("Project newer");
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("Library newer");
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("Library only");
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("Conflict");
  await expect(page.getByRole("region", { name: "Project and User sync" })).toContainText("In sync");
  await expect(page.getByRole("button", { name: "Pull to Project" })).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Push to User library" })).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Compare copies" })).toBeVisible();
  await expect(page.getByRole("listitem").filter({ hasText: "In-sync Skill" }).getByRole("button")).toHaveCount(0);
  await page.getByRole("button", { name: "Push to User library" }).first().click();
  await expect(page.getByRole("dialog", { name: "Review push for Search Skill" })).toBeVisible();
  await page.getByRole("button", { name: "Confirm sync" }).click();
  await expect(page.getByText("Project sync complete", { exact: true })).toBeVisible();
  await page.getByRole("radio", { name: "User" }).click();
  await expect(page.getByRole("button", { name: "Copy path" })).toBeVisible();
  await page.getByRole("radio", { name: "Global" }).click();
  await expect(page.getByText(/Global store is read-only/)).toBeVisible();
  await expect(page.getByRole("button", { name: /Locked item not found/ })).toHaveAttribute("title", "Policy lock reason");
  await page.getByRole("button", { name: "Enable global writes" }).click();
  await expect(page.getByText(/Global store is read-only/)).toBeHidden();
});
