#!/bin/sh
set -eu
printf '%s\n' 'MAST_TEST_DISABLE_NATIVE_WATCHERS=1' > "${NEXTEST_ENV:?nextest setup environment required}"
