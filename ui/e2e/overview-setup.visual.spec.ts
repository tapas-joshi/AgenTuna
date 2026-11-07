import { expect, test } from "./support/tauri-mock";

const POPULATED_OVERVIEW = {
  scopes: [
    { scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: true },
    { scope: "user", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: true },
    { scope: "project", skills: 1, mcp: 0, rules: 0, locks: 0, last_edited: "2026-11-06T10:00:00Z", disabled: 0, store_ready: true },
  ],
  attention: [],
  attention_total: 0,
  changes: [],
  user_path: "/sandbox/user",
  global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] },
  startup_warnings: [],
};

test("a scope without a store is set up through a reviewed plan", async ({ page }) => {
  await page.addInitScript((populated) => {
    type Mocks = { commandOverrides: Record<string, unknown>; finishSetup?: () => void };
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: Mocks }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.overview_load = {
      scopes: [
        { scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: true },
        { scope: "user", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: true },
        { scope: "project", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: false },
      ],
      attention: [],
      attention_total: 0,
      changes: [],
      user_path: "/sandbox/user",
      global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] },
      startup_warnings: [],
    };
    mocks.commandOverrides.project_sync_status = [];
    mocks.commandOverrides.overview_setup_preview = {
      preview_id: "preview-setup",
      scope: "project",
      path: "/sandbox/Projects/AgenTuna/.agentuna",
      files: [
        { path: "skills", action: "mkdir", is_new: true },
        { path: "rules", action: "mkdir", is_new: true },
        { path: "backups", action: "mkdir", is_new: true },
        { path: ".state/journals", action: "mkdir", is_new: true },
        { path: "mcp.json", action: "write", is_new: true },
        { path: "store.json", action: "write", is_new: true },
      ],
      foreign: ["README.md"],
      already_ready: false,
    };
    const gate = Promise.withResolvers<unknown>();
    mocks.commandOverrides.overview_setup_apply = gate.promise;
    mocks.finishSetup = () => {
      mocks.commandOverrides.overview_load = populated;
      gate.resolve({ operation_id: "op-setup", scope: "project", path: "/sandbox/Projects/AgenTuna/.agentuna", files_changed: 6 });
    };
  }, POPULATED_OVERVIEW);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeVisible();
  const projectCard = page.locator("article").filter({ hasText: "Project" });
  await expect(projectCard.getByText("This scope has no AgenTuna store yet.")).toBeVisible();
  await expect(page).toHaveScreenshot("overview-setup-empty.png", { animations: "disabled", caret: "hide" });
  await projectCard.getByRole("button", { name: "Set up the Project scope" }).click();
  const dialog = page.getByRole("dialog", { name: "Set up the Project store" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("store.json")).toBeVisible();
  await expect(dialog.getByText("Left untouched: README.md")).toBeVisible();
  await expect(dialog).toHaveScreenshot("overview-setup-review.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: "Create store" }).click();
  await page.evaluate(() => {
    (window as typeof window & { __AT_TEST_MOCKS__: { finishSetup?: () => void } }).__AT_TEST_MOCKS__.finishSetup?.();
  });
  await expect(page.getByText("Scope store created", { exact: true })).toBeVisible();
  await expect(projectCard.getByText("This scope has no AgenTuna store yet.")).toBeHidden();
  await expect(projectCard.getByRole("button", { name: "Set up the Project scope" })).toHaveCount(0);
});
