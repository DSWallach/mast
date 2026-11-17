#!/bin/sh
# Emit provenance for the checkout invoking a build, independently of Cargo's cache.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
revision=$(git -C "$root" rev-parse HEAD 2>/dev/null) || revision=unknown
if [ "$revision" != unknown ] && [ -n "$(git -C "$root" status --porcelain --untracked-files=no)" ]; then
    revision="$revision-dirty"
fi
printf '%s\n' "$revision"
