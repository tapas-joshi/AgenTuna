import { expect, test } from "./support/tauri-mock";

test("Skills list renders populated table, filter chips, native banner and shadowed rows", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });

  await expect(main.getByText("code-review", { exact: true })).toBeVisible();
  await expect(main.getByText("native-helper", { exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Unmanaged skills banner" })).toBeVisible();
  // A manager-installed link names the folder it links to; a broken one names its reason.
  await expect(main.getByText("linked-library-skill", { exact: true })).toBeVisible();
  await expect(main.getByText("links to /sandbox/home/.cc-switch/skills/linked-library-skill")).toBeVisible();
  await expect(main.getByText("Broken link", { exact: true })).toBeVisible();
  await expect(main.getByText("dangling symbolic link", { exact: true })).toBeVisible();
  await expect(page).toHaveScreenshot("skills-populated.png", { animations: "disabled", caret: "hide" });

  // Filter chips
  const filterGroup = page.getByRole("group", { name: "Filter chips" });
  await filterGroup.getByText("Project").click();
  await expect(main.getByText("native-helper", { exact: true })).toBeVisible();
  await expect(main.getByText("code-review", { exact: true })).toBeHidden();
  await expect(page).toHaveScreenshot("skills-filtered.png", { animations: "disabled", caret: "hide" });
});

test("Skills STATUS column names locked and shadowed rows", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    const base = {
      description: null,
      native: false,
      native_path: null,
      native_link_target: null,
      native_state: null,
      native_problem: null,
      update_available: false,
      updated_at: "2026-11-06T12:00:00Z",
    };
    mocks.commandOverrides.skills_list = {
      rows: [
        {
          ...base,
          item_id: "itm_locked_skill",
          name: "policy-skill",
          scope: "global",
          state: "active",
          shadowed_by: null,
          description: "Pinned by the Global policy lock",
          agents: [{ agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/global/skills/policy-skill" }],
          locked: true,
          lock_scope: "global",
          lock_reason: "Pinned by policy",
        },
        {
          ...base,
          item_id: "itm_shadowed_skill",
          name: "old-helper",
          scope: "user",
          state: "active",
          shadowed_by: "project",
          description: "User copy is shadowed by the Project copy",
          agents: [{ agent: "codex", state: "shadowed", enabled: false, config_path: "/sandbox/user/skills/old-helper" }],
          locked: false,
          lock_scope: null,
          lock_reason: null,
        },
      ],
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed" },
        { id: "codex", label: "Codex", presence: "installed" },
        { id: "omp", label: "omp", presence: "installed" },
        { id: "cursor", label: "Cursor", presence: "not_installed" },
      ],
      project_id: "prj_visual_fixture",
      project_name: "AgenTuna",
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });
  await expect(main.getByText("policy-skill", { exact: true })).toBeVisible();
  await expect(main.getByText("Locked by Global")).toBeVisible();
  await expect(main.getByTitle("Pinned by policy")).toBeVisible();
  await expect(main.getByText("Shadowed by Project")).toBeVisible();
  const shadowedRow = page.locator('[data-row-id="itm_shadowed_skill"]');
  await expect(shadowedRow).toHaveAttribute("data-tone", "dimmed");
  await expect(page.locator('[data-row-id="itm_locked_skill"]')).toHaveAttribute("data-tone", "default");
  await expect(page).toHaveScreenshot("skills-status-badges.png", { animations: "disabled", caret: "hide" });
});

test("MCP list renders servers matrix and filter chips", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const main = page.getByRole("main", { name: "MCP Servers" });

  await expect(main.getByText("Visual server", { exact: true })).toBeVisible();
  await expect(main.getByText("Native server", { exact: true })).toBeVisible();
  await expect(main.getByRole("columnheader", { name: "OMP" })).toBeVisible();
  const visualRow = main.getByRole("row").filter({ hasText: "Visual server" });
  await expect(visualRow.getByRole("group", { name: "omp" }).getByRole("switch")).toBeEnabled();
  // Agents that are not installed get no column: design 06 lists only installed agents.
  await expect(visualRow.getByRole("group", { name: "Cursor" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Unmanaged servers banner" })).toBeVisible();
  await expect(page).toHaveScreenshot("mcp-list-populated.png", { animations: "disabled", caret: "hide" });
});

test("MCP detail renders configuration, tools, health history and Loaded by", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const main = page.getByRole("main", { name: "MCP Servers" });

  // Open menu on Visual server and click Open
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const detailMain = page.getByRole("main", { name: "MCP Server Detail" });
  await expect(detailMain.getByRole("heading", { name: "Visual server" })).toBeVisible();
  await expect(detailMain.getByText("stdio")).toBeVisible();
  await expect(detailMain.getByText("npx")).toBeVisible();
  await expect(detailMain.getByRole("heading", { name: "Health" })).toBeVisible();
  await expect(detailMain.getByText("Loaded by")).toBeVisible();
  await expect(page).toHaveScreenshot("mcp-detail-populated.png", { animations: "disabled", caret: "hide" });
});
