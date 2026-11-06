#!/usr/bin/env bash
# Installs (or removes) the Linux desktop integration described by
# `crates/at-app/src/app/linux.rs` (TF-039), without any packaging:
#
#   <data>/applications/agentuna.desktop
#   <data>/icons/hicolor/<size>x<size>/apps/agentuna.png   (32, 128, 256)
#   <bin>/agentuna-app                                     (the binary the entry launches)
#
# Paths follow XDG: XDG_DATA_HOME (default ~/.local/share) plus the app directory
# `agentuna`, and XDG_BIN_HOME (default ~/.local/bin) for the binary.
#
# The desktop entry is validated with `desktop-file-validate` when that tool is
# installed; elsewhere the script checks the required keys itself. Existing files are
# replaced; --uninstall removes only the files this script installs.
#
# AppImage and .deb packaging are a documented follow-up, not part of this script.
#
# Options:
#   --data-home DIR   XDG data home (default $XDG_DATA_HOME or ~/.local/share)
#   --bin-dir DIR     where to install the binary (default $XDG_BIN_HOME or ~/.local/bin)
#   --binary PATH     binary to install (default target/release/agentuna-app)
#   --no-binary       install only the desktop entry and icons
#   --uninstall       remove the installed files
#   --dry-run         print every action without changing anything
#   -h, --help        show this help

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="agentuna"
SRC_ICONS="$ROOT/crates/at-app/icons"

DATA_BASE=""
BIN_DIR=""
BINARY="$ROOT/target/release/agentuna-app"
INSTALL_BINARY=1
UNINSTALL=0
DRY_RUN=0

usage() {
    cat <<EOF
Usage: scripts/install-linux.sh [options]

Installs the AgenTuna desktop entry, hicolor icons and (unless --no-binary) the
agentuna-app binary. --uninstall removes exactly those files.

  --data-home DIR   XDG data home (default \$XDG_DATA_HOME or ~/.local/share)
  --bin-dir DIR     binary directory (default \$XDG_BIN_HOME or ~/.local/bin)
  --binary PATH     binary to install (default target/release/agentuna-app)
  --no-binary       skip the binary; the desktop entry then expects it on PATH
  --uninstall       remove the desktop entry and icons
  --dry-run         print the actions without writing anything
  -h, --help        show this help
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
        --data-home)
            [[ $# -ge 2 ]] || fail "--data-home needs a path"
            DATA_BASE="$2"
            shift 2
            ;;
        --bin-dir)
            [[ $# -ge 2 ]] || fail "--bin-dir needs a path"
            BIN_DIR="$2"
            shift 2
            ;;
        --binary)
            [[ $# -ge 2 ]] || fail "--binary needs a path"
            BINARY="$2"
            shift 2
            ;;
        --no-binary) INSTALL_BINARY=0; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# --- XDG destinations ---------------------------------------------------------

home="${HOME:-}"
[[ -n "$home" ]] || fail "HOME is not set"

xdg_dir() {
    local variable="$1"
    local fallback="$2"
    local value="${!variable:-}"
    # The XDG spec says a relative value is invalid and must be ignored.
    if [[ -n "$value" && "$value" == /* ]]; then
        printf '%s' "$value"
    else
        printf '%s/%s' "$home" "$fallback"
    fi
}

if [[ -z "$DATA_BASE" ]]; then
    DATA_BASE="$(xdg_dir XDG_DATA_HOME ".local/share")"
fi
if [[ "$DATA_BASE" != /* ]]; then
    fail "--data-home must be an absolute path: $DATA_BASE"
fi
if [[ -z "$BIN_DIR" ]]; then
    BIN_DIR="$(xdg_dir XDG_BIN_HOME ".local/bin")"
fi
if [[ "$BIN_DIR" != /* ]]; then
    fail "--bin-dir must be an absolute path: $BIN_DIR"
fi

DATA_HOME="$DATA_BASE/$APP_NAME"
DESKTOP_TARGET="$DATA_HOME/applications/agentuna.desktop"
BIN_TARGET="$BIN_DIR/agentuna-app"

ICON_SOURCES=(32x32.png 128x128.png 128x128@2x.png)
ICON_SIZES=(32 128 256)

validate_desktop_entry() {
    local file="$1"
    if command -v desktop-file-validate >/dev/null 2>&1; then
        desktop-file-validate "$file"
        return
    fi
    # No validator installed: check the keys the desktop menu needs.
    grep -qxF '[Desktop Entry]' "$file" || fail "missing [Desktop Entry] group in $file"
    local key
    for key in Type Name Exec Icon Terminal Categories; do
        grep -Eq "^${key}=.+" "$file" || fail "missing $key in $file"
    done
    grep -Eq '^Categories=[A-Za-z0-9-]+;$' "$file" || fail 'Categories must end with a semicolon'
    echo "note: desktop-file-validate is not installed; checked the required keys instead"
}

refresh_caches() {
    local applications="$DATA_HOME/applications"
    local hicolor="$DATA_HOME/icons/hicolor"
    if [[ -d "$applications" ]] && command -v update-desktop-database >/dev/null 2>&1; then
        run update-desktop-database -q "$applications" || true
    fi
    if [[ -d "$hicolor" ]] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
        run gtk-update-icon-cache -q -t -f "$hicolor" || true
    fi
}

# --- Uninstall ----------------------------------------------------------------

if (( UNINSTALL )); then
    note "Removing the AgenTuna desktop integration"
    for size in "${ICON_SIZES[@]}"; do
        icon="$DATA_HOME/icons/hicolor/${size}x${size}/apps/agentuna.png"
        if [[ -e "$icon" ]]; then
            run rm -f "$icon"
        fi
        if (( ! DRY_RUN )); then
            rmdir "$DATA_HOME/icons/hicolor/${size}x${size}/apps" 2>/dev/null || true
            rmdir "$DATA_HOME/icons/hicolor/${size}x${size}" 2>/dev/null || true
        fi
    done
    if [[ -e "$DESKTOP_TARGET" ]]; then
        run rm -f "$DESKTOP_TARGET"
    fi
    # Generated by update-desktop-database when it is installed.
    if [[ -e "$DATA_HOME/applications/mimeinfo.cache" ]]; then
        run rm -f "$DATA_HOME/applications/mimeinfo.cache"
    fi
    if (( ! DRY_RUN )); then
        rmdir "$DATA_HOME/applications" 2>/dev/null || true
    fi
    if (( INSTALL_BINARY )) && [[ -e "$BIN_TARGET" ]]; then
        run rm -f "$BIN_TARGET"
    fi
    if (( ! DRY_RUN )); then
        rmdir "$DATA_HOME/icons/hicolor" 2>/dev/null || true
        rmdir "$DATA_HOME/icons" 2>/dev/null || true
        rmdir "$DATA_HOME" 2>/dev/null || true
    fi
    refresh_caches
    if (( DRY_RUN )); then
        note "Uninstalled (dry run)"
    else
        note "Uninstalled"
    fi
    exit 0
fi

# --- Preconditions ------------------------------------------------------------

[[ -d "$SRC_ICONS" ]] || fail "missing icon sources: $SRC_ICONS"
for source in "${ICON_SOURCES[@]}"; do
    [[ -f "$SRC_ICONS/$source" ]] || fail "missing icon: $SRC_ICONS/$source"
done
if (( INSTALL_BINARY )); then
    [[ -f "$BINARY" ]] || fail "missing release binary: $BINARY (run: cargo build --release -p at-app, or pass --no-binary)"
    [[ -x "$BINARY" ]] || fail "not executable: $BINARY"
fi

# --- Install ------------------------------------------------------------------

note "Installing the AgenTuna desktop integration under $DATA_HOME"
run install -d -m 0755 "$DATA_HOME/applications"
for index in "${!ICON_SIZES[@]}"; do
    size="${ICON_SIZES[$index]}"
    source="${ICON_SOURCES[$index]}"
    run install -d -m 0755 "$DATA_HOME/icons/hicolor/${size}x${size}/apps"
    run install -m 0644 "$SRC_ICONS/$source" "$DATA_HOME/icons/hicolor/${size}x${size}/apps/agentuna.png"
done

if (( DRY_RUN )); then
    echo "+ write $DESKTOP_TARGET"
else
    cat >"$DESKTOP_TARGET" <<'AGENTUNA_DESKTOP_ENTRY'
[Desktop Entry]
Type=Application
Version=1.5
Name=AgenTuna
GenericName=AI Configuration Studio
Comment=Manage agent skills, MCP servers and configuration across coding agents
Exec=agentuna-app
Icon=agentuna
Terminal=false
Categories=Development;
Keywords=agent;config;mcp;skills;
StartupNotify=true
AGENTUNA_DESKTOP_ENTRY
    chmod 0644 "$DESKTOP_TARGET"
    validate_desktop_entry "$DESKTOP_TARGET"
fi

if (( INSTALL_BINARY )); then
    run install -d -m 0755 "$BIN_DIR"
    run install -m 0755 "$BINARY" "$BIN_TARGET"
fi

refresh_caches

note "Installed $DESKTOP_TARGET"
for size in "${ICON_SIZES[@]}"; do
    echo "    icon ${size}x${size}: $DATA_HOME/icons/hicolor/${size}x${size}/apps/agentuna.png"
done
if (( INSTALL_BINARY )); then
    echo "    binary: $BIN_TARGET"
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) echo "    note: $BIN_DIR is not on PATH; the menu entry needs it to launch agentuna-app" ;;
    esac
fi
if (( DRY_RUN )); then
    note "Dry run: nothing was written"
fi
