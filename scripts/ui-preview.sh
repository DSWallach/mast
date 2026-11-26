#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
case "$(uname -s)" in
    Darwin|Linux) ;;
    *)
        echo 'UI preview needs the macOS Metal or the Linux Vulkan offscreen renderer.' >&2
        exit 1
        ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"

# Linux renders offscreen through Vulkan: no window, compositor or desktop service. Drop the
# session's display and bus variables so nothing in the process can reach them, and move the
# XDG directories under the target directory so nothing is written under ~/.config, ~/.local
# or ~/.cache. Render with the software driver (lavapipe) unless the caller chose drivers
# with VK_DRIVER_FILES: pixels are then the same on every host, and a vendor driver can't
# probe the system D-Bus while the Vulkan loader scans it.
prepare_linux() {
    [ "$(uname -s)" = Linux ] || return 0
    unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR
    preview_home=$CARGO_TARGET_DIR/ui-preview-home
    mkdir -p "$preview_home"
    XDG_CONFIG_HOME=$preview_home/config
    XDG_DATA_HOME=$preview_home/data
    XDG_CACHE_HOME=$preview_home/cache
    XDG_STATE_HOME=$preview_home/state
    export XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME
    if [ -z "${VK_DRIVER_FILES:-}" ]; then
        # The 64-bit loader can't load lvp_icd.i686.json, so name this machine's driver first.
        # MAST_PREVIEW_VULKAN_ICD_DIRS (space separated) replaces the search path; it exists for
        # scripts/tests/test-ui-preview-linux.sh.
        for icd in "lvp_icd.$(uname -m).json" 'lvp_icd*.json'; do
            for dir in ${MAST_PREVIEW_VULKAN_ICD_DIRS:-/usr/share/vulkan/icd.d /etc/vulkan/icd.d /usr/local/share/vulkan/icd.d}; do
                for candidate in "$dir"/$icd; do
                    if [ -f "$candidate" ]; then VK_DRIVER_FILES=$candidate; break 3; fi
                done
            done
        done
        if [ -z "${VK_DRIVER_FILES:-}" ]; then
            echo 'UI preview on Linux needs the lavapipe Vulkan driver (package mesa-vulkan-drivers), or set VK_DRIVER_FILES to a Vulkan driver.' >&2
            exit 1
        fi
        export VK_DRIVER_FILES
        # Loaders older than 1.3.207 read the previous name.
        VK_ICD_FILENAMES=$VK_DRIVER_FILES
        export VK_ICD_FILENAMES
    fi
}
if [ "${1:-}" = "--serve" ]; then
    if [ "$#" -ne 2 ] || [ -z "$2" ]; then
        echo 'Usage: scripts/ui-preview.sh --serve REQUEST_DIRECTORY' >&2
        exit 2
    fi
    ulimit -n 4096 2>/dev/null || true
    prepare_linux
    exec python3 scripts/ui-preview-server.py "$2"
fi
# Capture the original flags before translating them into renderer settings.
preview_command=scripts/ui-preview.sh
for arg do
    quoted=$(printf '%s' "$arg" | sed "s/'/'\\\\''/g")
    preview_command="$preview_command '$quoted'"
done
while [ "$#" -gt 0 ]; do
    case "$1" in
        --themes|--screens|--styles|--layouts)
            option=$1
            if [ "$#" -lt 2 ] || [ -z "$2" ]; then
                echo "Missing value for $option" >&2
                exit 2
            fi
            case "$option" in
                --themes) MAST_PREVIEW_THEMES=$2; export MAST_PREVIEW_THEMES ;;
                --screens) MAST_PREVIEW_SCREENS=$2; export MAST_PREVIEW_SCREENS ;;
                --styles) MAST_PREVIEW_STYLES=$2; export MAST_PREVIEW_STYLES ;;
                --layouts) MAST_PREVIEW_LAYOUTS=$2; export MAST_PREVIEW_LAYOUTS ;;
            esac
            shift 2
            ;;
        --light-only) MAST_PREVIEW_LIGHT_ONLY=1; export MAST_PREVIEW_LIGHT_ONLY; shift ;;
        -h|--help)
            echo 'Usage: scripts/ui-preview.sh [--light-only] [--themes a,b] [--screens chat,details,...] [--styles default,...] [--layouts minimal,...] | --serve REQUEST_DIRECTORY'
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done
ulimit -n 4096 2>/dev/null || true
prepare_linux
export MAST_UI_COMMAND="$preview_command"
exec cargo test -p mast-ui --features snapshot --test ui_preview render_ui_preview -- --ignored --nocapture --test-threads=1
