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
      const mocks = { settings, projects, policyMode: "normal", commandOverrides: {} as Record<string, unknown> };
      Object.defineProperty(window, "__AT_TEST_MOCKS__", { value: mocks });
      const paths = { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
      const policy = { offline: false, marketplace_enabled: false, background_health: "local", updates_opt_in: false, statement: "No automatic network calls.", allowed_hosts: [], actions: [], events: [], capacity: 100 };
      const emptyOverview = { scopes: [{ scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0 }, { scope: "user", skills: 1, mcp: 0, rules: 0, locks: 0, last_edited: "2026-11-06T10:00:00Z", disabled: 0 }, { scope: "project", skills: 0, mcp: 1, rules: 0, locks: 0, last_edited: null, disabled: 0 }], attention: [], attention_total: 0, changes: [], user_path: "/sandbox/user", global_path: "/sandbox/global", global_writable: true, global_reason: null };
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
      const ipc = {
        invoke: async (command: string, args?: { key?: string; value?: unknown; itemId?: string; path?: string; event?: unknown; handler?: number; wizard?: unknown; request?: unknown }) => {
          if (Object.hasOwn(mocks.commandOverrides, command)) return mocks.commandOverrides[command];
          switch (command) {
            case "app_info": return { name: "AgenTuna", version: "0.1.0" };
            case "settings_load": return settings;
            case "settings_set": if (args?.key) settings.values[args.key] = args.value; return null;
            case "projects_list": return mocks.projects;
            case "overview_load": return emptyOverview;
            case "recovery_sheet": return null;
            case "recovery_discard": return null;
            case "recovery_restore": return null;
            case "effective_query": {
              const request = args?.request as { kind?: string } | undefined;
              const effectiveKind = request?.kind ?? "skill";
              const name = effectiveKind === "mcp_server" ? "Postgres" : effectiveKind === "rule" ? "Repository guidance" : "Code Review";
              return { items: [{ logical_id: `logical_visual_${effectiveKind}`, item_id: skill.item_id, kind: effectiveKind, name, winner_scope: "user", source_path: `/sandbox/user/${effectiveKind}/${name.toLowerCase().replaceAll(" ", "-")}`, agents: [{ agent: "claude_code", state: "linked", loads: true }, { agent: "codex", state: "linked", loads: true }], loads_in_agents: 2, lock: null }], disabled_here: [], diagnostics: [], load_diagnostics: [] };
            }
            case "why_trace": return { trace: { item_id: "itm_visual_skill", active_scope: "user", agent: "claude_code", steps: [{ scope: "global", status: "Absent", location: null, reason: "No definition at Global." }, { scope: "user", status: "Winning", location: { path: "/sandbox/user/skills/code-review/SKILL.md", byte_span: null, line: null, rendering_path: null }, reason: "This User copy wins for Claude Code." }, { scope: "project", status: "Absent", location: null, reason: "No Project override." }, { scope: "local", status: "Absent", location: null, reason: "No Local override." }], verdict: { on: true, decisive_step: 1 } }, load_diagnostics: [] };
            case "migrate_scan": return { items: [{ kind: "skill", name: "Visual migration skill", agents: ["claude_code"], path: "/sandbox/home/.claude/skills/visual-migration", scope: "user", content_hash: "a".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }, { kind: "mcp_server", name: "Local docs server", agents: ["codex", "omp"], path: "/sandbox/home/.claude.json", scope: "user", content_hash: "b".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }, { kind: "rule", name: "Project guidance", agents: ["claude_code"], path: "/sandbox/Projects/AgenTuna/CLAUDE.md", scope: "project", content_hash: "c".repeat(64), parse_error: null, store_link: false, ownership: "not_in_agentuna" }], partial: false, skipped_roots: [] };
            case "item_lookup": return { kind: "skill", scope: "user", project_id: null };
            case "skill_detail_load": return skill;
            case "skill_compare_copies": return { winner: { scope: "project", markdown: "# Code Review\n\nProject copy.", hash: "1".repeat(64), deployments: ["claude_code"] }, shadow: { scope: "user", markdown: "# Code Review\n\nUser copy.", hash: "2".repeat(64), deployments: ["codex"] }, identical: false, diff: "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Code Review\n-User copy.\n+Project copy.\n" };
            case "skill_agent_preview": return { preview_id: "preview-enable-visual", enabled: Boolean((args?.request as { enabled?: unknown } | undefined)?.enabled), mode: "copy", paths: ["/sandbox/user/skills/code-review"], coupled_agents: [], warnings: [], no_op: false };
            case "skill_agent_apply": return { operation_id: "op-enable-visual", changed: true, paths_written: ["/sandbox/user/skills/code-review"], backup_hint: null };
            case "remote_backup_settings": return { device_name: "Visual workstation", repo_url: "https://example.invalid/agentuna-backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: true };
            case "remote_backup_configure": return { device_name: "Visual workstation", repo_url: "https://example.invalid/agentuna-backup.git", branch: "main", last_push_at: null, last_pull_at: null, last_commit: null, status: "idle", error_kind: null, token_saved: true };
            case "skill_library_catalog": return { sources: [], skills: [], target_scope: String(args?.scope ?? "user") };
            case "project_sync_status": return [];
            case "project_sync_preview": return { preview_id: "preview-sync-visual", item_id: "itm_visual_skill", name: "Code Review", direction: "push", files: [{ path: "/sandbox/user/skills/code-review/SKILL.md", action: "write", diff: "--- old\n+++ new\n", backup_required: true }], missing_source_paths: [] };
            case "project_sync_apply": return { operation_id: "op-sync-visual", files_changed: 1 };
            case "overview_enable_global_writes": emptyOverview.global_writable = true; return null;
            case "app_paths": return { support_dir: "/sandbox/support", db_path: "/sandbox/support/index.sqlite", log_dir: "/sandbox/support/logs", backup_dir: "/sandbox/support/backups", cache_dir: "/sandbox/support/cache", user_store: "/sandbox/user", project_store: "/sandbox/Projects/AgenTuna/.config-studio", global_store: "/sandbox/global", global_store_source: "default" };
            case "remote_backup_push": return null;
            case "remote_backup_pull": return null;
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
            case "overwrite_move_aside": return { original: args?.path ?? "/sandbox/user/agents/skill", moved_to: "/sandbox/user/agents/skill.agentuna-backup-20261106T100000Z" };
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
