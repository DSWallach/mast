#!/usr/bin/env bash
# Install a built Mast binary, its icons, and a desktop entry into the current
# user's ~/.local hierarchy for Linux desktop launchers.
#
# Usage: scripts/install-linux-app.sh [path/to/mast]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BIN="${1:-$TARGET_DIR/installed/mast}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_DIR="$HOME/.local/bin"

if [[ ! -x "$BIN" ]]; then
  echo "Binary not found or not executable: $BIN" >&2
  echo "Build it first with: cargo build --profile installed -p mast" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
STAGED_BIN="$(mktemp "$BIN_DIR/.mast.XXXXXXXX")"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGED_BIN" "$TMP_DIR"' EXIT

install -m 755 "$BIN" "$STAGED_BIN"
mv -f "$STAGED_BIN" "$BIN_DIR/mast"

for size in 16 32 64 128 256 512; do
  install -Dm644 "$ROOT/dist/brand/mast-icon-${size}.png" \
    "$DATA_DIR/icons/hicolor/${size}x${size}/apps/mast.png"
done
install -Dm644 "$ROOT/dist/brand/mast-icon.svg" \
  "$DATA_DIR/icons/hicolor/scalable/apps/mast.svg"

cat > "$TMP_DIR/mast.desktop" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=Mast
GenericName=Agent Workspace
Comment=Run a team of coding agents from one outline of your work
Exec=$BIN_DIR/mast %U
TryExec=$BIN_DIR/mast
Icon=mast
Terminal=false
Categories=Development;IDE;
Keywords=mast;ai;agent;claude;codex;coding;
MimeType=x-scheme-handler/mast;
StartupNotify=true
StartupWMClass=zeron
EOF

mkdir -p "$DATA_DIR/applications"
if command -v desktop-file-install >/dev/null 2>&1; then
  desktop-file-install --dir="$DATA_DIR/applications" "$TMP_DIR/mast.desktop"
else
  install -Dm644 "$TMP_DIR/mast.desktop" "$DATA_DIR/applications/mast.desktop"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DATA_DIR/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$DATA_DIR/icons/hicolor" || true
fi

echo "Installed $BIN_DIR/mast and $DATA_DIR/applications/mast.desktop"
