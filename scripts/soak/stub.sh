#!/bin/sh
# Fail-closed stand-in for gh, claude, codex, security, ... during perf-soak.
printf '%s\n' "$(basename "$0")" >> "${MAST_SOAK_STUB_LOG:-/dev/null}"
echo "soak: $(basename "$0") is disabled" >&2
exit 127
