# Skills Manager (skillsmanager.dev) — reference analysis

Captured 2026-11-04 with ego-browser. Source: https://skillsmanager.dev (v1.43.3, MIT, github.com/xingkongliang/skills-manager, 7.3k stars).
Screenshots and raw text: `research/skillsmanager/`.

## What it is

A desktop app (Tauri; macOS, Windows, Linux) that keeps one skill library in `~/.skills-manager/skills` and deploys skills into each agent's own folder. Skills only. It does not manage MCP servers, instruction files or scopes.

## Features observed

| Feature | Evidence | What it does |
|---|---|---|
| Central library | `01-hero.png`, FAQ | Skills are plain folders under `~/.skills-manager/skills`. The SQLite DB holds only metadata and can be rebuilt from the folders. |
| Symlink by default, copy optional | FAQ | Copy mode is a setting, for tools that don't follow symlinks. |
| Per-agent agent icons on every card | `03-features.png` | Each skill card shows a row of agent icons; clicking one deploys or removes that skill for that agent. |
| Presets | `02b-ask-deploy.png`, `app-ws-cursor-after.jpg` | Named skill groups (Everyday, Frontend, Docs). Apply a preset to an agent in one click. A skill can be in several presets. Applied state shows as `2/7`, `3/4`, `✓`. |
| Workspace per agent | `app-ws-cursor-after.jpg` | Sidebar lists each agent with a skill count. An agent page shows its path, `9 / 9 managed / 9 in sync`, tag filters and preset chips. |
| Project workspaces | `03d-project-and-backup.png`, app sidebar | Repos as workspaces. Card states: **Project only**, **In sync**, **Center newer**. Sync either direction (push up / pull down icons). |
| Refuses to overwrite | `02c-ask-refuse.png`, FAQ | If an agent folder already has an unmanaged skill with that name, deploy fails, names the path, and leaves it untouched. Card marked **Local only**. Offers adopt or move aside. |
| Install from anywhere | `03-features.png` | skills.sh marketplace with install counts; also Git URL, folder, .zip. |
| Update badge | `03-features.png` | When a git-sourced skill's source changes, an **Update** badge shows; auto-update optional. |
| Agent-operable CLI + skill | `02-ask.png` | `npx skills add xingkongliang/skills-manager` installs a skill that teaches agents to call `skills-manager-cli --json`. The app refreshes live as the agent works. |
| GitHub backup | `03d-project-and-backup.png` | Library backed up to a private repo; multi-machine sync; token in OS keychain; conflicts never overwrite. |
| 56 agents with known folders | `03c-features-more.png` | Searchable agent list with paths. Unknown tools: add by pointing at a folder. |
| Trust signals | `01-hero.png` | No account, no telemetry; signed and notarized. |

## Where AgenTuna already differs

- AgenTuna covers **MCP servers and instruction files**, not just skills, with JSON/TOML rendering per agent.
- AgenTuna has **Global / User / Project scope precedence**, locks, shadowing and a "why is this on?" trace. Skills Manager has a flat library plus project workspaces.
- AgenTuna shows a **file diff before every write**; Skills Manager does not show diffs in its marketing.

## Ideas worth taking (ranked)

1. **Refuse-to-overwrite as a hard rule.** Matches our "native" state. Make it explicit: deploying onto an unmanaged folder fails with the path and two buttons (Adopt, Move aside). Today our designs only show the native badge.
2. **Presets.** Our design has no grouping. A preset = named set of skills + MCP servers, applied per agent or per project. Useful for "Frontend repo" vs "Data repo".
3. **Agent CLI + skill so agents manage AgenTuna.** `agentuna --json skills deploy …` plus a SKILL.md. Cheap to add once the core exists; strong fit for this audience.
4. **Sync-direction states for project items.** Adopt their wording: In sync, Project only, Library newer, Project newer, with push/pull actions. Clearer than our current "Drifted".
5. **Per-agent workspace header:** `path · N / N managed · N in sync`. Good one-line health summary for our Agents page.
6. **Update badge for git-sourced skills**, with optional auto-update.
7. **Marketplace tab** (skills.sh) with install counts inside Add skill → Source. Also accept a folder or `.zip`.
8. **Copy vs symlink setting** per agent, for tools that don't follow links.
9. **Backup to a private GitHub repo.** We already back up locally; remote backup is a later feature.
10. **Agent registry with custom folders**, so unknown agents can be added without a release.

## Correction to our designs

Skills Manager lists omp at `~/.omp/agent/skills`. On this Mac that folder does not exist; skills live in `~/.omp/agent/managed-skills` (symlinked into `~/.skills-manager/skills`). AgenTuna should detect both paths rather than hard-code one.

## Unverified

- Behaviour beyond the marketing page (MCP support, scope handling, conflict UI) was not tested; the app was not opened.
- Install counts and agent counts are as stated on the site.
