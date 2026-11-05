# Technical stories: shared contract

Every subagent writing technical stories MUST follow this file. Do not invent other crate, module or table names; if one is missing, use the closest listed name and note it in `notes`.

## Stack (decided)
- Rust workspace; Tauri 2 desktop shell (system WebView, WKWebView on macOS) with a Vite + React + TypeScript frontend in `ui/`. macOS first, Windows/Linux later. See decision 20.
- tokio runtime owned by the app; background results reach the UI as Tauri events and command results; the webview never touches the filesystem.
- Core crates (decided by research): keyring 4.x (Keychain), notify 8.x + notify-debouncer-full (watching), toml_edit (TOML, format-preserving), jsonc-parser CST/span patching (JSON, format-preserving; never re-serialize a whole agent file), atomic-write-file + optimistic concurrency (hash/mtime recheck before rename), fs4 (advisory locks on AgenTuna's own files), std symlink / junction (Windows), rmcp (MCP client, child-process + streamable HTTP), saphyr (SKILL.md frontmatter), similar (diffs), shell out to `git` for clone/fetch, Tauri 2 menu API (muda) for the native macOS menu bar, rusqlite (bundled SQLite) + refinery or rusqlite_migration (schema migrations), tracing (logs), directories (paths), self_update or custom updater with signature check. Frontend defaults (decision 20): TanStack Table + TanStack Virtual (tables), react-markdown with a sanitizer (markdown preview), CodeMirror 6 with @codemirror/merge (editor, highlighting, diff), generated TS types from Rust.

## Workspace crates (use these names exactly)
| Crate | Role |
|---|---|
| `at-model` | Plain domain types: Scope, ItemKind (Skill, McpServer, Rule), ItemId, Agent, AgentId, Deployment, ItemState, SyncLabel, Lock, Preset. No IO. |
| `at-fs` | Safe write pipeline: diff preview, backup, atomic write, optimistic concurrency, refuse-to-overwrite, symlink/copy, path resolution, file watcher. |
| `at-adapters` | One adapter per agent (claude_code, codex, omp, cursor, custom): detect install, list native items, read/write native formats, render deployments. Trait `AgentAdapter`. |
| `at-store` | AgenTuna store (~/.agentuna, ./.agentuna, Global path): canonical skills/, mcp.json, rules/; load/save; ID allocation. |
| `at-engine` | Scope resolution (precedence, locks, shadowing, disable-inherited), effective config, why-trace, planner that turns intents into a `ChangeSet` of file operations, apply/undo. |
| `at-db` | SQLite index + history (see schema below). Rebuildable from files except history/backups catalogue. |
| `at-mcp` | MCP health checks (rmcp), tool listing, OAuth sign-in for HTTP servers, scheduler. |
| `at-secrets` | Keychain read/write, ${VAR} reference resolution, secret inventory. |
| `at-git` | git CLI wrapper: clone/fetch to temp, list skills in repo, source tracking, update checks, remote backup repo. |
| `at-migrate` | Scan native items, conflict detection, migration plan, apply, undo. |
| `at-cli` | `agentuna` binary, `--json` output, same engine. |
| `at-app` | Tauri 2 shell (Rust): `app::commands` (typed commands over the engine), `app::bridge` (tokio jobs, cancellation, progress events), `app::state`, `app::menu`. Frontend in `ui/` (Vite + React + TypeScript): `shell`, `theme`, `components`, `screens/{overview,skills,skill_detail,mcp_list,mcp_detail,mcp_form,effective,drift,instructions,agents,add_skill,migrate,settings}`, `overlays/{palette,project_picker,menus,dialogs,toasts}`, `state`, `bridge`. |

## SQLite (`at-db`) tables (use these names; add columns freely)
`schema_version`, `projects`, `agents`, `agent_paths`, `items` (canonical items in AgenTuna store), `item_versions` (content hash per scope copy), `native_items` (found only in agent folders), `deployments` (item × agent × scope state), `locks`, `health_checks`, `operations` (audit log / history / recent changes), `operation_files` (files touched per operation), `backups`, `drift_events`, `sources` (git/marketplace/library origin per item), `presets`, `preset_members`, `preset_applications`, `settings`, `secret_refs` (names only, never values), `remote_backup_state`.

## Technical story format
Write JSON Lines (one object per line) to the file you are assigned. Fields:
```json
{"id":"TX-001","epic":"E03","stories":["S-014","S-015"],"features":["F07","X06"],"layer":"Core","title":"…","description":"…","acceptance":["…","…"],"depends_on":["TF-004"],"points":5,"crates":["at-mcp"],"notes":""}
```
- `layer` is exactly one of: `Core`, `Adapters`, `DB`, `MCP`, `Secrets`, `Git`, `UI`, `CLI`, `Platform`, `Test`.
- `points` is one of 1, 2, 3, 5, 8, 13. Split anything bigger than 13.
- `stories` lists the product story IDs (S-001…S-065 from docs/backlog/backlog.json) the technical story serves; may be empty only for pure foundation work.
- `features` lists feature IDs from docs/backlog/features.md it serves.
- `acceptance`: 2-5 concrete, testable statements (inputs, outputs, files, edge cases). No vague "works well".
- `depends_on` may reference IDs from other slices using their prefixes (see below). Only reference IDs that plausibly exist; the integrator will check.
- One story = one deliverable a developer can finish in ≤ ~1 week. Separate layers into separate stories (e.g. engine logic vs UI screen vs DB table) rather than one mixed story.
- UI stories name the `ui/` module (or `at-app` command) and the screen states (empty, loading, error, populated) they must render.

## ID prefixes per slice
| Prefix | Slice |
|---|---|
| TF | Foundation, platform, DB, safe write pipeline, packaging, CI, test harness |
| TA | Agent adapters, scope engine, effective config, instructions, agents screen |
| TS | Skills, add-skill flow, presets, updates, marketplace |
| TM | MCP servers, health, secrets |
| TC | Central store, migration, sync & drift, backups, remote backup |
| TU | UI shell, design system components, navigation, overlays, command palette, project picker, CLI, accessibility, menu bar |

Foundation story IDs other slices may depend on (TF slice must create these exact IDs and titles):
- TF-001 Cargo workspace and crate skeletons
- TF-002 at-model domain types
- TF-003 at-db SQLite connection, migrations runner, schema v1
- TF-004 at-fs safe write pipeline (diff, backup, atomic write, concurrency check)
- TF-005 at-fs refuse-to-overwrite guard
- TF-006 at-fs symlink/copy deployment primitive
- TF-007 at-fs file watcher service with debounce
- TF-008 app async bridge (tokio runtime, job handles, Tauri events)
- TF-009 operations log (audit/history) write API
- TF-010 test harness: temp HOME fixture with fake agent folders

UI slice must create: TU-001 theme tokens (CSS variables, Tailwind CSS 4), TU-002 component library (button, badge, chip, toggle, checkbox, input, segmented, radio card, status dot), TU-003 table component (virtualized, sortable, per-agent cells), TU-004 modal/popover/menu framework, TU-005 toast service, TU-006 app shell (sidebar, top bar, routing), TU-007 diff viewer component, TU-008 code/markdown editor component.

## Integration decisions (binding; added after cross-slice review)
1. **ItemId** = `itm_` + UUIDv7, allocated once, survives rename and scope move. Owned by TF-002. DB `items` PK is `id`; `kind`, `scope`, `project_id`, `slug` are columns with `UNIQUE(kind, scope, project_id, slug)`.
2. **Global store path** owned by TF-022 `AppPaths`: default `/Library/Application Support/AgenTuna/global`, override env `AGENTUNA_GLOBAL_STORE`, then settings key `global_store_path`. All other stories read it from AppPaths.
3. **Project store layout** (`<project>/.agentuna/`): `store.json` (the only item manifest: `{schema_version, items:[{id, kind, slug, path, hash}]}`), `skills/`, `mcp.json` (`{schema_version, servers:{<slug>: def}}`, content only), `rules/`, `local/` = Local scope content (gitignored), `.state/` = journals, locks, staging, tmp (gitignored), `backups/` (gitignored). User store `~/.agentuna/` has the same layout minus `local/`. Every versioned file uses the key `schema_version`.
4. **Local scope** is a real, writable fourth scope for all kinds, stored in `.agentuna/local/` and rendered to agent-local files (`.claude/settings.local.json`, `CLAUDE.local.md`). Add/edit flows show Global, User, Project plus a collapsed "Local (only you, this repo)" option.
5. **Write pipeline**: `WriteRequest` gains `follow_symlink: bool` (default false). Only agent config writers (e.g. TM-005 for `~/.claude.json`) may set it. Multi-file writes use a `ChangeSet` of `FileOp`s (types in TF-052, at-model) applied by TF-053 (at-fs journaled multi-file apply with backups + rollback). Migration (TC-009) builds on TF-053.
6. **Backups** owned by TC-014: layout `backups/<utc-ts>-<operation_id>/<scope>/<relpath>` with a `.json` sidecar; catalogue table `backups` column `content_hash`. TF-004 writes into the operation folder supplied by the caller. Retention: a backup is pruned only when it is older than `max(30, backup_retention_days)` days AND is not among the newest 3 for its original path.
7. **Undo**: TC-010 is generic `undo_operation(op_id)` for every operation kind, restoring from `operation_files` + backups, window fixed at 30 days (constant `UNDO_WINDOW_DAYS` in TF-023), with migrate-specific link cleanup.
8. **DDL ownership**: only TF stories create or alter tables. Domain stories own repositories and queries and depend on the TF table story. Extra tables: `secret_ref_uses(secret_name, item_id, scope)`, `preset_application_members(application_id, item_id, prior_enabled)`.
9. **Settings** owned by TF-023 with these keys: `health_interval_minutes` (0=off, 5, 15, 60; default 5), `background_health` (`off`|`local`|`all`; default `local`, meaning scheduled checks only for stdio servers already tested once; HTTP checks need `all`), `backup_retention_days` (min 30), `default_link_mode` (`link`|`copy`), `link_mode.<agent>`, `native_banner_hidden.<kind>`, `global_store_path`, `network.marketplace_enabled`. TU-020 is removed; Preferences (TU-019) edits TF-023.
10. **Network policy** owned by TF-036 (`NetworkAction` registry in at-model). Scheduled HTTP health checks are a registered action gated by `background_health=all`. TU-043 only renders policy state. Engine stories depend on TF-036, never on TU stories.
11. **Scope switcher**: TU-040 is the widget (rendering, a11y, snapshots). TA-053 owns scope state, routing and persistence and depends on TU-040. Shortcuts: Cmd+1..7 screens (TU-016), Cmd+Shift+1/2/3/4 scopes Global/User/Project/Local.
12. **SyncLabel** computed only in TA-005; TC-015 consumes it. **Adopt** owned by TC-011; TF-042 keeps refusal remedies and `move_aside`. Native discovery capability TS-026 is a dependency of TC-006; links into the AgenTuna store are classified Managed and excluded from native rows.
13. **Rule files**: TA-011 owns managed-block rendering; TA-012 saves through TA-011 and refuses to write inside AgenTuna markers by hand.
14. Every screen/overlay UI story depends on TU-006 (shell) and TU-009 (app state).
15. **Migrations**: TF-003 migration v1 creates only `schema_version` and the runner. TF-011..TF-018 each own one numbered migration (v2..v9) that creates its tables with every column other slices read. No other story writes DDL.
16. **Disable tombstones** live in `store.json` as `disabled: [{item_id, kind, agents: [..] | null}]` per scope, owned by TC-002; TA-003, TM-004 and TS-011 read/write that key.
17. **Local skills** deploy into the project agent skills folder (e.g. `.claude/skills/<slug>`), and AgenTuna adds each such path to `.git/info/exclude` so it is never committed; adapters own the per-agent Local skills path.
18. **Settings keys added** to TF-023: `auto_update_skills`, `last_scope.<project_id>`, `last_project_id`. Per-agent link mode key is `link_mode.<agent>` only.
19. **Backup pruning** implemented and tested only in TF-050; TC-014 owns layout and catalogue, not pruning.
20. **UI stack: Tauri 2 + React/TypeScript** (user decision, 2026-11-05, chosen for fidelity to the FigCraft screens). `at-app` is the Tauri Rust backend; the frontend lives in `ui/`. The webview is presentation only (see AGENTS.md "Tauri and frontend implementation rules"); every action is a typed Tauri command over the same engine intents as the CLI. Frontend toolchain: pnpm, Vite, React 19, TypeScript 7, Tailwind CSS 4 with Radix UI primitives (shadcn/ui pattern), Vitest + Testing Library for unit tests, Playwright for visual and keyboard tests, axe-core for accessibility checks. Rust uses edition 2024. Core crates are unaffected.
