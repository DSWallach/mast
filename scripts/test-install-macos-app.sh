#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
directory=$(mktemp -d "${TMPDIR:-/tmp}/mast-install-test.XXXXXXXX")
trap 'rm -r "$directory"' EXIT
app="$directory/Test.app"
mkdir -p "$app/Contents/MacOS"
cp "$root/dist/macos/Info-dev.plist" "$app/Contents/Info.plist"
printf 'test binary\n' > "$directory/binary"
printf 'previous binary\n' > "$app/Contents/MacOS/zeron"
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

for iteration in 1 2; do
  bash "$root/scripts/install-macos-app.sh" "$directory/binary" "$app" computer.mikey.mast.dev
done

cmp "$directory/binary" "$app/Contents/MacOS/mast"
[[ $(readlink "$app/Contents/MacOS/zeron") == mast ]]
plist="$app/Contents/Info.plist"
buddy=/usr/libexec/PlistBuddy
[[ $("$buddy" -c 'Print :CFBundleExecutable' "$plist") == mast ]]
[[ $("$buddy" -c 'Print :CFBundleIdentifier' "$plist") == computer.mikey.mast.dev ]]
[[ $("$buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:0' "$plist") == mast ]]
[[ $("$buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:1' "$plist") == zeron ]]
! "$buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:2' "$plist" >/dev/null 2>&1
printf '%s\n' --force --sign - --identifier computer.mikey.mast.dev "$app" | cmp - "$directory/codesign-args"
printf '%s\n' -f "$app" | cmp - "$directory/lsregister-args"
echo 'macOS installer test passed'
