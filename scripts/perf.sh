#!/bin/sh
# TF-035 quick performance gate.
#
#   scripts/perf.sh --quick   run the sandboxed headless performance budgets
#
# Prints one line per budget with the measured value and verdict, and exits
# non-zero when a budget is exceeded. The test seeds a fixed workload (3,000
# items, 9,000 deployments, 3 agents, 10,000 operations) into a temporary home,
# so the run is deterministic and never touches the real HOME, Keychain, agent
# folders or the network. GUI budgets (first paint, scroll frame time, webview
# RSS, watcher wakeups) are not measurable headlessly.
set -eu

usage() {
    printf 'usage: %s --quick\n' "${0##*/}" >&2
    printf 'Runs the headless performance budgets and exits non-zero on a miss.\n' >&2
}

if [ "$#" -ne 1 ] || [ "$1" != "--quick" ]; then
    usage
    exit 2
fi

if command -v cargo >/dev/null 2>&1; then
    cargo_bin=cargo
elif [ -x "${HOME:-/nonexistent}/.cargo/bin/cargo" ]; then
    cargo_bin=${HOME}/.cargo/bin/cargo
else
    printf 'cargo not found; add ~/.cargo/bin to PATH\n' >&2
    exit 127
fi

exec "$cargo_bin" test --locked --offline -p at-app --test performance -- --nocapture
