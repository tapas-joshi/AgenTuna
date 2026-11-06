#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "--quick" ]]; then
  echo "usage: scripts/perf.sh --quick" >&2
  exit 2
fi

source "$HOME/.cargo/env"
cargo test -p at-app --test performance -- --nocapture
