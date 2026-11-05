# Technical stories

269 technical stories, 1,349 points, for the Tauri + React/TypeScript + Rust build of AgenTuna. Also in `design/agentuna-backlog.xlsx`, sheet **Tech Stories**.

| File | Slice | Stories | Points |
|---|---|---|---|
| `TF-foundation.jsonl` | Workspace, domain types, SQLite schema, safe write pipeline, packaging, CI, safety (E06) | 54 | 218 |
| `TA-scope-agents.jsonl` | Agent adapters, scope engine, effective config, instructions, agents (E01, E08, E09) | 58 | 273 |
| `TS-skills.jsonl` | Skills, add-skill flow, presets, updates, marketplace (E02, E04, E11, E14) | 46 | 240 |
| `TM-mcp.jsonl` | MCP servers, health checks, Keychain secrets (E03) | 33 | 181 |
| `TC-store-migrate.jsonl` | AgenTuna store, migration, sync & drift, backups, remote backup (E05, E07, E13) | 40 | 234 |
| `TU-shell-cli.jsonl` | Design-system components, shell, navigation, overlays, project picker, palette, CLI (E10, E12) | 38 | 203 |

- `CONTRACT.md`: crate names, SQLite tables, story format, and the 19 binding integration decisions made during review.
- Inputs: `docs/backlog/backlog.json` (epics, product stories), `docs/backlog/features.md` (every screen, overlay and behaviour, F01–F16, O01–O14, X01–X11).

## Checks that pass
- Every product story S-001..S-065 and every feature ID is referenced by at least one technical story.
- All `depends_on` IDs exist; the dependency graph has no cycles.
- Every story has 2–5 acceptance items, a legal layer and points in {1,2,3,5,8,13}.
- Two independent review rounds (gap audit, consistency audit); all findings resolved.
