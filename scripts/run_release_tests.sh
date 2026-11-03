#!/usr/bin/env bash
# Run the test suites affected by the changes since a base ref.
#
#   scripts/run_release_tests.sh [options] [BASE_REF]
#
# BASE_REF defaults to origin/work-outline-implementation. Changed paths
# (BASE_REF...HEAD plus uncommitted changes) are mapped to workspace crates, widened
# to every crate that depends on them, and only those suites run:
#   - non-UI crates: cargo nextest, profile `release` (quarantined tests excluded)
#   - mast-ui:       cargo test -p mast-ui --lib -- --test-threads=1
# Changes to Cargo.toml, Cargo.lock, rust-toolchain.toml or .config/ select everything.
# Changes that touch no crate (docs, scripts) run nothing.
#
# Options:
#   --all               run every suite regardless of the diff
#   --build             also build the installed binary in parallel, in its own target
#                       directory (target/release-build), so it cannot block the tests
#   --quarantine        additionally run the quarantine profile (failures are reported, not fatal)
#   --dry-run           print the selection and exit
#   --timeout SECONDS   wall-clock limit per suite group (default 5400)
#   -h, --help
set -euo pipefail

base_ref="origin/work-outline-implementation"
run_all=0; run_build=0; run_quarantine=0; dry_run=0; suite_timeout=5400
while [ $# -gt 0 ]; do
  case "$1" in
    --all) run_all=1 ;;
    --build) run_build=1 ;;
    --quarantine) run_quarantine=1 ;;
    --dry-run) dry_run=1 ;;
    --timeout) suite_timeout="$2"; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) base_ref="$1" ;;
  esac
  shift
done

cd "$(git rev-parse --show-toplevel)"

# Select packages: prints "all" or a space-separated list of package names.
select_packages() {
  local changed
  changed="$( { git diff --name-only "$base_ref"...HEAD; git diff --name-only HEAD; git ls-files --others --exclude-standard; } | sort -u)"
  CHANGED="$changed" python3 - <<'PY'
import json, os, subprocess
meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"]))
root = meta["workspace_root"] + "/"
members = {p["id"]: p for p in meta["packages"]}
dirs = {}
for p in members.values():
    dirs[p["manifest_path"][len(root):].rsplit("/", 1)[0]] = p["name"]
names = set(dirs.values())
dependents = {n: set() for n in names}
for p in members.values():
    for d in p["dependencies"]:
        if d["name"] in names:
            dependents[d["name"]].add(p["name"])
everything = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".config/")
hit, wide = set(), False
for f in filter(None, os.environ["CHANGED"].split("\n")):
    if f in everything[:3] or f.startswith(everything[3]):
        wide = True
        continue
    for d, n in dirs.items():
        if f.startswith(d + "/"):
            hit.add(n)
if wide:
    print("all"); raise SystemExit
stack = list(hit)
while stack:
    for dep in dependents[stack.pop()]:
        if dep not in hit:
            hit.add(dep); stack.append(dep)
print(" ".join(sorted(hit)))
PY
}

if [ "$run_all" = 1 ]; then selection="all"; else selection="$(select_packages)"; fi
echo "base: $base_ref   selection: ${selection:-<none>}"

non_ui_packages=(); run_ui=0
if [ "$selection" = "all" ]; then
  run_ui=1
  while read -r p; do [ "$p" != mast-ui ] && [ "$p" != mast ] && non_ui_packages+=(-p "$p"); done < <(
    cargo metadata --format-version 1 --no-deps --offline | python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]')
else
  for p in $selection; do
    case "$p" in mast-ui) run_ui=1 ;; mast) ;; *) non_ui_packages+=(-p "$p") ;; esac
  done
fi
echo "nextest packages: ${non_ui_packages[*]:-<none>}   ui suite: $run_ui   installed build: $run_build"
[ "$dry_run" = 1 ] && exit 0

with_time_limit() { perl -e 'alarm shift; exec @ARGV' "$suite_timeout" "$@"; }
status=0; build_pid=""
if [ "$run_build" = 1 ]; then
  ( CARGO_TARGET_DIR="$PWD/target/release-build" cargo build --profile installed -p mast \
      > target/release-build.log 2>&1 ) &
  build_pid=$!
  echo "installed build started (pid $build_pid, log target/release-build.log)"
fi

started=$(date +%s)
if [ ${#non_ui_packages[@]} -gt 0 ]; then
  command -v cargo-nextest >/dev/null || { echo "cargo-nextest missing: cargo install cargo-nextest --locked" >&2; exit 3; }
  echo "== nextest (release profile)"
  with_time_limit cargo nextest run --profile release "${non_ui_packages[@]}" || status=1
  if [ "$run_quarantine" = 1 ]; then
    echo "== nextest (quarantine profile; failures here are expected)"
    with_time_limit cargo nextest run --profile quarantine --no-tests=pass "${non_ui_packages[@]}" || echo "quarantine: some tests failed (not fatal)"
  fi
fi
if [ "$run_ui" = 1 ]; then
  echo "== mast-ui"
  with_time_limit cargo test -p mast-ui --lib -- --test-threads=1 || status=1
fi
echo "test wall time: $(( $(date +%s) - started )) s"

if [ -n "$build_pid" ]; then
  if wait "$build_pid"; then echo "installed build ok: target/release-build/installed/zeron"
  else echo "installed build FAILED, see target/release-build.log" >&2; status=1; fi
fi
[ $status = 0 ] && echo "RESULT: pass" || echo "RESULT: FAIL"
exit $status
