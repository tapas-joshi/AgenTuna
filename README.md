# AgenTuna

A local-first desktop app and CLI for managing skills, MCP servers, and rule files across coding agents and Global, User, Project, and Local scopes.

Rust core + Tauri 2 shell (`crates/at-app`) + React/TypeScript frontend (`ui/`). macOS first.

## Downloads

Builds are attached to each [GitHub release](https://github.com/tapas-joshi/AgenTuna/releases):
macOS Apple Silicon (`.dmg`, `.app.zip`), Linux x86_64 (`.tar.gz`, `.deb`, `.AppImage`) and the
`agentuna` CLI for both, with a `SHA256SUMS` file. What changed is in [CHANGELOG.md](CHANGELOG.md);
how releases are built is in [docs/releasing.md](docs/releasing.md). Windows is not built yet.
macOS builds are ad-hoc signed and not notarized (right-click the app and choose **Open** the
first time); the Linux desktop app has not been launched by the maintainers yet.

### Homebrew

```sh
brew tap tapas-joshi/agentuna
brew install --cask agentuna     # the macOS Apple Silicon app + the `agentuna` command
brew install agentuna-cli        # only the command-line tool (macOS Apple Silicon, Linux x86_64)
```

The cask verifies the download against its pinned SHA-256 and clears the quarantine flag, so the
unnotarized preview opens without the Gatekeeper prompt. The tap lives in
[tapas-joshi/homebrew-agentuna](https://github.com/tapas-joshi/homebrew-agentuna).

## Try it (MVP-1)

Use a throwaway sandbox first: AgenTuna reads and edits real agent configuration (Claude Code, Codex, omp), so the first run should never point at your real home folder. Every path is injectable through environment variables, and every write is a reviewed, backed-up, undoable change.

```sh
SANDBOX="$(mktemp -d)"; mkdir -p "$SANDBOX"/{home,support,user-store,global-store,tmp,config,data,cache}
# Put a few native items in the sandbox home, for example:
#   $SANDBOX/home/.claude/skills/<name>/SKILL.md   and   $SANDBOX/home/.claude.json (an "mcpServers" object)
export HOME="$SANDBOX/home" AGENTUNA_HOME="$SANDBOX/support" AGENTUNA_USER_STORE="$SANDBOX/user-store" \
       AGENTUNA_GLOBAL_STORE="$SANDBOX/global-store" TMPDIR="$SANDBOX/tmp" \
       XDG_CONFIG_HOME="$SANDBOX/config" XDG_DATA_HOME="$SANDBOX/data" XDG_CACHE_HOME="$SANDBOX/cache"
```

Build with the real toolchain first (`source "$HOME/.cargo/env"` if cargo is not on your PATH, in a shell that still has your real `HOME`), then run the commands below with the sandbox variables exported:

```sh
cargo build -p at-cli                      # target/debug/agentuna
agentuna doctor                            # health of the sandbox
agentuna migrate scan                      # what lives only in agent folders
agentuna migrate plan --strategy links     # review the plan (resolve conflicts with --keep-both <name>)
agentuna migrate apply --plan <PLAN_ID> --yes
agentuna skills list; agentuna mcp list; agentuna effective --scope user
agentuna drift list                        # edit a migrated skill outside AgenTuna, then check again
agentuna migrate undo <OPERATION_ID> --yes # restores every original byte-for-byte
```

The desktop app is the same engine behind a window. Build the self-contained bundle and launch it with the same sandbox variables (the first run in your real home is possible only after you have rehearsed in a sandbox):

```sh
scripts/bundle.sh && scripts/sign.sh --adhoc          # target/release/AgenTuna.app (ad-hoc signed, not notarized)
env -i HOME="$HOME" AGENTUNA_HOME="$AGENTUNA_HOME" AGENTUNA_USER_STORE="$AGENTUNA_USER_STORE" \
    AGENTUNA_GLOBAL_STORE="$AGENTUNA_GLOBAL_STORE" TMPDIR="$TMPDIR" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
    XDG_DATA_HOME="$XDG_DATA_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" PATH=/usr/bin:/bin \
    target/release/AgenTuna.app/Contents/MacOS/agentuna-app
```

In the app: Skills and MCP Servers show native items with a "not in AgenTuna" banner; "Review & migrate" runs scan, resolve, plan, apply and Done; Undo on the Done screen restores the originals. Logs live in `$AGENTUNA_HOME/logs`. Notarization and the in-place updater need an Apple Developer ID and are not part of this build. Implementation status per story is in `design/agentuna-backlog.xlsx` (sheet Tech Stories, columns Status and Status notes).

## Building and Verification

### Prerequisites
- Rust 1.88+ with `rustfmt` and `clippy` (`rust-toolchain.toml` pins the channel).
- Node 20+ and pnpm. Xcode Command Line Tools on macOS.

### Setup

```sh
pnpm --dir ui install
```

### Run

```sh
# Desktop app (starts Vite and the Tauri shell)
pnpm --dir ui tauri dev

# CLI
cargo run -p at-cli -- --version
```

### Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # also regenerates ui/src/generated/*.ts from Rust types
cargo doc --workspace --no-deps
pnpm --dir ui test              # Vitest + Testing Library
pnpm --dir ui build             # typecheck + production bundle
```

### UI visual snapshots

Playwright runs the production Vite preview at 1x and 2x scale. Install the Chromium browser outside the repository with `pnpm --dir ui exec playwright install chromium`, then run `pnpm --dir ui run visual`. A difference over 0.1% of pixels fails the test, which writes the `-expected`, `-actual` and `-diff` images plus one merged `expected | actual | diff` image per failure under `ui/test-results/side-by-side/`. Review changed images under `ui/e2e/snapshots/`; update the baselines deliberately with `UPDATE_SNAPSHOTS=1 cargo test -p at-app --test snapshots` (or `pnpm --dir ui run visual:update`). The informational `visual` CI job runs the comparison on macOS and uploads `ui/test-results` and `ui/playwright-report` when it fails.

The small-window visual suite checks all main screens at 960×640, 1180×720 and 1280×800 (1x), including scrollable tables and reachable actions. Run `pnpm --dir ui exec playwright test e2e/small-window.visual.spec.ts`; screenshots are attached to `ui/playwright-report/`, with baselines only for the Skills list and MCP editor at each size.

Commit changes to `ui/src/generated/`; do not edit those files by hand.
