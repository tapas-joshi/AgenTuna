#!/usr/bin/env bash
# Signs target/release/AgenTuna.app for local use or distribution (TF-031).
#
# With SIGNING_IDENTITY and TEAM_ID in the environment the bundle is signed with that
# Developer ID Application identity, the hardened runtime and a secure timestamp, in the
# order macOS requires: the inner executables first, then the embedded frameworks, then
# the app bundle itself. Without credentials it is ad-hoc signed (`codesign -s -`) so a
# local build still runs; such a build reports `Local build (unsigned)` in About and
# cannot be notarized (see scripts/notarize.sh).
#
# Entitlements are scripts/entitlements.plist: exactly `com.apple.security.network.client`
# (MCP over HTTP, git fetches, the updater). There is no allow-jit, no
# allow-unsigned-executable-memory and no disable-library-validation; the script rejects an
# entitlements file that gains any of them.
#
# Environment (never a file in the repository):
#   SIGNING_IDENTITY  Developer ID Application identity; unset or "-" means ad-hoc
#   TEAM_ID           Apple Developer team identifier, required with SIGNING_IDENTITY
#
# Options:
#   --app PATH          app bundle (default target/release/AgenTuna.app)
#   --identity NAME     overrides SIGNING_IDENTITY
#   --team-id ID        overrides TEAM_ID
#   --entitlements PATH overrides scripts/entitlements.plist
#   --adhoc             force an ad-hoc signature even when credentials are present
#   --dry-run           print the signing commands without signing or verifying
#   -h, --help          show this help

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
APP="$TARGET_DIR/release/AgenTuna.app"
IDENTITY="${SIGNING_IDENTITY:-}"
TEAM_ID="${TEAM_ID:-}"
ENTITLEMENTS="$ROOT/scripts/entitlements.plist"
ADHOC=0
DRY_RUN=0

usage() {
    cat <<EOF
Usage: scripts/sign.sh [options]

Signs \$APP with the hardened runtime. Without SIGNING_IDENTITY it ad-hoc signs so
local builds run; with it, Developer ID Application plus a secure timestamp.

  --app PATH           app bundle (default $APP)
  --identity NAME      Developer ID identity (default \$SIGNING_IDENTITY)
  --team-id ID         team identifier (default \$TEAM_ID)
  --entitlements PATH  entitlements plist (default scripts/entitlements.plist)
  --adhoc              force ad-hoc signing
  --dry-run            print the commands, change nothing
  -h, --help           show this help
EOF
}

fail() {
    echo "error: $*" >&2
    exit 1
}

note() {
    echo "==> $*"
}

run() {
    printf '+'
    printf ' %q' "$@"
    printf '\n'
    if (( DRY_RUN )); then
        return 0
    fi
    "$@"
}

while (( $# > 0 )); do
    case "$1" in
        --app)
            [[ $# -ge 2 ]] || fail "--app needs a path"
            APP="$2"
            shift 2
            ;;
        --identity)
            [[ $# -ge 2 ]] || fail "--identity needs a name"
            IDENTITY="$2"
            shift 2
            ;;
        --team-id)
            [[ $# -ge 2 ]] || fail "--team-id needs an identifier"
            TEAM_ID="$2"
            shift 2
            ;;
        --entitlements)
            [[ $# -ge 2 ]] || fail "--entitlements needs a path"
            ENTITLEMENTS="$2"
            shift 2
            ;;
        --adhoc) ADHOC=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# --- Preconditions ------------------------------------------------------------

[[ "$(uname -s)" == "Darwin" ]] || fail "signing requires macOS"
command -v codesign >/dev/null 2>&1 || fail "codesign is required"
[[ -d "$APP" ]] || fail "app bundle not found: $APP (run scripts/bundle.sh first)"
APP_BINARY="$APP/Contents/MacOS/agentuna-app"
[[ -f "$APP_BINARY" ]] || fail "app executable missing: $APP_BINARY"
[[ -f "$APP/Contents/Info.plist" ]] || fail "Info.plist missing: $APP/Contents/Info.plist"
[[ -f "$ENTITLEMENTS" ]] || fail "entitlements not found: $ENTITLEMENTS"

plutil -lint "$ENTITLEMENTS" >/dev/null || fail "invalid entitlements plist: $ENTITLEMENTS"

# Reject an entitlements file that would weaken the hardened runtime.
for forbidden in \
    com.apple.security.cs.allow-jit \
    com.apple.security.cs.allow-unsigned-executable-memory \
    com.apple.security.cs.disable-library-validation; do
    if grep -q "$forbidden" "$ENTITLEMENTS"; then
        fail "entitlements must not contain $forbidden"
    fi
done
if ! plutil -extract 'com\.apple\.security\.network\.client' raw -o - "$ENTITLEMENTS" >/dev/null 2>&1; then
    fail "entitlements must declare com.apple.security.network.client"
fi

if (( ADHOC )) || [[ -z "$IDENTITY" || "$IDENTITY" == "-" ]]; then
    SIGNING_MODE="adhoc"
    IDENTITY="-"
    SIGN_ARGS=(--force --options runtime --sign - --entitlements "$ENTITLEMENTS")
    note "Ad-hoc signing: this is a local build (unsigned); it cannot be notarized"
else
    SIGNING_MODE="developer-id"
    [[ -n "$TEAM_ID" ]] || fail "TEAM_ID must be set with SIGNING_IDENTITY=$IDENTITY"
    SIGN_ARGS=(--force --options runtime --timestamp --sign "$IDENTITY" --entitlements "$ENTITLEMENTS")
    note "Developer ID signing as $IDENTITY (team $TEAM_ID)"
fi

# --- Sign: inner executables, then frameworks, then the app -------------------

macho_files() {
    local root="$1"
    local file
    while IFS= read -r -d '' file; do
        if file -b "$file" | grep -q 'Mach-O'; then
            printf '%s\0' "$file"
        fi
    done < <(find "$root" -type f -print0 2>/dev/null)
}

sign_one() {
    run codesign "${SIGN_ARGS[@]}" "$1"
}

note "Signing the inner executables"
signed_any=0
for directory in "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"; do
    [[ -d "$directory" ]] || continue
    while IFS= read -r -d '' binary; do
        sign_one "$binary"
        signed_any=1
    done < <(macho_files "$directory")
done
(( signed_any )) || fail "no Mach-O executable found below $APP/Contents"

FRAMEWORKS="$APP/Contents/Frameworks"
if [[ -d "$FRAMEWORKS" ]]; then
    note "Signing the embedded frameworks"
    # Nested libraries first, then the framework bundles that seal them.
    while IFS= read -r -d '' nested; do
        sign_one "$nested"
    done < <(macho_files "$FRAMEWORKS")
    while IFS= read -r -d '' framework; do
        sign_one "$framework"
    done < <(find "$FRAMEWORKS" -mindepth 1 -maxdepth 1 -name '*.framework' -print0)
fi

note "Signing the app bundle"
sign_one "$APP"

# --- Verify -------------------------------------------------------------------

if (( DRY_RUN )); then
    echo "+ codesign --verify --deep --strict --verbose=2 $APP"
    echo "+ codesign -dv --verbose=4 $APP"
    note "Dry run: nothing was signed"
    exit 0
fi

note "Verifying the signature"
codesign --verify --deep --strict --verbose=2 "$APP" \
    || fail "codesign --verify --deep --strict failed for $APP"

DETAILS="$(codesign -dv --verbose=4 "$APP" 2>&1)" || fail "codesign -dv failed for $APP"
printf '%s\n' "$DETAILS"

if [[ "$SIGNING_MODE" == "adhoc" ]]; then
    case "$DETAILS" in
        *"flags=0x"*"adhoc"*) ;;
        *) fail "the app is not ad-hoc signed although ad-hoc signing was requested" ;;
    esac
    note "Signed ad-hoc: local build (unsigned)"
else
    case "$DETAILS" in
        *"Authority=Developer ID Application"*) ;;
        *) fail "the app is not signed with a Developer ID Application authority" ;;
    esac
    case "$DETAILS" in
        *"runtime"*) ;;
        *) fail "the hardened runtime flag is missing from the signature" ;;
    esac
    case "$DETAILS" in
        *"Timestamp="*) ;;
        *) fail "the signature has no secure timestamp" ;;
    esac
    case "$DETAILS" in
        *"TeamIdentifier=$TEAM_ID"*) ;;
        *) fail "the signature team identifier does not match TEAM_ID=$TEAM_ID" ;;
    esac
    note "Signed for distribution: Developer ID Application, hardened runtime, secure timestamp"
fi
