#!/usr/bin/env bash
# Builds target/release/AgenTuna.app for the host architecture (or both with --universal).
#
# The bundle contains:
#   Contents/Info.plist                     validated with plutil
#   Contents/MacOS/agentuna-app             stripped, 0755
#   Contents/Resources/AppIcon.icns
#   Contents/Resources/bin/agentuna         the packaged CLI installed by the app
#
# Optional environment overrides:
#   AGENTUNA_ICON           path to the .icns copied to AppIcon.icns
#   AGENTUNA_PLIST_TEMPLATE path to an Info.plist template; @VERSION@ and @BUILD@ are
#                           substituted, then the file is validated
#   CARGO_TARGET_DIR        target directory (defaults to <repo>/target)
#
# Signing, notarization and DMG creation are separate stories (TF-030, TF-031).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
APP_NAME="AgenTuna"
APP_BUNDLE="$TARGET_DIR/release/$APP_NAME.app"
ICON="${AGENTUNA_ICON:-$ROOT/crates/at-app/icons/icon.icns}"
PLIST_TEMPLATE="${AGENTUNA_PLIST_TEMPLATE:-}"
UNIVERSAL=0

usage() {
    cat <<EOF
Usage: scripts/bundle.sh [--universal]

Builds $APP_BUNDLE and validates the result.
  --universal  build arm64 + x86_64 and lipo them into one universal binary
  -h, --help   show this help
EOF
}

while (( $# > 0 )); do
    case "$1" in
        --universal) UNIVERSAL=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

fail() {
    echo "error: $*" >&2
    exit 1
}

# --- Preconditions -----------------------------------------------------------

[[ -f "$ROOT/Cargo.toml" ]] || fail "not inside the AgenTuna repository (no Cargo.toml)"
[[ -f "$ICON" ]] || fail "missing app icon: $ICON"
[[ -d "$ROOT/ui/node_modules" ]] || fail "UI dependencies missing; run: pnpm --dir ui install --frozen-lockfile"

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/p' "$ROOT/Cargo.toml" \
    | sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1)"
[[ -n "$VERSION" ]] || fail "missing workspace version in $ROOT/Cargo.toml"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "workspace version is not semver: $VERSION"

IFS=. read -r VERSION_MAJOR VERSION_MINOR VERSION_PATCH <<<"$VERSION"
# Numeric, strictly increasing with the semver: 0.1.0 -> 1000, 0.1.1 -> 1001, 1.0.0 -> 1000000.
BUILD_NUMBER="$((10#$VERSION_MAJOR * 1000000 + 10#$VERSION_MINOR * 1000 + 10#$VERSION_PATCH))"
(( BUILD_NUMBER > 0 )) || fail "computed CFBundleVersion is not positive: $BUILD_NUMBER"

# --- Build -------------------------------------------------------------------

echo "==> Building UI assets"
(cd "$ROOT" && pnpm --dir ui build)

build_host() {
    (cd "$ROOT" && cargo build --release --locked -p at-app -p at-cli)
}

build_target() {
    local triple="$1"
    (cd "$ROOT" && cargo build --release --locked --target "$triple" -p at-app -p at-cli)
}

APP_BINARIES=()
CLI_BINARIES=()

if (( UNIVERSAL )); then
    echo "==> Building universal binaries (arm64 + x86_64)"
    UNIVERSAL_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
    for triple in "${UNIVERSAL_TARGETS[@]}"; do
        if command -v rustup >/dev/null 2>&1; then
            rustup target list --installed | grep -qx "$triple" \
                || fail "Rust target $triple is not installed; run: rustup target add $triple"
        fi
    done
    for triple in "${UNIVERSAL_TARGETS[@]}"; do
        echo "==> cargo build --release --target $triple"
        build_target "$triple"
        APP_BINARIES+=("$TARGET_DIR/$triple/release/agentuna-app")
        CLI_BINARIES+=("$TARGET_DIR/$triple/release/agentuna")
    done
else
    echo "==> Building host release binaries"
    build_host
    APP_BINARIES+=("$TARGET_DIR/release/agentuna-app")
    CLI_BINARIES+=("$TARGET_DIR/release/agentuna")
fi

for binary in "${APP_BINARIES[@]}" "${CLI_BINARIES[@]}"; do
    [[ -f "$binary" ]] || fail "missing built binary: $binary"
done

# --- Assemble the bundle -----------------------------------------------------

echo "==> Assembling $APP_BUNDLE"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources/bin"

cp "${APP_BINARIES[0]}" "$APP_BUNDLE/Contents/MacOS/agentuna-app"
cp "${CLI_BINARIES[0]}" "$APP_BUNDLE/Contents/Resources/bin/agentuna"

if (( UNIVERSAL )); then
    lipo -create -output "$APP_BUNDLE/Contents/MacOS/agentuna-app" "${APP_BINARIES[@]}"
    lipo -create -output "$APP_BUNDLE/Contents/Resources/bin/agentuna" "${CLI_BINARIES[@]}"
fi

chmod 0755 "$APP_BUNDLE/Contents/MacOS/agentuna-app" "$APP_BUNDLE/Contents/Resources/bin/agentuna"
strip "$APP_BUNDLE/Contents/MacOS/agentuna-app"
strip "$APP_BUNDLE/Contents/Resources/bin/agentuna"

cp "$ICON" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
chmod 0644 "$APP_BUNDLE/Contents/Resources/AppIcon.icns"

for font_dir in "$ROOT/ui/src/fonts" "$ROOT/design/fonts" "$ROOT/crates/at-app/fonts"; do
    if [[ -d "$font_dir" ]]; then
        mkdir -p "$APP_BUNDLE/Contents/Resources/fonts"
        cp "$font_dir"/*.ttf "$font_dir"/*.otf "$APP_BUNDLE/Contents/Resources/fonts/" 2>/dev/null || true
    fi
done

PLIST="$APP_BUNDLE/Contents/Info.plist"
if [[ -n "$PLIST_TEMPLATE" ]]; then
    [[ -f "$PLIST_TEMPLATE" ]] || fail "missing Info.plist template: $PLIST_TEMPLATE"
    sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD_NUMBER/g" "$PLIST_TEMPLATE" >"$PLIST"
else
    cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleExecutable</key>
    <string>agentuna-app</string>
    <key>CFBundleIdentifier</key>
    <string>com.agentuna.app</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$BUILD_NUMBER</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 AgenTuna Contributors</string>
</dict>
</plist>
EOF
fi
chmod 0644 "$PLIST"

# --- Validation --------------------------------------------------------------

echo "==> Validating Info.plist"
if ! plutil -lint "$PLIST" >/dev/null; then
    fail "invalid Info.plist: $PLIST"
fi

echo "==> Checking linked libraries"
check_dylibs() {
    local binary="$1"
    local offending
    offending="$(otool -L "$binary" | tail -n +2 | awk '{print $1}' \
        | grep -Ev '^(/usr/lib/|/System/|/Library/Apple/)' || true)"
    if [[ -n "$offending" ]]; then
        fail "$binary links non-system libraries (add them to Contents/Frameworks): $offending"
    fi
    if otool -l "$binary" | grep -q __debug_info; then
        fail "$binary still contains debug information; strip failed"
    fi
}
check_dylibs "$APP_BUNDLE/Contents/MacOS/agentuna-app"
check_dylibs "$APP_BUNDLE/Contents/Resources/bin/agentuna"

EXPECTED_VERSION="agentuna $VERSION"
REPORTED_VERSION="$("$APP_BUNDLE/Contents/Resources/bin/agentuna" --version)"
[[ "$REPORTED_VERSION" == "$EXPECTED_VERSION" ]] \
    || fail "bundled CLI reports '$REPORTED_VERSION', expected '$EXPECTED_VERSION'"

SIZE_KB="$(du -sk "$APP_BUNDLE" | awk '{print $1}')"
(( SIZE_KB < 60 * 1024 )) || fail "bundle is ${SIZE_KB} KiB, over the 60 MB budget"

echo "==> Built $APP_BUNDLE"
echo "    CFBundleShortVersionString: $VERSION"
echo "    CFBundleVersion: $BUILD_NUMBER"
echo "    CLI: $APP_BUNDLE/Contents/Resources/bin/agentuna --version -> $REPORTED_VERSION"
echo "    Size: ${SIZE_KB} KiB"
