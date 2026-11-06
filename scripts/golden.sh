#!/usr/bin/env bash
# TF-034: golden-file verification for the at-adapters format-preservation suite.
#
#   scripts/golden.sh --check    compare tests/golden/** byte-for-byte (CI mode)
#   scripts/golden.sh --update   regenerate expected files; commit the result deliberately
#   scripts/golden.sh --diff     show unified diffs for mismatches (writes target/golden-diff/)
#   scripts/golden.sh --update-manifest   regenerate the fixture sha256 manifest
#
# --check runs the golden test with UPDATE_GOLDEN unset. The suite is offline: fixtures are
# copied into a sandboxed temp home and no network, Keychain or real agent folder is touched.
# MANIFEST.sha256 pins every fixture's sha256 so an accidental agent-format change is visible;
# a fixture added or edited without a manifest update fails --check. --update never touches the
# manifest: refreshing it is a separate deliberate step.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES="$ROOT/crates/at-adapters/tests/fixtures"
GOLDEN="$ROOT/crates/at-adapters/tests/golden"
MANIFEST="$FIXTURES/MANIFEST.sha256"
DIFF_DIR="$ROOT/target/golden-diff"

mode="check"
case "${1:-}" in
  --check) mode="check" ;;
  --update) mode="update" ;;
  --diff) mode="diff" ;;
  --update-manifest) mode="update-manifest" ;;
  "") mode="check" ;;
  *) echo "usage: $0 [--check|--update|--diff|--update-manifest]" >&2; exit 2 ;;
esac

# Every golden case must have its expected file; a missing one is a failure, never a silent
# create (AC: "missing expected file").
missing=0
for dir in "$GOLDEN"/*/; do
  if [[ -d "$dir" ]] && ! find "$dir" -name 'expected.*' -type f | grep -q .; then
    echo "missing expected file: ${dir#"$ROOT"/}expected.*" >&2
    missing=1
  fi
done
if [[ "$missing" -ne 0 ]]; then
  exit 1
fi

write_manifest() {
  (
    cd "$FIXTURES"
    find . -type f ! -name 'MANIFEST.sha256' | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do
      printf '%s  %s\n' "$(shasum -a 256 "$f" | awk '{print $1}')" "$f"
    done
  )
}

check_manifest() {
  local tmp
  tmp="$(mktemp)"
  write_manifest > "$tmp"
  if [[ ! -f "$MANIFEST" ]]; then
    rm -f "$tmp"
    echo "missing manifest: $MANIFEST" >&2
    return 1
  fi
  if ! diff -u "$MANIFEST" "$tmp" > "$DIFF_DIR.manifest.diff"; then
    if [[ -s "$DIFF_DIR.manifest.diff" ]]; then
      echo "fixture manifest mismatch; a fixture changed without a manifest update." >&2
      echo "Review the diff, then run scripts/golden.sh --update-manifest to accept it:" >&2
      cat "$DIFF_DIR.manifest.diff" >&2
    fi
    rm -f "$tmp" "$DIFF_DIR.manifest.diff"
    return 1
  fi
  rm -f "$tmp" "$DIFF_DIR.manifest.diff"
  return 0
}

case "$mode" in
  update-manifest)
    write_manifest > "$MANIFEST"
    echo "fixture manifest regenerated: $MANIFEST"
    exit 0
    ;;
  update)
    UPDATE_GOLDEN=1 cargo test -p at-adapters --test golden
    echo "golden expected files regenerated; review and commit the diff"
    exit 0
    ;;
esac

mkdir -p "$DIFF_DIR"

# Manifest check runs in --check and --diff so a fixture change fails even when outputs match.
check_manifest

# --check and --diff: run the suite with UPDATE_GOLDEN unset.
rm -f "$DIFF_DIR"/*.diff 2>/dev/null || true
set +e
output="$(UPDATE_GOLDEN= cargo test -p at-adapters --test golden 2>&1)"
status=$?
set -e
printf '%s\n' "$output"

if [[ "$status" -ne 0 ]]; then
  # Persist the failure output; the test's unified diffs name fixture path and line numbers.
  printf '%s\n' "$output" > "$DIFF_DIR/golden-test.log"
  if [[ "$mode" == "diff" ]]; then
    echo "log written to $DIFF_DIR/golden-test.log"
  fi
  exit "$status"
fi

echo "golden corpus matches expected bytes"
