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
# Changes that touch no crate (docs, scripts) select no crate suites.
# Every non-dry run checks the preview server protocol and the flow wrapper.
#
# Options:
#   --all               run every suite regardless of the diff
#   --build             also build the installed binary in parallel, in its own target
#                       directory (target/release-build), so it cannot block the tests
#   --quarantine        additionally run the quarantine profile (failures are reported, not fatal)
#   --dry-run           print the selection and exit
# The UI driver scenarios (crates/ui-driver/scenarios/*.yaml) run after the suites whenever the
# selection includes mast-ui-driver. They need the platform sandbox (sandbox-exec on macOS or
# Bubblewrap on Linux) and run outside nextest. They report results without changing the exit status.
# On RESULT: pass (clean tree) it also runs the budget tests and writes the perf-gate stamp
# target/perf-gate/<HEAD>.json (scripts/perf-gate.py); the install script requires it.
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
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) base_ref="$1" ;;
  esac
  shift
done

cd "$(git rev-parse --show-toplevel)"

# Permit a checkout-local nextest installation without changing the user's PATH.
if ! command -v cargo-nextest >/dev/null && [ -x "$PWD/target/test-tools/cargo-nextest" ]; then
  export PATH="$PWD/target/test-tools:$PATH"
fi

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
run_scenarios=0
case " $selection " in *" all "*|*" mast-ui-driver "*) run_scenarios=1 ;; esac
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
echo "== UI preview and flow scripts"
with_time_limit python3 -B scripts/tests/test-ui-preview-server.py || status=1
with_time_limit python3 -B scripts/tests/test-ui-flow.py || status=1
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
  echo "== UI preview provenance"
  with_time_limit cargo test -p mast-ui --features snapshot --test ui_preview \
    provenance:: -- --test-threads=1 || status=1
fi
echo "test wall time: $(( $(date +%s) - started )) s"
if [ "$run_scenarios" = 1 ]; then
  echo "== ui-driver scenarios (report only)"
  with_time_limit cargo test -p mast-ui-driver --test scenarios -- --ignored run_all_scenarios \
    --test-threads=1 --nocapture 2>&1 || true
  echo "scenarios: report only, not part of RESULT"
fi

if [ -n "$build_pid" ]; then
  if wait "$build_pid"; then echo "installed build ok: target/release-build/installed/zeron"
  else echo "installed build FAILED, see target/release-build.log" >&2; status=1; fi
fi
# The clean check builds and inspects the installed binary before any stamp is written.
if [ "$status" = 0 ]; then
  echo "== installed build containment"
  scripts/check-installed-clean.sh || status=1
fi
# Budget tests (post-mortem 2026-11-16, P1) always run here, whatever the diff selected, so the
# perf-gate stamp can name them. Any failure is a new failure and blocks the stamp.
budget_json="[]"
if [ $status = 0 ]; then
  echo "== budget tests"
  budget_log="$(mktemp "${TMPDIR:-/tmp}/budget-tests.XXXXXX")"
  {
    with_time_limit cargo nextest run --profile release -p mast-engine --test engine -E 'test(/^idle_spawn_budget::/)' --no-capture 2>&1 || echo "BUDGET-FAIL engine"
    with_time_limit cargo test -p mast-ui --lib change_request_retries_stay_within_a_budget -- --test-threads=1 2>&1 || echo "BUDGET-FAIL ui change_request"
    with_time_limit cargo test -p mast-ui --lib the_index_is_built_once_per_snapshot -- --test-threads=1 2>&1 || echo "BUDGET-FAIL ui index"
  } | tee "$budget_log"
  if grep -q '^BUDGET-FAIL' "$budget_log"; then status=1; else
    budget_json="$(python3 -I - "$budget_log" <<'PY'
import json, re, sys
text = open(sys.argv[1]).read()
out = []
m = re.search(r"idle budget: (\d+) git spawns .*?budget (\d+)", text)
if m:
    out.append({"name": "mast-engine::idle_spawn_budget", "result": "pass", "git_spawns": int(m[1]), "budget": int(m[2])})
for name in re.findall(r"^test (\S+::\S+) \.\.\. ok$", text, re.M):
    out.append({"name": "mast-ui::" + name, "result": "pass"})
print(json.dumps(out))
PY
)"
    case "$budget_json" in *idle_spawn_budget*change_request*the_index*|*idle_spawn_budget*the_index*change_request*) ;; *) echo "budget tests did not all report a pass: $budget_json" >&2; status=1 ;; esac
  fi
  rm -f "$budget_log"
fi
if [ $status = 0 ]; then
  echo "RESULT: pass"
  python3 -I "$(dirname "$0")/perf-gate.py" write --section release_tests --json \
    "$(python3 -I -c 'import json,sys; print(json.dumps({"result":"pass","selection":sys.argv[1],"budget_tests":json.loads(sys.argv[2])}))' "${selection:-none} (base $base_ref)" "$budget_json")" || true
else
  echo "RESULT: FAIL"
fi
exit $status
