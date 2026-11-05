# AgenTuna

A local-first desktop app and CLI for managing skills, MCP servers, and rule files across coding agents and Global, User, Project, and Local scopes.

Rust core + Tauri 2 shell (`crates/at-app`) + React/TypeScript frontend (`ui/`). macOS first.

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

Commit changes to `ui/src/generated/`; do not edit those files by hand.
