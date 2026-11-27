#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
directory=$(mktemp -d "${TMPDIR:-/tmp}/mast-install-test.XXXXXXXX")
trap 'rm -r "$directory"' EXIT
app="$directory/Test.app"
mkdir -p "$app/Contents/MacOS"
cp "$root/dist/macos/Info-dev.plist" "$app/Contents/Info.plist"
printf 'test binary\n' > "$directory/binary"
printf 'previous binary\n' > "$app/Contents/MacOS/zeron"  # stale pre-0.3.0 alias
cat > "$directory/codesign" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$MAST_TEST_DIRECTORY/codesign-args"
EOF
cat > "$directory/lsregister" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$MAST_TEST_DIRECTORY/lsregister-args"
EOF
chmod +x "$directory/codesign" "$directory/lsregister"
export MAST_TEST_DIRECTORY="$directory"
export MAST_CODESIGN="$directory/codesign" MAST_LSREGISTER="$directory/lsregister"

# The perf gate: no stamp means no install, and nothing is copied.
export MAST_PERF_GATE_DIR="$directory/no-stamps" MAST_PERF_GATE_OVERRIDES_FILE="$directory/overrides.md"
if bash "$root/scripts/install-macos-app.sh" "$directory/binary" "$app" computer.mikey.mast.dev 2>/dev/null; then
  echo 'install without a perf-gate stamp must be refused' >&2; exit 1
fi
[[ $(cat "$app/Contents/MacOS/zeron") == 'previous binary' ]]
if bash "$root/scripts/install-macos-app.sh" "$directory/binary" "$app" computer.mikey.mast.dev --skip-perf-gate "  " 2>/dev/null; then
  echo 'an empty override reason must be refused' >&2; exit 1
fi

for iteration in 1 2; do
  bash "$root/scripts/install-macos-app.sh" "$directory/binary" "$app" computer.mikey.mast.dev --skip-perf-gate "installer test"
done
[[ $(grep -c 'installer test' "$directory/overrides.md") == 2 ]]

cmp "$directory/binary" "$app/Contents/MacOS/mast"
[[ ! -e "$app/Contents/MacOS/zeron" && ! -L "$app/Contents/MacOS/zeron" ]]  # the alias is removed
plist="$app/Contents/Info.plist"
buddy=/usr/libexec/PlistBuddy
[[ $("$buddy" -c 'Print :CFBundleExecutable' "$plist") == mast ]]
[[ $("$buddy" -c 'Print :CFBundleIdentifier' "$plist") == computer.mikey.mast.dev ]]
[[ $("$buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:0' "$plist") == mast ]]
! "$buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:1' "$plist" >/dev/null 2>&1
printf '%s\n' --force --sign - --identifier computer.mikey.mast.dev "$app" | cmp - "$directory/codesign-args"
printf '%s\n' -f "$app" | cmp - "$directory/lsregister-args"
echo 'macOS installer test passed'
