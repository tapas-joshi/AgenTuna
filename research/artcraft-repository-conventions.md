# ArtCraft repository conventions used for AgenTuna

Reviewed public `main` branches on 2026-11-04. These sources are mutable; this is
an evidence note, not a dependency pin. Root `AGENTS.md` is the deliverable.
No upstream code, dependencies, branding or assets were copied into AgenTuna.

## Primary sources

- [FigCraft AGENTS.md](https://github.com/storytold/figcraft/blob/main/AGENTS.md)
- [GridCraft AGENTS.md](https://github.com/storytold/gridcraft/blob/main/AGENTS.md)
- [WordCraft AGENTS.md](https://github.com/storytold/wordcraft/blob/main/AGENTS.md)
- [FigCraft Cargo.toml](https://github.com/storytold/figcraft/blob/main/Cargo.toml)
- [FigCraft command engine](https://github.com/storytold/figcraft/blob/main/crates/engine/src/cmd.rs)
- [FigCraft control protocol](https://github.com/storytold/figcraft/blob/main/docs/control-protocol.md)
- [WordCraft recovery rules](https://github.com/storytold/wordcraft/blob/main/docs/recovery.md)
- [craft-libs AGENTS.md](https://github.com/storytold/craft-libs/blob/main/AGENTS.md)
- [craft-libs CLAUDE.md](https://github.com/storytold/craft-libs/blob/main/CLAUDE.md)
- [craft-libs library catalogue and license](https://github.com/storytold/craft-libs/blob/main/README.md)

## Transferable rules

| Evidence | AgenTuna adaptation |
| --- | --- |
| App instructions say “Everything is a command”; menus, shortcuts, CLI, control channel and MCP dispatch shared command IDs. | One typed intent/engine implementation for business actions; no second mutation path in widgets or CLI. |
| FigCraft layering keeps GUI-toolkit and dialog dependencies out of lower layers; UI uses theme tokens. | Pure domain types, IO isolated behind dedicated modules, replaceable presentation layer, shared tokens. |
| App instructions reject production unwrap/expect/panic/todo/unimplemented/unreachable and forbid unsafe; craft-libs repeats defensive Rust. | Typed failures, bounded inputs, workspace lint inheritance and no placeholders in shipped paths. |
| Headless command tests and actual screenshot inspection are both required by the app instructions. | Sandbox CLI/core smoke runs plus actual screen interaction and screenshot inspection. |
| WordCraft recovery uses temporary files, synchronization, validation and retained previous generations. | Backups and journaled ChangeSets; disk rollback is explicit rather than inferred from panic recovery. |
| craft-libs keeps mechanism in shared crates and policy in apps. | Reuse only behind reviewed seams; AgenTuna retains scope, consent and deployment policy. |
| craft-libs CLAUDE.md points to AGENTS.md instead of reproducing it. | One canonical root rules file; do not create a second drifting copy. |

## Do not transplant

- App manifests patch their GUI toolkit and windowing crates to reviewed Storytold forks.
  Same stack does not imply AgenTuna needs the same fork commits or suite libraries.
- The app workspaces use `xtask` gates and shared `craft-common-actions` workflows.
  AgenTuna has neither a workspace nor CI yet. Its Cargo commands are labelled prospective.
- Figma/Office parity, browser/WASM targets, font repositories, sibling checkout
  paths, automatic commits/pushes and contributor-credit machinery are not AgenTuna requirements.
- craft-libs is MIT OR Apache-2.0, but redistributed code needs applicable notices
  and assets have their own licenses. Do not copy ArtCraft branding.

## AgenTuna-specific additions, not claims about ArtCraft

Dotfile ownership, format-preserving JSON/TOML patches, stale-plan detection,
external-writer race caveats, Keychain redaction, shared agent roots, consent to
execute MCP servers and migration undo come from AgenTuna's product and technical
contract. They must not be presented as inherited ArtCraft implementation.

## Verification limits

No AgenTuna build or runtime verification is possible yet: the repository contains
planning and design artifacts, not Cargo manifests. Repository guidance and
manifest conventions were inspected; upstream apps were not built in this task.
