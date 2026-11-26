#!/usr/bin/env bash
# Unit test for scripts/perf-gate.py (stamp write/check/override) on a throwaway git repo.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
gate="$here/../perf-gate.py"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/perf-gate-test.XXXXXX")
trap 'rm -r "$tmp"' EXIT
repo="$tmp/repo"; mkdir -p "$repo" && cd "$repo"
git init -q -b main . && git config user.email t@example.invalid && git config user.name t
mkdir -p docs crates/a/src crates/a/tests && echo 1 > docs/a.md && echo 1 > crates/a/src/lib.rs && echo 1 > crates/a/tests/t.rs
git add -A && git commit -q -m base
export MAST_PERF_GATE_REPO="$repo" MAST_PERF_GATE_OVERRIDES_FILE="$tmp/overrides.md"
sha256_of() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"; else sha256sum "$1"; fi | cut -d' ' -f1; }
printf 'binary' > "$tmp/bin"; digest=$(sha256_of "$tmp/bin")
fails=0
expect() { # expect <0|1|3> <label> cmd...
  local want=$1 label=$2; shift 2
  out=$("$@" 2>&1); got=$?
  if [ "$got" != "$want" ]; then echo "FAIL: $label (exit $got, want $want): $out"; fails=$((fails+1)); else echo "ok: $label"; fi
}
has() { case "$out" in *"$1"*) ;; *) echo "FAIL: output lacks '$1': $out"; fails=$((fails+1)) ;; esac; }
G() { python3 -I "$gate" "$@"; }
tests='{"result":"pass","selection":"x","budget_tests":[{"name":"b","result":"pass"}]}'
soak() { echo "{\"pass\": $1, \"mode\": \"$2\", \"candidate_sha256\": \"$digest\", \"baseline\": {\"cpu_pct\": 1}}"; }

expect 1 "no stamp refuses" G check
has "no stamp for HEAD"
G write --section release_tests --json "$tests" >/dev/null
expect 1 "release tests only refuses" G check
has "no passing soak"
G write --section soak --json "$(soak false real)" >/dev/null
expect 1 "failed soak refuses" G check
G write --section soak --json "$(soak true synthetic)" >/dev/null
expect 1 "synthetic soak refuses" G check
has "needs --real"
G write --section soak --json "$(soak true real)" >/dev/null
expect 0 "full stamp passes" G check
expect 0 "matching binary passes" G check --binary "$tmp/bin"
printf 'other' > "$tmp/other"
expect 1 "different binary refuses" G check --binary "$tmp/other"
has "not the one that was soaked"

stamped=$(git rev-parse HEAD)
echo 2 >> docs/a.md; echo 2 >> crates/a/tests/t.rs; git commit -qam "tests and docs only"
expect 0 "docs/tests-only commit after stamp passes" G check --binary "$tmp/bin"
has "docs/tests only"
echo 2 >> crates/a/src/lib.rs; git commit -qam "source change"
expect 1 "source change after stamp refuses" G check
has "stale"; has "crates/a/src/lib.rs"

echo dirty >> crates/a/src/lib.rs
expect 3 "write refuses on a dirty tree" G write --section soak --json "$(soak true real)"
git checkout -q -- .

expect 0 "override records a line" G override --reason "bench host down"
grep -q "$(git rev-parse HEAD) bench host down" "$tmp/overrides.md" && echo "ok: override line has HEAD and reason" || { echo "FAIL: override line"; fails=$((fails+1)); }
expect 2 "override needs a reason" G override --reason "  "

# Overlapping writers: the release-tests and soak sections come from two runs that can overlap.
# Each stamp write must wait for the stamp lock, and neither section may drop the other.
echo 4 >> docs/a.md; git commit -qam "overlap base"
stamp_dir="$repo/target/perf-gate"; stamp="$stamp_dir/$(git rev-parse HEAD).json"
mkdir -p "$stamp_dir"; lock="$stamp_dir/.perf-gate.lock"
python3 -I -c 'import fcntl,os,sys,time
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT)
fcntl.flock(fd, fcntl.LOCK_EX)
print("held", flush=True)
time.sleep(2)' "$lock" > "$tmp/held" 2>&1 &
holder=$!
for _ in $(seq 50); do [ -s "$tmp/held" ] && break; sleep 0.1; done
G write --section soak --json "$(soak true real)" > "$tmp/blocked.out" 2>&1 &
writer=$!
sleep 1
if [ -e "$stamp" ]; then echo "FAIL: a writer did not wait for the stamp lock"; fails=$((fails+1)); else echo "ok: a writer waits for the stamp lock"; fi
wait "$writer"; got=$?
wait "$holder"
[ "$got" = 0 ] && echo "ok: the writer finishes once the lock is free" || { echo "FAIL: writer after lock (exit $got): $(cat "$tmp/blocked.out")"; fails=$((fails+1)); }

rounds_bad=0
for _ in $(seq 20); do
  rm -f "$stamp"
  G write --section release_tests --json "$tests" > /dev/null 2>&1 &
  first=$!
  G write --section soak --json "$(soak true real)" > /dev/null 2>&1 &
  second=$!
  wait "$first"; wait "$second"
  python3 -I -c 'import json, sys
s = json.load(open(sys.argv[1]))
assert s.get("release_tests") == "pass" and s["soak"]["pass"] is True and s["budget_tests"], s' "$stamp" 2>/dev/null || rounds_bad=$((rounds_bad+1))
done
[ "$rounds_bad" = 0 ] && echo "ok: 20 overlapping writer rounds keep both sections" || { echo "FAIL: $rounds_bad of 20 overlapping rounds lost a section"; fails=$((fails+1)); }
G check --binary "$tmp/bin" > /dev/null 2>&1 && echo "ok: stamp from overlapping writers passes the gate" || { echo "FAIL: gate refuses the overlapping-writer stamp"; fails=$((fails+1)); }

[ -n "$stamped" ] && [ "$fails" = 0 ] && echo "perf-gate tests passed" || { echo "$fails perf-gate test(s) failed"; exit 1; }
