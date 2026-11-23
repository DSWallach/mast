#!/bin/sh
# Start a detached command on a remote build host and wait for it through an exit marker.
# Never matches processes by command line; liveness is a pid check on the job's own pid file.
set -eu

usage() {
    cat >&2 <<'EOF'
Usage:
  scripts/remote-cargo.sh start <host> <remote worktree> <slot> <name> -- <command...>
  scripts/remote-cargo.sh wait <host> <name> <minutes|Ns>
  scripts/remote-cargo.sh status <host> <name>
<name> is [A-Za-z0-9._-]+ and is single-use; <slot> is [A-Za-z0-9_-]+ (a, b). The command after
`--` runs verbatim (normally `cargo ...`). Files on the host: /tmp/<name>.log|.pid|.exit.
Exit codes (wait): 0 job ok, 1 job failed, 2 unknown job/usage, 124 timeout, 125 job died
without an exit marker, 255 ssh failure. start/status: 0 ok, 1 refused/not running.
EOF
    exit 2
}

# Single-quote every argument so the remote login shell passes them through untouched.
quote() {
    for arg in "$@"; do
        printf "'%s' " "$(printf %s "$arg" | sed "s/'/'\\\\''/g")"
    done
}

# run_remote <host> <script> [args...]: feed <script> to `sh -s` on the host.
run_remote() {
    host=$1 script=$2
    shift 2
    printf '%s\n' "$script" | ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30 \
        "$host" "sh -s -- $(quote "$@")"
}

START=$(cat <<'EOF'
dir=$1 slot=$2 name=$3
shift 3
f=/tmp/$name
slotf=/tmp/remote-cargo-slot-$slot.job
if [ ! -d "$dir" ]; then echo "no such directory on host: $dir"; exit 1; fi
if [ -e "$f.log" ] || [ -e "$f.exit" ]; then echo "name in use: $name"; exit 1; fi
(set -C; echo $$ > "$f.pid") 2>/dev/null || { echo "name in use: $name"; exit 1; }
busy() {
    [ -n "$1" ] && [ ! -e "/tmp/$1.exit" ] && p=$(cat "/tmp/$1.pid" 2>/dev/null) &&
        [ -n "$p" ] && kill -0 "$p" 2>/dev/null
}
exec 9> "/tmp/remote-cargo-slot-$slot.lock"
if ! flock -w 10 9; then echo "slot $slot lock timeout"; rm -f "$f.pid"; exit 1; fi
old=$(cat "$slotf" 2>/dev/null || true)
if busy "$old"; then echo "slot $slot busy: $old"; rm -f "$f.pid"; exit 1; fi
echo "$name" > "$slotf"
exec 9>&-
setsid nohup sh -c '
name=$1 slot=$2 dir=$3
shift 3
echo $$ > "/tmp/$name.pid"
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
    echo "runner did not start"; rm -f "$f.pid" "$slotf"; exit 1
fi
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
    p=$(cat "$f.pid" 2>/dev/null || true)
    if ! { [ -n "$p" ] && kill -0 "$p" 2>/dev/null; }; then
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
p=$(cat "$f.pid" 2>/dev/null || true)
if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then echo running; exit 0; fi
echo died; exit 1
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
*) usage ;;
esac
