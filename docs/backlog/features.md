# Feature inventory (every item must be covered by at least one technical story)

## Screens (FigCraft design file design/agentuna-screens.figcraft, page "App flow (prototype)")
F01 App shell: sidebar nav (Overview, Skills, MCP Servers, Instructions, Agents, Effective config, Sync & drift), logo, agents-on-this-Mac block, account block, top bar
F02 Scope switcher (Global / User / Project) in top bar, each scope has its own Overview
F03 Overview (Project, User, Global variants): scope cards with counts/locks/last edited, effective summary bar with per-scope proportions, Needs attention list (failing, shadowed, drift, missing agent), Recent changes timeline
F04 Skills list: filter chips (All/Global/User/Project/Local/Not in AgenTuna/Shadowed), filter box, table with scope badge, per-agent cells (managed / off / native with source path / locked / shadowed), ownership and deployment states, row menu, Adopt and Resolve buttons, "not in AgenTuna" banner with Review & migrate and Hide. Indicate shared discovery roots and explain coupled agent effects before a reviewed change; never present fictitious independent switches.
F05 Skills multi-select: checkboxes, select all, bulk bar (Promote, Enable for agent, Move to scope, Disable, clear)
F06 Skill detail: SKILL.md view, Edit, Promote, Delete, enabled-for toggles per agent, shadow note with Compare copies / Keep User copy only, history
F07 MCP Servers list: scope/ownership chips, search, per-server/per-agent matrix, transport, source paths, health, native rows, Adopt/Resolve, banner and row menu. Import from apps opens a read-only review of supported User/Project/Local sources and authorized Global sources, keeping same-name definitions separate for conflict resolution; the user proceeds to migration review before any write. Cell actions plan a reviewed change rather than directly overwriting same-name native definitions; distinguish not tested from healthy and show errors without colour alone.
F08 MCP server detail (per server): configuration (transport, defined in, command/URL, env with Keychain refs), tools list, health with last 5 checks, Fix token / Sign in, loaded-by toggles, why-is-this-on + Open trace, Disable in this repo, Test connection, Edit
F09 Add MCP server form: name, transport stdio/HTTP, command, env vars with Keychain secret refs, + Add variable, scope radio cards (Global needs admin), agent checkboxes (not-installed agent disabled), test connection panel with tools, files-that-will-change list with JSON diff preview, Save/Cancel
F10 Edit MCP server form (same as add, prefilled)
F11 Effective config: tabs Skills/MCP servers/Instructions, resolved list with source scope and agent count, "disabled here" rows, why-is-this-on trace across Global/User/Project/Local, change-for-this-repo actions (Disable in this repo, Override in Project, Edit User definition), per-agent file list
F12 Sync & drift: changed-outside-AgenTuna list, in-sync count + watching, backups list with Restore, drift banner, split diff (AgenTuna's last known vs on disk), Adopt on-disk / Restore AgenTuna version / Open in editor
F13 Instructions: rule file list per scope (CLAUDE.md, AGENTS.md, CLAUDE.local.md), editor with line numbers and markdown highlighting, Edit/Preview, unsaved-changes indicator, Save/Discard, New rule file
F14 Agents: card per agent (installed version, config paths, format, managed counts, items outside AgenTuna), Let AgenTuna manage this agent toggle, missing agent with Set path, Rescan
F15 Add skill flow: 1 Source (write new / import from git / from library), 2 Details (new: name, use-when description, template Blank/Checklist/Workflow, live SKILL.md preview; git: repo URL, Fetch, found skills list with checkboxes, SKILL.md preview; library: sources = other scopes and other projects with counts, skill checkboxes, name-clash warning, Copy vs Link), 3 Scope & agents (scope cards, agent checkboxes with target paths, name clash check), 4 Review (summary, files to create, stage in git option), Done (written files, Back to Skills, Add another, Edit SKILL.md)
F16 Migrate to AgenTuna flow: 1 Scan (scanned folders with counts, found items with type/scope/source path/ownership/agent), 2 Resolve conflicts (side-by-side diff, keep A / keep B / keep both renamed), 3 Plan (store locations, per-item originals and destinations, agent selections, shared-root effects, replace with links vs leave originals where supported, dry-run file list and expected states), Done (summary, Undo migration within 30 days subject to current-state checks). Import never silently replaces an unmanaged original.

## Overlays / dialogs / menus
O01 Command palette (Cmd+K): search skills, servers, files, agents, actions; keyboard nav
O02 Project picker (Cmd+P / chevron): lists all detected projects with path, counts, last used, scrollable; search filter with highlight; Add project folder; Scan ~/Projects
O03 Add menu (Skill / MCP server / Rule file)
O04 Account menu (Preferences, Backups & restore, Keychain secrets, Agents on this Mac)
O05 Row menu for skills (Open, Promote, Move to scope, Duplicate, Delete)
O06 Server menu (Edit, Why is this on?, Test connection, Disable in this repo)
O07 Promote / Move skills dialog (scope choice, overwrite warning)
O08 Enable-for-agent popover (agent checkboxes, Apply)
O09 Disable skills in this repo confirm; Disable server in this repo confirm; Delete skill confirm
O10 Compare copies dialog (side-by-side diff, Keep Project / Keep User)
O11 Connect agent / Set path dialog (path input, Browse)
O12 New rule file dialog (file type, scope)
O13 Toast notifications (success/warning, auto-dismiss 2.5 s), e.g. saved, test result, locked, shadowed, native, adopt first
O14 Keyboard: Esc closes overlays, Cmd+K, Cmd+P, Cmd+S save in editors

## Cross-cutting behaviours (from board pages "Scope model" and "Central store & migration")
X01 Scopes Local > Project > User > Global, subject to Global locks; skills resolve by declared identity, MCP by server identity, and rules compose according to verified native adapter semantics. Lower scopes can disable inherited definitions unless locked.
X02 AgenTuna store: ~/.agentuna/ (User), ./.agentuna/ (Project, committed); skills/, mcp.json, rules/AGENTS.md; Global store location follows AppPaths in the integration contract.
X03 Deployment: skills via symlink or copy only where supported, MCP rendered into verified native JSON/TOML, rules rendered using verified adapter semantics. Shared discovery roots produce coupled effects and must be explained before applying.
X04 Item states: Linked, Off, Native, Conflict, Drifted; sync labels In sync / Project only / Library newer / Project newer
X05 New secret values live in the OS credential store; files contain references only where the target agent's expansion mechanism is verified. Refuse unsupported secret deployment rather than write plaintext or pretend expansion works.
X06 Every write: reviewed diff preview, verified backups, journaled multi-file apply, stale-state checks and atomic replacement where supported. Checks cannot provide atomic compare-and-swap against noncooperating external writers; preserve recoverable versions and communicate residual risk.
X07 Refuse to overwrite unmanaged folders/entries; offer Adopt or Move aside
X08 Detection on start and on file change (watcher), Rescan
X09 No account, no telemetry; network only on explicit action (git fetch, marketplace, updates, remote backup)
X10 Agent paths (verify, do not hard-code a single path): Claude Code ~/.claude/skills, ~/.claude.json, .claude/skills, .mcp.json, CLAUDE.md, .claude/settings.local.json; Codex ~/.codex/config.toml ([mcp_servers.<name>] tables), ~/.codex/skills, ~/.agents/skills, .agents/skills, AGENTS.md; omp ~/.omp/agent/mcp.json, ~/.omp/agent/managed-skills (also check ~/.omp/agent/skills); Cursor ~/.cursor/skills (not installed on dev machine)
X11 Backlog-only features from Skills Manager research: refuse-to-overwrite, presets, agent-operable CLI + skill, sync labels, per-agent health line, update badge + auto-update, marketplace (skills.sh) with install counts, folder/.zip import, copy vs link setting, GitHub private-repo backup + multi-machine sync, custom agents by folder
