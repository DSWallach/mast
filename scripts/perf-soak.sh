#!/bin/bash
# Headless engine soak: run a candidate Mast binary (and optionally the installed
# one) against the same scratch data for N seconds and fail if the candidate is
# worse. Post-mortem 2026-11-16; HANDOFF "Perf gate before every install", gate 3.
#
#   scripts/perf-soak.sh <candidate> [--baseline <installed>] [--seconds N]
#                        [--synthetic | --real | --template DIR]
#
# Data source (default --synthetic):
#   --synthetic   generate temp repos/worktrees + Spaces/chats (soak_seed test)
#   --real        `sqlite3 -readonly .backup` of ~/.zeron/profiles/local/docs.sqlite3,
#                 plus device-id and local-profile.json, into a scratch template
#   --template D  reuse a template dir made by an earlier run
#
# Isolation (every binary, same for both): sandbox-exec denies non-loopback network and
# all file writes outside the run dir (and the user temp/cache dirs), denies reads
# of ~/.zeron ~/.claude ~/.codex gh config and Keychains; empty environment, scratch
# HOME, PATH-first shims (read-only `git` guard, fail-closed gh/claude/codex/...).
# MAST_SOAK=1 additionally makes the candidate refuse to start unless a probe
# connect fails, git is the guard, and the workspace scope is Local.
#
# Fails (exit 1) when candidate avg CPU or git spawns/min (whole run, and steady state after
# the first 60 s) > RATIO x baseline
# (baseline floors: CPU 3%, 10 spawns/min, so near-idle noise cannot trip it) or
# candidate RSS grows more than RSS_MAX_MB between warm-up and the end.
# With --real/--template and a baseline it also writes the soak result (pass or fail, with
# CPU, physical footprint, git spawns/min and baseline numbers) into the perf-gate stamp for HEAD.
# Exit 2: could not run (sandbox unavailable, engine refused/died). Never falls back.
set -u
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$SCRIPTS/.." && pwd)"
SECONDS_TOTAL=180
RATIO=${RATIO:-1.5}
RSS_MAX_MB=${RSS_MAX_MB:-500}
CPU_FLOOR=${CPU_FLOOR:-3}
SPAWN_FLOOR=${SPAWN_FLOOR:-10}
MODE=synthetic; TEMPLATE=""; CAND=""; BASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --baseline) BASE="$2"; shift 2 ;;
    --seconds) SECONDS_TOTAL="$2"; shift 2 ;;
    --synthetic) MODE=synthetic; shift ;;
    --real) MODE=real; shift ;;
    --template) MODE=template; TEMPLATE="$2"; shift 2 ;;
    -h|--help) sed -n 2,28p "$0"; exit 0 ;;
    *) [ -z "$CAND" ] && CAND="$1" || { echo "unexpected: $1" >&2; exit 2; }; shift ;;
  esac
done
[ -n "$CAND" ] && [ -x "$CAND" ] || { echo "candidate binary missing or not executable: $CAND" >&2; exit 2; }
[ -z "$BASE" ] || [ -x "$BASE" ] || { echo "baseline not executable: $BASE" >&2; exit 2; }
case "$SECONDS_TOTAL" in ''|*[!0-9]*) echo "--seconds must be a number" >&2; exit 2 ;; esac
[ "$SECONDS_TOTAL" -ge 70 ] || { echo "--seconds must be >= 70 (a sample is taken at 60 s)" >&2; exit 2; }
command -v sandbox-exec >/dev/null || { echo "sandbox-exec not found; refusing to soak without it" >&2; exit 2; }
LOAD=$(uptime | sed 's/.*load averages*: *//' | awk '{gsub(",","");print $1}')
awk -v l="$LOAD" 'BEGIN{exit !(l>12)}' && { echo "load $LOAD > 12; not starting" >&2; exit 2; }
if pgrep -f "perf-soak.sh" | grep -qv "^$$\$"; then
  for p in $(pgrep -f "perf-soak.sh"); do [ "$p" != "$$" ] && [ "$p" != "$PPID" ] && { echo "another soak is running (pid $p)" >&2; exit 2; }; done
fi

ROOT=$(mktemp -d /private/tmp/mast-soak.XXXXXX) || exit 2
echo "soak root: $ROOT"
REAL_HOME="$HOME"
REAL_GIT=$(command -v git)

# ---- template data dir -------------------------------------------------------
if [ "$MODE" = synthetic ]; then
  TEMPLATE="$ROOT/template"
  mkdir -p "$TEMPLATE"
  echo "seeding synthetic data (cargo test, nice 10) ..."
  ( cd "$REPO" && MAST_SOAK_SEED_ROOT="$TEMPLATE" nice -n 10 cargo test -p mast-engine --test soak_seed -- --ignored --nocapture 2>&1 | grep -E "soak seed ok|error|panicked" ) || true
  [ -f "$TEMPLATE/data/profiles/local/docs.sqlite3" ] || { echo "seeding failed" >&2; exit 2; }
elif [ "$MODE" = real ]; then
  TEMPLATE="$ROOT/template"
  SRC="$REAL_HOME/.zeron"
  mkdir -p "$TEMPLATE/data/profiles/local"
  sqlite3 -readonly "$SRC/profiles/local/docs.sqlite3" ".backup '$TEMPLATE/data/profiles/local/docs.sqlite3'" || { echo "backup failed" >&2; exit 2; }
  cp "$SRC/device-id" "$SRC/local-profile.json" "$TEMPLATE/data/" || { echo "identity copy failed" >&2; exit 2; }
fi
[ -d "$TEMPLATE/data" ] || { echo "template has no data/ dir" >&2; exit 2; }

# ---- one run -----------------------------------------------------------------
free_port() { local p; while :; do p=$((20000 + RANDOM % 30000)); nc -z 127.0.0.1 "$p" 2>/dev/null || { echo "$p"; return; }; done; }
cpu_seconds() { ps -o cputime= -p "$1" 2>/dev/null | awk '{n=split($1,a,":"); s=0; for(i=1;i<=n;i++) s=s*60+a[i]; print s}'; }
rss_mb() { ps -o rss= -p "$1" 2>/dev/null | awk '{printf "%d", $1/1024}'; }

# run <label> <binary>; writes $ROOT/<label>.result (key=value lines)
run_one() {
  local label="$1" bin="$2" dir="$ROOT/$1"
  mkdir -p "$dir/home" "$dir/shims" "$dir/tmp"
  cp -c -R "$TEMPLATE/data" "$dir/data" || return 2
  cp "$SCRIPTS/soak/git-guard.sh" "$dir/shims/git"
  for n in gh claude codex security npm npx node cursor-agent agent gemini opencode curl ssh; do cp "$SCRIPTS/soak/stub.sh" "$dir/shims/$n"; done
  chmod +x "$dir"/shims/*
  local utemp ucache
  utemp=$(getconf DARWIN_USER_TEMP_DIR); ucache=$(getconf DARWIN_USER_CACHE_DIR)
  sed -e "s#@SOAK_DIR@#$dir#g" -e "s#@USER_TEMP@#${utemp%/}#g" -e "s#@USER_CACHE@#${ucache%/}#g" \
      -e "s#@REAL_HOME@#$REAL_HOME#g" "$SCRIPTS/soak/sandbox.sb.in" > "$dir/sandbox.sb"
  : > "$dir/git.log"; : > "$dir/stub.log"
  local ipc cb; ipc=$(free_port); cb=$(free_port)
  local start; start=$(date +%s)
  env -i HOME="$dir/home" PATH="$dir/shims:/usr/bin:/bin:/usr/sbin:/sbin" TMPDIR="$dir/tmp" \
      MAST_SOAK=1 MAST_SOAK_SHIM_DIR="$dir/shims" MAST_SOAK_GIT_LOG="$dir/git.log" \
      MAST_SOAK_STUB_LOG="$dir/stub.log" MAST_SOAK_REAL_GIT="$REAL_GIT" \
      MAST_DATA_DIR="$dir/data" MAST_IPC_PORT="$ipc" MAST_CALLBACK_PORT="$cb" \
      MAST_LINEAR_KEY_STORE=memory RUST_LOG="info,loro_internal=warn,loro=warn" \
      sandbox-exec -f "$dir/sandbox.sb" "$bin" headless > "$dir/engine.log" 2>&1 &
  local pid=$!
  # sandbox-exec execs the target, so $pid is the engine.
  local peak=0 warm_rss="" t=0 sampled60=0 cpu60=0 git60=0 t60=0
  while [ "$t" -lt "$SECONDS_TOTAL" ]; do
    sleep 5; t=$(( $(date +%s) - start ))
    kill -0 "$pid" 2>/dev/null || { echo "engine exited early (see $dir/engine.log)" >&2; tail -5 "$dir/engine.log" >&2; echo "failed=exited" > "$ROOT/$label.result"; return 2; }
    local r; r=$(rss_mb "$pid"); [ -n "$r" ] && [ "$r" -gt "$peak" ] && peak=$r
    if [ -z "$warm_rss" ] && [ "$t" -ge 30 ]; then warm_rss=$r; fi
    if [ "$sampled60" = 0 ] && [ "$t" -ge 60 ]; then
      sampled60=1; t60=$t; cpu60=$(cpu_seconds "$pid"); git60=$(wc -l < "$dir/git.log" | tr -d ' ')
      # In the background: `sample` of a busy multi-GB process can take minutes and must not stretch the window.
      ( python3 "$SCRIPTS/perf-sample.py" --pid "$pid" --seconds 3 --top 8 > "$dir/sample60.txt" 2>&1 ) &
    fi
  done
  # Metrics are frozen here, before the end sample, so both binaries get the same window.
  local cpu_end end_rss git_end wall; cpu_end=$(cpu_seconds "$pid"); end_rss=$(rss_mb "$pid")
  git_end=$(wc -l < "$dir/git.log" | tr -d ' '); wall=$(( $(date +%s) - start ))
  local foot; foot=$(vmmap --summary "$pid" 2>/dev/null | awk '/^Physical footprint:/ {v=$3; u=substr(v,length(v)); n=substr(v,1,length(v)-1)+0; if(u=="G")n*=1024; else if(u=="K")n/=1024; else if(u=="B")n/=1048576; printf "%d", n; exit}')
  echo "--- $label @ ${t60}s ---"; head -14 "$dir/sample60.txt" 2>/dev/null
  echo "--- $label @ end ---"; python3 "$SCRIPTS/perf-sample.py" --pid "$pid" --seconds 3 --top 8 2>&1 | head -14
  kill -TERM "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  local a r2; a=$(grep -c '^A ' "$dir/git.log"); r2=$(grep -c '^R ' "$dir/git.log")
  grep -q "outbound connect failed as required" "$dir/engine.log" && pre=yes || pre=no
  {
    echo "wall=$wall"
    echo "cpu_pct=$(awk -v c="$cpu_end" -v w="$wall" 'BEGIN{printf "%.1f", 100*c/w}')"
    echo "peak_rss_mb=$peak"
    echo "warm_rss_mb=${warm_rss:-0}"
    echo "end_rss_mb=${end_rss:-0}"
    echo "footprint_mb=${foot:-0}"
    echo "git_spawns=$((a + r2))"
    echo "git_refused=$r2"
    echo "steady_cpu_pct=$(awk -v c="$cpu_end" -v c0="$cpu60" -v w="$wall" -v w0="$t60" 'BEGIN{printf "%.1f", 100*(c-c0)/(w-w0)}')"
    echo "steady_git_per_min=$(awk -v n="$git_end" -v n0="$git60" -v w="$wall" -v w0="$t60" 'BEGIN{printf "%.1f", (n-n0)*60/(w-w0)}')"
    echo "git_per_min=$(awk -v n="$((a + r2))" -v w="$wall" 'BEGIN{printf "%.1f", n*60/w}')"
    echo "stub_calls=$(wc -l < "$dir/stub.log" | tr -d ' ')"
    echo "preflight_logged=$pre"
  } > "$ROOT/$label.result"
  echo "git subcommands ($label):"; sed 's/^[AR] /&/' "$dir/git.log" | sort | uniq -c | sort -rn | head -8
  return 0
}
get() { grep "^$2=" "$ROOT/$1.result" | cut -d= -f2; }

run_one candidate "$CAND" || { echo "candidate run failed" >&2; exit 2; }
if [ -n "$BASE" ]; then
  LOAD=$(uptime | sed 's/.*load averages*: *//' | awk '{gsub(",","");print $1}')
  awk -v l="$LOAD" 'BEGIN{exit !(l>12)}' && { echo "load $LOAD > 12; stopping before baseline" >&2; exit 2; }
  run_one baseline "$BASE" || { echo "baseline run failed" >&2; exit 2; }
fi

# ---- compare -----------------------------------------------------------------
echo; echo "=== perf-soak: ${SECONDS_TOTAL}s, data=$MODE ==="
printf '%-18s %12s %12s\n' metric candidate baseline
for k in cpu_pct steady_cpu_pct peak_rss_mb end_rss_mb git_per_min steady_git_per_min git_spawns git_refused stub_calls; do
  printf '%-18s %12s %12s\n' "$k" "$(get candidate $k)" "$( [ -n "$BASE" ] && get baseline $k || echo -)"
done
FAIL=0
cc=$(get candidate cpu_pct); cg=$(get candidate git_per_min)
grow=$(( $(get candidate end_rss_mb) - $(get candidate warm_rss_mb) ))
echo "candidate RSS growth after warm-up: ${grow} MB (limit ${RSS_MAX_MB})"
[ "$grow" -gt "$RSS_MAX_MB" ] && { echo "FAIL: RSS grew ${grow} MB"; FAIL=1; }
if [ -n "$BASE" ]; then
  bc_=$(get baseline cpu_pct); bg=$(get baseline git_per_min)
  sc=$(get candidate steady_cpu_pct); sb=$(get baseline steady_cpu_pct)
  sgc=$(get candidate steady_git_per_min); sgb=$(get baseline steady_git_per_min)
  awk -v c="$sc" -v b="$sb" -v r="$RATIO" -v f="$CPU_FLOOR" 'BEGIN{lim=(b>f?b:f)*r; exit !(c>lim)}' && { echo "FAIL: steady-state CPU ${sc}% > ${RATIO}x baseline ${sb}% (floor ${CPU_FLOOR}%)"; FAIL=1; }
  awk -v c="$sgc" -v b="$sgb" -v r="$RATIO" -v f="$SPAWN_FLOOR" 'BEGIN{lim=(b>f?b:f)*r; exit !(c>lim)}' && { echo "FAIL: steady-state git ${sgc}/min > ${RATIO}x baseline ${sgb}/min (floor ${SPAWN_FLOOR}/min)"; FAIL=1; }
  awk -v c="$cc" -v b="$bc_" -v r="$RATIO" -v f="$CPU_FLOOR" 'BEGIN{lim=(b>f?b:f)*r; exit !(c>lim)}' && { echo "FAIL: candidate CPU ${cc}% > ${RATIO}x baseline ${bc_}% (floor ${CPU_FLOOR}%)"; FAIL=1; }
  awk -v c="$cg" -v b="$bg" -v r="$RATIO" -v f="$SPAWN_FLOOR" 'BEGIN{lim=(b>f?b:f)*r; exit !(c>lim)}' && { echo "FAIL: candidate git ${cg}/min > ${RATIO}x baseline ${bg}/min (floor ${SPAWN_FLOOR}/min)"; FAIL=1; }
else
  echo "(no baseline: only the RSS-growth rule applied)"
fi
# Perf-gate stamp (scripts/perf-gate.py): real-data soak with a baseline, keyed by HEAD.
if [ "$MODE" != synthetic ] && [ -n "$BASE" ]; then
  [ "$FAIL" = 0 ] && PASSED=true || PASSED=false
  STAMP_JSON=$(python3 -I - "$ROOT" "$PASSED" "$MODE" "$SECONDS_TOTAL" "$(shasum -a 256 "$CAND" | cut -d' ' -f1)" <<'PY'
import json, sys
root, passed, mode, seconds, digest = sys.argv[1:6]
def res(label):
    out = {}
    for line in open(f"{root}/{label}.result"):
        k, _, v = line.strip().partition("=")
        try: out[k] = float(v)
        except ValueError: out[k] = v
    return out
def pick(r):
    return {"cpu_pct": r["cpu_pct"], "steady_cpu_pct": r["steady_cpu_pct"], "footprint_mb": r["footprint_mb"],
            "rss_mb": r["end_rss_mb"], "git_spawns_per_min": r["git_per_min"], "steady_git_spawns_per_min": r["steady_git_per_min"]}
soak = {"pass": passed == "true", "mode": mode, "seconds": int(seconds), "candidate_sha256": digest, **pick(res("candidate")),
        "baseline": pick(res("baseline"))}
print(json.dumps(soak))
PY
)
  python3 -I "$SCRIPTS/perf-gate.py" write --section soak --json "$STAMP_JSON" || echo "WARN: soak stamp not written"
fi
[ "$(get candidate preflight_logged)" = yes ] || echo "WARN: candidate did not log the network preflight"
[ "$FAIL" = 0 ] && echo "PASS" || echo "RESULT: FAIL (artifacts kept in $ROOT)"
exit "$FAIL"
