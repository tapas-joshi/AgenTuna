import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

const scopes = [
  { scope: "global", skills: 5, mcp: 2, rules: 0, locks: 2, last_edited: "2026-11-04T10:00:00Z", last_edited_by: "ui", disabled: 0, store_ready: true, store_root: "/sandbox/global", native_roots: [], git_uncommitted: null },
  { scope: "user", skills: 11, mcp: 4, rules: 1, locks: 0, last_edited: "2026-11-06T08:00:00Z", last_edited_by: "ui", disabled: 0, store_ready: true, store_root: "/sandbox/user", native_roots: ["~/.claude", "~/.codex", "~/.omp", "~/.agents"], git_uncommitted: null },
  { scope: "project", skills: 5, mcp: 1, rules: 1, locks: 0, last_edited: "2026-11-07T09:00:00Z", last_edited_by: "ui", disabled: 1, store_ready: true, store_root: "./.agentuna", native_roots: ["./.claude", "./.mcp.json", "AGENTS.md"], git_uncommitted: 1 },
];

const overview = {
  scopes,
  effective: { skills: 21, mcp: 7, rules: 2, shadowed_skills: 2, disabled: 1 },
  attention: [
    { item_id: "itm_visual_mcp", title: "Health check failed: postgres", subtitle: "missing_github_token", kind: "failing_health_check", screen: "mcp", row_id: "itm_visual_mcp" },
    { item_id: "itm_visual_skill", title: "Skill shadowed: release-notes", subtitle: "Winning copy is at project", kind: "shadowed_skill", screen: "skills", row_id: "itm_visual_skill" },
    { item_id: "itm_visual_skill", title: "Drift detected: .claude/skills/release-notes/SKILL.md", subtitle: "2026-12-29T15:00:00Z", kind: "drift_event", screen: "drift", row_id: "17" },
    { item_id: null, title: "Agent not managed: Cursor", subtitle: "0.44.0", kind: "missing_agent", screen: "agents", row_id: "cursor" },
  ],
  attention_total: 4,
  changes: [
    { id: "op-1", kind: "edit_mcp", summary: "Added MCP server postgres", created_at: "2026-12-29T15:45:00Z", file_count: 2, scope: "user", files: [{ path: "/sandbox/user/mcp.json", scope: "user" }] },
    { id: "op-2", kind: "deploy", summary: "Enabled skill ui-ux-pro-max for omp", created_at: "2026-12-29T14:40:00Z", file_count: 1, scope: "user", files: [{ path: "/sandbox/user/.omp/agent/mcp.json", scope: "user" }] },
    { id: "op-3", kind: "edit_skill", summary: "Updated skill release-notes", created_at: "2026-12-28T18:00:00Z", file_count: 2, scope: "project", files: [{ path: "/sandbox/Projects/AgenTuna/.agents/skills/release-notes/SKILL.md", scope: "project" }] },
  ],
  user_path: "/sandbox/user",
  global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [{ item_id: "itm_visual_skill", scope: "global", reason: "Org policy" }] },
  startup_warnings: [],
};

const agentCards = [
  { id: "claude_code", name: "Claude Code", installed: true, version: "2.1.0", managed: true, paths: ["~/.claude/skills", "~/.claude.json"], health: "~/.claude/skills · 14 / 16 managed · 16 in sync", link_mode: "link", presence: "installed", format: "JSON + Markdown skills", managed_skills: 14, managed_servers: 5, native_skills: 2, native_servers: 1, copy_fallback: false, primary_path: "~/.claude/skills", needs_path: false, in_sync: 16, total: 16, linked_deployments: 16 },
  { id: "codex", name: "Codex", installed: true, version: "0.54.0", managed: true, paths: ["~/.codex/skills", "~/.codex/config.toml"], health: "~/.codex/skills · 14 / 16 managed · 16 in sync", link_mode: "copy", presence: "installed", format: "TOML + Markdown skills", managed_skills: 14, managed_servers: 4, native_skills: 2, native_servers: 1, copy_fallback: true, primary_path: "~/.codex/skills", needs_path: false, in_sync: 15, total: 16, linked_deployments: 0 },
  { id: "omp", name: "omp", installed: true, version: "1.8.2", managed: true, paths: ["~/.omp/agent/managed-skills", "~/.omp/agent/mcp.json"], health: "~/.omp/agent/managed-skills · 12 / 14 managed · 13 in sync", link_mode: "link", presence: "installed", format: "JSON + Markdown skills", managed_skills: 12, managed_servers: 3, native_skills: 2, native_servers: 1, copy_fallback: false, primary_path: "~/.omp/agent/managed-skills", needs_path: false, in_sync: 13, total: 14, linked_deployments: 13 },
  { id: "cursor", name: "Cursor", installed: false, version: null, managed: false, paths: [], health: "0 / 0 managed · 0 in sync", link_mode: "link", presence: "not_installed", format: "—", managed_skills: 0, managed_servers: 0, native_skills: 3, native_servers: 0, copy_fallback: false, primary_path: null, needs_path: true, in_sync: 0, total: 0, linked_deployments: 0 },
];

const projectRows = [
  { id: "prj_visual_fixture", name: "ai-coding-config-studio", path: "/Users/tester/Projects/Personal/ai-coding-config-studio", last_used_at: "2026-11-07T10:30:00Z", skill_count: 5, server_count: 1 },
  { id: "prj_visual_2", name: "nutrition-app", path: "/Users/tester/Projects/Personal/nutrition-app", last_used_at: "2026-11-06T10:00:00Z", skill_count: 3, server_count: 2 },
  { id: "prj_visual_3", name: "payments-service", path: "/Users/tester/Projects/Work/payments-service", last_used_at: "2026-11-04T10:00:00Z", skill_count: 6, server_count: 1 },
  { id: "prj_visual_4", name: "notes-site", path: "/Users/tester/Projects/Personal/notes-site", last_used_at: "2026-10-20T10:00:00Z", skill_count: 0, server_count: 0 },
  { id: "prj_visual_5", name: "api-gateway", path: "/Users/tester/Projects/Work/api-gateway", last_used_at: "2026-09-11T10:00:00Z", skill_count: 4, server_count: 3 },
];

const libraryCatalog = {
  sources: [
    { id: "/sandbox/user", label: "User scope", kind: "scope", count: 2, last_used_at: null },
    { id: "/sandbox/Projects/AgenTuna", label: "AgenTuna", kind: "project", count: 2, last_used_at: "2026-12-28T10:00:00Z" },
  ],
  skills: [
    { id: "u1", source_id: "/sandbox/user", source_label: "User scope", path: "/sandbox/user/skills/ui-ux-pro-max", name: "ui-ux-pro-max", description: "Design review checklist for interfaces.", error: null, clash_reason: null },
    { id: "u2", source_id: "/sandbox/user", source_label: "User scope", path: "/sandbox/user/skills/release-notes", name: "release-notes", description: "Draft release notes from merged pull requests.", error: null, clash_reason: null },
    { id: "p1", source_id: "/sandbox/Projects/AgenTuna", source_label: "AgenTuna", path: "/sandbox/Projects/AgenTuna/.agents/skills/changelog", name: "changelog", description: "Keep a changelog in the repository.", error: null, clash_reason: "A skill with this name already exists in the target scope." },
    { id: "p2", source_id: "/sandbox/Projects/AgenTuna", source_label: "AgenTuna", path: "/sandbox/Projects/AgenTuna/.agents/skills/team-style", name: "team-style", description: "Match the team's writing style.", error: null, clash_reason: null },
  ],
  target_scope: "user",
  search_root: "/sandbox/user/skills",
  skipped: [{ label: "old-prototype", path: "/sandbox/Projects/old-prototype", reason: "The project folder no longer exists. Rescan projects or remove it from the list." }],
};

async function seed(page: Page): Promise<void> {
  await page.addInitScript(({ overviewFixture, agents, projectsFixture, library }) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown>; settings: { values: Record<string, unknown> } } }).__AT_TEST_MOCKS__;
    mocks.settings.values.last_project_id = "prj_visual_fixture";
    mocks.settings.values["last_scope.prj_visual_fixture"] = "project";
    mocks.commandOverrides.overview_load = () => overviewFixture;
    mocks.commandOverrides.agents_list = () => agents;
    mocks.commandOverrides.agents_rescan = () => agents;
    mocks.commandOverrides.projects_list = () => projectsFixture;
    mocks.commandOverrides.skill_library_catalog = () => library;
  }, { overviewFixture: overview, agents: agentCards, projectsFixture: projectRows, library: libraryCatalog });
}

async function openOverview(page: Page): Promise<void> {
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await expect(page.getByRole("main", { name: "Overview" })).toBeVisible();
  await expect(page.getByText("Effective in this repo")).toBeVisible();
}

async function switchScope(page: Page, scope: "Global" | "User" | "Project"): Promise<void> {
  await page.getByRole("radiogroup", { name: "Scope" }).first().getByRole("radio", { name: scope }).click();
}

test("design parity: overview project scope", async ({ page }) => {
  await seed(page);
  await openOverview(page);
  await expect(page.getByRole("heading", { level: 1, name: "Overview" })).toBeVisible();
  await page.screenshot({ path: "test-results/design/overview-project.png", animations: "disabled", caret: "hide" });
});

test("design parity: overview user scope", async ({ page }) => {
  await seed(page);
  await openOverview(page);
  await switchScope(page, "User");
  await expect(page.getByRole("heading", { level: 1, name: "Overview · User scope" })).toBeVisible();
  await page.screenshot({ path: "test-results/design/overview-user.png", animations: "disabled", caret: "hide" });
});

test("design parity: overview global scope", async ({ page }) => {
  await seed(page);
  await openOverview(page);
  await switchScope(page, "Global");
  await expect(page.getByRole("heading", { level: 1, name: "Overview · Global scope" })).toBeVisible();
  await page.screenshot({ path: "test-results/design/overview-global.png", animations: "disabled", caret: "hide" });
});

test("design parity: agents cards", async ({ page }) => {
  await seed(page);
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.getByRole("navigation", { name: "Workspace" }).getByRole("button", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("main", { name: "Agents" })).toBeVisible();
  await expect(page.getByText("JSON + Markdown skills").first()).toBeVisible();
  await page.screenshot({ path: "test-results/design/agents.png", animations: "disabled", caret: "hide" });
});

test("design parity: project picker with and without search", async ({ page }) => {
  await seed(page);
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await expect(page.getByText("Effective in this repo")).toBeVisible();
  await page.keyboard.press("Meta+p");
  const picker = page.locator(".at-project-picker");
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("combobox", { name: "Search projects" })).toBeVisible();
  await page.screenshot({ path: "test-results/design/project-picker.png", animations: "disabled", caret: "hide" });
  await picker.getByRole("combobox", { name: "Search projects" }).fill("studio");
  await expect(picker.getByText(/1 match|1 matches/)).toBeVisible();
  await page.screenshot({ path: "test-results/design/project-picker-search.png", animations: "disabled", caret: "hide" });
});

test("design parity: add-skill library, scope, review and done steps", async ({ page }) => {
  await seed(page);
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await expect(page.getByRole("heading", { name: "Add a skill" })).toBeVisible();
  await page.getByRole("button", { name: /From your library/ }).click();
  await page.getByRole("button", { name: /^Continue/ }).click();
  await expect(page.getByRole("heading", { name: "From your library" })).toBeVisible();
  await expect(page.getByRole("button", { name: /User scope/ })).toBeVisible();
  await page.screenshot({ path: "test-results/design/add-skill-library.png", animations: "disabled", caret: "hide" });

  await page.getByRole("checkbox", { name: /ui-ux-pro-max/ }).click();
  await page.getByRole("button", { name: /AgenTuna 2/ }).click();
  await expect(page.getByText(/Already in this repo/)).toBeVisible();
  await page.screenshot({ path: "test-results/design/add-skill-library-selected.png", animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: /^Continue with 1/ }).click();
  await expect(page.getByRole("heading", { name: "Scope & agents" })).toBeVisible();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await page.screenshot({ path: "test-results/design/add-skill-scope.png", animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: /^Continue/ }).click();
  await expect(page.getByRole("heading", { name: "Review" })).toBeVisible();
  await page.screenshot({ path: "test-results/design/add-skill-review.png", animations: "disabled", caret: "hide" });

  await page.getByRole("button", { name: "Create skill" }).click();
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__;
    mocks.emit("agentuna:job-finished", { id: "job_visual_skill_apply", name: "Add skill", state: "succeeded", error: null });
  });
  await expect(page.getByRole("heading", { name: /is ready/ })).toBeVisible();
  await page.screenshot({ path: "test-results/design/add-skill-done.png", animations: "disabled", caret: "hide" });
});
