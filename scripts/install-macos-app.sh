#!/usr/bin/env bash
set -euo pipefail

usage="Usage: $0 <built binary> <app path> <bundle id> [--skip-perf-gate \"<reason>\"]"
skip_reason=""
positional=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-perf-gate)
      [[ $# -ge 2 && -n "${2// }" ]] || { echo "--skip-perf-gate needs a reason" >&2; exit 2; }
      skip_reason=$2; shift 2 ;;
    *) positional+=("$1"); shift ;;
  esac
done
if [[ ${#positional[@]} -ne 3 ]]; then
  echo "$usage" >&2
  exit 2
fi
set -- "${positional[@]}"

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

# Perf gate (HANDOFF "Perf gate before every install"): a stamp for HEAD with release
# tests and a real-data soak of this exact binary, or an explicit logged override.
gate="$(cd "$(dirname "$0")" && pwd)/perf-gate.py"
if [[ -n "$skip_reason" ]]; then
  python3 -I "$gate" override --reason "$skip_reason"
else
  python3 -I "$gate" check --binary "$binary" || {
    echo "Install refused by the perf gate. Fix it, or: $0 ... --skip-perf-gate \"<reason>\"" >&2
    exit 1
  }
fi

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
