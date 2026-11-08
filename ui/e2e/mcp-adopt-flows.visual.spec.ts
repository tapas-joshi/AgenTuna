import { expect, test } from "./support/tauri-mock";

test("MCP list screen (design 06-mcp-servers)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.agents_list = [
      { id: "claude_code", name: "Claude Code", installed: true, managed: true, presence: "installed" },
      { id: "codex", name: "Codex", installed: true, managed: true, presence: "installed" },
      { id: "omp", name: "omp", installed: true, managed: true, presence: "installed" },
      { id: "cursor", name: "Cursor", installed: false, managed: false, presence: "not_installed" },
    ];
    mocks.commandOverrides.mcp_server_list = {
      servers: [
        { id: "itm_1", slug: "github", name: "github", scope: "user", transport: "stdio · npx", native: false, locked: false, lock_scope: null, lock_reason: null, health: "failed", source_path: "~/.claude.json", agents: [{ agent: "claude_code", state: "linked", config_path: null, enabled: true }, { agent: "codex", state: "linked", config_path: null, enabled: true }, { agent: "omp", state: "linked", config_path: null, enabled: true }] },
        { id: "itm_2", slug: "postgres", name: "postgres", scope: "project", transport: "stdio · npx", native: false, locked: false, lock_scope: null, lock_reason: null, health: "ok", source_path: "./.mcp.json", agents: [{ agent: "claude_code", state: "linked", config_path: null, enabled: true }, { agent: "codex", state: "linked", config_path: null, enabled: true }, { agent: "omp", state: "linked", config_path: null, enabled: true }] },
        { id: "itm_3", slug: "filesystem", name: "filesystem", scope: "global", transport: "stdio · npx", native: false, locked: true, lock_scope: "global", lock_reason: "global lock", health: "ok", source_path: "/Library/Application Support/AgenTuna/global/mcp.json", agents: [{ agent: "claude_code", state: "locked", config_path: null, enabled: true }, { agent: "codex", state: "locked", config_path: null, enabled: true }, { agent: "omp", state: "locked", config_path: null, enabled: true }] },
        { id: "", slug: null, name: "context7", scope: "user", transport: "stdio · npx", native: true, locked: false, lock_scope: null, lock_reason: null, health: "unknown", source_path: "~/.codex/config.toml", agents: [{ agent: "codex", state: "native", config_path: "~/.codex/config.toml", enabled: true }] },
        { id: "", slug: null, name: "puppeteer", scope: "user", transport: "stdio · npx", native: true, locked: false, lock_scope: null, lock_reason: null, health: "unknown", source_path: "~/.claude.json", agents: [{ agent: "claude_code", state: "native", config_path: "~/.claude.json", enabled: true }, { agent: "omp", state: "native", config_path: "~/.omp/agent/mcp.json", enabled: true }] },
      ],
      project_id: "prj_visual_fixture",
      project_name: "ai-coding-config-studio",
      project_agents: ["claude_code", "codex", "omp"],
      global_writable: true,
      global_reason: null,
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const main = page.getByRole("main", { name: "MCP Servers" });

  await expect(main.getByRole("heading", { name: "MCP Servers" })).toBeVisible();
  await expect(main.getByRole("button", { name: "Import from apps" })).toBeVisible();
  await expect(main.getByRole("button", { name: "+ Add server" })).toBeVisible();
  await expect(main.getByRole("columnheader", { name: "Claude Code" })).toBeVisible();
  await expect(main.getByRole("columnheader", { name: "Codex" })).toBeVisible();
  await expect(main.getByRole("columnheader", { name: "omp" })).toBeVisible();
  await expect(main.getByRole("columnheader", { name: "Cursor" })).toBeHidden();
  await expect(main.getByText("context7")).toBeVisible();
  await expect(page).toHaveScreenshot("06-mcp-servers-screen.png", { animations: "disabled", caret: "hide" });
});

test("Import from apps screen (design 06b-mcp-import-from-apps)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.migrate_scan_start = "job_migrate_scan";
    mocks.commandOverrides.migrate_scan_result = {
      items: [
        { kind: "mcp_server", name: "puppeteer", agents: ["claude_code"], path: "~/.claude.json", scope: "user", content_hash: "h1", parse_error: null, store_link: false, state: "not_in_agentuna", ownership: "new", mcp_summary: { transport: "stdio", command: "npx", argument_count: 2, url: null, env_names: [], header_names: [] } },
        { kind: "mcp_server", name: "puppeteer", agents: ["omp"], path: "~/.omp/agent/mcp.json", scope: "user", content_hash: "h2", parse_error: null, store_link: false, state: "conflict", ownership: "conflict", mcp_summary: { transport: "stdio", command: "npx", argument_count: 2, url: null, env_names: ["PUPPETEER_TOKEN"], header_names: [] } },
        { kind: "mcp_server", name: "context7", agents: ["claude_code"], path: "~/.claude.json", scope: "user", content_hash: "h3", parse_error: null, store_link: false, state: "not_in_agentuna", ownership: "new", mcp_summary: { transport: "http", command: null, argument_count: null, url: "mcp.context7.com/mcp", env_names: [], header_names: ["Authorization"] } },
        { kind: "mcp_server", name: "context7", agents: ["claude_code"], path: "./.mcp.json", scope: "project", content_hash: "h4", parse_error: null, store_link: false, state: "conflict", ownership: "conflict", mcp_summary: { transport: "http", command: null, argument_count: null, url: "mcp.context7.com/mcp", env_names: [], header_names: [] } },
        { kind: "mcp_server", name: "sentry", agents: ["claude_code"], path: "~/.claude.json", scope: "user", content_hash: "h5", parse_error: null, store_link: false, state: "conflict", ownership: "managed_differs", mcp_summary: { transport: "stdio", command: "npx", argument_count: 3, url: null, env_names: ["SENTRY_AUTH_TOKEN"], header_names: [] } },
      ],
      roots: [
        { path: "~/.claude.json", agents: ["claude_code"], new_items: 2, in_sync: 0 },
        { path: "~/.omp/agent/mcp.json", agents: ["omp"], new_items: 1, in_sync: 0 },
      ],
      skipped_roots: [],
      warnings: [],
      partial: false,
    };
  });

  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await page.getByRole("button", { name: "Import from apps" }).click();

  const main = page.getByRole("main", { name: "Move into AgenTuna" });
  await expect(main.getByText("MCP Servers / Import from apps")).toBeVisible();
  await expect(main.getByRole("heading", { name: "Review before import" })).toBeVisible();
  await expect(main.getByText(/5 definitions across User, Project and Local sources/)).toBeVisible();
  await expect(main.getByText(/npx · 2 args/).first()).toBeVisible();
  await expect(main.getByText(/PUPPETEER_TOKEN/)).toBeVisible();
  await expect(main).not.toContainText("sk-live-fixture-secret");
  await expect(page).toHaveScreenshot("06b-mcp-import-from-apps-screen.png", { animations: "disabled", caret: "hide" });
});

test("Add MCP server form (design 03-mcp-server-editor)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_plan_preview = {
      change_set_id: "cs-visual-mcp",
      preview_id: "preview-visual-mcp",
      files: [
        { agent: "Claude Code", scope: "user", path: "~/.claude.json", additions: 6, deletions: 0, diff: "+++ ~/.claude.json > mcpServers\n+ \"github\": {\n+   \"command\": \"npx\",\n+   \"args\": [\"-y\", \"@modelcontextprotocol/server-github\"],\n+   \"env\": { \"GITHUB_PERSONAL_ACCESS_TOKEN\": \"${GITHUB_PERSONAL_ACCESS_TOKEN}\" }\n+ }", selected: true, cannot_write_reason: null },
        { agent: "Codex", scope: "user", path: "~/.codex/config.toml", additions: 6, deletions: 0, diff: "", selected: true, cannot_write_reason: null },
        { agent: "omp", scope: "user", path: "~/.omp/agent/mcp.json", additions: 6, deletions: 0, diff: "", selected: true, cannot_write_reason: null },
      ],
      unsupported_fields: [],
      warnings: [],
      store_init: null,
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await page.getByRole("button", { name: "+ Add server" }).click();

  const main = page.getByRole("main", { name: "Add MCP server" });
  await expect(main.getByRole("heading", { name: "Add MCP server" })).toBeVisible();
  await main.getByRole("textbox", { name: "Name" }).fill("github");
  await main.getByRole("textbox", { name: "Command" }).fill("npx -y @modelcontextprotocol/server-github");

  // Scope cards
  await expect(main.getByLabel("Global")).toBeVisible();
  await expect(main.getByLabel("User")).toBeVisible();
  await expect(main.getByLabel("Project")).toBeVisible();

  // Select Claude Code agent
  await main.getByRole("button", { name: "Claude Code" }).click();

  // Preview button in sticky footer
  await page.getByRole("button", { name: "Review changes" }).click();
  await expect(main.getByText("Files that will change")).toBeVisible();
  await expect(main.getByRole("button", { name: "Save to User scope" })).toBeVisible();
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    const scrollContainer = document.querySelector("#shell-main") || document.querySelector("main");
    if (scrollContainer) scrollContainer.scrollTop = 0;
  });
  await expect(page).toHaveScreenshot("03-mcp-server-editor-screen.png", { animations: "disabled", caret: "hide" });
});

test("Edit MCP server form (design 03b-edit-mcp-server)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_server_detail = {
      item_id: "itm_github",
      name: "github",
      scope: "user",
      description: "GitHub issues, PRs, code search",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-github"],
      cwd: null,
      url: null,
      env: [
        { key: "GITHUB_PERSONAL_ACCESS_TOKEN", value: "${GITHUB_PERSONAL_ACCESS_TOKEN}", is_secret: true, keychain_account: "github-pat" },
      ],
      defined_in: ["~/.claude.json", "~/.codex/config.toml", "~/.omp/agent/mcp.json"],
      tools: ["create_issue", "search_code", "get_pull_request", "list_commits"],
      health_status: "ok",
      health_error_code: null,
      health_error_text: null,
      last_checks: [],
      cadence_text: "every 5 min",
      is_oauth: false,
      needs_auth: false,
      locked: false,
      locked_by: null,
      lock_reason: null,
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed", enabled: true, state: "linked", config_path: "~/.claude.json" },
        { id: "codex", label: "Codex", presence: "installed", enabled: true, state: "linked", config_path: "~/.codex/config.toml" },
        { id: "omp", label: "omp", presence: "installed", enabled: true, state: "linked", config_path: "~/.omp/agent/mcp.json" },
      ],
      why_summary: "Defined at User.",
    };
    mocks.commandOverrides.mcp_plan_preview = {
      change_set_id: "cs-edit-github",
      preview_id: "preview-edit-github",
      files: [
        { agent: "Claude Code", scope: "user", path: "~/.claude.json", additions: 6, deletions: 0, diff: "+++ ~/.claude.json > mcpServers\n+ \"github\": {\n+   \"command\": \"npx\",\n+   \"args\": [\"-y\", \"@modelcontextprotocol/server-github\"],\n+   \"env\": { \"GITHUB_PERSONAL_ACCESS_TOKEN\": \"${GITHUB_PERSONAL_ACCESS_TOKEN}\" }\n+ }", selected: true, cannot_write_reason: null },
        { agent: "Codex", scope: "user", path: "~/.codex/config.toml", additions: 6, deletions: 0, diff: "", selected: true, cannot_write_reason: null },
        { agent: "omp", scope: "user", path: "~/.omp/agent/mcp.json", additions: 6, deletions: 0, diff: "", selected: true, cannot_write_reason: null },
      ],
      unsupported_fields: [],
      warnings: [],
      store_init: null,
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  // Open server and click Edit
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Edit" }).click();

  const main = page.getByRole("main", { name: /Edit/ });
  await expect(main.getByRole("heading", { name: /Edit/ })).toBeVisible();
  await expect(main.getByRole("textbox", { name: "Command" })).toHaveValue("npx");
  await page.getByRole("button", { name: "Add argument" }).click();
  await main.getByRole("textbox", { name: "Argument 3" }).fill("--verbose");
  await page.getByRole("button", { name: "Review edit" }).click();
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    const scrollContainer = document.querySelector("#shell-main") || document.querySelector("main");
    if (scrollContainer) scrollContainer.scrollTop = 0;
  });
  await expect(page).toHaveScreenshot("03b-edit-mcp-server-screen.png", { animations: "disabled", caret: "hide" });
});

test("Native MCP server detail with Adopt CTA (design 10-mcp-server-github)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_server_detail = {
      item_id: "native:github",
      name: "github",
      scope: "user",
      description: "GitHub issues, PRs, code search",
      transport: "stdio",
      command: "npx -y @modelcontextprotocol/server-github",
      args: [],
      cwd: null,
      url: null,
      env: [
        { key: "GITHUB_PERSONAL_ACCESS_TOKEN", value: "${GITHUB_PERSONAL_ACCESS_TOKEN}", is_secret: true, keychain_account: "github-pat" },
      ],
      defined_in: ["~/.claude.json", "~/.codex/config.toml", "~/.omp/agent/mcp.json"],
      tools: ["create_issue", "search_code", "get_pull_request", "list_commits", "create_branch"],
      health_status: "failed",
      health_error_code: "401",
      health_error_text: "Unauthorized",
      last_checks: [
        { checked_at: "1", status: "failed", latency_ms: 120, trigger: "auto", error_text: "401 Unauthorized" },
        { checked_at: "2", status: "failed", latency_ms: 110, trigger: "auto", error_text: "401 Unauthorized" },
        { checked_at: "3", status: "ok", latency_ms: 80, trigger: "auto", error_text: null },
        { checked_at: "4", status: "ok", latency_ms: 85, trigger: "auto", error_text: null },
        { checked_at: "5", status: "ok", latency_ms: 90, trigger: "auto", error_text: null },
      ],
      cadence_text: "every 5 min",
      is_oauth: false,
      needs_auth: true,
      locked: false,
      locked_by: null,
      lock_reason: null,
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed", enabled: true, state: "native", config_path: "~/.claude.json" },
        { id: "codex", label: "Codex", presence: "installed", enabled: true, state: "native", config_path: "~/.codex/config.toml" },
        { id: "omp", label: "omp", presence: "installed", enabled: true, state: "native", config_path: "~/.omp/agent/mcp.json" },
        { id: "cursor", label: "Cursor", presence: "not_installed", enabled: false, state: "none", config_path: null },
      ],
      why_summary: "Defined at User. Nearest scope wins; nothing overrides it here.",
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  // Open Native server
  await page.getByRole("button", { name: "Actions for Native server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const main = page.getByRole("main", { name: "MCP Server Detail" });
  await expect(main.getByRole("heading", { name: "github" })).toBeVisible();
  await expect(main.getByText("not in AgenTuna")).toBeVisible();
  await expect(main.getByRole("button", { name: "Adopt server" })).toBeVisible();
  await expect(main.getByRole("button", { name: "Edit" })).toBeHidden();
  await expect(page).toHaveScreenshot("10-mcp-server-github-screen.png", { animations: "disabled", caret: "hide" });
});

test("Postgres MCP server detail (design 10-mcp-server-postgres)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.mcp_server_detail = {
      item_id: "itm_postgres",
      name: "postgres",
      scope: "project",
      description: "Query the dev database",
      transport: "stdio",
      command: "npx -y @modelcontextprotocol/server-postgres postgresql://localhost/dev",
      args: [],
      cwd: null,
      url: null,
      env: [],
      defined_in: ["./.mcp.json"],
      tools: ["query"],
      health_status: "ok",
      health_error_code: null,
      health_error_text: null,
      last_checks: [
        { checked_at: "1", status: "ok", latency_ms: 38, trigger: "auto", error_text: null },
        { checked_at: "2", status: "ok", latency_ms: 35, trigger: "auto", error_text: null },
        { checked_at: "3", status: "ok", latency_ms: 36, trigger: "auto", error_text: null },
        { checked_at: "4", status: "ok", latency_ms: 40, trigger: "auto", error_text: null },
        { checked_at: "5", status: "ok", latency_ms: 38, trigger: "auto", error_text: null },
      ],
      cadence_text: "every 5 min",
      is_oauth: false,
      needs_auth: false,
      locked: false,
      locked_by: null,
      lock_reason: null,
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed", enabled: true, state: "linked", config_path: "./.mcp.json" },
        { id: "codex", label: "Codex", presence: "installed", enabled: true, state: "linked", config_path: "./.mcp.json" },
        { id: "omp", label: "omp", presence: "installed", enabled: true, state: "linked", config_path: "./.mcp.json" },
        { id: "cursor", label: "Cursor", presence: "not_installed", enabled: false, state: "none", config_path: null },
      ],
      why_summary: "Defined at Project. Nearest scope wins; nothing overrides it here.",
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const main = page.getByRole("main", { name: "MCP Server Detail" });
  await expect(main.getByRole("heading", { name: "postgres" })).toBeVisible();
  await expect(main.getByText("Healthy · 38 ms")).toBeVisible();
  await expect(main.getByText("Loaded by")).toBeVisible();
  await expect(main.getByText("Cursor")).toBeVisible();
  await expect(main.getByText("not installed")).toBeVisible();
  await expect(page).toHaveScreenshot("10-mcp-server-postgres-screen.png", { animations: "disabled", caret: "hide" });
});

test("AdoptModal dialog (design overlay-adopt-item)", async ({ page }) => {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.adopt_preview = {
      source: ".agents/skills/release-notes/",
      store_root: ".agentuna",
      store_destination: ".agentuna/skills/release-notes/",
      backup_dir: ".agentuna/backups/2026-11-04-1432/",
      slug: "release-notes",
      native_name: "release-notes",
      kind: "skill",
      agents: ["codex", "omp"],
      handling: "link",
      renamed: false,
      current: "present",
    };
  });

  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await page.getByRole("button", { name: "Actions for Native server" }).click();
  await page.getByRole("menuitem", { name: "Adopt…" }).click();

  const dialog = page.getByRole("dialog", { name: "Adopt release-notes" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("ORIGINAL · READ-ONLY")).toBeVisible();
  await expect(dialog.getByText(".agents/skills/release-notes/")).toBeVisible();
  await expect(dialog.getByText("Codex + omp · one shared folder")).toBeVisible();
  await expect(dialog.getByText("AGENTUNA DESTINATION")).toBeVisible();
  await expect(dialog.getByText(".agentuna/skills/release-notes/")).toBeVisible();
  await expect(dialog.getByText(/Backup:/)).toBeVisible();
  await expect(dialog.getByText("What changes")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Adopt" })).toBeVisible();
  await expect(page).toHaveScreenshot("overlay-adopt-item-screen.png", { animations: "disabled", caret: "hide" });
});
