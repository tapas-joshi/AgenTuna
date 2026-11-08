import { test as base, expect, type Page } from "@playwright/test";

/** Browser-only Tauri IPC substitute shared by every visual spec. */
export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(() => {
      const fixedNow = 1_798_560_000_000;
      const settings = { schema_version: 1, values: {} as Record<string, unknown> };
      const projects: Array<Record<string, unknown>> = [];
      const project = { id: "prj_visual_fixture", name: "AgenTuna", path: "/sandbox/Projects/AgenTuna", last_used_at: "2026-11-06T10:00:00Z", skill_count: 5, server_count: 2 };
      projects.push(project);
      settings.values.last_project_id = project.id;
      const mocks = { settings, projects, policyMode: "normal", commandOverrides: {} as Record<string, unknown>, calls: [] as string[] };
      Object.defineProperty(window, "__AT_TEST_MOCKS__", { value: mocks });
      Object.defineProperty(window, "__AT_TOAST_DISMISS_MS__", { value: 30_000 });
      const paths = { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
      const policy = { offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false, statement: "No automatic network calls.", allowed_hosts: [], actions: [], events: [], capacity: 100 };
      const emptyOverview = {
        scopes: [
          { scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: false },
          { scope: "user", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: false },
          { scope: "project", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0, store_ready: false },
        ],
        attention: [],
        attention_total: 0,
        changes: [],
        user_path: "/sandbox/user",
        global_path: "/sandbox/global",
        global_writable: true,
        global_reason: null,
        global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] },
        startup_warnings: [],
      };
      const remoteBackupSettings = { device_name: "Visual workstation", repo_url: "https://example.invalid/agentuna-backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: true };
      const remoteBackupStatus = { repo_url: remoteBackupSettings.repo_url, device_name: remoteBackupSettings.device_name, branch: "main", last_push_at: null, last_sync_at: null, last_commit: null, conflict_count: 1, offline: false, status: "idle", error_kind: null };
      const remoteBackupConflicts = [{ id: "remote-device-20261106T100000Z", device_name: "travel-laptop", timestamp: "20261106T100000Z", files: [{ path: "skills/review-helper/SKILL.md", local_path: "/sandbox/user/skills/review-helper/SKILL.md", remote_path: "/sandbox/user/.state/remote-conflicts/travel-laptop/20261106T100000Z/skills/review-helper/SKILL.md", local_exists: true, remote_size: 142, local_size: 136 }] }];
      const remoteBackupCommit = "a".repeat(40);
      const remoteBackupCommits = [{ commit: remoteBackupCommit, committed_at: "2026-11-06T10:00:00Z", message: "Backup from travel-laptop" }];
      const remoteBackupPreview = { preview_id: "preview-remote-conflict", summary: "Keep the remote copy of skills/review-helper/SKILL.md.", files: [{ path: "skills/review-helper/SKILL.md", action: "write", diff: "--- local\n+++ remote\n", before_hash: "1".repeat(64), after_hash: "2".repeat(64), backup_required: true }], archived_remote_path: null };
      const skill = {
        item_id: "itm_visual_skill", name: "Code Review", slug: "code-review", scope: "user",
        path: "/sandbox/user/skills/code-review",
        markdown: "---\nname: Code Review\ndescription: A deterministic visual fixture.\n---\n\n# Code Review\n\nA deterministic visual fixture.\n",
        description: "A deterministic visual fixture.", hash: "0".repeat(64), updated_at: "2026-11-06T10:00:00Z",
        source: null, mode: "copy", shadowed_by: "project", shadow_chain: ["project", "user"], deployments: [], history: [],
        missing: false, editable: true, update_available: false, source_commit_sha: null,
        source_remote_sha: null, source_last_checked_at: null, auto_update: "inherit",
      };
      let createdRule: { item_id: string; scope: string; path: string; managed: boolean; content: string; hash: string; agents: string[]; read_error: null } | null = null;
      const callbacks = new Map<number, (payload: unknown) => void>();
      const eventCallbacks = new Map<string, Set<number>>();
      const pendingEvents = new Map<string, unknown>();
      let nextCallback = 1;
      let skillAddStageRequested = false;
      Object.defineProperty(mocks, "emit", { value: (event: string, payload: unknown) => {
        const ids = eventCallbacks.get(event);
        if (!ids?.size) { pendingEvents.set(event, payload); return; }
        for (const callbackId of ids) callbacks.get(callbackId)?.({ event, id: 1, payload });
      } });
      const agentCards = [
        { id: "claude_code", name: "Claude Code", installed: true, version: "1.2.0", managed: true, paths: ["/sandbox/home/.claude/skills"], health: "~/.claude/skills · 2 / 3 managed · 2 in sync", link_mode: "link", presence: "installed", format: "JSON + Markdown skills", managed_skills: 2, managed_servers: 1, native_skills: 1, native_servers: 1, copy_fallback: false, primary_path: "/sandbox/home/.claude/skills", needs_path: false, in_sync: 2, total: 3, linked_deployments: 2 },
        { id: "cursor", name: "Cursor", installed: false, version: null, managed: false, paths: [], health: "!<path> · 0 / 0 managed · 0 in sync", link_mode: "link", presence: "not_installed", format: "—", managed_skills: 0, managed_servers: 0, native_skills: 0, native_servers: 0, copy_fallback: false, primary_path: null, needs_path: true, in_sync: 0, total: 0, linked_deployments: 0 },
      ];
      const presetFixture = { id: "preset_visual", name: "Code review", scope: "user", created_at: "2026-11-06T10:00:00Z", members: [{ item_id: "itm_visual_skill", name: "Code Review", kind: "skill", enabled: true, exists: true }, { item_id: "itm_visual_gone", name: "Missing item", kind: "mcp_server", enabled: true, exists: false }], ratios: [{ target: "claude_code", enabled: 2, total: 3, complete: false }], applications: [{ id: 11, target_kind: "agent", target: "claude_code", applied_at: "2026-11-06T10:00:00Z", member_count: 1 }], missing_members: ["Missing item"], applicable: true };
      const driftRowFixture = { event_id: 17n, kind: "drifted", item_id: "itm_visual_skill", item_name: "Code Review", agent: "codex", scope: "project", path: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md", detected_at: "2026-11-06T10:00:00Z", changed_file_count: 1, can_adopt: true, can_restore: true, link_target: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review" };
      const driftSnapshotFixture = { rows: [], in_sync_count: 41, watched_count: 42, watcher: { status: "active", path: null, error: null } };
      const driftPreviewFixture = { event_id: driftRowFixture.event_id, path: driftRowFixture.path, canonical_path: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review/SKILL.md", previous_version_available: true, diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1 +1 @@\n-# Code Review\n+# Code Review checklist\n", on_disk_content: null, redacted: false };
      const backupFixture = { id: "backup-visual-drift", original_path: driftRowFixture.path, created_at: "2026-11-06T09:45:00Z", size: 128n, operation_id: "op-visual-drift", restorable: true, unavailable_reason: null };
      const backupPreviewFixture = { id: backupFixture.id, original_path: driftRowFixture.path, created_at: backupFixture.created_at, diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1 +1 @@\n-# Code Review checklist\n+# Code Review\n", destination_exists: true, redacted: false };
      const migratePlanCounts = { items: 3, files_created: 6, files_backed_up: 2, links_written: 2, files_left_in_place: 0 };
      const migratePlanFixture = { plan_hash: "d".repeat(64), backup_dir: "/sandbox/support/backups/migration", strategy: "replace_with_links", counts: migratePlanCounts, scopes: [{ scope: "project", store_path: "/sandbox/Projects/AgenTuna/.config-studio", counts: migratePlanCounts, steps: [{ seq: 1, action: "backuporiginal", scope: "project", item_name: "Visual migration skill", source: "/sandbox/home/.claude/skills/visual-migration", destination: null, backup_path: "/sandbox/support/backups/migration/visual-migration", bytes: 128, agents: [] }, { seq: 2, action: "createstoreitem", scope: "project", item_name: "Visual migration skill", source: "/sandbox/home/.claude/skills/visual-migration", destination: "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-migration", backup_path: null, bytes: 128, agents: ["claude_code"] }] }], files_checked: 2 };
      const migrateScanFixture = { items: [{ kind: "skill", name: "Visual migration skill", agents: ["claude_code"], path: "/sandbox/home/.claude/skills/visual-migration", scope: "user", content_hash: "a".repeat(64), parse_error: null, store_link: false, state: "not_in_agentuna", ownership: "not_in_agentuna" }, { kind: "mcp_server", name: "Local docs server", agents: ["codex", "omp"], path: "/sandbox/home/.claude.json", scope: "user", content_hash: "b".repeat(64), parse_error: null, store_link: false, state: "conflict", ownership: "conflict" }, { kind: "rule", name: "Project guidance", agents: ["claude_code"], path: "/sandbox/Projects/AgenTuna/CLAUDE.md", scope: "project", content_hash: "c".repeat(64), parse_error: null, store_link: false, state: "not_in_agentuna", ownership: "not_in_agentuna" }], partial: false, skipped_roots: [], warnings: [], roots: [{ path: "/sandbox/home/.claude/skills", kind: "skill", agents: ["claude_code"], found_items: 1, new_items: 1, skipped_items: 0, in_sync: 0, status: "scanned", unreadable_reason: null }, { path: "/sandbox/home/.codex/skills", kind: "skill", agents: ["codex", "omp"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "empty", unreadable_reason: null }, { path: "/sandbox/home/.claude.json", kind: "mcp_server", agents: ["codex", "omp"], found_items: 1, new_items: 1, skipped_items: 0, in_sync: 0, status: "scanned", unreadable_reason: null }, { path: "/sandbox/Projects/AgenTuna/CLAUDE.md", kind: "rule", agents: ["claude_code"], found_items: 1, new_items: 1, skipped_items: 0, in_sync: 0, status: "scanned", unreadable_reason: null }, { path: "/sandbox/home/.cursor/skills", kind: "skill", agents: ["cursor"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "not_present", unreadable_reason: null }, { path: "/sandbox/home/.agents/skills", kind: "skill", agents: ["codex", "omp"], found_items: 0, new_items: 0, skipped_items: 0, in_sync: 0, status: "unreadable", unreadable_reason: "permission denied" }] };
      (migrateScanFixture.items[1] as Record<string, unknown>).mcp_summary = { transport: "stdio", command: "node", argument_count: 2, url: null, env_names: ["LOCAL_DOCS_KEY"], header_names: [] };
      const ipc = {
        invoke: async (command: string, args?: { key?: string; value?: unknown; itemId?: string; path?: string; url?: string; reference?: string | null; kind?: string; importId?: string; scope?: string; event?: unknown; handler?: number; wizard?: unknown; request?: unknown; jobId?: string; commit?: string; confirmationToken?: string; groupId?: string; relativePath?: string; side?: string; resolution?: string; previewId?: string; confirmDeletions?: boolean }) => {
          mocks.calls.push(command);
          const override = mocks.commandOverrides[command];
          if (typeof override === "function") return (override as (args: unknown) => unknown)(args);
          if (Object.hasOwn(mocks.commandOverrides, command)) return override;
          switch (command) {
            case "app_info": return { name: "AgenTuna", version: "0.1.0" };
            case "open_external_link": return null;
            case "app_update_status": return { status: { state: "idle" }, can_install: false, defer_reason: null, can_cancel: false };
            case "app_signing_status": return "developer_id";
            case "app_update_check": return { status: { state: "idle" }, can_install: false, defer_reason: null, can_cancel: false };
            case "app_update_install_request": return { status: { state: "queued", version: "0.2.0", reason: "editor_dirty", requested_at: "2026-11-06T10:00:00Z" }, can_install: false, defer_reason: "editor_dirty", can_cancel: true };
            case "app_update_install_resume": return { status: { state: "queued", version: "0.2.0", reason: "editor_dirty", requested_at: "2026-11-06T10:00:00Z" }, can_install: false, defer_reason: "editor_dirty", can_cancel: true };
            case "app_update_install_cancel": return { status: { state: "idle" }, can_install: false, defer_reason: null, can_cancel: false };
            case "app_menu_set_add_skill_enabled": return null;
            case "settings_load": return settings;
            case "settings_set": if (args?.key) settings.values[args.key] = args.value; return null;
            case "instructions_list": return [...(createdRule ? [createdRule] : []), { item_id: "itm_visual_rule", scope: "project", path: "/sandbox/Projects/AgenTuna/AGENTS.md", managed: true, content: "# Repository guidance\n\nReview changes carefully.\n", hash: "c".repeat(64), agents: ["codex", "omp"], read_error: null }, { item_id: "itm_visual_user_rule", scope: "user", path: "/sandbox/home/.claude/CLAUDE.md", managed: false, content: "# Personal preferences\n\nPrefer small PRs.\n", hash: "e".repeat(64), agents: ["claude_code"], read_error: null }];
            case "instruction_create_options": return { global_writable: true, global_reason: null, local_available: true, local_reason: null, has_project: true };
            case "instruction_create_preview": {
              const request = args as { fileType?: string; scope?: string } | undefined;
              const filename = request?.fileType ?? "AGENTS.md";
              const path = request?.scope === "local"
                ? `/sandbox/Projects/AgenTuna/${filename}`
                : `/sandbox/Projects/AgenTuna/.config-studio/rules/${filename}`;
              return { path, exists: false, reason: null };
            }
            case "instruction_create": {
              const request = args?.request as { file_type?: string; scope?: string } | undefined;
              const filename = request?.file_type ?? "AGENTS.md";
              const path = request?.scope === "local"
                ? `/sandbox/Projects/AgenTuna/${filename}`
                : `/sandbox/Projects/AgenTuna/.config-studio/rules/${filename}`;
              createdRule = { item_id: "itm_visual_created_rule", scope: request?.scope ?? "project", path, managed: true, content: "# New rule\n", hash: "d".repeat(64), agents: ["codex", "omp"], read_error: null };
              return path;
            }
            case "instruction_save": {
              const request = args?.request as { path?: string; content?: string } | undefined;
              if (createdRule !== null && request?.path === createdRule.path) createdRule = { ...createdRule, content: request.content ?? createdRule.content, hash: "f".repeat(64) };
              return { status: "saved", conflict: null, backup_path: "/sandbox/support/backups/AGENTS.md", hash: "f".repeat(64), unreadable_path: null };
            }
            case "agents_list": return agentCards;
            case "custom_agent_preview": {
              const request = args?.request as { display_name?: string; root?: string; skills_subfolder?: string | null; mcp_format?: string; mcp_file?: string | null; rule_files?: string[] } | undefined;
              const root = request?.root ?? "";
              return {
                skills_path: root && request?.skills_subfolder ? `${root}/${request.skills_subfolder}` : null,
                mcp_path: request?.mcp_format === "none" || !root ? null : `${root}/${request?.mcp_file ?? "mcp.json"}`,
                rule_paths: (request?.rule_files ?? []).map((file) => file.startsWith("/") ? file : `${root}/${file}`),
                valid: Boolean(request?.display_name?.trim() && root.startsWith("/")),
                reason: !request?.display_name?.trim() ? "Enter a display name." : !root.startsWith("/") ? "Root folder must be an absolute path." : null,
                existing_agent_id: null,
              };
            }
            case "custom_agent_create": {
              const request = args?.request as { display_name?: string; root?: string; skills_subfolder?: string | null; mcp_format?: string } | undefined;
              const name = request?.display_name ?? "Visual Agent";
              const skillsPath = `${request?.root ?? "/sandbox/visual-agent"}/${request?.skills_subfolder ?? "skills"}`;
              return { id: `custom:${name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`, name, installed: true, version: null, managed: true, paths: [skillsPath], health: "0 skills · 0 servers", link_mode: "link", presence: "installed", format: request?.mcp_format === "none" ? "MCP not supported" : "Custom MCP configuration", managed_skills: 0, managed_servers: 0, native_skills: 0, native_servers: 0, copy_fallback: false, primary_path: skillsPath, needs_path: false, in_sync: 0, total: 0, linked_deployments: 0 };
            }
            case "agents_rescan": return agentCards;
            case "agent_manage_set": return null;
            case "agent_link_mode_set": return null;
            case "agent_path_choose": return "/sandbox/Applications/Cursor.app";
            case "agent_path_candidates": return [];
            case "agent_path_validate": return { exists: true, is_directory: true, structure_warning: null, reason: null };
            case "agent_path_set": return { ...agentCards[1], presence: "configured_only", primary_path: "/sandbox/Applications/Cursor.app", health: "/sandbox/Applications/Cursor.app · 0 / 0 managed · 0 in sync" };
            case "presets_list": return [presetFixture];
            case "preset_member_candidates": return [{ item_id: "itm_visual_candidate", name: "Release notes", kind: "skill", scope: "user" }];
            case "preset_create": return "preset_visual_new";
            case "preset_rename": return null;
            case "preset_delete": return null;
            case "preset_member_set": return null;
            case "preset_apply": return { operation_id: "op-preset-visual", changed: true, changed_members: 1, enabled: 3, total: 3, complete: true, warnings: [] };
            case "preset_unapply": return { operation_id: "op-preset-unapply", changed: true, changed_members: 1, enabled: 1, total: 3, complete: false, warnings: [] };
            case "migrate_conflicts": return { conflicts: [{ kind: "skill", name: "review-helper", copies: [{ agent: "claude_code", scope: "user", path: "/sandbox/home/.claude/skills/review-helper", raw_hash: "a".repeat(64), normalized_hash: "a".repeat(64) }, { agent: "codex", scope: "user", path: "/sandbox/home/.codex/skills/review-helper", raw_hash: "b".repeat(64), normalized_hash: "b".repeat(64) }], formatting_only: false, unified_diff: "--- A\n+++ B\n- old copy\n+ new copy\n", proposed_resolution: "needs_choice" }] };
            case "projects_list": return mocks.projects;
            case "overview_load": return emptyOverview;
            case "overview_setup_preview": return { preview_id: "preview-visual-scope-setup", scope: String(args?.scope ?? "project"), path: "/sandbox/Projects/AgenTuna/.config-studio", files: [{ path: "skills", action: "mkdir", is_new: true }, { path: "mcp", action: "mkdir", is_new: true }, { path: "AGENTS.md", action: "write", is_new: true }], foreign: [], already_ready: false };
            case "overview_setup_apply": return { operation_id: "op-visual-scope-setup", scope: String(args?.scope ?? "project"), path: "/sandbox/Projects/AgenTuna/.config-studio", files_changed: 3 };
            case "recovery_sheet": return null;
            case "recovery_discard": return null;
            case "recovery_restore": return null;
            case "effective_counts": return { skills: 5, mcp_servers: 2, rules: 1 };
            case "effective_query": {
              const request = args?.request as { kind?: string } | undefined;
              const effectiveKind = request?.kind ?? "skill";
              const name = effectiveKind === "mcp_server" ? "Postgres" : effectiveKind === "rule" ? "Repository guidance" : "Code Review";
              const agentStates = effectiveKind === "mcp_server"
                ? [
                    { agent: "claude_code", state: "native", loads: true, path: "~/.claude.json" },
                    { agent: "codex", state: "native", loads: true, path: "~/.codex/config.toml" },
                    { agent: "omp", state: "native", loads: true, path: "~/.omp/agent/mcp.json" },
                  ]
                : [
                    { agent: "claude_code", state: "linked", loads: true, path: "~/.claude/skills/code-review" },
                    { agent: "codex", state: "linked", loads: true, path: "~/.agents/skills/code-review" },
                  ];
              return {
                items: [{
                  logical_id: `logical_visual_${effectiveKind}`,
                  item_id: skill.item_id,
                  kind: effectiveKind,
                  name,
                  winner_scope: "user",
                  source_path: effectiveKind === "mcp_server"
                    ? "~/.claude.json · ~/.codex/config.toml · ~/.omp/agent/mcp.json"
                    : `/sandbox/user/${effectiveKind}/${name.toLowerCase().replaceAll(" ", "-")}`,
                  agents: agentStates,
                  loads_in_agents: agentStates.filter((a) => a.loads).length,
                  lock: null,
                }],
                disabled_here: [],
                diagnostics: [],
                load_diagnostics: [],
              };
            }
            case "why_trace": return { trace: { item_id: "itm_visual_skill", active_scope: "user", agent: "claude_code", steps: [{ scope: "global", status: "Absent", location: null, reason: "No definition at Global." }, { scope: "user", status: "Winning", location: { path: "/sandbox/user/skills/code-review/SKILL.md", byte_span: null, line: null, rendering_path: null }, reason: "This User copy wins for Claude Code." }, { scope: "project", status: "Absent", location: null, reason: "No Project override." }, { scope: "local", status: "Absent", location: null, reason: "No Local override." }], verdict: { on: true, decisive_step: 1 } }, load_diagnostics: [] };
            case "migrate_scan": return migrateScanFixture;
            case "migrate_scan_start": return "job_visual_scan";
            case "migrate_scan_result": return migrateScanFixture;
            case "migrate_plan": return migratePlanFixture;
            case "migrate_apply": return "job_visual_migrate_apply";
            case "migrate_apply_result": return { job_id: "job_visual_migrate_apply", operation_id: "op_visual_migration", completed: 5n, touched_paths: ["/sandbox/Projects/AgenTuna/.config-studio/skills/visual-migration"], warnings: [], backup_dir: "/sandbox/support/backups/migration", scopes: [{ scope: "project", store_path: "/sandbox/Projects/AgenTuna/.config-studio", counts: migratePlanFixture.counts }], strategy: "replace_with_links", plan_hash: "d".repeat(64), counts: migratePlanFixture.counts, steps: migratePlanFixture.scopes.flatMap((entry) => entry.steps) };
            case "operation_undo_check": return { operation_id: "op_visual_migration", known: true, already_undone: false, expired: false, window_days: 30, expires_at_unix_ms: BigInt(Date.now() + 30 * 86400 * 1000), conflicted_paths: [] };
            case "item_lookup": return { kind: "skill", scope: "user", project_id: null };
            case "skill_detail_load": return skill;
            case "skill_compare_copies": return { winner: { scope: "project", markdown: "# Code Review\n\nProject copy.", hash: "1".repeat(64), deployments: ["claude_code"] }, shadow: { scope: "user", markdown: "# Code Review\n\nUser copy.", hash: "2".repeat(64), deployments: ["codex"] }, identical: false, diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Code Review\n-User copy.\n+Project copy.\n" };
            case "skill_agent_preview": return { preview_id: "preview-enable-visual", enabled: Boolean((args?.request as { enabled?: unknown } | undefined)?.enabled), mode: "copy", paths: ["/sandbox/user/skills/code-review"], coupled_agents: [], warnings: [], no_op: false };
            case "skill_agent_apply": return { operation_id: "op-enable-visual", enabled: true, changed: true };
            case "skill_agent_discard": return true;
            case "skill_transfer_options": {
              const request = args?.request as { item_id?: string; transfer?: string; project_id?: string | null } | undefined;
              const duplicate = request?.transfer === "duplicate";
              return {
                item_id: request?.item_id ?? "itm_visual_skill",
                name: "Code Review",
                source_scope: "user",
                source_path: "/sandbox/user/skills/code-review",
                targets: duplicate
                  ? [{ scope: "user", path: "/sandbox/user", available: true, reason: null }]
                  : [
                    { scope: "global", path: "/sandbox/global", available: true, reason: null },
                    { scope: "user", path: "/sandbox/user", available: true, reason: null },
                    { scope: "project", path: "/sandbox/Projects/AgenTuna/.config-studio", available: request?.project_id !== null, reason: null },
                  ],
              };
            }
            case "skill_transfer_preview": {
              const request = args?.request as { transfer?: string; target_scope?: string; resolution?: string | null; new_slug?: string | null } | undefined;
              const duplicate = request?.transfer === "duplicate";
              const collision = !duplicate && request?.resolution == null;
              const targetScope = request?.target_scope ?? "global";
              return {
                preview_id: collision ? null : `preview-transfer-${request?.resolution ?? "duplicate"}`,
                name: "Code Review",
                source_scope: "user",
                target_scope: targetScope,
                source_path: "/sandbox/user/skills/code-review",
                target_path: `/sandbox/${targetScope}/skills/${request?.new_slug ?? "code-review"}`,
                source_hash: "0".repeat(64),
                target_hash: collision ? "1".repeat(64) : null,
                removes_source_after_backup: request?.transfer === "move",
                backup_location: "/sandbox/support/backups/skills",
                overwrite: collision ? {
                  target_hash: "1".repeat(64),
                  source_hash: "0".repeat(64),
                  files: [{ path: "SKILL.md", change: "changed" }, { path: "examples/review.md", change: "added" }],
                } : null,
                files: collision ? [] : ["SKILL.md", "examples/review.md"],
              };
            }
            case "skill_transfer_apply": return { operation_id: "op-skill-transfer", backup_location: "/sandbox/support/backups/skills", target_path: "/sandbox/global/skills/code-review" };
            case "skill_transfer_bulk_preview": {
              const request = args?.request as { item_ids?: string[]; target_scope?: string } | undefined;
              return {
                preview_id: "preview-bulk-transfer",
                target_scope: request?.target_scope ?? "global",
                backup_location: "/sandbox/support/backups/skills",
                items: (request?.item_ids ?? ["itm_visual_skill"]).map((item_id) => ({ item_id, name: "Code Review", reason: null })),
              };
            }
            case "skill_transfer_bulk_apply": return { operation_id: "op-bulk-transfer", backup_location: "/sandbox/support/backups/skills", applied_count: 2n };
            case "skill_delete_preview": return { preview_id: "preview-delete-skill", path: "/sandbox/user/skills/code-review", files: ["SKILL.md"], file_count: 1n, name: "Code Review", scope: "user", next_effective: { item_id: "itm_project_copy", slug: "code-review", scope: "project" }, deployments: ["/sandbox/home/.codex/skills/code-review"] };
            case "skill_delete_apply": return { operation_id: "op-skill-delete", backup_folder: "/sandbox/support/backups/skills/code-review" };
            case "skill_disable_in_repo_preview": return { preview_id: "preview-disable-in-repo", name: "Code Review", scope: "user", override_path: "/sandbox/Projects/AgenTuna/.config-studio/disabled-skills.json", links: ["/sandbox/Projects/AgenTuna/.agents/skills/code-review"], agents: ["Codex", "omp"], backup_location: "/sandbox/support/backups/skills" };
            case "skill_disable_in_repo_apply": return { operation_id: "op-disable-in-repo", backup_location: "/sandbox/support/backups/skills", agents_disabled: 2n };
            case "bulk_skill_apply": return { batch_id: "batch-skill-actions", events: [], operation_id: "op-bulk-skill-actions", files_changed: 2, applied: true, refusal: null };
            case "skill_scope_options": {
              const request = args?.request as { scope?: string; names?: string[] } | undefined;
              const projectSkills = "/sandbox/Projects/AgenTuna/.agents/skills";
              const userSkills = "/sandbox/home/.agents/skills";
              const scopeRoot = request?.scope === "global" ? "/sandbox/global/skills"
                : request?.scope === "project" || request?.scope === "local" ? projectSkills : userSkills;
              const targets = (request?.names ?? []).map((name) => ({
                name,
                path: `${scopeRoot}/${name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "")}`,
              }));
              return {
                scopes: [
                  { scope: "global", store_path: "/sandbox/global", available: true, disabled_reason: null, note: "Shared across all projects.", lock_note: null },
                  { scope: "user", store_path: "/sandbox/user", available: true, disabled_reason: null, note: "Only for this account.", lock_note: null },
                  { scope: "project", store_path: "/sandbox/Projects/AgenTuna/.config-studio", available: true, disabled_reason: null, note: "Committed with this repository.", lock_note: null },
                  { scope: "local", store_path: "/sandbox/Projects/AgenTuna/.agentuna/local", available: true, disabled_reason: null, note: "Local to this repository.", lock_note: null },
                ],
                agents: [
                  { agent: "codex", name: "Codex", installed: true, disabled_reason: null, skills_dir: scopeRoot, targets, shared_with: ["omp"] },
                  { agent: "omp", name: "omp", installed: true, disabled_reason: null, skills_dir: scopeRoot, targets, shared_with: ["codex"] },
                  { agent: "cursor", name: "Cursor", installed: false, disabled_reason: "Cursor is not installed.", skills_dir: null, targets: [], shared_with: [] },
                ],
                clashes: [],
              };
            }
            case "drift_snapshot": return driftSnapshotFixture;
            case "drift_rescan": return driftSnapshotFixture;
            case "drift_watcher_set_paused": {
              const paused = Boolean((args as { paused?: boolean } | undefined)?.paused);
              return { status: paused ? "paused" : "active", path: null, error: null };
            }
            case "drift_preview": {
              const eventId = (args as { eventId?: number } | undefined)?.eventId;
              return { ...driftPreviewFixture, event_id: eventId === undefined ? driftPreviewFixture.event_id : BigInt(eventId) };
            }
            case "drift_adopt": return { operation_id: "op-visual-drift-adopt" };
            case "drift_restore": return { operation_id: "op-visual-drift-restore" };
            case "drift_open_in_editor": return null;
            case "backups_list": return [backupFixture];
            case "backup_preview": return { ...backupPreviewFixture, id: (args as { backupId?: string } | undefined)?.backupId ?? backupFixture.id };
            case "backup_restore": return { backup_id: backupFixture.id, path: backupFixture.original_path, operation_id: "op-visual-backup-restore" };
            case "remote_backup_settings": return remoteBackupSettings;
            case "remote_backup_configure": return remoteBackupSettings;
            case "remote_backup_create_repo": return { job_id: "job_visual_create_repo" };
            case "status": return remoteBackupStatus;
            case "backup_now": return "job_visual_remote_backup";
            case "sync_now": return "job_visual_remote_sync";
            case "restore_commits": return remoteBackupCommits;
            case "restore_confirmation": return { confirmation_token: "one-use-visual-grant", device_name: remoteBackupSettings.device_name, repo_url: remoteBackupSettings.repo_url, commit: args?.commit ?? remoteBackupCommit };
            case "restore": return "job_visual_remote_restore";
            case "conflicts_list": return remoteBackupConflicts;
            case "conflicts_reveal": return null;
            case "conflicts_resolve_preview": return remoteBackupPreview;
            case "conflicts_resolve_apply": return { operation_id: "op-remote-conflict-visual", files_changed: 1 };
            case "skill_library_catalog": return { sources: [], skills: [], target_scope: String(args?.scope ?? "user"), search_root: "/sandbox/user/skills", skipped: [] };
            case "project_sync_status": return [];
            case "project_sync_preview": return { preview_id: "preview-sync-visual", item_id: "itm_visual_skill", name: "Code Review", direction: "push", files: [{ path: "/sandbox/user/skills/code-review/SKILL.md", action: "write", diff: "--- old\n+++ new\n", backup_required: true }], missing_source_paths: [] };
            case "project_sync_apply": return { operation_id: "op-sync-visual", files_changed: 1 };
            case "overview_enable_global_writes": emptyOverview.global_writable = true; return null;
            case "reveal_item": return null;
            case "app_paths": return { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
            case "skill_version_compare": return { item_id: skill.item_id, versions: [] };
            case "skill_edit_preview": return { preview_id: "preview-visual", expected_hash: skill.hash, diff: "", changed: false };
            case "skill_edit_apply": return { item_id: skill.item_id, updated_at: "2026-11-06T10:00:00Z" };
            case "skill_keep_copy_preview": return { preview_id: "preview-visual-keep-copy", files: ["/sandbox/user/skills/code-review/SKILL.md"], diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Code Review\n-Native helper copy.\n+Replaced copy.\n", identical: false };
            case "skill_keep_copy_apply": return { operation_id: "op-visual-keep-copy", expected_hash: "e".repeat(64) };
            case "skill_reveal": return null;
            case "secrets_list": return [];
            case "network_policy": return mocks.policyMode === "error" ? Promise.reject(new Error("mock network policy unavailable")) : mocks.policyMode === "pending" ? new Promise(() => undefined) : policy;
            case "network_clear_events": return { ...policy, events: [] };
            case "network_set_offline": return { ...policy, offline: Boolean((args as { offline?: boolean } | undefined)?.offline) };
            case "deep_link_take": return [];
            case "skill_marketplace_search": return { results: [{ id: "example/visual-skill", name: "Visual Skill", author: "Example", installs: 12, source: "https://example.invalid/visual-skill", description: "A deterministic visual fixture." }], offline: false, stale: false, provenance: "Deterministic marketplace fixture" };
            case "skill_import_git_start": return { job_id: "job_visual_git_import", import_id: "session_visual_git_import" };
            case "skill_import_local_start": return { job_id: "job_visual_local_import", import_id: "session_visual_local_import" };
            case "skill_import_job_result": return { candidates: [{ id: "git-review-checklist", name: "Review checklist", description: "A deterministic import candidate.", markdown: "---\nname: Review checklist\n---\n\n# Review checklist\n\n1. Review the changes.", error: null }, { id: "git-validation-checklist", name: "Validation checklist", description: "A second candidate.", markdown: "---\nname: Validation checklist\n---\n\n# Validation checklist", error: null }, { id: "invalid-import", name: null, description: null, markdown: null, error: "The SKILL.md frontmatter is invalid." }], error: null };
            case "skill_import_release": return true;
            case "skill_auto_update_set": return null;
            case "skill_update_ignore": return null;
            case "skill_source_check": return { status: "current", remote_sha: "a".repeat(40), local_modified: false };
            case "skill_update_preview": return { preview_id: "preview-update-visual", files: [{ path: "/sandbox/user/skills/code-review/SKILL.md", diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1 +1 @@\n-old\n+new\n", backup_required: true }] };
            case "skill_update_apply": return { operation_id: "op-update-visual", files_changed: 1 };
            case "skill_import_choose_folder": return "/sandbox/fixtures/skills";
            case "skill_import_choose_zip": return "/sandbox/fixtures/skills.zip";
            case "mcp_plan_preview": return {
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
            case "mcp_save": return { operation_id: "op-visual-mcp", files_changed: 1 };
            case "mcp_launch_prepare": return { approval_id: "apr_visual_mcp", request_hash: "0".repeat(64), transport: "stdio", command: "/usr/bin/true", args: [], cwd: null, value_names: [], url: null, expires_in_ms: 120000 };
            case "mcp_test_draft": return { ok: true, server_name: "Visual server", server_version: null, transport: "stdio", latency_ms: 1, tools_count: 0, tools: [], error: null, stderr_excerpt: null };
            case "mcp_server_list": return {
              servers: [
                { id: "itm_visual_mcp", slug: "visual-server", name: "Visual server", scope: "global", transport: "stdio", native: false, locked: false, lock_scope: null, lock_reason: null, health: "ok", source_path: "/sandbox/global/mcp.json", agents: [{ agent: "claude_code", state: "linked", config_path: "/sandbox/home/.claude.json", enabled: true }, { agent: "omp", state: "linked", config_path: "/sandbox/home/.omp/mcp.json", enabled: true }] },
                { id: "", slug: null, name: "Native server", scope: "global", transport: "unknown", native: true, locked: false, lock_scope: null, lock_reason: null, health: "unknown", source_path: "/sandbox/home/.codex/config.toml", agents: [{ agent: "codex", state: "native", config_path: "/sandbox/home/.codex/config.toml", enabled: true }] },
              ],
              project_id: "prj_visual_fixture",
              project_name: "AgenTuna",
              project_agents: ["claude_code"],
              global_writable: true,
              global_reason: null,
            };
            case "mcp_server_disable": return { stage: "preview", preview: { change_set_id: "cs-visual-mcp-action", preview_id: "preview-visual-mcp-action", files: [{ agent: "claude_code", scope: "global", path: "/sandbox/home/.claude.json", additions: 0, deletions: 1, diff: "--- a/.claude.json\n+++ b/.claude.json\n" }], skipped_agents: [], warnings: [] } };
            case "mcp_server_delete": return { stage: "preview", preview: { change_set_id: "cs-visual-mcp-delete", preview_id: "preview-visual-mcp-delete", files: [{ agent: "AgenTuna", scope: "global", path: "/sandbox/global/mcp.json", additions: 0, deletions: 2, diff: "" }], skipped_agents: [], warnings: [] } };
            case "mcp_server_test": return { server_name: "Visual server", tools_count: 2, latency_ms: 3, tools: ["ping", "echo"] };
            case "mcp_server_enable": return { stage: "preview", preview: { change_set_id: "cs-visual-mcp-enable", preview_id: "preview-visual-mcp-enable", files: [], skipped_agents: [], warnings: [] } };
            case "mcp_server_copy_preview": return { preview_id: "preview-visual-mcp-copy", name: "Visual server", source_scope: "global", target_scope: "project", source_slug: "visual-server", target_slug: "visual-server", target_item_id: "itm_visual_mcp_copy", replaces_existing: false, backup_location: "/sandbox/support/backups/op-visual-mcp-copy", files: ["/sandbox/Projects/AgenTuna/.config-studio/mcp/visual-server.json", "/sandbox/Projects/AgenTuna/.mcp.json"], agents: [{ agent: "claude_code", path: "/sandbox/Projects/AgenTuna/.mcp.json", effect: "create" }], refusals: [], warnings: [], collision: null, store_init: null };
            case "mcp_server_copy_apply": return { operation_id: "op-visual-mcp-copy", target_item_id: "itm_visual_mcp_copy", target_slug: "visual-server", files_changed: 2, backup_location: "/sandbox/support/backups/op-visual-mcp-copy" };
            case "mcp_server_detail": return {
              item_id: "itm_visual_mcp",
              name: "Visual server",
              scope: "global",
              description: "Visual test fixture MCP server",
              transport: "stdio",
              command: "npx",
              args: ["-y", "@modelcontextprotocol/server-postgres"],
              cwd: null,
              url: null,
              env: [
                { key: "DB_HOST", value: "localhost", is_secret: false, keychain_account: null },
              ],
              defined_in: ["/sandbox/global/mcp.json"],
              tools: ["query", "schema"],
              health_status: "ok",
              health_error_code: null,
              health_error_text: null,
              last_checks: [
                { checked_at: "100", status: "ok", latency_ms: 3, trigger: "manual", error_text: null },
              ],
              cadence_text: "every 5 min",
              is_oauth: false,
              needs_auth: false,
              locked: false,
              locked_by: null,
              lock_reason: null,
              agents: [
                { id: "claude_code", label: "Claude Code", presence: "installed", reason: null, enabled: true, state: "linked", config_path: "/sandbox/home/.claude.json" },
              ],
              why_summary: "Visual server is defined in global scope and active for 1 agent(s).",
            };
            case "skills_list": return {
              rows: [
                {
                  item_id: "itm_visual_skill",
                  name: "code-review",
                  scope: "user",
                  state: "active",
                  shadowed_by: null,
                  description: "Automated review guidance for pull requests",
                  native: false,
                  native_path: null,
                  update_available: false,
                  updated_at: "2026-11-06T12:00:00Z",
                  agents: [
                    { agent: "codex", state: "linked", enabled: true, config_path: "/sandbox/home/.codex/skills/code-review" },
                    { agent: "omp", state: "linked", enabled: true, config_path: "/sandbox/home/.omp/skills/code-review" },
                  ],
                },
                {
                  item_id: "",
                  name: "native-helper",
                  scope: "project",
                  state: null,
                  shadowed_by: null,
                  description: null,
                  native: true,
                  native_path: "/sandbox/project/.agents/skills/native-helper",
                  update_available: false,
                  updated_at: "2026-11-06T12:00:00Z",
                  agents: [
                    { agent: "codex", state: "native", enabled: true, config_path: "/sandbox/project/.agents/skills/native-helper" },
                    { agent: "omp", state: "native", enabled: true, config_path: "/sandbox/project/.agents/skills/native-helper" },
                  ],
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
            case "effective_lock": return { locked: true, reason: String((args?.request as { reason?: string } | undefined)?.reason ?? "") };
            case "effective_unlock": return { locked: false, reason: "Approved version" };
            case "secrets_create": return null;
            case "secrets_rotate": return null;
            case "secrets_delete": return null;
            case "window_state_reset": return null;
            case "logs_reveal": return null;
            case "cli_install": return { link_path: "/sandbox/bin/agentuna", created: true };
            case "cli_uninstall": return { link_path: "/sandbox/bin/agentuna", removed: true, warning: null };
            case "plugin:event|listen": {
              const event = String(args?.event ?? "");
              const ids = eventCallbacks.get(event) ?? new Set<number>();
              if (args?.handler !== undefined) {
                ids.add(args.handler);
                const payload = pendingEvents.get(event);
                if (pendingEvents.has(event)) {
                  callbacks.get(args.handler)?.({ event, id: 1, payload });
                  pendingEvents.delete(event);
                }
              }
              eventCallbacks.set(event, ids);
              return 1;
            }
            case "plugin:event|unlisten": {
              const event = String(args?.event ?? "");
              if (args?.handler !== undefined) eventCallbacks.get(event)?.delete(args.handler);
              return null;
            }
            case "overwrite_move_aside": return { original: args?.path ?? "/sandbox/user/agents/skill", moved_to: "/sandbox/user/agents/skill.agentuna-backup-20261106T100000Z" };
            case "overwrite_undo_move": return { operation_id: "op-visual-move-aside", original: "/sandbox/user/agents/skill", moved_back_from: "/sandbox/user/agents/skill.agentuna-backup-20261106T100000Z" };
            case "adopt_preview": {
              const path = args?.path ?? "/sandbox/user/agents/skill";
              const name = path.split("/").filter(Boolean).pop() ?? "skill";
              return { source: path, store_root: "/sandbox/user", store_destination: `/sandbox/user/skills/${name}`, backup_dir: "/sandbox/user/backups/2026-11-04-1432", slug: name.toLowerCase(), native_name: name, kind: "skill", agents: ["claude_code"], handling: "link", renamed: false, current: "present" };
            }
            case "adopt_apply": {
              const path = args?.path ?? "/sandbox/user/agents/skill";
              const name = path.split("/").filter(Boolean).pop() ?? "skill";
              return { operation_id: "op_visual_adopt", store_destination: `/sandbox/user/skills/${name}`, slug: name.toLowerCase(), handling: "link", summary: `Adopted \`${name}\` as \`${name.toLowerCase()}\` (link).` };
            }
            case "operation_undo": return { undo_operation_id: "op_visual_undo", summary: "Operation was undone." };
            case "logs_tail": return { lines: [], truncated: false };
            case "diagnostics_export": return "cancelled";
            case "projects_choose_folder": return null;
            case "projects_scan": return "job-visual";
            case "jobs_cancel": return null;
            case "cli_install_status": return { installed: false };
            case "diagnostics_report_copy": return "Diagnostics unavailable in browser preview.";
            case "skill_template_preview": {
              const request = args as { name?: string; description?: string; template?: string } | undefined;
              const name = request?.name?.trim() ?? "";
              const slug = name ? name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") : null;
              return { slug, markdown: `# ${name || "New skill"}\n\n${request?.description?.trim() || "Skill instructions."}\n\nTemplate: ${request?.template ?? "blank"}`, name_error: name && !slug ? "Enter a name with letters or numbers." : null, description_error: null };
            }
            case "skill_wizard_transition": {
              const input = args?.wizard as { step: string; source: string | null; details: Record<string, unknown>; name: string; scope: string | null; agents: string[]; completed: boolean } | undefined;
              const event = args?.event as { type: string; payload?: unknown } | undefined;
              const wizard = input ? structuredClone(input) : { step: "source", source: null, details: { repo_url: "", repo_reference: "", library_selections: [], library_mode: "copy", marketplace_id: "", folder_path: "", zip_path: "", description: "", template: null, fetched_repo_selections: [], folder_selections: [], zip_selections: [] }, name: "", scope: null, agents: [], completed: false };
              const effects: Array<Record<string, unknown>> = [];
              if (event?.type === "select_source") wizard.source = String(event.payload);
              if (event?.type === "set_name") wizard.name = String(event.payload);
              if (event?.type === "set_details") wizard.details = event.payload as Record<string, unknown>;
              if (event?.type === "set_scope") wizard.scope = String(event.payload);
              if (event?.type === "set_agents") wizard.agents = event.payload as string[];
              if (event?.type === "next" && wizard.step === "source") wizard.step = "details";
              else if (event?.type === "next" && wizard.step === "details") wizard.step = "scope_agents";
              else if (event?.type === "next" && wizard.step === "scope_agents") wizard.step = "review";
              else if (event?.type === "next" && wizard.step === "review") effects.push({ type: "ready_to_import" });
              if (event?.type === "back" && wizard.step === "details") wizard.step = "source";
              else if (event?.type === "back" && wizard.step === "scope_agents") wizard.step = "details";
              else if (event?.type === "back" && wizard.step === "review") wizard.step = "scope_agents";
              if (event?.type === "import_completed") { wizard.step = "done"; wizard.completed = true; }
              if (event?.type === "cancel" && !wizard.source) effects.push({ type: "discard_draft" });
              return [wizard, effects];
            }
            case "skill_add_preview": {
              const request = args?.request as { stage_requested?: boolean; wizard?: { scope?: string } } | undefined;
              skillAddStageRequested = Boolean(request?.stage_requested);
              const projectScope = request?.wizard?.scope === "project";
              const root = projectScope ? "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill" : "/sandbox/user/skills/visual-skill";
              return { preview_id: "preview-visual-skill-add", import_id: null, items: [], canonical_dir: root, files: [{ path: `${root}/SKILL.md`, package_path: "SKILL.md", diff: null }, { path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", package_path: "SKILL.md", diff: null }], deployments: [{ agent: "omp", path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", mode: "symlink" }], stage_available: projectScope, stage_requested: skillAddStageRequested, requires_overwrite_confirmation: false, store_init: null };
            }
            case "skill_add_apply_start":
              return { job_id: "job_visual_skill_apply", result_id: "result_visual_skill_apply" };
            case "skill_add_apply_result": {
              const root = "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill";
              return { skill_names: ["Visual Skill"], operation_id: "op_visual_skill_add", paths_written: [`${root}/SKILL.md`, "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill"], deployments: [{ agent: "omp", path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", mode: "symlink" }], staged: skillAddStageRequested, staged_paths: skillAddStageRequested ? ["skills/visual-skill/SKILL.md"] : [], stage_error: null, backup_hint: "Operation op_visual_skill_add is recorded. Backups for replaced files, if any, are stored in /sandbox/support/backups and can be restored through operation history and recovery." };
            }
            default: throw new Error(`Unmocked Tauri command: ${command}`);
          }
        },
        transformCallback: (callback: (payload: unknown) => void) => {
          const id = nextCallback++;
          callbacks.set(id, callback);
          return id;
        },
        unregisterCallback: (id: number) => { callbacks.delete(id); },
      };
      Object.defineProperty(window, "__TAURI_INTERNALS__", { value: ipc, configurable: true });
      const NativeDate = Date;
      class FrozenDate extends NativeDate {
        constructor(...args: ConstructorParameters<typeof Date>) { if (args.length === 0) super(fixedNow); else super(...args); }
        static now() { return fixedNow; }
      }
      Object.defineProperty(window, "Date", { value: FrozenDate });
      Object.defineProperty(mocks, "sampleProject", { value: project });
    });
    await use(page);
  },
});

/**
 * Serves a `count`-row `skills_list` snapshot to the app before it boots.
 *
 * The rows are generated inside the page so a 3,000-row fixture costs no IPC
 * payload. Ten per cent are native-only discoveries, which exercises the
 * STATUS column's cross-row lookups; names are unique so every row takes the
 * common path.
 */
export async function mockSkillsScale(page: Page, count: number): Promise<void> {
  await page.addInitScript((total: number) => {
    const host = window as unknown as {
      __AT_TEST_MOCKS__?: { commandOverrides: Record<string, unknown> };
    };
    const mocks = host.__AT_TEST_MOCKS__;
    if (mocks === undefined) {
      throw new Error("mockSkillsScale needs the tauri-mock init script to run first");
    }
    const rows = Array.from({ length: total }, (_, index) => {
      const native = index % 10 === 7;
      const name = `perf-skill-${String(index).padStart(4, "0")}`;
      const scope = index % 2 === 0 ? "user" : "project";
      const path = `/sandbox/${scope}/skills/${name}`;
      return {
        item_id: native ? "" : `itm_perf_${index}`,
        name,
        scope,
        state: native ? null : "active",
        shadowed_by: null,
        description: native ? null : `Fixture skill ${index}`,
        native,
        native_path: native ? `/sandbox/project/.agents/skills/${name}` : null,
        update_available: false,
        source: null,
        source_commit_sha: null,
        source_remote_sha: null,
        source_last_checked_at: null,
        updated_at: "2026-11-06T12:00:00Z",
        agents: native
          ? [
              { agent: "codex", state: "native", enabled: true, config_path: `/sandbox/project/.agents/skills/${name}` },
              { agent: "omp", state: "native", enabled: true, config_path: `/sandbox/project/.agents/skills/${name}` },
            ]
          : [
              { agent: "codex", state: "linked", enabled: true, config_path: path },
              { agent: "omp", state: index % 3 === 0 ? "off" : "linked", enabled: index % 3 !== 0, config_path: path },
            ],
        locked: false,
        lock_scope: null,
        lock_reason: null,
      };
    });
    mocks.commandOverrides["skills_list"] = {
      rows,
      agents: [
        { id: "claude_code", label: "Claude Code", presence: "installed" },
        { id: "codex", label: "Codex", presence: "installed" },
        { id: "omp", label: "omp", presence: "installed" },
      ],
      project_id: "prj_visual_fixture",
      project_name: "AgenTuna",
    };
  }, count);
}

export { expect };
