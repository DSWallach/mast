#!/usr/bin/env bash
# Test for the Linux branch of scripts/ui-preview.sh. `uname` and `cargo` are stubbed, so no
# build or GPU is needed: the stub cargo records the environment the preview would run in.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
script="$here/../ui-preview.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/ui-preview-linux-test.XXXXXX")
trap 'rm -r "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/icd"
cat > "$tmp/bin/uname" <<'STUB'
#!/bin/sh
if [ "${1:-}" = -m ]; then echo x86_64; else echo "${FAKE_UNAME:-Linux}"; fi
STUB
cat > "$tmp/bin/cargo" <<'STUB'
#!/bin/sh
{
  echo "args=$*"
  for name in DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR \
    XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME VK_DRIVER_FILES VK_ICD_FILENAMES; do
    eval "value=\${$name-<unset>}"
    echo "$name=$value"
  done
} > "$CARGO_RECORD"
exit "${CARGO_EXIT:-0}"
STUB
chmod +x "$tmp/bin/"*
export CARGO_RECORD="$tmp/record" CARGO_TARGET_DIR="$tmp/target"
fails=0
has() { # has <text> <label>
  if grep -qxF -- "$1" "$tmp/record"; then echo "ok: $2"; else echo "FAIL: $2 (missing '$1')"; sed 's/^/    /' "$tmp/record"; fails=$((fails + 1)); fi
}
run() { PATH="$tmp/bin:$PATH" "$script" "$@"; }

# A session with a display and bus: none of it may reach the render process.
out=$(DISPLAY=:0 WAYLAND_DISPLAY=wayland-0 WAYLAND_SOCKET=3 \
  DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus XDG_RUNTIME_DIR=/run/user/1000 \
  XDG_CONFIG_HOME=/home/user/.config VK_DRIVER_FILES=/driver/custom.json \
  run --light-only --screens chat 2>&1); status=$?
[ "$status" = 0 ] && echo "ok: exits 0" || { echo "FAIL: exit $status: $out"; fails=$((fails + 1)); }
has "args=test -p mast-ui --features snapshot --test ui_preview render_ui_preview -- --ignored --nocapture --test-threads=1" "runs the preview test"
for name in DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR; do
  has "$name=<unset>" "$name is unset for the render"
done
for name in XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME; do
  if grep -q "^$name=$tmp/target/ui-preview-home/" "$tmp/record"; then echo "ok: $name is inside the target directory"; else echo "FAIL: $name escapes the target directory"; sed 's/^/    /' "$tmp/record"; fails=$((fails + 1)); fi
done
[ -d "$tmp/target/ui-preview-home" ] && echo "ok: scratch home exists" || { echo "FAIL: scratch home missing"; fails=$((fails + 1)); }
has "VK_DRIVER_FILES=/driver/custom.json" "a caller's VK_DRIVER_FILES is kept"
has "VK_ICD_FILENAMES=<unset>" "a caller's drivers are not renamed for old loaders"

# The failing render's status propagates (exec leaves no wrapper to swallow it).
CARGO_EXIT=7 VK_DRIVER_FILES=/driver/custom.json run >/dev/null 2>&1; status=$?
[ "$status" = 7 ] && echo "ok: render failure status propagates" || { echo "FAIL: exit $status, want 7"; fails=$((fails + 1)); }

# Without VK_DRIVER_FILES the script pins this machine's lavapipe, not the 32-bit one.
: > "$tmp/icd/lvp_icd.i686.json"; : > "$tmp/icd/lvp_icd.x86_64.json"
out=$(MAST_PREVIEW_VULKAN_ICD_DIRS="$tmp/missing $tmp/icd" run 2>&1); status=$?
[ "$status" = 0 ] || { echo "FAIL: exit $status: $out"; fails=$((fails + 1)); }
has "VK_DRIVER_FILES=$tmp/icd/lvp_icd.x86_64.json" "lavapipe for this architecture is pinned"
has "VK_ICD_FILENAMES=$tmp/icd/lvp_icd.x86_64.json" "older loaders get the same driver"
rm "$tmp/icd/lvp_icd.x86_64.json"
out=$(MAST_PREVIEW_VULKAN_ICD_DIRS="$tmp/icd" run 2>&1); status=$?
[ "$status" = 0 ] || { echo "FAIL: exit $status: $out"; fails=$((fails + 1)); }
has "VK_DRIVER_FILES=$tmp/icd/lvp_icd.i686.json" "any lavapipe is the fallback"
rm "$tmp/icd/lvp_icd.i686.json"
out=$(MAST_PREVIEW_VULKAN_ICD_DIRS="$tmp/icd" run 2>&1); status=$?
case "$out" in
  *lavapipe*) [ "$status" = 1 ] && echo "ok: missing lavapipe is explained" || { echo "FAIL: exit $status, want 1"; fails=$((fails + 1)); } ;;
  *) echo "FAIL: missing lavapipe not explained: $out"; fails=$((fails + 1)) ;;
esac

# --help does not set anything up, and other systems are refused.
rm -rf "$tmp/target"
run --help >/dev/null 2>&1
[ ! -e "$tmp/target/ui-preview-home" ] && echo "ok: --help creates nothing" || { echo "FAIL: --help created the scratch home"; fails=$((fails + 1)); }
out=$(FAKE_UNAME=Plan9 run 2>&1); status=$?
if [ "$status" = 1 ] && [[ "$out" == *"Metal or the Linux Vulkan"* ]]; then echo "ok: unsupported systems are refused"; else echo "FAIL: unsupported system: exit $status: $out"; fails=$((fails + 1)); fi

[ "$fails" = 0 ] && echo "all passed" || { echo "$fails failure(s)"; exit 1; }
