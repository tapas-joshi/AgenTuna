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
      const emptyOverview = { scopes: [{ scope: "global", skills: 0, mcp: 0, rules: 0, locks: 0, last_edited: null, disabled: 0 }, { scope: "user", skills: 1, mcp: 0, rules: 0, locks: 0, last_edited: "2026-11-06T10:00:00Z", disabled: 0 }, { scope: "project", skills: 0, mcp: 1, rules: 0, locks: 0, last_edited: null, disabled: 0 }], attention: [], attention_total: 0, changes: [] };
      const skill = { item_id: "itm_visual_skill", name: "Code Review", slug: "code-review", scope: "user", path: "/sandbox/user/skills/code-review", markdown: "# Code Review\n\nA deterministic visual fixture.", description: "Review code changes.", hash: "fixture-hash", updated_at: "2026-11-06T10:00:00Z", source: null, mode: "copy", shadowed_by: null, shadow_chain: ["user"], deployments: [], history: [], editable: true };
      const callbacks = new Map<number, (payload: unknown) => void>();
      const eventCallbacks = new Map<string, Set<number>>();
      let nextCallback = 1;
      Object.defineProperty(mocks, "emit", { value: (event: string, payload: unknown) => {
        for (const callbackId of eventCallbacks.get(event) ?? []) callbacks.get(callbackId)?.({ event, id: 1, payload });
      } });
      const ipc = {
        invoke: async (command: string, args?: { key?: string; value?: unknown; itemId?: string; path?: string; event?: string; handler?: number }) => {
          if (Object.hasOwn(mocks.commandOverrides, command)) return mocks.commandOverrides[command];
          switch (command) {
            case "app_info": return { name: "AgenTuna", version: "0.1.0" };
            case "settings_load": return settings;
            case "settings_set": if (args?.key) settings.values[args.key] = args.value; return null;
            case "projects_list": return mocks.projects;
            case "overview_load": return emptyOverview;
            case "item_lookup": return { kind: "skill", scope: "user", project_id: null };
            case "skill_detail_load": return skill;
            case "skill_compare_copies": return { item_id: skill.item_id, copies: [] };
            case "skill_version_compare": return { item_id: skill.item_id, versions: [] };
            case "skill_edit_preview": return { preview_id: "preview-visual", expected_hash: skill.hash, diff: "", changed: false };
            case "skill_edit_apply": return { item_id: skill.item_id, updated_at: "2026-11-06T10:00:00Z" };
            case "skill_reveal": return null;
            case "secrets_list": return [];
            case "network_policy": return mocks.policyMode === "error" ? Promise.reject(new Error("mock network policy unavailable")) : mocks.policyMode === "pending" ? new Promise(() => undefined) : policy;
            case "app_paths": return paths;
            case "recovery_sheet": return null;
            case "plugin:event|listen": {
              const ids = eventCallbacks.get(args?.event ?? "") ?? new Set<number>();
              if (args?.handler !== undefined) ids.add(args.handler);
              eventCallbacks.set(args?.event ?? "", ids);
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
            case "skill_wizard_transition": return [{ step: "source" }, []];
            default: return null;
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
