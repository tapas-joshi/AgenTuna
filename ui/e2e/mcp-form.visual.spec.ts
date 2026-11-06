import { expect, test } from "./support/tauri-mock";

test("MCP add form reviews and saves a real-shaped preview", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & {
      __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> };
    }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_plan_preview = {
      change_set_id: "cs-visual-mcp",
      preview_id: "preview-visual-mcp",
      files: [{
        agent: "codex",
        scope: "user",
        path: "/fixtures/user/.codex/config.toml",
        additions: 3,
        deletions: 0,
        diff: "+[mcp_servers.search]\n+command = \"npx\"",
        selected: true,
        cannot_write_reason: null,
      }],
    };
    mocks.commandOverrides.mcp_save = { operation_id: "op-visual-mcp", files_changed: 2 };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "Add MCP server" }).click();
  await expect(page.getByRole("heading", { name: "Add MCP server" })).toBeVisible();
  await page.getByLabel("Name").fill("Search");
  await page.getByRole("textbox", { name: "Command" }).fill("npx");
  await page.getByRole("button", { name: "Codex" }).click();
  await page.getByRole("button", { name: "Review changes" }).click();
  await expect(page.getByText("Files that will change")).toBeVisible();
  await expect(page.getByText("/fixtures/user/.codex/config.toml")).toBeVisible();
  await page.screenshot({ path: "test-results/mcp-add-review.png", animations: "disabled" });
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("MCP server saved.", { exact: true })).toBeVisible();
});
