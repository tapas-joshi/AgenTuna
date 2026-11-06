# Contributing

## Checks before a pull request

Run the checks supported by the current workspace:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
pnpm --dir ui test
pnpm --dir ui build
```

The `rust` job in `.github/workflows/ci.yml` runs the Rust checks, `frontend`
runs the UI checks and `supply chain` runs `cargo deny`. Formatting and clippy are separate
steps, so a failure names the exact fix. Test logs and golden diffs from a failed run are
attached to the run as the `rust-failure-logs` artifact.

Install the pre-commit hook once per clone; it runs actionlint over `.github/workflows`:

```bash
pre-commit install
```

## Branch protection

`main` is protected. Configure the repository to:

- require a pull request before merging, with at least one approval;
- require the `rust`, `frontend` and `supply chain` checks to pass, and require
  branches to be up to date before merging;
- block force pushes and deletions;
- require linear history (squash or rebase).

Release signing and notarization are a separate workflow from CI and never run on pull
requests.
