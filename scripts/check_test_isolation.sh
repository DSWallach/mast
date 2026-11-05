#!/bin/sh
# Check test sources for home-dependent paths and missing process isolation.
set -eu
cd "$(dirname "$0")/.."
exec python3 scripts/check_test_isolation.py
