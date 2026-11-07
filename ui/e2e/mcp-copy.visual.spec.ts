import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

/**
 * Effective config → MCP servers, where an inherited User definition offers
 * "Override in Project". The reviewed copy dialog is the subject; every fixture is
 * deterministic and lives in the shared Tauri mock (or in this file's init scripts).
 */
async function openCopyDialog(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  const main = page.getByRole("main", { name: "Effective config" });
  await main.getByRole("tab", { name: "MCP servers" }).click();
  const section = main.getByRole("region", { name: "Change it for this repo" });
  await expect(section).toBeVisible();
  await section.getByRole("button", { name: "Override in Project" }).click();
  return page.getByRole("dialog", { name: "Copy Postgres to project?" });
}

test("MCP copy review lists the exact files and per-agent effects before applying", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.effective_query = {
      items: [
        {
          logical_id: "logical_visual_mcp",
          item_id: "itm_visual_mcp",
          kind: "mcp_server",
          name: "Postgres",
          winner_scope: "user",
          source_path: "/sandbox/user/mcp/postgres.json",
          agents: [
            { agent: "claude_code", state: "linked", loads: true },
            { agent: "codex", state: "linked", loads: false },
          ],
          loads_in_agents: 1,
          lock: null,
        },
      ],
      disabled_here: [],
      diagnostics: [],
      load_diagnostics: [],
    };
    mocks.commandOverrides.mcp_server_copy_preview = {
      preview_id: "preview-visual-mcp-copy",
      name: "Postgres",
      source_scope: "user",
      target_scope: "project",
      source_slug: "postgres",
      target_slug: "postgres",
      target_item_id: "itm_visual_mcp_copy",
      replaces_existing: false,
      backup_location: "/sandbox/support/backups/op-visual-mcp-copy",
      files: [
        "/sandbox/Projects/AgenTuna/.config-studio/mcp/postgres.json",
        "/sandbox/Projects/AgenTuna/.mcp.json",
      ],
      agents: [{ agent: "claude_code", path: "/sandbox/Projects/AgenTuna/.mcp.json", effect: "create" }],
      refusals: [],
      warnings: [],
      collision: null,
    };
    mocks.commandOverrides.mcp_server_copy_apply = {
      operation_id: "op-visual-mcp-copy",
      target_item_id: "itm_visual_mcp_copy",
      target_slug: "postgres",
      files_changed: 2,
      backup_location: "/sandbox/support/backups/op-visual-mcp-copy",
    };
  });

  const dialog = await openCopyDialog(page);
  const reviewed = dialog.getByRole("region", { name: "Reviewed copy" });
  await expect(reviewed).toBeVisible();
  await expect(reviewed.getByText("Adds a new item to the destination scope.")).toBeVisible();
  await expect(reviewed.getByText("Files (2)")).toBeVisible();
  await expect(reviewed.getByText("/sandbox/Projects/AgenTuna/.config-studio/mcp/postgres.json", { exact: true })).toBeVisible();
  await expect(reviewed.getByText("/sandbox/Projects/AgenTuna/.mcp.json", { exact: true })).toBeVisible();
  await expect(reviewed.getByText("claude_code · create · /sandbox/Projects/AgenTuna/.mcp.json")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Copy server" })).toBeEnabled();
  await expect(page).toHaveScreenshot("mcp-copy-review.png", { animations: "disabled", caret: "hide" });

  await dialog.getByRole("button", { name: "Copy server" }).click();
  await expect(page.getByText(/^Success: Copied into Project as postgres\. Backup: \/sandbox\/support\/backups\/op-visual-mcp-copy/)).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "mcp_server_copy_apply").length)).toBe(1);
});

test("MCP copy asks Replace or Rename when the project already defines the server", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.effective_query = {
      items: [
        {
          logical_id: "logical_visual_mcp",
          item_id: "itm_visual_mcp",
          kind: "mcp_server",
          name: "Postgres",
          winner_scope: "user",
          source_path: "/sandbox/user/mcp/postgres.json",
          agents: [{ agent: "claude_code", state: "linked", loads: true }],
          loads_in_agents: 1,
          lock: null,
        },
      ],
      disabled_here: [],
      diagnostics: [],
      load_diagnostics: [],
    };
    mocks.commandOverrides.mcp_server_copy_preview = {
      preview_id: null,
      name: "Postgres",
      source_scope: "user",
      target_scope: "project",
      source_slug: "postgres",
      target_slug: "postgres",
      target_item_id: null,
      replaces_existing: false,
      backup_location: "/sandbox/support/backups/op-visual-mcp-copy",
      files: [],
      agents: [],
      refusals: [],
      warnings: [],
      collision: { slug: "postgres", item_id: "itm_project_postgres", scope: "project" },
    };
  });

  const dialog = await openCopyDialog(page);
  const collision = dialog.getByRole("region", { name: "Destination collision" });
  await expect(collision).toBeVisible();
  await expect(collision.getByText("project scope already defines postgres")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Replace with backup" })).toBeEnabled();
  await expect(dialog.getByRole("button", { name: "Copy under a new name" })).toBeDisabled();
  await expect(page).toHaveScreenshot("mcp-copy-collision.png", { animations: "disabled", caret: "hide" });

  await dialog.getByRole("textbox", { name: "New server id" }).fill("postgres-team");
  // The reviewed plan after Rename keeps both definitions; swap the mock to that answer.
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_server_copy_preview = {
      preview_id: "preview-visual-mcp-copy-renamed",
      name: "Postgres",
      source_scope: "user",
      target_scope: "project",
      source_slug: "postgres",
      target_slug: "postgres-team",
      target_item_id: "itm_visual_mcp_copy_renamed",
      replaces_existing: true,
      backup_location: "/sandbox/support/backups/op-visual-mcp-copy-renamed",
      files: ["/sandbox/Projects/AgenTuna/.config-studio/mcp/postgres-team.json"],
      agents: [{ agent: "codex", path: "/sandbox/Projects/AgenTuna/.codex/config.toml", effect: "edit" }],
      refusals: [],
      warnings: [],
      collision: null,
    };
  });
  await dialog.getByRole("button", { name: "Copy under a new name" }).click();
  const reviewed = dialog.getByRole("region", { name: "Reviewed copy" });
  await expect(reviewed).toBeVisible();
  await expect(reviewed.getByText("Replaces the existing destination definition after a backup.")).toBeVisible();
  await expect(reviewed.getByText("/sandbox/Projects/AgenTuna/.config-studio/mcp/postgres-team.json")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Copy server" })).toBeEnabled();
  await expect(page).toHaveScreenshot("mcp-copy-renamed.png", { animations: "disabled", caret: "hide" });
});

test("MCP copy names refused agents so a partial copy is never mistaken for a working one", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.effective_query = {
      items: [
        {
          logical_id: "logical_visual_mcp",
          item_id: "itm_visual_mcp",
          kind: "mcp_server",
          name: "Postgres",
          winner_scope: "user",
          source_path: "/sandbox/user/mcp/postgres.json",
          agents: [{ agent: "claude_code", state: "linked", loads: true }],
          loads_in_agents: 1,
          lock: null,
        },
      ],
      disabled_here: [],
      diagnostics: [],
      load_diagnostics: [],
    };
    mocks.commandOverrides.mcp_server_copy_preview = {
      preview_id: "preview-visual-mcp-copy-partial",
      name: "Postgres",
      source_scope: "user",
      target_scope: "project",
      source_slug: "postgres",
      target_slug: "postgres",
      target_item_id: "itm_visual_mcp_copy",
      replaces_existing: false,
      backup_location: "/sandbox/support/backups/op-visual-mcp-copy",
      files: ["/sandbox/Projects/AgenTuna/.mcp.json"],
      agents: [{ agent: "claude_code", path: "/sandbox/Projects/AgenTuna/.mcp.json", effect: "edit" }],
      refusals: [{ agent: "omp", reason: "omp: refused secret `API_TOKEN` for field `TOKEN`" }],
      warnings: ["Codex and omp read the shared .agents root, so the same copy loads in both."],
      collision: null,
    };
  });

  const dialog = await openCopyDialog(page);
  const refused = dialog.getByRole("region", { name: "Refused agents" });
  await expect(refused).toBeVisible();
  await expect(refused.getByText("omp: refused secret `API_TOKEN` for field `TOKEN`")).toBeVisible();
  await expect(dialog.getByText("Codex and omp read the shared .agents root, so the same copy loads in both.")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Copy server" })).toBeEnabled();
  await expect(page).toHaveScreenshot("mcp-copy-refusal.png", { animations: "disabled", caret: "hide" });
});
