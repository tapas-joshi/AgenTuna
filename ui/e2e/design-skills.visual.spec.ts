import { expect, test } from "./support/tauri-mock";

/**
 * Design-fidelity captures for the Skills area (design images 02-skills,
 * 02b-skills-selected, 02c-skills-shared-root, 09-skill-detail).
 *
 * The fixtures name the states the designs draw: active, overrides, locked,
 * native-only, two copies over one shared discovery root, and a project copy
 * that shadows a user copy.
 */
const baseRow = {
  state: "active",
  shadowed_by: null,
  native: false,
  native_path: null,
  update_available: false,
  source: null,
  source_commit_sha: null,
  source_remote_sha: null,
  source_last_checked_at: null,
  updated_at: "2026-11-06T12:00:00Z",
  locked: false,
  lock_scope: null,
  lock_reason: null,
};

const projectPath = "/sandbox/Projects/AgenTuna";
const home = "/sandbox/home";

const rows = [
  {
    ...baseRow,
    item_id: "itm_changelog",
    name: "changelog",
    scope: "user",
    description: "Group commits into a readable changelog.",
    agents: [
      { agent: "claude_code", state: "linked", enabled: true, config_path: `${home}/.claude/skills/changelog` },
      { agent: "codex", state: "linked", enabled: true, config_path: `${home}/.codex/skills/changelog` },
      { agent: "omp", state: "off", enabled: false, config_path: null },
    ],
  },
  {
    ...baseRow,
    item_id: "itm_enable_demo",
    name: "enable-demo",
    scope: "project",
    description: "Smoke skill that proves per-agent deployment.",
    agents: [
      { agent: "claude_code", state: "linked", enabled: true, config_path: `${projectPath}/.claude/skills/enable-demo` },
      { agent: "codex", state: "linked", enabled: true, config_path: `${projectPath}/.agents/skills/enable-demo` },
      { agent: "omp", state: "linked", enabled: true, config_path: `${projectPath}/.agents/skills/enable-demo` },
    ],
  },
  {
    ...baseRow,
    item_id: "itm_commit_message",
    name: "commit-message",
    scope: "global",
    description: "House commit message rules.",
    locked: true,
    lock_scope: "global",
    lock_reason: "Pinned by the team policy lock",
    agents: [
      { agent: "claude_code", state: "linked", enabled: true, config_path: "/sandbox/global/claude/skills/commit-message" },
      { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/global/codex/skills/commit-message" },
      { agent: "omp", state: "linked", enabled: true, config_path: "/sandbox/global/omp/skills/commit-message" },
    ],
  },
  {
    ...baseRow,
    item_id: "itm_design_review",
    name: "design-review",
    scope: "user",
    description: "Review a UI against the design system.",
    agents: [
      { agent: "claude_code", state: "linked", enabled: true, config_path: `${home}/.claude/skills/design-review` },
      { agent: "codex", state: "off", enabled: false, config_path: null },
      { agent: "omp", state: "off", enabled: false, config_path: null },
    ],
  },
  {
    ...baseRow,
    item_id: "",
    name: "pdf-extract",
    scope: "user",
    state: null,
    native: true,
    description: "Pull text out of PDFs before review.",
    native_path: `${home}/.claude/skills/pdf-extract`,
    agents: [
      { agent: "claude_code", state: "native", enabled: true, config_path: `${home}/.claude/skills/pdf-extract` },
    ],
  },
  {
    ...baseRow,
    item_id: "",
    name: "release-notes",
    scope: "project",
    state: null,
    native: true,
    description: "Draft release notes from merged pull requests.",
    native_path: `${projectPath}/.agents/skills/release-notes`,
    agents: [
      { agent: "claude_code", state: "native", enabled: true, config_path: `${projectPath}/.claude/skills/release-notes` },
      { agent: "codex", state: "native", enabled: true, config_path: `${projectPath}/.agents/skills/release-notes` },
      { agent: "omp", state: "native", enabled: true, config_path: `${projectPath}/.agents/skills/release-notes` },
    ],
  },
  {
    ...baseRow,
    item_id: "",
    name: "review-sql",
    scope: "user",
    state: null,
    native: true,
    description: "Check migrations before they ship.",
    native_path: `${home}/.claude/skills/review-sql`,
    agents: [
      { agent: "claude_code", state: "native", enabled: true, config_path: `${home}/.claude/skills/review-sql` },
      { agent: "codex", state: "native", enabled: true, config_path: `${home}/.codex/skills/review-sql` },
    ],
  },
  {
    ...baseRow,
    item_id: "itm_tdd_project",
    name: "tdd",
    scope: "project",
    description: "Use when building features test-first, red-green-refactor.",
    agents: [
      { agent: "claude_code", state: "linked", enabled: true, config_path: `${projectPath}/.claude/skills/tdd` },
      { agent: "codex", state: "linked", enabled: true, config_path: `${projectPath}/.agents/skills/tdd` },
      { agent: "omp", state: "linked", enabled: true, config_path: `${projectPath}/.agents/skills/tdd` },
    ],
  },
  {
    ...baseRow,
    item_id: "",
    name: "tdd",
    scope: "user",
    state: null,
    native: true,
    shadowed_by: "project",
    description: "Your own copy of the test-first skill.",
    native_path: `${home}/.claude/skills/tdd`,
    agents: [
      { agent: "claude_code", state: "native", enabled: true, config_path: `${home}/.claude/skills/tdd` },
    ],
  },
];

const listView = {
  rows,
  agents: [
    { id: "claude_code", label: "Claude Code", presence: "installed" },
    { id: "codex", label: "Codex", presence: "installed" },
    { id: "omp", label: "omp", presence: "installed" },
  ],
  project_id: "prj_visual_fixture",
  project_name: "AgenTuna",
};

const tddDetail = {
  item_id: "itm_tdd_project",
  name: "tdd",
  slug: "tdd",
  scope: "project",
  path: `${projectPath}/.claude/skills/tdd`,
  markdown:
    "---\nname: tdd\ndescription: Use when building features test-first, red-green-refactor.\n---\n\n# TDD\n\n1. Write one failing test.\n2. Make it pass with the smallest change.\n3. Refactor with tests green.\n4. Repeat. Never batch tests.\n",
  description: "Use when building features test-first, red-green-refactor.",
  hash: "a".repeat(64),
  updated_at: "2026-11-06T12:00:00Z",
  source: null,
  mode: "link",
  shadowed_by: null,
  shadow_chain: ["project", "user"],
  deployments: [
    { agent_id: "claude_code", agent_name: "Claude Code", state: "linked", mode: "link", target_path: `${projectPath}/.claude/skills/tdd` },
    { agent_id: "codex", agent_name: "Codex", state: "linked", mode: "link", target_path: `${projectPath}/.agents/skills/tdd` },
    { agent_id: "omp", agent_name: "omp", state: "linked", mode: "link", target_path: `${projectPath}/.agents/skills/tdd` },
  ],
  history: [
    { content_hash: "b".repeat(64), captured_at: "2026-11-05T09:00:00Z", operation_kind: "skill_edit", operation_summary: "Edited skill" },
  ],
  missing: false,
  editable: true,
  update_available: false,
  source_commit_sha: null,
  source_remote_sha: null,
  source_last_checked_at: null,
  auto_update: "inherit",
};

const tddCompare = {
  winner: {
    scope: "project",
    markdown: "---\nname: tdd\n---\n\n# TDD\n\n1. Write one failing test.\n2. Make it pass.\n",
    hash: "a".repeat(64),
    deployments: ["claude_code", "codex", "omp"],
    path: `${projectPath}/.claude/skills/tdd`,
    mtime: "2026-11-06T11:48:00Z",
  },
  shadow: {
    scope: "user",
    markdown: "---\nname: tdd\n---\n\n# TDD\n\nWrite a test.\n",
    hash: "c".repeat(64),
    deployments: [],
    path: `${home}/.claude/skills/tdd`,
    mtime: "2026-10-28T10:00:00Z",
  },
  identical: false,
  diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # TDD\n-Write a test.\n+1. Write one failing test.\n",
};

test("Skills list matches 02-skills: header, chips and status vocabulary", async ({ page }) => {
  await page.addInitScript((view) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skills_list = view;
  }, listView);
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });

  await expect(main.getByRole("button", { name: "Import from git" })).toBeVisible();
  await expect(main.getByRole("button", { name: "+ New skill" })).toBeVisible();
  await expect(main.getByText("Overrides User copy")).toBeVisible();
  await expect(main.getByText("Locked by Global")).toBeVisible();
  await expect(main.getByText("Claude only")).toBeVisible();
  await expect(main.getByText("2 copies · 1 shared root")).toBeVisible();
  await expect(main.getByText("Shadowed by Project")).toBeVisible();
  // One shared folder block inside the native row, not two independent agent cells.
  const releaseNotes = main.locator(
    '[data-row-id="native:/sandbox/Projects/AgenTuna/.agents/skills/release-notes"]',
  );
  await expect(releaseNotes.locator('[data-slot="shared-folder"]')).toHaveCount(1);
  await expect(releaseNotes.getByText("1 folder · 2 agents")).toBeVisible();
  await expect(page).toHaveScreenshot("design-skills-list.png", { animations: "disabled", caret: "hide" });
});

test("Skills selection bar matches 02b", async ({ page }) => {
  await page.addInitScript((view) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skills_list = view;
  }, listView);
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });

  await main.getByRole("checkbox", { name: "Select changelog" }).click();
  await main.getByRole("checkbox", { name: "Select design-review" }).click();
  const bar = page.getByRole("region", { name: "Bulk skill actions" });
  await expect(bar).toContainText("2 selected");
  await expect(bar.getByRole("button", { name: "Promote to…" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Enable for agent…" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Move to scope…" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Disable" })).toBeVisible();
  await expect(bar.getByRole("button", { name: "Clear selection" })).toBeVisible();
  await expect(page).toHaveScreenshot("design-skills-selected.png", { animations: "disabled", caret: "hide" });
});

test("Native shared root detail matches 02c", async ({ page }) => {
  await page.addInitScript((view) => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skills_list = view;
  }, listView);
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("button", { name: "Actions for release-notes" }).first().click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const detail = page.getByRole("main", { name: "Native skill detail" });
  await expect(detail.getByRole("heading", { name: "One folder, two agents" })).toBeVisible();
  await expect(detail.getByText("Shared discovery root")).toBeVisible();
  await expect(detail.getByRole("heading", { name: "Codex + omp" })).toBeVisible();
  await expect(detail.getByRole("heading", { name: "Claude Code" })).toBeVisible();
  await expect(detail.getByText("Own copy · native, not managed")).toBeVisible();
  await expect(detail.getByRole("button", { name: "Adopt into AgenTuna…" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Leave alone" })).toBeVisible();
  await expect(page).toHaveScreenshot("design-skills-shared-root.png", { animations: "disabled", caret: "hide" });
});

test("Skill detail matches 09: frontmatter, header actions and shadow card", async ({ page }) => {
  await page.addInitScript(
    ({ view, detailFixture, compareFixture }) => {
      const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
      mocks.commandOverrides.skills_list = view;
      mocks.commandOverrides.skill_detail_load = detailFixture;
      mocks.commandOverrides.skill_compare_copies = compareFixture;
    },
    { view: listView, detailFixture: tddDetail, compareFixture: tddCompare },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("button", { name: "Actions for tdd" }).first().click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const detail = page.getByRole("main", { name: "Skill detail" });
  await expect(detail.getByRole("navigation", { name: "Breadcrumb" })).toContainText("Skills");
  await expect(detail.getByRole("heading", { name: "tdd", exact: true })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Edit", exact: true })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Promote…" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Delete" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "More actions" })).toBeVisible();
  // Frontmatter is its own block, not one flattened paragraph.
  const frontmatter = detail.locator('[data-slot="skill-frontmatter"]');
  await expect(frontmatter).toBeVisible();
  await expect(frontmatter).toContainText("name:");
  await expect(frontmatter).toContainText("description:");
  await expect(detail.getByRole("heading", { name: "Shadows a User copy" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Compare copies" })).toBeVisible();
  await expect(detail.getByRole("button", { name: "Keep User copy only" })).toBeVisible();
  await expect(page).toHaveScreenshot("design-skill-detail.png", {
    animations: "disabled",
    caret: "hide",
    fullPage: true,
  });
  await expect(detail.locator('[aria-label="Shadowed copies"]')).toHaveScreenshot(
    "design-skill-detail-shadow-card.png",
    { animations: "disabled", caret: "hide" },
  );
});
