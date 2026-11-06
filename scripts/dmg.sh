#!/usr/bin/env bash
# Builds dist/AgenTuna-<version>-<arch>.dmg from a signed AgenTuna.app bundle (TF-030).
#
# Layout, matching the release contract:
#   volume name   AgenTuna <version>
#   window        640x400, app icon on the left, Applications alias on the right,
#                 generated background image
#   filesystem    HFS+, final image UDBZ (UDIF bzip2)
#   signature     the dmg is signed (Developer ID when AGENTUNA_SIGN_IDENTITY is set,
#                 ad-hoc otherwise); the app bundle must verify with codesign first
#   checksum      dist/AgenTuna-<version>-<arch>.dmg.sha256
#
# The script is idempotent: an existing image is replaced atomically after the new one
# passed `hdiutil verify`, a test mount and the checksum comparison, and every temporary
# image and mount is removed on exit.
#
# Signing credentials, notarization and stapling belong to TF-031; this script only
# consumes an already signed bundle. It refuses to build from an unsigned bundle.
#
# create-dmg is not required: the same layout is produced with hdiutil and one
# best-effort Finder pass (icon positions and background need a graphical session;
# a headless build still produces a valid image with the app and the Applications alias).
#
# Optional environment:
#   AGENTUNA_SIGN_IDENTITY  codesign identity for the dmg (default: ad-hoc "-")
#   AGENTUNA_DMG_BACKGROUND path to a PNG background (default: generated 640x400 gradient)
#   AGENTUNA_DMG_LAYOUT     set to 0 to skip the Finder pass (headless CI, tests)
#   CARGO_TARGET_DIR        target directory (defaults to <repo>/target)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
APP_BUNDLE="$TARGET_DIR/release/AgenTuna.app"
OUT_DIR="$ROOT/dist"
SIGN_IDENTITY="${AGENTUNA_SIGN_IDENTITY:--}"
BACKGROUND="${AGENTUNA_DMG_BACKGROUND:-}"
SIGN_DMG=1
KEEP_WORK=0
WORK=""
MOUNT_POINT=""
VERIFY_MOUNT=""
RW_IMAGE=""
FINAL_IMAGE=""

usage() {
    cat <<EOF
Usage: scripts/dmg.sh [options]

Builds \$OUT_DIR/AgenTuna-<version>-<arch>.dmg and its .sha256 file.

  --app PATH          app bundle to package (default: $APP_BUNDLE)
  --out-dir PATH      output directory (default: $OUT_DIR)
  --identity NAME     codesign identity for the dmg (default: ad-hoc)
  --background PNG    background image (default: generated gradient)
  --no-sign           do not sign the dmg (local experiments only)
  --keep-work         keep the temporary work directory for debugging
  -h, --help          show this help

The app bundle must be signed and pass: codesign --verify --deep --strict
EOF
}

fail() {
    echo "error: $*" >&2
    exit 1
}

note() {
    echo "==> $*"
}

cleanup() {
    if [[ -n "$VERIFY_MOUNT" && -d "$VERIFY_MOUNT" ]]; then
        hdiutil detach "$VERIFY_MOUNT" -force >/dev/null 2>&1 || true
    fi
    if [[ -n "$MOUNT_POINT" && -d "$MOUNT_POINT" ]]; then
        hdiutil detach "$MOUNT_POINT" -force >/dev/null 2>&1 || true
    fi
    if (( KEEP_WORK == 0 )) && [[ -n "$WORK" && -d "$WORK" ]]; then
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

while (( $# > 0 )); do
    case "$1" in
        --app) APP_BUNDLE="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --identity) SIGN_IDENTITY="$2"; shift 2 ;;
        --background) BACKGROUND="$2"; shift 2 ;;
        --no-sign) SIGN_DMG=0; shift ;;
        --keep-work) KEEP_WORK=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# --- Preconditions: checked before anything is created ------------------------

[[ "$(uname -s)" == "Darwin" ]] || fail "dmg creation requires macOS"
command -v hdiutil >/dev/null 2>&1 || fail "hdiutil is required"
command -v codesign >/dev/null 2>&1 || fail "codesign is required"
[[ -d "$APP_BUNDLE" ]] || fail "app bundle not found: $APP_BUNDLE (run scripts/bundle.sh first)"
APP_BINARY="$APP_BUNDLE/Contents/MacOS/agentuna-app"
[[ -x "$APP_BINARY" ]] || fail "app executable missing: $APP_BINARY"
PLIST="$APP_BUNDLE/Contents/Info.plist"
[[ -f "$PLIST" ]] || fail "Info.plist missing: $PLIST"

# Refuse an unsigned or broken bundle before creating any image.
if ! codesign --verify --deep --strict "$APP_BUNDLE" >/dev/null 2>&1; then
    fail "the app bundle is not signed or fails codesign --verify: $APP_BUNDLE" \
        $'\n       sign it first (TF-031), e.g.: codesign --force --deep --sign - "$APP_BUNDLE"'
fi

VERSION="$(plutil -extract CFBundleShortVersionString raw -o - "$PLIST" 2>/dev/null || true)"
[[ -n "$VERSION" ]] || fail "CFBundleShortVersionString missing from $PLIST"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || fail "CFBundleShortVersionString is not semver: $VERSION"

ARCH_LIST="$(lipo -archs "$APP_BINARY" 2>/dev/null || uname -m)"
ARCH_LIST="${ARCH_LIST// /}"
if [[ "$ARCH_LIST" == *"arm64"* && "$ARCH_LIST" == *"x86_64"* ]]; then
    ARCH="universal"
else
    ARCH="${ARCH_LIST%% *}"
    [[ -n "$ARCH" ]] || ARCH="$(uname -m)"
fi

VOLUME_NAME="AgenTuna $VERSION"
IMAGE_NAME="AgenTuna-$VERSION-$ARCH.dmg"
FINAL_IMAGE="$OUT_DIR/$IMAGE_NAME"
CHECKSUM_FILE="$FINAL_IMAGE.sha256"

mkdir -p "$OUT_DIR"
WORK="$(mktemp -d "$OUT_DIR/.dmg-work.XXXXXX")"
MNT="$WORK/mount"
VERIFY_MNT="$WORK/verify-mount"

note "Packaging $APP_BUNDLE as $IMAGE_NAME (volume: $VOLUME_NAME)"

# --- Background ---------------------------------------------------------------

if [[ -z "$BACKGROUND" ]]; then
    BACKGROUND="$WORK/background.png"
    # A 640x400 diagonal-ish gradient: P3 PPM is trivial to generate portably, and
    # sips converts it to the PNG that Finder expects.
    awk 'BEGIN {
        print "P3"
        print "640 400"
        print "255"
        for (y = 0; y < 400; y++) {
            for (x = 0; x < 640; x++) {
                r = 244 - int((x + y) / 1040 * 18)
                g = 246 - int((x + y) / 1040 * 14)
                b = 249 - int((x + y) / 1040 * 10)
                printf "%d %d %d ", r, g, b
            }
        }
    }' >"$WORK/background.ppm"
    sips -s format png "$WORK/background.ppm" --out "$BACKGROUND" >/dev/null 2>&1 \
        || fail "could not generate the dmg background with sips"
elif [[ ! -f "$BACKGROUND" ]]; then
    fail "background image not found: $BACKGROUND"
fi

# --- Writable image with the layout ------------------------------------------

SIZE_KB=$(( $(du -sk "$APP_BUNDLE" | awk '{print $1}') + 65536 ))
RW_IMAGE="$WORK/rw.dmg"
mkdir -p "$MNT"
note "Creating a ${SIZE_KB} KiB HFS+ image"
hdiutil create -quiet -size "${SIZE_KB}k" -fs HFS+ -volname "$VOLUME_NAME" -ov "$RW_IMAGE" >/dev/null
hdiutil attach -quiet -nobrowse -noautoopen -mountpoint "$MNT" "$RW_IMAGE" >/dev/null
MOUNT_POINT="$MNT"

note "Copying the app and the Applications alias"
ditto "$APP_BUNDLE" "$MNT/AgenTuna.app"
ln -s /Applications "$MNT/Applications"
mkdir -p "$MNT/.background"
cp "$BACKGROUND" "$MNT/.background/background.png"

# Finder layout is best effort: it needs a graphical session and Finder automation
# permission. Without either, the image is still valid and contains both entries.
# Set AGENTUNA_DMG_LAYOUT=0 to skip it (headless CI, tests).
if [[ "${AGENTUNA_DMG_LAYOUT:-1}" != "0" ]] \
    && command -v osascript >/dev/null 2>&1 \
    && [[ "$(launchctl managername 2>/dev/null || true)" == "Aqua" ]]; then
    cat >"$WORK/layout.applescript" <<'APPLESCRIPT'
tell application "Finder"
    tell disk "@VOLUME@" 
        open
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set the bounds of container window to {200, 120, 840, 520}
        set viewOptions to the icon view options of container window
        set arrangement of viewOptions to not arranged
        set icon size of viewOptions to 96
        set background picture of viewOptions to file ".background:background.png"
        set position of item "AgenTuna.app" of container window to {160, 200}
        set position of item "Applications" of container window to {480, 200}
        close
    end tell
end tell
APPLESCRIPT
    sed -e "s|@VOLUME@|$VOLUME_NAME|" "$WORK/layout.applescript" >"$WORK/layout-filled.applescript"
    # Finder automation can block on a permission prompt; never hang the build.
    if ! perl -e 'alarm 20; exec @ARGV' osascript "$WORK/layout-filled.applescript" >/dev/null 2>&1; then
        echo "warning: Finder layout was not applied (automation permission?); the image is still valid" >&2
    fi
    sync
fi

note "Detaching and compressing to UDBZ"
if ! hdiutil detach -quiet "$MNT" >/dev/null 2>&1; then
    hdiutil detach -force "$MNT" >/dev/null 2>&1 || fail "could not detach $MNT"
fi
MOUNT_POINT=""

CONVERTED="$WORK/converted"
hdiutil convert -quiet "$RW_IMAGE" -format UDBZ -ov -o "$CONVERTED" >/dev/null
BUILT_IMAGE="$CONVERTED.dmg"
[[ -f "$BUILT_IMAGE" ]] || fail "hdiutil convert produced no image"

# --- Verify before publishing -------------------------------------------------

note "Verifying the image checksum"
hdiutil verify -quiet "$BUILT_IMAGE" >/dev/null

note "Mounting the image read-only to check its contents"
mkdir -p "$VERIFY_MNT"
hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$VERIFY_MNT" "$BUILT_IMAGE" >/dev/null
VERIFY_MOUNT="$VERIFY_MNT"
[[ -d "$VERIFY_MNT/AgenTuna.app" ]] || fail "the image does not contain AgenTuna.app"
[[ -L "$VERIFY_MNT/Applications" ]] || fail "the image does not contain an Applications alias"
[[ "$(readlink "$VERIFY_MNT/Applications")" == "/Applications" ]] \
    || fail "the Applications alias does not point at /Applications"

visible_entries="$(find "$VERIFY_MNT" -mindepth 1 -maxdepth 1 ! -name '.*' -print | wc -l | tr -d '[:space:]')"
[[ "$visible_entries" == "2" ]] \
    || fail "the image shows $visible_entries visible entries; expected AgenTuna.app and Applications only"

hdiutil detach -quiet "$VERIFY_MNT" >/dev/null
VERIFY_MOUNT=""

if (( SIGN_DMG )); then
    note "Signing the image (identity: $SIGN_IDENTITY)"
    if [[ "$SIGN_IDENTITY" == "-" ]]; then
        codesign --force --sign - "$BUILT_IMAGE" >/dev/null 2>&1 \
            || fail "ad-hoc signing the dmg failed"
    else
        codesign --force --timestamp --sign "$SIGN_IDENTITY" "$BUILT_IMAGE" >/dev/null 2>&1 \
            || fail "signing the dmg with '$SIGN_IDENTITY' failed"
    fi
    codesign --verify "$BUILT_IMAGE" >/dev/null 2>&1 || fail "the signed dmg fails codesign --verify"
fi

# --- Publish atomically -------------------------------------------------------

CHECKSUM="$(shasum -a 256 "$BUILT_IMAGE" | awk '{print $1}')"
TMP_CHECKSUM="$WORK/checksum"
printf '%s  %s\n' "$CHECKSUM" "$IMAGE_NAME" >"$TMP_CHECKSUM"

note "Publishing $FINAL_IMAGE"
mv -f "$BUILT_IMAGE" "$FINAL_IMAGE"
mv -f "$TMP_CHECKSUM" "$CHECKSUM_FILE"

# The published checksum is the one just read back from the published image.
PUBLISHED="$(cd "$OUT_DIR" && shasum -a 256 "$IMAGE_NAME" | awk '{print $1}')"
[[ "$PUBLISHED" == "$CHECKSUM" ]] || fail "the published image does not match its checksum"

NOTE_SIGNED="ad-hoc"
(( SIGN_DMG )) || NOTE_SIGNED="unsigned"
echo "==> Built $FINAL_IMAGE"
echo "    volume:   $VOLUME_NAME"
echo "    arch:     $ARCH"
echo "    size:     $(du -h "$FINAL_IMAGE" | awk '{print $1}')"
echo "    sha256:   $CHECKSUM"
echo "    signature: $NOTE_SIGNED"
echo "    checksum: $CHECKSUM_FILE"
