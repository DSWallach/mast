#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <built binary> <app path> <bundle id>" >&2
  exit 2
fi

binary=$1
app=$2
bundle_id=$3
contents="$app/Contents"
plist="$contents/Info.plist"
plist_buddy=/usr/libexec/PlistBuddy
lsregister=${MAST_LSREGISTER:-/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister}
codesign_command=${MAST_CODESIGN:-codesign}

[[ -f "$binary" && -f "$plist" && -d "$contents/MacOS" && "$bundle_id" =~ ^[A-Za-z0-9.-]+$ ]] || {
  echo "Binary, app bundle, or bundle identifier is invalid" >&2
  exit 1
}

staged_binary=$(mktemp "$contents/MacOS/.mast.XXXXXXXX")
trap 'rm -f "$staged_binary"' EXIT
install -m 755 "$binary" "$staged_binary"
mv -f "$staged_binary" "$contents/MacOS/mast"
ln -sfn mast "$contents/MacOS/zeron"

"$plist_buddy" -c 'Set :CFBundleExecutable mast' "$plist"
"$plist_buddy" -c "Set :CFBundleIdentifier $bundle_id" "$plist"

# Preserve existing URL types and schemes, adding missing schemes to the first
# URL type. Dev bundles may not have any URL types yet.
"$plist_buddy" -c 'Print :CFBundleURLTypes' "$plist" >/dev/null 2>&1 || \
  "$plist_buddy" -c 'Add :CFBundleURLTypes array' "$plist"
"$plist_buddy" -c 'Print :CFBundleURLTypes:0' "$plist" >/dev/null 2>&1 || \
  "$plist_buddy" -c 'Add :CFBundleURLTypes:0 dict' "$plist"
"$plist_buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes' "$plist" >/dev/null 2>&1 || \
  "$plist_buddy" -c 'Add :CFBundleURLTypes:0:CFBundleURLSchemes array' "$plist"
for scheme in mast zeron; do
  if ! "$plist_buddy" -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes' "$plist" | \
    awk -v scheme="$scheme" '$1 == scheme { found = 1 } END { exit !found }'; then
    "$plist_buddy" -c "Add :CFBundleURLTypes:0:CFBundleURLSchemes: string $scheme" "$plist"
  fi
done

"$codesign_command" --force --sign - --identifier "$bundle_id" "$app"
"$lsregister" -f "$app"
