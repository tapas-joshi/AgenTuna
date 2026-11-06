import { test as base, expect } from "@playwright/test";

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
      const paths = { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
      const policy = { offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false, statement: "No automatic network calls.", allowed_hosts: [], actions: [], events: [], capacity: 100 };
      const emptyOverview = {
        scopes: [
          { scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0 },
          { scope: "user", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0 },
          { scope: "project", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0 },
        ],
        attention: [],
        attention_total: 0,
        changes: [],
        user_path: "/sandbox/user",
        global_path: "/sandbox/global",
        global_writable: true,
        global_reason: null,
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
      const migratePlanFixture = { plan_id: "itm_visual_plan", plan_hash: "d".repeat(64), scope: "project", strategy: "replace_with_links", counts: { items: 3, files_created: 6, files_backed_up: 2, links_written: 2, files_left_in_place: 0 }, steps: [{ seq: 1, action: "backuporiginal", item_name: "Visual migration skill", source: "/sandbox/home/.claude/skills/visual-migration", destination: null, backup_path: "/sandbox/support/backups/migration/visual-migration", bytes: 128, agents: [] }, { seq: 2, action: "createstoreitem", item_name: "Visual migration skill", source: "/sandbox/home/.claude/skills/visual-migration", destination: "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-migration", backup_path: null, bytes: 128, agents: ["claude_code"] }], files_checked: 2 };
      const ipc = {
        invoke: async (command: string, args?: { key?: string; value?: unknown; itemId?: string; path?: string; event?: unknown; handler?: number; wizard?: unknown; request?: unknown; jobId?: string; commit?: string; confirmationToken?: string; groupId?: string; relativePath?: string; side?: string; resolution?: string; previewId?: string; confirmDeletions?: boolean }) => {
          mocks.calls.push(command);
          if (Object.hasOwn(mocks.commandOverrides, command)) return mocks.commandOverrides[command];
          switch (command) {
            case "app_info": return { name: "AgenTuna", version: "0.1.0" };
            case "settings_load": return settings;
            case "settings_set": if (args?.key) settings.values[args.key] = args.value; return null;
            case "instructions_list": return [{ item_id: "itm_visual_rule", scope: "project", path: "/sandbox/Projects/AgenTuna/AGENTS.md", managed: true, content: "# Repository guidance\n\nReview changes carefully.\n", hash: "c".repeat(64), agents: ["codex", "omp"], read_error: null }, { item_id: "itm_visual_user_rule", scope: "user", path: "/sandbox/home/.claude/CLAUDE.md", managed: false, content: "# Personal preferences\n\nPrefer small PRs.\n", hash: "e".repeat(64), agents: ["claude_code"], read_error: null }];
            case "instruction_save": return { status: "saved", conflict: null, backup_path: "/sandbox/support/backups/AGENTS.md", hash: "f".repeat(64), unreadable_path: null };
            case "agents_list": return agentCards;
            case "agents_rescan": return agentCards;
            case "agent_manage_set": return null;
            case "agent_link_mode_set": return null;
            case "agent_path_choose": return "/sandbox/Applications/Cursor.app";
            case "agent_path_set": return { ...agentCards[1], presence: "configured_only", primary_path: "/sandbox/Applications/Cursor.app", health: "/sandbox/Applications/Cursor.app · 0 / 0 managed · 0 in sync" };
            case "presets_list": return [presetFixture];
            case "preset_member_candidates": return [{ item_id: "itm_visual_candidate", name: "Release notes", kind: "skill", scope: "user" }];
            case "preset_create": return "preset_visual_new";
            case "preset_rename": return null;
            case "preset_delete": return null;
            case "preset_member_set": return null;
            case "preset_apply": return { operation_id: "op-preset-visual", changed: true, changed_members: 1, enabled: 3, total: 3, complete: true, warnings: [] };
            case "preset_unapply": return { operation_id: "op-preset-unapply", changed: true, changed_members: 1, enabled: 1, total: 3, complete: false, warnings: [] };
            case "migrate_conflicts": return { conflicts: [{ kind: "skill", name: "review-helper", copies: [{ agent: "claude_code", scope: "user", path: "/sandbox/home/.claude/skills/review-helper", raw_hash: "a".repeat(64), normalized_hash: "a".repeat(64) }, { agent: "codex", scope: "user", path: "/sandbox/home/.codex/skills/review-helper", raw_hash: "b".repeat(64), normalized_hash: "b".repeat(64) }], formatting_only: false, unified_diff: "--- A\n+++ B\n", proposed_resolution: "needs_choice" }] };
            case "projects_list": return mocks.projects;
            case "overview_load": return emptyOverview;
            case "recovery_sheet": return null;
            case "recovery_discard": return null;
            case "recovery_restore": return null;
            case "effective_counts": return { skills: 5, mcp_servers: 2, rules: 1 };
            case "effective_query": {
              const request = args?.request as { kind?: string } | undefined;
              const effectiveKind = request?.kind ?? "skill";
              const name = effectiveKind === "mcp_server" ? "Postgres" : effectiveKind === "rule" ? "Repository guidance" : "Code Review";
              return { items: [{ logical_id: `logical_visual_${effectiveKind}`, item_id: skill.item_id, kind: effectiveKind, name, winner_scope: "user", source_path: `/sandbox/user/${effectiveKind}/${name.toLowerCase().replaceAll(" ", "-")}`, agents: [{ agent: "claude_code", state: "linked", loads: true }, { agent: "codex", state: "linked", loads: true }], loads_in_agents: 2, lock: null }], disabled_here: [], diagnostics: [], load_diagnostics: [] };
            }
            case "why_trace": return { trace: { item_id: "itm_visual_skill", active_scope: "user", agent: "claude_code", steps: [{ scope: "global", status: "Absent", location: null, reason: "No definition at Global." }, { scope: "user", status: "Winning", location: { path: "/sandbox/user/skills/code-review/SKILL.md", byte_span: null, line: null, rendering_path: null }, reason: "This User copy wins for Claude Code." }, { scope: "project", status: "Absent", location: null, reason: "No Project override." }, { scope: "local", status: "Absent", location: null, reason: "No Local override." }], verdict: { on: true, decisive_step: 1 } }, load_diagnostics: [] };
            case "migrate_scan": return { items: [{ kind: "skill", name: "Visual migration skill", agents: ["claude_code"], path: "/sandbox/home/.claude/skills/visual-migration", scope: "user", content_hash: "a".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }, { kind: "mcp_server", name: "Local docs server", agents: ["codex", "omp"], path: "/sandbox/home/.claude.json", scope: "user", content_hash: "b".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }, { kind: "rule", name: "Project guidance", agents: ["claude_code"], path: "/sandbox/Projects/AgenTuna/CLAUDE.md", scope: "project", content_hash: "c".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }], partial: false, skipped_roots: [], roots: [{ path: "/sandbox/home/.claude/skills", agents: ["claude_code"], new_items: 1, in_sync: 0 }, { path: "/sandbox/home/.claude.json", agents: ["codex", "omp"], new_items: 1, in_sync: 0 }, { path: "/sandbox/Projects/AgenTuna/CLAUDE.md", agents: ["claude_code"], new_items: 1, in_sync: 0 }] };
            case "migrate_plan": return migratePlanFixture;
            case "item_lookup": return { kind: "skill", scope: "user", project_id: null };
            case "skill_detail_load": return skill;
            case "skill_compare_copies": return { winner: { scope: "project", markdown: "# Code Review\n\nProject copy.", hash: "1".repeat(64), deployments: ["claude_code"] }, shadow: { scope: "user", markdown: "# Code Review\n\nUser copy.", hash: "2".repeat(64), deployments: ["codex"] }, identical: false, diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Code Review\n-User copy.\n+Project copy.\n" };
            case "skill_agent_preview": return { preview_id: "preview-enable-visual", enabled: Boolean((args?.request as { enabled?: unknown } | undefined)?.enabled), mode: "copy", paths: ["/sandbox/user/skills/code-review"], coupled_agents: [], warnings: [], no_op: false };
            case "skill_agent_apply": return { operation_id: "op-enable-visual", enabled: true, changed: true };
            case "remote_backup_settings": return remoteBackupSettings;
            case "remote_backup_configure": return remoteBackupSettings;
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
            case "skill_library_catalog": return { sources: [], skills: [], target_scope: String(args?.scope ?? "user") };
            case "project_sync_status": return [];
            case "project_sync_preview": return { preview_id: "preview-sync-visual", item_id: "itm_visual_skill", name: "Code Review", direction: "push", files: [{ path: "/sandbox/user/skills/code-review/SKILL.md", action: "write", diff: "--- old\n+++ new\n", backup_required: true }], missing_source_paths: [] };
            case "project_sync_apply": return { operation_id: "op-sync-visual", files_changed: 1 };
            case "overview_enable_global_writes": emptyOverview.global_writable = true; return null;
            case "reveal_item": return null;
            case "app_paths": return { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
            case "skill_version_compare": return { item_id: skill.item_id, versions: [] };
            case "skill_edit_preview": return { preview_id: "preview-visual", expected_hash: skill.hash, diff: "", changed: false };
            case "skill_edit_apply": return { item_id: skill.item_id, updated_at: "2026-11-06T10:00:00Z" };
            case "skill_reveal": return null;
            case "secrets_list": return [];
            case "network_policy": return mocks.policyMode === "error" ? Promise.reject(new Error("mock network policy unavailable")) : mocks.policyMode === "pending" ? new Promise(() => undefined) : policy;
            case "skill_marketplace_search": return { results: [{ id: "example/visual-skill", name: "Visual Skill", author: "Example", installs: 12, source: "https://example.invalid/visual-skill", description: "A deterministic visual fixture." }], offline: false, stale: false, provenance: "Deterministic marketplace fixture" };
            case "skill_import_git_candidates": return { candidates: [{ id: "skills/review-checklist", name: "Review checklist", description: "A deterministic Git import candidate.", markdown: "---\nname: Review checklist\ndescription: A deterministic Git import candidate.\n---\n\n# Review checklist\n\n1. Review the changes.", error: null }], error: null };
            case "skill_auto_update_set": return null;
            case "skill_update_ignore": return null;
            case "skill_source_check": return { status: "current", remote_sha: "a".repeat(40), local_modified: false };
            case "skill_update_preview": return { preview_id: "preview-update-visual", files: [{ path: "/sandbox/user/skills/code-review/SKILL.md", diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1 +1 @@\n-old\n+new\n", backup_required: true }] };
            case "skill_update_apply": return { operation_id: "op-update-visual", files_changed: 1 };
            case "skill_import_choose_folder": return null;
            case "skill_import_choose_zip": return null;
            case "skill_import_candidates": return { candidates: [{ id: "visual-folder-skill", name: "Imported visual skill", description: "A deterministic local import candidate.", markdown: "# Imported visual skill\n\nFixture content.", error: null }], error: null };
            case "mcp_plan_preview": return { change_set_id: "cs-visual-mcp", preview_id: "preview-visual-mcp", files: [] };
            case "mcp_save": return { operation_id: "op-visual-mcp", files_changed: 1 };
            case "mcp_test_connection": return { ok: true, server_name: "Visual server", server_version: null, transport: "stdio", latency_ms: 1, tools_count: 0, tools: [], error: null, stderr_excerpt: null };
            case "mcp_disable_in_repo": return null;
            case "mcp_delete": return null;
            case "mcp_server_list": return {
              servers: [
                { id: "itm_visual_mcp", slug: "visual-server", name: "Visual server", scope: "global", transport: "stdio", native: false, locked: false, lock_scope: null, lock_reason: null, health: "ok", source_path: "/sandbox/global/mcp.json", agents: [{ agent: "claude_code", state: "linked", config_path: "/sandbox/home/.claude.json", enabled: true }] },
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
            case "effective_lock": return { locked: true, reason: String((args?.request as { reason?: string } | undefined)?.reason ?? "") };
            case "effective_unlock": return { locked: false, reason: "Approved version" };
            case "secrets_create": return null;
            case "secrets_rotate": return null;
            case "secrets_delete": return null;
            case "window_state_reset": return null;
            case "logs_reveal": return null;
            case "cli_install": return { link_path: "/sandbox/bin/agentuna", created: true };
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
            case "adopt_preview": {
              const path = args?.path ?? "/sandbox/user/agents/skill";
              const name = path.split("/").filter(Boolean).pop() ?? "skill";
              return { source: path, store_root: "/sandbox/user", store_destination: `/sandbox/user/skills/${name}`, slug: name.toLowerCase(), native_name: name, kind: "skill", agents: ["claude_code"], handling: "link", renamed: false, current: "present" };
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
              const wizard = input ? structuredClone(input) : { step: "source", source: null, details: { repo_url: "", repo_reference: "", library_selection: null, marketplace_id: "", folder_path: "", zip_path: "", template: null, fetched_repo_selection: null }, name: "", scope: null, agents: [], completed: false };
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
              if (event?.type === "import_completed") { wizard.step = "done"; wizard.completed = true; }
              if (event?.type === "cancel" && !wizard.source) effects.push({ type: "discard_draft" });
              return [wizard, effects];
            }
            case "skill_add_preview": {
              const request = args?.request as { stage_requested?: boolean; wizard?: { scope?: string } } | undefined;
              skillAddStageRequested = Boolean(request?.stage_requested);
              const projectScope = request?.wizard?.scope === "project";
              const root = projectScope ? "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill" : "/sandbox/user/skills/visual-skill";
              return { preview_id: "preview-visual-skill-add", canonical_dir: root, files: [{ path: `${root}/SKILL.md`, package_path: "SKILL.md", diff: null }, { path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", package_path: "SKILL.md", diff: null }], deployments: [{ agent: "omp", path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", mode: "symlink" }], stage_available: projectScope, stage_requested: skillAddStageRequested, requires_overwrite_confirmation: false };
            }
            case "skill_add_apply": {
              const root = "/sandbox/Projects/AgenTuna/.config-studio/skills/visual-skill";
              return { skill_name: "Visual Skill", operation_id: "op_visual_skill_add", paths_written: [`${root}/SKILL.md`, "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill"], deployments: [{ agent: "omp", path: "/sandbox/Projects/AgenTuna/.agents/skills/visual-skill", mode: "symlink" }], staged: skillAddStageRequested, staged_paths: skillAddStageRequested ? ["skills/visual-skill/SKILL.md"] : [], stage_error: null, backup_hint: "Operation op_visual_skill_add is recorded. Backups for replaced files, if any, are stored in /sandbox/support/backups and can be restored through operation history and recovery." };
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

export { expect };
