import { expect, test } from "./support/tauri-mock";

test("add-skill source matches the three-card design", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await expect(page.getByRole("heading", { name: "Add a skill" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Wizard progress" })).toBeVisible();
  await expect(page.locator('[data-source-card="true"]')).toHaveCount(3);
  await expect(page.getByRole("button", { name: /Write a new skill/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Import from git/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /From your library/ })).toBeVisible();
  await page.screenshot({ path: "test-results/add-skill-source.png", animations: "disabled" });
});
test("marketplace source respects disabled network policy", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Marketplace" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Marketplace browsing is unavailable because network policy disables it.", { exact: true })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Search marketplace" })).toHaveCount(0);
});
test("offline marketplace shows cached policy and makes no request until searched", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.network_policy = {
      offline: true, marketplace_enabled: false, background_health: "local", updates_opt_in: false,
      statement: "Offline mode is active.", allowed_hosts: [], actions: [], events: [], capacity: 100,
    };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Marketplace" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Offline policy is active; cached marketplace results may be available.", { exact: true })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).not.toContain("skill_marketplace_search");
});


test("marketplace search runs only after explicit Search", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.network_policy = {
      offline: false, marketplace_enabled: true, background_health: "local", updates_opt_in: false,
      statement: "Network access is user controlled.", allowed_hosts: [], actions: [], events: [], capacity: 100,
    };
    mocks.commandOverrides.skill_marketplace_search = {
      results: [{ id: "example/visual-skill", name: "Visual Skill", author: "Example", installs: 1200, source: "https://example.invalid/visual-skill", description: "A visual test fixture." }],
      offline: false, stale: false, provenance: "Marketplace results",
    };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Marketplace" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Marketplace search is enabled. Search runs only after you press Search.", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "skill_marketplace_search"))).toHaveLength(0);
  const query = page.getByRole("textbox", { name: "Search marketplace" });
  await expect(page.getByText("Visual Skill", { exact: true })).toHaveCount(0);
  await query.fill("visual");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const result = page.getByRole("button", { name: /Visual Skill/ });
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "skill_marketplace_search"))).toHaveLength(1);
  await expect(result).toBeVisible();
  await expect(result).toContainText("1.2k installs");
  await expect(result).toContainText("Source: https://example.invalid/visual-skill");
  await expect(page.getByText("Marketplace results", { exact: true })).toBeVisible();
  await result.click();
  await expect(result).toHaveAttribute("aria-pressed", "true");
});
test("Git import uses bridge-job progress, validates candidates, preserves selection on Back, and matches the Git design", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Import from git/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Repository URL").fill("https://example.invalid/skills.git");
  await page.getByLabel("Branch, tag or commit (optional)").fill("release-1");
  await expect(page.getByRole("button", { name: "Fetch skills" })).toBeEnabled();
  await expect(page.getByRole("checkbox", { name: /Review checklist/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Fetch skills" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-progress", { id: "job_visual_git_import", name: "Fetch git skills", fraction: 0.4, label: "Receiving Git objects…" }));
  await expect(page.getByText("Receiving Git objects…")).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-git-progress.png", { animations: "disabled", caret: "hide" });
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_git_import", name: "Fetch git skills", state: "succeeded", error: null }));
  await expect(page.getByText("3 skills ready to select.")).toBeVisible();
  const first = page.getByRole("checkbox", { name: /Review checklist/ });
  const second = page.getByRole("checkbox", { name: /Validation checklist/ });
  const invalid = page.getByRole("checkbox", { name: /Invalid skill/ });
  await expect(invalid).toBeDisabled();
  await expect(invalid.locator("xpath=..")).toHaveAttribute("title", "The SKILL.md frontmatter is invalid.");
  await page.getByRole("button", { name: "Select all" }).click();
  await expect(first).toBeChecked();
  await expect(second).toBeChecked();
  await expect(invalid).not.toBeChecked();
  await page.getByRole("button", { name: "Select none" }).click();
  await expect(first).not.toBeChecked();
  await first.check();
  await expect(page.getByRole("region", { name: "Selected skill preview" })).toContainText("# Review checklist");
  await expect(page).toHaveScreenshot("add-skill-git-candidates.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("main", { name: "Add a skill" }).getByRole("button", { name: "Back" }).click();
  await expect(page.getByRole("heading", { name: "Where does the skill come from?" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Next" })).toBeEnabled();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByLabel("Repository URL")).toHaveValue("https://example.invalid/skills.git");
  await expect(page.getByLabel("Branch, tag or commit (optional)")).toHaveValue("release-1");
  await expect(first).toBeChecked();
});
test("Git Back and forward restores fetched candidates, selection, and scroll position", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_import_job_result = {
      candidates: Array.from({ length: 8 }, (_, index) => ({
        id: `scroll-skill-${index + 1}`,
        name: `Scroll Skill ${index + 1}`,
        description: `Candidate ${index + 1}`,
        markdown: `# Scroll Skill ${index + 1}`,
        error: null,
      })),
      error: null,
    };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Import from git/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Repository URL").fill("https://example.invalid/scroll.git");
  await page.getByRole("button", { name: "Fetch skills" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_git_import", name: "Fetch git skills", state: "succeeded", error: null }));
  await expect(page.getByText("8 skills ready to select.")).toBeVisible();
  const list = page.getByTestId("git-import-candidate-list");
  const selected = page.getByRole("checkbox", { name: /Scroll Skill 1/ });
  await selected.check();
  await list.evaluate((element) => {
    element.scrollTop = 100;
    element.dispatchEvent(new Event("scroll", { bubbles: true }));
  });
  await expect.poll(() => list.evaluate((element) => element.scrollTop)).toBe(100);
  await expect(list.locator('input[type="checkbox"]')).toHaveCount(8);
  await page.getByRole("main", { name: "Add a skill" }).getByRole("button", { name: "Back" }).click();
  await expect(page.getByRole("heading", { name: "Where does the skill come from?" })).toBeVisible();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByLabel("Repository URL")).toHaveValue("https://example.invalid/scroll.git");
  await expect.poll(() => list.evaluate((element) => element.scrollTop)).toBe(100);
  await expect(selected).toBeChecked();
  await expect(list.locator('input[type="checkbox"]')).toHaveCount(8);
});
test("Git import with no skills shows an empty state without an error notification", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_import_job_result = { candidates: [], error: null };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Import from git/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Repository URL").fill("https://example.invalid/empty.git");
  await page.getByRole("button", { name: "Fetch skills" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_git_import", name: "Fetch git skills", state: "succeeded", error: null }));
  await expect(page.getByRole("main", { name: "Add a skill" }).getByRole("status").filter({ hasText: "No skills found. Check that this public repository contains a SKILL.md." })).toBeVisible();
  await expect(page.getByRole("list", { name: "Notifications" }).getByText("No skills found")).toHaveCount(0);
});

test("folder with no SKILL.md shows a clear empty state", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_import_job_result = { candidates: [], error: null };
  });
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Local folder" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("Folder path").fill("/sandbox/fixtures/empty");
  await page.getByRole("button", { name: "Scan folder" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_local_import", name: "Scan skill folder", state: "succeeded", error: null }));
  await expect(page.getByRole("main", { name: "Add a skill" }).getByRole("status").filter({ hasText: "No SKILL.md files were found in this source." })).toBeVisible();
});

test("folder import browses a source, reports progress, and previews detected candidates", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Local folder" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("button", { name: "Browse…" }).click();
  await expect(page.getByLabel("Folder path")).toHaveValue("/sandbox/fixtures/skills");
  await page.getByRole("button", { name: "Scan folder" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-progress", { id: "job_visual_local_import", name: "Scan skill folder", fraction: 0.5, label: "Scanning selected folder…" }));
  await expect(page.getByText("Scanning selected folder…")).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-folder-progress.png", { animations: "disabled", caret: "hide" });
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_local_import", name: "Scan skill folder", state: "succeeded", error: null }));
  await expect(page.getByText("3 skills ready to select.")).toBeVisible();
  const candidate = page.getByRole("checkbox", { name: /Review checklist/ });
  await candidate.check();
  await expect(page.getByRole("region", { name: "Selected skill preview" })).toContainText("# Review checklist");
  await expect(page).toHaveScreenshot("add-skill-folder-candidates.png", { animations: "disabled", caret: "hide" });
});

test("ZIP import cancellation and traversal failures stay visible and never continue to preview", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.locator("details").getByText("More import options").click();
  await page.getByRole("button", { name: "Zip file" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("button", { name: "Browse…" }).click();
  await expect(page.getByLabel("ZIP path")).toHaveValue("/sandbox/fixtures/skills.zip");
  await page.getByRole("button", { name: "Scan ZIP" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-progress", { id: "job_visual_local_import", name: "Scan ZIP file", fraction: 0.3, label: "Extracting archive…" }));
  await expect(page.getByText("Extracting archive…")).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-zip-progress.png", { animations: "disabled", caret: "hide" });
  await page.getByRole("button", { name: "Cancel import" }).click();
  await expect.poll(async () => page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls.filter((command) => command === "jobs_cancel").length)).toBe(1);
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_local_import", name: "Scan ZIP file", state: "cancelled", error: null }));
  await expect(page.getByRole("main", { name: "Add a skill" }).getByRole("alert")).toContainText("Import cancelled. You can retry");
  await expect(page.getByRole("button", { name: "Scan ZIP" })).toBeEnabled();
  await page.getByRole("button", { name: "Scan ZIP" }).click();
  await expect(page.getByRole("button", { name: "Cancel import" })).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_local_import", name: "Scan ZIP file", state: "failed", error: { kind: "failed", message: "unsafe archive entry `../../escape/SKILL.md`: absolute paths and traversal are not allowed" } }));
  await expect(page.getByRole("main", { name: "Add a skill" }).getByRole("alert")).toContainText("../../escape/SKILL.md");
  await expect(page.getByRole("checkbox", { name: /Review checklist/ })).toHaveCount(0);
  await expect(page).toHaveScreenshot("add-skill-zip-traversal-error.png", { animations: "disabled", caret: "hide" });
  expect(await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { calls: string[] } }).__AT_TEST_MOCKS__.calls)).not.toContain("skill_import_job_result");
});

test("library import keeps cross-source selections, skips destination clashes, and reviews Link paths", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_library_catalog = {
      sources: [
        { id: "user", label: "User scope", kind: "scope", count: 1, last_used_at: null },
        { id: "/sandbox/Projects/AgenTuna", label: "AgenTuna project", kind: "project", count: 2, last_used_at: null },
      ],
      skills: [
        { id: "user-skill", source_id: "user", source_label: "User scope", path: "/sandbox/user/skills/user-skill", name: "User Skill", description: "A user-scope source.", error: null, clash_reason: null },
        { id: "project-skill", source_id: "/sandbox/Projects/AgenTuna", source_label: "AgenTuna project", path: "/sandbox/Projects/AgenTuna/.config-studio/skills/project-skill", name: "Project Skill", description: "A project source.", error: null, clash_reason: null },
        { id: "clash-skill", source_id: "/sandbox/Projects/AgenTuna", source_label: "AgenTuna project", path: "/sandbox/Projects/AgenTuna/.config-studio/skills/existing-skill", name: "Existing Skill", description: null, error: null, clash_reason: "A skill with this name already exists in the target scope." },
      ],
      target_scope: "User skills",
    };
    mocks.commandOverrides.skill_add_preview = {
      preview_id: "preview-library-visual",
      import_id: null,
      items: [
        { name: "User Skill", source_path: "/sandbox/user/skills/user-skill", target_path: "/sandbox/user/skills/user-skill", mode: "link" },
        { name: "Project Skill", source_path: "/sandbox/Projects/AgenTuna/.config-studio/skills/project-skill", target_path: "/sandbox/user/skills/project-skill", mode: "link" },
      ],
      canonical_dir: "/sandbox/user/skills",
      files: [],
      deployments: [],
      stage_available: false,
      stage_requested: false,
      requires_overwrite_confirmation: false,
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /From your library/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByLabel("Library source")).toBeVisible();
  await expect(page.getByLabel("Library source")).toContainText("AgenTuna project · 2 skills");
  await page.getByRole("checkbox", { name: /User Skill/ }).check();
  await page.getByLabel("Library source").selectOption("/sandbox/Projects/AgenTuna");
  const projectSkill = page.getByRole("checkbox", { name: /Project Skill/ });
  await expect(projectSkill).toBeVisible();
  const clash = page.getByRole("checkbox", { name: /Existing Skill/ });
  await expect(clash).toBeDisabled();
  await expect(clash.locator("xpath=..")).toContainText("A skill with this name already exists in the target scope.");
  await page.getByRole("button", { name: "Select all available in this source" }).click();
  await expect(projectSkill).toBeChecked();
  await expect(clash).not.toBeChecked();
  await expect(page.getByRole("region", { name: "Selected library skills" })).toContainText("User Skill");
  await expect(page.getByRole("region", { name: "Selected library skills" })).toContainText("Project Skill");
  await page.getByRole("radio", { name: /Link/ }).check();
  await expect(page.getByRole("button", { name: "Next" })).toBeEnabled();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByRole("radio", { name: /User/ }).click();
  await page.getByLabel("Codex").check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByRole("heading", { name: "Review skill" })).toBeVisible();
  await expect(page.getByText("Link · Project Skill")).toBeVisible();
  await expect(page.getByText("/sandbox/Projects/AgenTuna/.config-studio/skills/project-skill → /sandbox/user/skills/project-skill")).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-library-review.png", { animations: "disabled", caret: "hide" });
});
test("library Copy selection is shown as a copied folder in Review", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.skill_library_catalog = {
      sources: [{ id: "user", label: "User scope", kind: "scope", count: 1, last_used_at: null }],
      skills: [{ id: "library-skill", source_id: "user", source_label: "User scope", path: "/sandbox/user/skills/reusable", name: "Reusable Skill", description: "A library source.", error: null, clash_reason: null }],
      target_scope: "User skills",
    };
    mocks.commandOverrides.skill_add_preview = {
      preview_id: "preview-library-copy",
      import_id: null,
      items: [{ name: "Reusable Skill", source_path: "/sandbox/user/skills/reusable", target_path: "/sandbox/user/skills/reusable-copy", mode: "copy" }],
      canonical_dir: "/sandbox/user/skills",
      files: [],
      deployments: [],
      stage_available: false,
      stage_requested: false,
      requires_overwrite_confirmation: false,
    };
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /From your library/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("checkbox", { name: /Reusable Skill/ }).check();
  await expect(page.getByRole("radio", { name: /Copy/ })).toBeChecked();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByRole("radio", { name: /User/ }).click();
  await page.getByLabel("Codex").check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("Copy · Reusable Skill")).toBeVisible();
  await expect(page.getByText("/sandbox/user/skills/reusable → /sandbox/user/skills/reusable-copy")).toBeVisible();
  await expect(page).toHaveScreenshot("add-skill-library-copy-review.png", { animations: "disabled", caret: "hide" });
});

test("empty library state offers a direct Git-import shortcut", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /From your library/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("No other skill sources are available.")).toBeVisible();
  await page.getByRole("button", { name: "Import from Git instead" }).click();
  await expect(page.getByLabel("Repository URL")).toBeVisible();
});

test("review previews exact paths and Done reflects the applied Git result", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Write a new skill/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("radio", { name: /Blank/ }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByLabel("Skill name").first().fill("Visual Skill");
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByRole("radio", { name: /Project/ }).click();
  await page.getByLabel("omp").check();
  await page.getByRole("button", { name: "Next" }).click();
  await expect(page.getByText("/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill", { exact: true })).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: "test-results/add-skill-review.png", animations: "disabled" });
  await page.getByLabel("Stage these project-scope paths with Git").check();
  await page.getByRole("button", { name: "Create skill" }).click();
  await expect(page.getByText("Creating skill…")).toBeVisible();
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-progress", { id: "job_visual_skill_apply", name: "Create skill", fraction: 0.8, label: "Writing the reviewed files…" }));
  await expect(page.getByText("Writing the reviewed files…")).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await expect(page).toHaveScreenshot("add-skill-apply-progress.png", { animations: "disabled", caret: "hide" });
  await page.evaluate(() => (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("agentuna:job-finished", { id: "job_visual_skill_apply", name: "Create skill", state: "succeeded", error: null }));
  await expect(page.getByText("Operation: op_visual_skill_add")).toBeVisible();
  await expect(page.getByText(/Git staging: staged/)).toBeVisible();
  await expect(page.getByRole("list", { name: "Written file paths" })).toContainText("/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill/SKILL.md");
  await expect(page.getByRole("list", { name: "Actual deployments" })).toContainText("/sandbox/Projects/AgenTuna/.agents/skills/visual-skill");
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: "test-results/add-skill-done.png", animations: "disabled" });
});

test("new skill scope and agents review exact destinations before writing", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Add" }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await page.getByRole("button", { name: /Write a new skill/ }).click();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("radio", { name: /Blank/ }).click();
  await page.getByRole("main", { name: "Add a skill" }).getByLabel("Skill name").first().fill("Visual Skill");
  await page.getByRole("button", { name: "Next" }).click();

  const wizard = page.getByRole("main", { name: "Add a skill" });
  await expect(wizard.getByRole("heading", { name: "Scope & agents" })).toBeVisible();
  await wizard.getByRole("radio", { name: /Project/ }).click();
  await wizard.getByLabel("Codex").check();
  await expect(wizard.getByText("Visual Skill: /sandbox/Projects/AgenTuna/.agents/skills/visual-skill", { exact: true })).toBeVisible();
  await expect(wizard.getByText("Shared skills folder with omp; enabling this path loads the skill in both agents.", { exact: true })).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await expect(page).toHaveScreenshot("add-skill-scope-agents.png", { animations: "disabled", caret: "hide" });

  await wizard.getByRole("button", { name: "Show Local scope" }).click();
  await wizard.getByRole("radio", { name: /Local/ }).click();
  await expect(wizard.getByText("/sandbox/Projects/AgenTuna/.agentuna/local", { exact: true })).toBeVisible();
  await wizard.getByRole("radio", { name: /^Project/ }).click();
  await expect(wizard.getByRole("button", { name: "Next" })).toBeEnabled();
  await wizard.getByRole("button", { name: "Next" }).click();
  await expect(wizard.getByRole("heading", { name: "Review skill" })).toBeVisible();
  await expect(wizard.getByText("/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill", { exact: true })).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await expect(page).toHaveScreenshot("add-skill-scope-review.png", { animations: "disabled", caret: "hide" });
});
