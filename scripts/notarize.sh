#!/usr/bin/env bash
# Notarizes a Developer ID signed AgenTuna build and staples the tickets (TF-031).
#
# The app is submitted as a zip of the bundle, then the app and the dmg are each
# notarized with `xcrun notarytool submit --wait`, stapled and verified with
# `stapler validate` and `spctl`. A submission that is not Accepted fails the job and
# prints the submission id, the `notarytool log` command that fetches the log, and
# Apple's Notary API documentation for a log URL (the tool has no public web URL).
#
# Credentials are read only from the environment or a notarytool keychain profile;
# nothing is stored in the repository. Prefer the keychain profile or an API key in CI:
#
#   NOTARY_PROFILE        profile created by `xcrun notarytool store-credentials`
#   NOTARY_APPLE_ID       Apple ID, with NOTARY_PASSWORD (app-specific password) and
#                         NOTARY_TEAM_ID (falls back to TEAM_ID)
#   NOTARY_KEY_PATH       App Store Connect API key (.p8), with NOTARY_KEY_ID and the
#                         optional NOTARY_ISSUER_ID
#
# The app must already be signed by scripts/sign.sh with a Developer ID Application
# identity, hardened runtime and a secure timestamp; ad-hoc builds are refused.
#
# Options:
#   --app PATH             app bundle (default target/release/AgenTuna.app)
#   --dmg PATH             dmg to notarize (default: the only dist/AgenTuna-*.dmg)
#   --no-dmg               notarize only the app bundle
#   --keychain-profile P   notarytool profile name (overrides NOTARY_PROFILE)
#   --timeout DURATION     notarytool --wait timeout (default 30m)
#   --dry-run              print the plan; skip the submission and identity checks
#   -h, --help             show this help

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
APP="$TARGET_DIR/release/AgenTuna.app"
DMG=""
NO_DMG=0
DRY_RUN=0
TIMEOUT="30m"
PROFILE="${NOTARY_PROFILE:-}"
APPLE_ID="${NOTARY_APPLE_ID:-}"
PASSWORD="${NOTARY_PASSWORD:-}"
TEAM_ID="${NOTARY_TEAM_ID:-${TEAM_ID:-}}"
KEY_PATH="${NOTARY_KEY_PATH:-}"
KEY_ID="${NOTARY_KEY_ID:-}"
ISSUER="${NOTARY_ISSUER_ID:-}"

usage() {
    cat <<EOF
Usage: scripts/notarize.sh [options]

Submits $APP (and the dmg) to Apple's notary service, waits for the result, then
staples and validates both. Credentials come from NOTARY_PROFILE, or
NOTARY_APPLE_ID/NOTARY_PASSWORD/NOTARY_TEAM_ID, or NOTARY_KEY_PATH/NOTARY_KEY_ID.

  --app PATH             app bundle to notarize
  --dmg PATH             dmg to notarize (default: the only dist/AgenTuna-*.dmg)
  --no-dmg               skip the dmg
  --keychain-profile P   notarytool keychain profile
  --timeout DURATION     wait timeout (default 30m)
  --dry-run              print the plan without submitting
  -h, --help             show this help
EOF
}

fail() {
    echo "error: $*" >&2
    exit 1
}

usage_error() {
    echo "error: $*" >&2
    exit 2
}

note() {
    echo "==> $*"
}

warn() {
    echo "warning: $*" >&2
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
            [[ $# -ge 2 ]] || usage_error "--app needs a path"
            APP="$2"
            shift 2
            ;;
        --dmg)
            [[ $# -ge 2 ]] || usage_error "--dmg needs a path"
            DMG="$2"
            shift 2
            ;;
        --no-dmg) NO_DMG=1; shift ;;
        --keychain-profile)
            [[ $# -ge 2 ]] || usage_error "--keychain-profile needs a name"
            PROFILE="$2"
            shift 2
            ;;
        --timeout)
            [[ $# -ge 2 ]] || usage_error "--timeout needs a duration"
            TIMEOUT="$2"
            shift 2
            ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# --- Authentication -----------------------------------------------------------

AUTH_ARGS=()
AUTH_LABEL=""
AUTH_KIND=""

if [[ -n "$PROFILE" ]]; then
    AUTH_KIND="keychain profile"
    AUTH_ARGS=(--keychain-profile "$PROFILE")
    AUTH_LABEL="--keychain-profile $PROFILE"
elif [[ -n "$KEY_PATH" || -n "$KEY_ID" ]]; then
    AUTH_KIND="API key"
    [[ -n "$KEY_PATH" ]] || usage_error "NOTARY_KEY_PATH must be set with NOTARY_KEY_ID"
    [[ -n "$KEY_ID" ]] || usage_error "NOTARY_KEY_ID must be set with NOTARY_KEY_PATH"
    [[ -f "$KEY_PATH" ]] || fail "API key file not found: $KEY_PATH"
    AUTH_ARGS=(--key "$KEY_PATH" --key-id "$KEY_ID")
    AUTH_LABEL="--key $KEY_PATH --key-id $KEY_ID"
    if [[ -n "$ISSUER" ]]; then
        AUTH_ARGS+=(--issuer "$ISSUER")
        AUTH_LABEL="$AUTH_LABEL --issuer $ISSUER"
    fi
elif [[ -n "$APPLE_ID" || -n "$PASSWORD" ]]; then
    AUTH_KIND="app-specific password"
    [[ -n "$APPLE_ID" ]] || usage_error "NOTARY_APPLE_ID must be set with NOTARY_PASSWORD"
    [[ -n "$PASSWORD" ]] || usage_error "NOTARY_PASSWORD must be set with NOTARY_APPLE_ID"
    [[ -n "$TEAM_ID" ]] || usage_error "NOTARY_TEAM_ID (or TEAM_ID) must be set with NOTARY_APPLE_ID"
    AUTH_ARGS=(--apple-id "$APPLE_ID" --password "$PASSWORD" --team-id "$TEAM_ID")
    # The password is masked: it must never be printed or logged.
    AUTH_LABEL="--apple-id $APPLE_ID --password <redacted> --team-id $TEAM_ID"
else
    if (( DRY_RUN )); then
        AUTH_KIND="none configured"
        AUTH_LABEL="<no credentials: set NOTARY_PROFILE, NOTARY_APPLE_ID/NOTARY_PASSWORD/NOTARY_TEAM_ID, or NOTARY_KEY_PATH/NOTARY_KEY_ID>"
    else
        usage_error "no notarization credentials: set NOTARY_PROFILE, NOTARY_APPLE_ID + NOTARY_PASSWORD + NOTARY_TEAM_ID, or NOTARY_KEY_PATH + NOTARY_KEY_ID"
    fi
fi

# --- Preconditions ------------------------------------------------------------

[[ "$(uname -s)" == "Darwin" ]] || fail "notarization requires macOS"
command -v xcrun >/dev/null 2>&1 || fail "xcrun is required"
command -v ditto >/dev/null 2>&1 || fail "ditto is required"
for tool in notarytool stapler spctl; do
    xcrun --find "$tool" >/dev/null 2>&1 || fail "xcrun $tool is unavailable"
done
[[ -d "$APP" ]] || fail "app bundle not found: $APP (run scripts/bundle.sh and scripts/sign.sh)"
[[ -f "$APP/Contents/Info.plist" ]] || fail "Info.plist missing: $APP/Contents/Info.plist"

if (( ! DRY_RUN )); then
    codesign --verify --deep --strict "$APP" \
        || fail "the app does not pass codesign --verify --deep --strict; sign it first"
fi

DETAILS="$(codesign -dv --verbose=4 "$APP" 2>&1 || true)"
DEVELOPER_ID=0
case "$DETAILS" in
    *"Authority=Developer ID Application"*) DEVELOPER_ID=1 ;;
esac
if (( DRY_RUN )); then
    (( DEVELOPER_ID )) || warn "the app is not Developer ID signed; a real run would refuse it"
else
    (( DEVELOPER_ID )) \
        || fail "notarization requires a Developer ID Application signature; this app is ad-hoc or differently signed"
    case "$DETAILS" in
        *"runtime"*) ;;
        *) fail "the hardened runtime flag is missing; re-sign with scripts/sign.sh" ;;
    esac
    case "$DETAILS" in
        *"Timestamp="*) ;;
        *) fail "the signature has no secure timestamp; re-sign with scripts/sign.sh" ;;
    esac
fi

if [[ -z "$DMG" ]] && (( ! NO_DMG )); then
    shopt -s nullglob
    candidates=("$ROOT"/dist/AgenTuna-*.dmg)
    shopt -u nullglob
    if (( ${#candidates[@]} == 1 )); then
        DMG="${candidates[0]}"
    elif (( ${#candidates[@]} == 0 )); then
        if (( DRY_RUN )); then
            warn "no dmg found under $ROOT/dist; the dry run skips the dmg"
        else
            fail "no dmg found under $ROOT/dist (run scripts/dmg.sh, pass --dmg, or use --no-dmg)"
        fi
    else
        fail "found ${#candidates[@]} dmgs under $ROOT/dist; pass --dmg explicitly"
    fi
fi
if [[ -n "$DMG" ]]; then
    [[ -f "$DMG" ]] || fail "dmg not found: $DMG"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/agentuna-notary.XXXXXX")"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

# --- Submission ---------------------------------------------------------------

reject() {
    local label="$1"
    local status="$2"
    local id="$3"
    local message="$4"

    echo "error: notarization was not accepted for $label (status: $status)" >&2
    if [[ -n "$message" ]]; then
        echo "       message: $message" >&2
    fi
    if [[ -n "$id" ]]; then
        echo "       submission: $id" >&2
        echo "       notary log: xcrun notarytool log $id $AUTH_LABEL" >&2
        echo "       log API:    https://developer.apple.com/documentation/notaryapi/get-previous-submissions" >&2
        local log="$WORK/notary-$label-log.json"
        if xcrun notarytool log "$id" "${AUTH_ARGS[@]}" >"$log" 2>/dev/null; then
            if [[ -s "$log" ]]; then
                echo "       log saved:  $log" >&2
            fi
        fi
    else
        echo "       notarytool printed no submission id; see the output above" >&2
    fi
    exit 1
}

submit() {
    local label="$1"
    local archive="$2"

    note "Submitting $label: $archive"
    if (( DRY_RUN )); then
        printf '+ xcrun notarytool submit %q --wait --timeout %q %s --output-format json\n' \
            "$archive" "$TIMEOUT" "$AUTH_LABEL"
        return 0
    fi

    local result="$WORK/notary-$label.json"
    if ! xcrun notarytool submit "$archive" "${AUTH_ARGS[@]}" \
        --wait --timeout "$TIMEOUT" --output-format json >"$result"; then
        echo "error: notarytool submit failed for $label; output: $result" >&2
        cat "$result" >&2 || true
        exit 1
    fi

    local status id message
    status="$(plutil -extract status raw -o - "$result" 2>/dev/null || true)"
    id="$(plutil -extract id raw -o - "$result" 2>/dev/null || true)"
    message="$(plutil -extract message raw -o - "$result" 2>/dev/null || true)"
    [[ -n "$status" ]] || {
        echo "error: notarytool returned no status for $label" >&2
        cat "$result" >&2 || true
        exit 1
    }

    if [[ "$status" != "Accepted" ]]; then
        reject "$label" "$status" "$id" "$message"
    fi
    note "Notarization accepted for $label${id:+ (submission $id)}"
}

note "Notarizing with $AUTH_KIND"

APP_ZIP="$WORK/AgenTuna.zip"
run ditto -c -k --keepParent "$APP" "$APP_ZIP"
submit "app" "$APP_ZIP"
run xcrun stapler staple "$APP"
run xcrun stapler validate "$APP"
run spctl --assess --type execute --verbose=4 "$APP"

if [[ -n "$DMG" ]]; then
    submit "dmg" "$DMG"
    run xcrun stapler staple "$DMG"
    run xcrun stapler validate "$DMG"
    run spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG"
fi

if (( DRY_RUN )); then
    note "Dry run: nothing was submitted"
else
    note "Notarized and stapled: $APP${DMG:+ and $DMG}"
fi
