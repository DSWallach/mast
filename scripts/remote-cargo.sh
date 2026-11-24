#!/bin/sh
# Start a detached command on a remote build host and wait for it through an exit marker.
# Never matches processes by command line; liveness is the job's own pid file, checked against the
# process start time recorded with it so a reused pid is not mistaken for the job.
set -eu

usage() {
    cat >&2 <<'EOF'
Usage:
  scripts/remote-cargo.sh start <host> <remote worktree> <slot> <name> -- cargo <args...>
  scripts/remote-cargo.sh wait <host> <name> <minutes|Ns>
  scripts/remote-cargo.sh status <host> <name>
  scripts/remote-cargo.sh stop <host> <name>
<name> is [A-Za-z0-9._-]+ and is single-use; <slot> is [A-Za-z0-9_-]+ (a, b). The command after
`--` must be a `cargo` command and runs verbatim. Files on the host:
/tmp/<name>.log|.pid|.start|.claim|.exit.
Exit codes (wait): 0 job ok, 1 job failed, 2 unknown job/usage, 124 timeout, 125 job died
without an exit marker, 255 ssh failure. start/status: 0 ok, 1 refused/not running.
stop: 0 stopped or already finished, 1 not running or not the job's own process, 2 unknown job.
EOF
    exit 2
}

# Single-quote every argument so the remote login shell passes them through untouched.
quote() {
    for arg in "$@"; do
        printf "'%s' " "$(printf %s "$arg" | sed "s/'/'\\\\''/g")"
    done
}

# starttime <pid> and alive <name>, defined in every remote script. alive is true when the job's
# runner is still the process that wrote its pid file: the launcher records the runner's start time
# in <name>.start once the runner has published its pid, so a pid the host has since reused for an
# unrelated process (the runner was killed without leaving an exit marker) does not count.
# starttime prefers field 22 of /proc/<pid>/stat (ticks since boot, which cannot drift between two
# reads) and falls back to the `ps` start time where there is no /proc.
ALIVE=$(cat <<'EOF'
starttime() {
    if [ -r "/proc/$1/stat" ]; then
        s=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
        # Drop "pid (comm" up to its last close paren; the comm may hold spaces. Field 22 is then 20th.
        rp=")"
        ( set -- ${s##*"$rp" }; eval "echo \${20}" )
    else
        ps -o lstart= -p "$1" 2>/dev/null | tr -s ' '
    fi
}
alive() {
    [ -n "$1" ] || return 1
    p=$(cat "/tmp/$1.pid" 2>/dev/null) || return 1
    { [ -n "$p" ] && kill -0 "$p" 2>/dev/null; } || return 1
    [ -s "/tmp/$1.start" ] || return 0
    [ "$(starttime "$p")" = "$(cat "/tmp/$1.start")" ]
}
EOF
)

# run_remote <host> <script> [args...]: feed <script> to `sh -s` on the host.
run_remote() {
    host=$1 script=$2
    shift 2
    printf '%s\n%s\n' "$ALIVE" "$script" | ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30 \
        "$host" "sh -s -- $(quote "$@")"
}

START=$(cat <<'EOF'
dir=$1 slot=$2 name=$3
shift 3
f=/tmp/$name
slotf=/tmp/remote-cargo-slot-$slot.job
if [ ! -d "$dir" ]; then echo "no such directory on host: $dir"; exit 1; fi
if [ -e "$f.log" ] || [ -e "$f.exit" ] || [ -e "$f.claim" ]; then echo "name in use: $name"; exit 1; fi
(set -C; echo $$ > "$f.pid") 2>/dev/null || { echo "name in use: $name"; exit 1; }
busy() { [ -n "$1" ] && [ ! -e "/tmp/$1.exit" ] && alive "$1"; }
exec 9> "/tmp/remote-cargo-slot-$slot.lock"
if ! flock -w 10 9; then echo "slot $slot lock timeout"; rm -f "$f.pid"; exit 1; fi
old=$(cat "$slotf" 2>/dev/null || true)
if busy "$old"; then echo "slot $slot busy: $old"; rm -f "$f.pid"; exit 1; fi
echo "$name" > "$slotf"
exec 9>&-
setsid nohup sh -c '
name=$1 slot=$2 dir=$3
shift 3
f=/tmp/$name
# Claim the job. The launcher claims it as abandoned when this runner is too late, so a late
# runner exits here instead of running a job nobody waits for.
(set -C; echo run > "$f.claim") 2>/dev/null || exit 0
# A stop signal reaches the whole group: the job dies and this runner still writes the marker.
trap : TERM
echo $$ > "$f.pid.tmp" && mv "$f.pid.tmp" "$f.pid"
PATH=$HOME/.cargo/bin:$PATH
(
    if [ -f "$HOME/Work/mast-slot-$slot.env" ]; then
        . "$HOME/Work/mast-slot-$slot.env"
    else
        CARGO_TARGET_DIR=$HOME/Work/mast-target-$slot
        export CARGO_TARGET_DIR
    fi
    cd "$dir" && exec "$@"
) > "/tmp/$name.log" 2>&1
rc=$?
echo "$rc" > "/tmp/$name.exit.tmp" && mv "/tmp/$name.exit.tmp" "/tmp/$name.exit"
' sh "$name" "$slot" "$dir" "$@" > /dev/null 2>&1 < /dev/null &
i=0
while [ "$(cat "$f.pid")" = "$$" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
if [ "$(cat "$f.pid")" = "$$" ]; then
    if (set -C; echo abandoned > "$f.claim") 2>/dev/null; then
        echo "runner did not start"; rm -f "$f.pid" "$slotf"; exit 1
    fi
    # The runner claimed the job just now: give it a moment to publish its pid.
    i=0
    while [ "$(cat "$f.pid")" = "$$" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
    if [ "$(cat "$f.pid")" = "$$" ]; then
        echo "runner started late and has not published its pid; the name stays in use and the job may still run, so check it with status"
        exit 1
    fi
fi
starttime "$(cat "$f.pid")" > "$f.start.tmp" && mv "$f.start.tmp" "$f.start"
echo "started $name pid $(cat "$f.pid") slot $slot log $f.log"
EOF
)

WAIT=$(cat <<'EOF'
name=$1 secs=$2
f=/tmp/$name
if [ ! -e "$f.pid" ] && [ ! -e "$f.exit" ]; then echo "unknown job: $name"; exit 2; fi
report() {
    if [ -e "$f.log" ]; then
        grep -aE '^error|^ *FAIL |FAILED|panicked' "$f.log" | cut -c1-200 | head -n 26 || true
        grep -aE 'test result|Summary' "$f.log" | cut -c1-200 | tail -n 4 || true
        echo "--- tail"
        tail -n 5 "$f.log" | cut -c1-200
    fi
}
i=0
while [ ! -e "$f.exit" ]; do
    if ! alive "$name"; then
        [ -e "$f.exit" ] && break
        echo "died without exit marker"; report; exit 125
    fi
    if [ "$i" -ge "$secs" ]; then echo "timeout after ${secs}s, still running"; report; exit 124; fi
    sleep 1
    i=$((i + 1))
done
code=$(cat "$f.exit")
echo "exit $code"
report
[ "$code" = 0 ]
EOF
)

STATUS=$(cat <<'EOF'
f=/tmp/$1
if [ -e "$f.exit" ]; then echo "exit $(cat "$f.exit")"; exit 0; fi
if [ ! -e "$f.pid" ]; then echo unknown; exit 1; fi
if alive "$1"; then echo running; exit 0; fi
echo died; exit 1
EOF
)

STOP=$(cat <<'EOF'
name=$1
f=/tmp/$name
if [ -e "$f.exit" ]; then echo "already finished: exit $(cat "$f.exit")"; exit 0; fi
if [ ! -e "$f.pid" ]; then echo "unknown job: $name"; exit 2; fi
if ! alive "$name"; then echo "not running: it died without an exit marker"; exit 1; fi
p=$(cat "$f.pid")
if [ ! -s "$f.start" ]; then echo "refusing: no recorded start time to tell pid $p from a reused one"; exit 1; fi
g=$(ps -o pgid= -p "$p" 2>/dev/null | tr -d ' ')
if [ "$g" != "$p" ]; then echo "refusing: pid $p does not lead its own process group"; exit 1; fi
# The runner leads the job's own session, so this reaches the job and nothing else.
kill -s TERM -- "-$p" 2>/dev/null || true
i=0
while [ ! -e "$f.exit" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
if [ -e "$f.exit" ]; then echo "stopped: exit $(cat "$f.exit")"; exit 0; fi
# TERM was not enough: kill the group, then write the marker the runner can no longer write.
kill -s KILL -- "-$p" 2>/dev/null || true
sleep 1
if [ ! -e "$f.exit" ]; then echo 137 > "$f.exit.tmp" && mv "$f.exit.tmp" "$f.exit"; fi
echo "killed: exit $(cat "$f.exit")"
EOF
)

valid() { # valid <value> <extra allowed chars>
    case $1 in '' | *[!A-Za-z0-9_$2-]*) return 1 ;; esac
}

[ "$#" -ge 2 ] || usage
cmd=$1 host=$2
case $host in -* | '') usage ;; esac
shift 2
case $cmd in
start)
    [ "$#" -ge 4 ] || usage
    dir=$1 slot=$2 name=$3
    shift 3
    [ "$1" = -- ] && [ "$#" -ge 2 ] || usage
    shift
    valid "$slot" '' && valid "$name" . || usage
    # The host runs what follows `--`, and an approval rule can only match the text of this call,
    # so the script itself keeps that to cargo.
    [ "$1" = cargo ] || { echo "only a cargo command runs on the build host" >&2; exit 2; }
    run_remote "$host" "$START" "$dir" "$slot" "$name" "$@"
    ;;
wait)
    [ "$#" -eq 2 ] || usage
    valid "$1" . || usage
    amount=${2%s}
    case $amount in '' | *[!0-9]*) usage ;; esac
    if [ "$amount" = "$2" ]; then secs=$((amount * 60)); else secs=$amount; fi
    run_remote "$host" "$WAIT" "$1" "$secs"
    ;;
status)
    [ "$#" -eq 1 ] || usage
    valid "$1" . || usage
    run_remote "$host" "$STATUS" "$1"
    ;;
stop)
    [ "$#" -eq 1 ] || usage
    valid "$1" . || usage
    run_remote "$host" "$STOP" "$1"
    ;;
*) usage ;;
esac
