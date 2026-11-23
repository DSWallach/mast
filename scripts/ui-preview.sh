#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
if [ "$(uname -s)" != Darwin ]; then
    echo 'UI preview requires the macOS GPUI headless Metal renderer; Linux is not supported yet.' >&2
    exit 1
fi
if [ "${1:-}" = "--serve" ]; then
    if [ "$#" -ne 2 ] || [ -z "$2" ]; then
        echo 'Usage: scripts/ui-preview.sh --serve REQUEST_DIRECTORY' >&2
        exit 2
    fi
    ulimit -n 4096
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
    exec python3 scripts/ui-preview-server.py "$2"
fi
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
ulimit -n 4096
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
cargo test -p mast-ui --features snapshot --test ui_preview render_ui_preview -- --ignored --nocapture --test-threads=1
