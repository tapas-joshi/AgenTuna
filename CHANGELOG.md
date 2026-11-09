# Changelog

All notable changes to AgenTuna are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- The Global scope default no longer comes from macOS on every platform. `AppPaths` asks
  `platform_roots::default_global_store` for the machine-wide location, so a Linux build resolves
  `/var/lib/agentuna/global` and a Windows build `C:\ProgramData\AgenTuna\global` instead of
  showing and probing `/Library/Application Support/AgenTuna/global`. `AGENTUNA_GLOBAL_STORE` and
  the `global_store_path` setting still take precedence, and the macOS default is unchanged.

## [0.1.0] - 2026-11-08

First public build. AgenTuna is a local-first desktop app and headless CLI that manages skills,
MCP servers and rule files for coding agents (Claude Code, Codex, omp, Cursor and custom agents)
across Global, User, Project and Local scopes. This is a **preview**: please test it on a copy of
your agent folders or in a throwaway project first.

### What you can do

- **See what every agent loads.** Overview, Skills, MCP Servers, Instructions, Agents and
  Effective config screens show, per agent, the source path, who owns it and why it is on (nearest
  scope wins: Local > Project > User > Global, subject to Global locks).
- **Manage skills and MCP servers centrally.** Add skills from a folder, ZIP, Git URL, your library
  or a template; enable or disable them per agent; move, promote, duplicate and delete them. Add,
  edit, test and copy MCP servers with a preview of every file that will change. Secrets stay in
  the macOS Keychain as references; values never appear in configs, logs or diffs.
- **Adopt what you already have.** The Migrate flow scans native agent folders (including folders
  with nothing in them), resolves conflicts, previews the exact file operations, applies them with
  backups, and can undo. `Import from apps` runs the same flow for MCP servers only.
- **Stay safe.** Every write is a reviewed plan that is re-checked just before it is applied,
  backed up, journaled and undoable; unmanaged files are never overwritten. Shared discovery roots
  (for example Codex and omp both reading `.agents/skills`) are shown as one coupled folder.
- **Notice drift.** Sync & drift watches managed files, shows a baseline diff when it can recover
  one, and offers adopt or restore with backups.
- **Script it.** The `agentuna` CLI uses the same engine as the app (`--json` for machines).

### Downloads

| Platform | Files |
|---|---|
| macOS Apple Silicon | `AgenTuna-0.1.0-macos-arm64.dmg`, `.app.zip`, `agentuna-cli-0.1.0-macos-arm64.tar.gz` |
| Linux x86_64 | `AgenTuna-0.1.0-linux-x86_64.tar.gz`, `.deb`, `.AppImage`, `agentuna-cli-0.1.0-linux-x86_64.tar.gz` |

Verify downloads with `SHA256SUMS`. Windows and Intel macOS builds are not provided yet.

### Known limitations

- **macOS builds are not notarized** (no Developer ID yet), so Gatekeeper blocks the first launch.
  On macOS 15 (Sequoia) and later: try to open the app once, then go to **System Settings >
  Privacy & Security**, scroll to the message about AgenTuna and click **Open Anyway**. On macOS 13
  and 14 you can instead right-click the app and choose **Open**. In a terminal:
  `xattr -dr com.apple.quarantine /Applications/AgenTuna.app`. The `agentuna` CLI tarball is not
  affected when downloaded with `curl`.
- **The Linux GUI has not been launched by the maintainers.** The Linux engine and CLI pass their
  tests in a Linux container and on a GitHub Linux runner; the desktop app is built but untested.
  Secrets on Linux need a running Secret Service (GNOME Keyring or KWallet).
- **In-app updates** work only on macOS and only for signed builds; elsewhere download the new
  release from GitHub.
- **Performance:** cold start to interactive is about 2 s and the first open of a 3,000-item Skills
  list about 0.8 s on an Apple M4; memory stays well under 250 MB.
- Nothing contacts the network unless you ask it to (Git imports, connection tests, update checks).
- Windows support is in progress (the workspace compiles on Windows; it has not been run there).

### Under the hood

Rust workspace (12 crates) with a Tauri 2 shell and a React/TypeScript front end; one typed command
engine behind the GUI and CLI; SQLite index; format-preserving config edits; 2,400+ Rust tests and
800+ UI tests; screenshot tests against the FigCraft designs.

[0.1.0]: https://github.com/tapas-joshi/AgenTuna/releases/tag/v0.1.0
