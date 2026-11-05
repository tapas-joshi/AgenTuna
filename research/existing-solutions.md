# Existing solutions before building AgenTuna

Research date: 2026-11-04. Two independent research slices plus parent-source
checks. Competitor comparisons use maintainer documentation, repository metadata
and selected source; CC Switch was subsequently tested with synthetic fixtures as
described below. Other applications were not runtime-tested. Documentation claims
are not independently verified safety guarantees.

## Conclusion

**This is an existing product category, not an empty market.** CC Switch is a
close combined desktop alternative. Subsequent naming research also found
[AgentHarbor](https://agentharbor.openxsecurity.com/), whose published feature
list includes MCP, skills, rules, multi-agent deployment, diff previews, drift,
backups and credential-store integration. Those AgentHarbor claims have not
been runtime-tested, and its project-scope and ownership guarantees remain
unverified. mTarsier advertises a skills + MCP GUI and CLI. Rulesync and
Sentry dotagents centralize cross-agent configuration declaratively.
Universal MCP Sync already implements the plan/preview/apply vocabulary.

Recommendation: evaluate CC Switch against AgenTuna's concrete scope/ownership
requirements before committing to a duplicate general-purpose manager. Continuing
AgenTuna remains reasonable if the required workflow is native project/local
configuration, source-path inspection, effective-config traces, shared-root
coupling and reviewed reversible adoption. This is a proposed positioning, not
proof that no other product offers those features.

Do not change the agreed Rust/Tauri stack, backlog, or MVP scope solely on this
research. Adopting a product, changing stacks or integrating a generator is a
separate decision.

## Strongest alternatives

| Solution | Documented overlap | Important distinction / limit | License |
| --- | --- | --- | --- |
| [CC Switch](https://github.com/farion1231/cc-switch) | Cross-platform Tauri desktop; centralized MCP, skills and prompts; per-app matrix; import from apps; backups; provider switching. README explicitly includes Oh My Pi. | MCP manual says import reads **only user-level config**, not Claude project-scoped servers or plugin servers. Same-name external edits can be overwritten by the DB definition. These are material differences from AgenTuna's stale-preview/ownership design. | MIT |
| [mTarsier](https://github.com/mcp360/mTarsier) | Rust/Tauri desktop; unified MCP dashboard, skills editor/marketplace, client discovery, backup/rollback; `tsr` CLI for MCP and skills. | README support table does not list omp. Full scope precedence, adoption ownership and undo after external edits were not established. Promised backup behaviour was not exercised. | MIT |
| [Rulesync](https://github.com/dyoshikawa/rulesync) | Generates native rules, skills, MCP, commands, hooks and subagents from `.rulesync/`; import and conversion workflows. Current support table includes Claude Code, Codex and oh-my-pi. | Primarily Node/TypeScript CLI/config generation rather than desktop inventory. A support tick can mean project, global or simulated mode, not every mode. Global/user naming must not be mapped to AgenTuna's machine-admin Global scope blindly. | MIT |
| [Sentry dotagents](https://github.com/getsentry/dotagents) | `agents.toml` + lockfile, skills/MCP/hooks/subagents/plugins, global and explicit project operation; offline sync/adoption and trust policy. Reusable TypeScript core. | Global means `~/.agents/`, effectively user-wide. README distinguishes Pi, not Oh My Pi. Shared `.agents/skills` projections can be visible to multiple clients. Full four-scope resolver and generic backup/undo not established. | MIT |
| [Skills Manager](https://github.com/xingkongliang/skills-manager) | Central skills library, per-agent deployment, project workspaces, presets, CLI, safe adoption and backup workflows. | Skill-focused; not a combined MCP/rules manager. See the earlier [visual research](skillsmanager.md). | MIT |

### Useful narrower building blocks

- [Vercel Skills](https://github.com/vercel-labs/skills): source discovery,
  skill creation/install/list/check/update/remove across many agents, project
  installs by default and user-wide via `-g`. Not an MCP or rule-file manager.
  MIT. The README states telemetry is enabled unless `DISABLE_TELEMETRY=1` or
  `DO_NOT_TRACK=1`; wrapping it unchanged would violate AgenTuna's no-telemetry
  policy. Source review is needed before any integration.
- [Universal MCP Sync](https://github.com/nawodyaishan/universal-mcp-sync): Go
  CLI/TUI for client detection, saved plans, exact target paths, redacted preview,
  stale-plan preflight, backups, atomic writes and rollback. MIT. A strong
  reference for MCP compatibility and apply flows, but not a skills manager or
  Rust library. The README says its library interface is pre-stable.
- [MCPR](https://github.com/mathematic-inc/mcpr): Go CLI with a central server
  definition, selective client sync, global and project-local `mcpr.json`.
  Apache-2.0. Its documented examples include plaintext environment/header values;
  do not assume AgenTuna's Keychain/reference contract is supplied by this tool.

## CC Switch: the most consequential findings

The [MCP manual](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/3-extensions/3.1-mcp.md)
explicitly documents:

1. One global MCP matrix, one server per row, per-app switches; import, add,
   search, edit, enable/disable-all, and limited bulk-toggle Undo.
2. Import reads user-level configs only. Project-default `claude mcp add`
   servers and plugin-bundled servers are not imported.
3. Unimported manual servers are left alone, but an externally edited entry
   sharing a DB-managed name is overwritten: “CC Switch's version wins.”
4. Deleting a server cannot be undone through that action.

Its [storage documentation](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/5-faq/5.1-config-files.md)
identifies `~/.cc-switch/cc-switch.db` as the source of truth and `profiles` as
bundles of provider/MCP/skills/prompt states. Such a bundle is not automatically
native repo-scoped resolution. It also lists file-backed OAuth credential files;
a Keychain-only credential design was **not established**. This is not a claim
that every credential is stored unencrypted or that the app is insecure.

The [skills guide](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/3-extensions/3.3-skills.md)
documents central skill storage and imports. The current README and
[omp adapter](https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/omp_config.rs)
are fresher than some manual tables: do not interpret an omitted omp table row as
proof omp is unsupported.

The README identifies **https://ccswitch.io** as its only official website.
Similarly named websites surfaced by search were not used as authoritative sources.
An [upstream screenshot](https://github.com/farion1231/cc-switch/blob/main/assets/release-notes/v4.0.0/main-en.png)
was inspected for layout context; it is not an AgenTuna asset or runtime verification.

## Installed app: interactive evaluation

The locally installed **CC Switch 4.0.25** was inspected on 2026-11-04
through macOS accessibility APIs. The existing profile was used read-only:

- Its navigation exposed MCP, Skills, Prompts, Sessions, Accounts, Usage, Apps
  and Settings. Claude Code, Codex and Oh My Pi appeared among agent selectors.
- MCP exposed an **Import from apps** action, **Add MCP** action, search field,
  and named per-server/per-agent checkboxes. Oh My Pi appeared as an MCP column
  in the running version, correcting the older user-manual table.
- Opening **Add MCP** displayed a form with Add and Cancel. Cancel was used;
  no server was submitted. The Skills page exposed a search field and a
  **Check Updates** control; neither an update nor an installation was run.

With user approval, the running app was closed, and a second instance launched
with `CC_SWITCH_TEST_HOME` and `HOME` pointing at a temporary directory. Its
open SQLite database and log were observed inside that directory. The original
app was reopened after evaluation. This is a targeted test setup, not proof that
all possible platform integrations are sandboxed.

- Synthetic user-level Claude and Codex MCP definitions with the same name
  appeared as one managed server, enabled for both agents, after first startup.
  **Import from apps** then reported no new servers and explained that CC Switch
  wins for existing definitions. No MCP server was launched.
- Turning off the Codex checkbox changed the sandbox Codex config from a server
  entry to an empty file. No equivalent change to real agent configs was made.
- A synthetic Claude skill was discovered as unmanaged. The import dialog
  explained the source agent and destination toggles; importing created a
  master copy under `.cc-switch/skills/` while the original skill file remained
  present and its directory was not a symlink.
- A synthetic project `.mcp.json` was present in the sandbox, but the app was
  not bound to that project. Its absence from the list does **not** prove
  project-scope discovery fails. Project selection, rollback, stale previews,
  collisions and undo were not exercised.

macOS `screencapture` failed to capture the display. This is an
accessibility-tree interaction review, **not a visual screenshot review**.
No private inventory, credentials or screenshots are retained in this report.

Design ideas for AgenTuna: use a compact per-agent MCP matrix with labeled
controls, searchable inventories, and an add drawer with a clear Cancel path.
Augment the matrix with scope, source and ownership details; do not present a
shared discovery root as an independent switch. These are design decisions,
not claims that CC Switch lacks all such context.

## Exclude or treat cautiously

- [MCP Router](https://github.com/mcp-router/mcp-router) is a runtime gateway,
  not merely a native-config synchronizer. Its
  [end-of-support notice](https://github.com/mcp-router/mcp-router/blob/main/docs/END_OF_SUPPORT.md)
  states support/security updates ended 2026-09-18 and recommends against new
  adoption. Its Sustainable Use License is not equivalent to MIT.
- [MCPHub Desktop](https://github.com/Jeamee/MCPHub-Desktop) documents MCP
  installation/configuration, with more-client support still a roadmap item.
  Its README says AGPL while the actual LICENSE file/API identify GPL-3.0.
  Resolve that discrepancy before any code reuse; it is not a default dependency.

## Build vs reuse

1. **Adopt an existing app** if the practical need is chiefly user-wide skills
   and MCP management. CC Switch and mTarsier merit an isolated evaluation.
2. **Build a focused AgenTuna** if project/local scopes and safe migration of
   existing native definitions remain unmet. Do not add provider routing,
   session analytics or usage billing merely because competitors have them.
3. **Integrate selectively**: import/export existing declarative manifests,
   source resolvers or adapter knowledge where licensing and contracts permit.
   MIT licensing does not make a TypeScript/Go engine a drop-in Rust library.
   A generator that writes live files cannot silently bypass AgenTuna's reviewed
   ChangeSet, ownership checks and journal.

Suggested sandbox evaluation: create different same-named skills in Claude and
Codex, a user MCP definition overridden in a project, and an external edit after
preview. Check source-path visibility, coupling warnings, apply refusal and undo
without touching a real agent home or Keychain. These tests are proposed, not run.

## Source snapshot

GitHub `main` commits observed during this investigation (not dependency pins):

- CC Switch: `ca58c0313360eae7c851fbd7553c1614c7af03e7`
- Rulesync: `d088ccc8f436255115e0e7fb95f9ce0778375a4a`
- dotagents: `81fa16f2bb6645dae9b8f8a5cb04297c838474fd`
- Vercel Skills: `9197d919034d19010ae303cf96bf19ad087dd4be`
- Skills Manager: `d30530c0ce473bc7c74fdb5952ebc08f2097eff2`
- Universal MCP Sync: `8b033a32ca2eff40c7ede15a46d88b9aef800211`
- MCPR: `c4d4f26a9d326fda6f18aecc7c3cc65a57ba2c3d`

Branches can advance between requests. No measured coverage percentages,
benchmark claims or maintenance guarantees are inferred from repository popularity.

## Current implementation state

Only research was added. No competing application or Rust toolchain was installed,
no agent configs were changed, and MVP implementation has not started. Rust
installation approval remains unresolved from the interrupted setup question.
