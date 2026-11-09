# Changelog

All notable changes to AgenTuna are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.3] - 2026-11-09

A skills-detection fix for people upgrading from 0.1.1.

### Fixed

- **Skills no longer stay hidden after upgrading from 0.1.1.** 0.1.1 stored the skills it found in
  agent folders, then never scanned again: the rows stayed marked missing, and the schema upgrade
  in 0.1.2 added the discovery columns empty, so the Skills screen showed nothing although the
  folders held your skills. AgenTuna now rescans agent folders in the background at every start —
  after the window is up, so startup is not slower — and an open screen reloads when the scan
  lands. A database in that older state heals on the first start with this version; a manual
  Rescan is no longer needed.

## [0.1.2] - 2026-11-10

Fixes skills not being detected on machines where a skills manager installs skills as symbolic
links, and a file watcher that gave up on large skill folders.

### Fixed

- **Skills installed through symbolic links are detected.** Folders in `~/.claude/skills`,
  `~/.codex/skills`, `~/.agents/skills` and omp's `managed-skills` that link into a skills-manager
  library (cc-switch, skills-manager and similar) were all rejected as "unreadable skill content",
  so only MCP servers showed up. The link's target folder is now validated (it must resolve inside
  your home or project folder), read, and shown as one skill listed under every agent that links to
  it, with "linked" and "links to" details. Broken links appear as "broken link" rows instead of
  disappearing. Links inside a skill folder, links leaving your home folder and link loops are still
  refused.
- **Migrating and adopting linked skills is safe.** The plan records each link's target; if you
  retarget or replace a link between review and apply, the plan is refused as stale, and undo refuses
  to touch a link you changed. Two agents linking one library folder are copied and backed up once.
- **The file watcher no longer fails with "the watch service did not start within 5s".** On macOS
  starting it blocked once per watched folder. It now starts in under 100 ms, seeds in the
  background, retries with backoff, and Sync & drift shows Watching, Watching (degraded) or Not
  watching with the reason and a Retry button. Skill links are watched through their target folder
  (only inside your home, project and store folders), bounded to 256 targets and 20,000 entries per
  folder.

### Changed

- The AgenTuna database moves to schema version 10 (link target and discovery state columns). It is
  upgraded in place and keeps existing rows; skills previously marked missing reappear after the
  next scan.

## [0.1.1] - 2026-11-09

Polish and platform fixes on top of 0.1.0, found by running the real app on macOS and Linux.

### Added

- **A real app icon:** three toggle switches (green on, slate off, blue on) on the navy tile,
  replacing the placeholder green square. macOS, Windows and Linux files come from one SVG.
- **Homebrew:** `brew tap tapas-joshi/agentuna`, then `brew install --cask agentuna` (macOS Apple
  Silicon app plus the `agentuna` command) or `brew install agentuna-cli` (macOS Apple Silicon and
  Linux x86_64). The cask clears the quarantine flag, so the unnotarized app opens without the
  Gatekeeper prompt. Homebrew 6+ asks you to trust third-party taps once (`brew trust
  tapas-joshi/agentuna`, or install with the fully qualified name).

### Fixed

- **Button labels are optically centred** in the macOS web view: labels sat about 0.75 CSS px low
  because the font's line box was centred instead of its glyphs.
- **Deleting a watched store folder is noticed again.** The watcher waited for an OS event that
  macOS does not always deliver; roots are now also re-checked by `stat` every few seconds.
- **AgenTuna's own writes no longer show up as "changed outside AgenTuna".** They are recognised by
  path and content hash for as long as the file still holds what AgenTuna wrote, not by a 5-second
  window.
- **The Global scope default is platform-specific:** Linux `/var/lib/agentuna/global`, Windows
  `C:\ProgramData\AgenTuna\global`; it showed the macOS path on every platform. macOS is unchanged,
  and `AGENTUNA_GLOBAL_STORE` and the `global_store_path` setting still take precedence.
- **macOS 15 first launch:** the right-click > Open override no longer works; the release notes and
  README now describe **System Settings > Privacy & Security > Open Anyway** (or Homebrew).

### Changed

- **The Linux desktop app has been launched and checked** (Ubuntu 22.04, WebKitGTK 4.1, under a
  virtual display): it starts with no special environment variables and renders Overview, Skills,
  MCP Servers and Preferences. Untested: real GPU and Wayland, HiDPI, installing the `.deb` and
  AppImage on a desktop, and unlocked-keyring secret writes.
- CI now runs the `rust` and screenshot checks on Apple Silicon runners, and three CI jobs cover
  Ubuntu and Windows (compile and tests; Windows tests have not run yet).

### Known limitations

- macOS builds are still not notarized (no Apple Developer ID): use Homebrew, **Open Anyway**, or
  `xattr -dr com.apple.quarantine /Applications/AgenTuna.app`.
- Windows builds are not provided yet.
- Performance on an Apple M4: cold start to interactive about 2 s, first open of a 3,000-item Skills
  list about 0.8 s; memory stays well under 250 MB.

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

[0.1.3]: https://github.com/tapas-joshi/AgenTuna/releases/tag/v0.1.3
[0.1.2]: https://github.com/tapas-joshi/AgenTuna/releases/tag/v0.1.2
[0.1.1]: https://github.com/tapas-joshi/AgenTuna/releases/tag/v0.1.1
[0.1.0]: https://github.com/tapas-joshi/AgenTuna/releases/tag/v0.1.0
