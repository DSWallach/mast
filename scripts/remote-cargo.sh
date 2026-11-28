#!/bin/sh
# Start a detached command on a remote build host and wait for it through an exit marker.
# Never matches processes by command line; liveness is the job's own pid file, checked against the
# process start time recorded with it so a reused pid is not mistaken for the job.
set -eu

usage() {
    cat >&2 <<'EOF'
Usage:
  scripts/remote-cargo.sh start [--device-serial <serial> | --admit | --quiet-window]
                                [--checkout <repo> <sha> <target dir> <0|1>]
                                <host> <remote worktree> <slot> <name> -- cargo <args...>
  scripts/remote-cargo.sh wait <host> <name> <minutes|Ns>
  scripts/remote-cargo.sh status <host> <name>
  scripts/remote-cargo.sh stop <host> <name>
  scripts/remote-cargo.sh fence <host> <name>
--checkout: create or move <remote worktree> to commit <sha> of <repo> as part of the start, under the slot
lock and only once the slot is idle, so no other start (from any machine) can move it between the checkout
and the job. It writes ~/Work/mast-slot-<slot>.env (CARGO_TARGET_DIR ~/<target dir>, CARGO_INCREMENTAL
<0|1>) unless an unmanaged file is there. <repo> and <target dir> are relative to $HOME.
fence: settle a start whose answer was lost. Prints "fenced" when the job has not started and now never
will, "started" when its runner claimed it, or "exit <code>". The name stays used either way.
--admit: concurrent slots. Only slots a b c, each in ~/Work/mast-wt-build-<slot> with CARGO_TARGET_DIR
~/Work/mast-target-<slot>, and CARGO_INCREMENTAL=0 except in slot a. Refuses when REMOTE_CARGO_MAX_JOBS (3)
jobs already run, when free disk after reserving 10 GiB per running job is under 15 GiB, or when
MemAvailable less 2 GiB per running job is under 4 GiB. It shares the device lock, so it excludes
--device-serial jobs, and it holds the quiet-window lock shared. --quiet-window: same slot policy; needs an
idle host and takes both locks exclusively. Every start, with or without an option, holds the quiet-window
lock shared, so no start runs during a window. The options cannot be combined.
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
dir=$1 slot=$2 name=$3 device=$4 mode=$5 repo=$6 sha=$7 target=$8 incr=$9
shift 9
f=/tmp/$name
pfx=${REMOTE_CARGO_SLOT_PREFIX:-/tmp/remote-cargo-slot-}
slotf=$pfx$slot.job
quietlock=${REMOTE_CARGO_QUIET_LOCK:-/tmp/remote-cargo-quiet.lock}
admitlock=${REMOTE_CARGO_ADMIT_LOCK:-/tmp/remote-cargo-admit.lock}
devlock=${REMOTE_CARGO_DEVICE_LOCK:-/tmp/remote-cargo-device.lock}
busy() { [ -n "$1" ] && [ ! -e "/tmp/$1.exit" ] && alive "$1"; }
fail() { echo "$1"; rm -f "$f.pid"; exit 1; }
# holder <file>: the first word of a holder file when that job is still running, else "unknown".
holder() { h=$(cut -d' ' -f1 "$1" 2>/dev/null || true); if busy "$h"; then echo "$h"; else echo unknown; fi; }
# conf <VAR> <default>: a limit from the environment or from ~/Work/mast-remote-cargo.conf (POSIX sh).
# The file is read in a subshell (its last command must succeed, or the start is
# refused), only for --admit and --quiet-window starts, so it cannot change this
# script's own variables.
conf() { ( if [ -f "$HOME/Work/mast-remote-cargo.conf" ]; then . "$HOME/Work/mast-remote-cargo.conf" || exit 1; fi; eval "printf %s \"\${$1:-$2}\"" ); }
confbad="refused: cannot read ~/Work/mast-remote-cargo.conf (a syntax error or a failing command)"
slotbad="refused: cannot read the slot env ~/Work/mast-slot-<slot>.env (a syntax error or a failing command)"
num() { case $2 in '' | *[!0-9]*) fail "refused: $1 is not a number: '$2'" ;; esac; }
# With --checkout the directory may not exist yet: it is created under the slot lock below.
[ "$sha" = - ] || case $dir in /*) ;; *) dir=$HOME/$dir ;; esac
if [ "$sha" = - ] && [ ! -d "$dir" ]; then echo "no such directory on host: $dir"; exit 1; fi
# physical <path>: the physical path of a directory, or of its parent when it does not exist yet.
physical() {
    if [ -d "$1" ]; then (cd "$1" && pwd -P); else
        up=$(cd "$(dirname "$1")" 2>/dev/null && pwd -P) || return 0
        printf '%s/%s\n' "$up" "$(basename "$1")"
    fi
}
if [ -e "$f.log" ] || [ -e "$f.exit" ] || [ -e "$f.claim" ]; then echo "name in use: $name"; exit 1; fi
(set -C; echo $$ > "$f.pid") 2>/dev/null || { echo "name in use: $name"; exit 1; }
if [ "$mode" != - ]; then
    # Resource policy for the concurrent slots: a known slot, its own checkout, its own cache.
    max=$(conf REMOTE_CARGO_MAX_JOBS 3) || fail "$confbad"; num REMOTE_CARGO_MAX_JOBS "$max"
    reserve=$(conf REMOTE_CARGO_JOB_RESERVE_KIB 10485760) || fail "$confbad"; num REMOTE_CARGO_JOB_RESERVE_KIB "$reserve"
    minmem=$(conf REMOTE_CARGO_MIN_MEM_KIB 4194304) || fail "$confbad"; num REMOTE_CARGO_MIN_MEM_KIB "$minmem"
    jobmem=$(conf REMOTE_CARGO_JOB_MEM_KIB 2097152) || fail "$confbad"; num REMOTE_CARGO_JOB_MEM_KIB "$jobmem"
    slots=$(conf REMOTE_CARGO_SLOTS "a b c") || fail "$confbad"
    ok=0
    for sl in $slots; do [ "$sl" = "$slot" ] && ok=1; done
    [ "$ok" = 1 ] || fail "refused: slot $slot is not one of: $slots"
    want=$(conf "REMOTE_CARGO_DIR_$slot" "$HOME/Work/mast-wt-build-$slot") || fail "$confbad"
    d1=$(physical "$dir"); d2=$(physical "$want")
    [ "$sha" != - ] || [ -d "$want" ] || d2=
    [ -n "$d2" ] && [ "$d1" = "$d2" ] || fail "refused: slot $slot must run in $want, not $dir"
    tgt=$(if [ -f "$HOME/Work/mast-slot-$slot.env" ]; then . "$HOME/Work/mast-slot-$slot.env" || exit 1; else CARGO_TARGET_DIR=$HOME/Work/mast-target-$slot; fi; printf %s "${CARGO_TARGET_DIR-}") || fail "$slotbad"
    [ "$tgt" = "$HOME/Work/mast-target-$slot" ] || fail "refused: slot $slot must use CARGO_TARGET_DIR $HOME/Work/mast-target-$slot, its env gives '$tgt'"
fi
if [ "$device" != - ] || [ "$mode" != - ]; then
    # The device lock is held by this launcher until it exits, then by the runner (it inherits the
    # descriptor), so it stays held for the job's whole life. The runner's cargo drops the
    # descriptor (see the runner), so a daemon it leaves behind cannot keep the device locked after
    # the job ends. --device-serial and --quiet-window take it exclusively and --admit shares it,
    # so serial jobs, admitted jobs and quiet windows exclude one another.
    if [ "$device" != - ] || [ "$mode" = quiet ]; then devopt=-x; else devopt=-s; fi
    exec 8> "$devlock"
    if ! flock $devopt -w "${REMOTE_CARGO_DEVICE_WAIT:-10}" 8; then
        if [ "$device" != - ]; then
            fail "device $device busy: $devlock is held by a running job (last claimed by $(holder "$devlock.holder")) or by admitted jobs"
        fi
        fail "refused: $devlock is held: exclusively by a --device-serial job (last claimed by $(holder "$devlock.holder")) or a quiet window ($(holder "$quietlock.holder")), or shared by admitted jobs when this start needs it exclusively"
    fi
fi
if [ "$device" != - ]; then
    # Free space on the filesystem that holds the worktrees and the shared Cargo target.
    avail=$(df -Pk "$HOME" | awk 'NR == 2 { print $4 }')
    case $avail in '' | *[!0-9]*) fail "cannot read free space for $HOME on device $device" ;; esac
    if [ "$avail" -lt 15728640 ]; then
        fail "refused: device $device has $((avail / 1048576)) GiB free on $HOME, below the 15 GiB floor"
    fi
    if [ "$avail" -lt 52428800 ]; then
        echo "warning: device $device has $((avail / 1048576)) GiB free on $HOME, below the 50 GiB planning target"
    fi
    echo "$name $device" > "$devlock.holder"
fi
# Every start takes the quiet-window lock, shared, and holds it for the job's life; a quiet window
# takes it exclusively. Plain and --device-serial starts therefore stay out of a window too. The
# runner inherits descriptor 6 and cargo drops it.
exec 6> "$quietlock"
if [ "$mode" = quiet ]; then qopt=-x; else qopt=-s; fi
if ! flock $qopt -w "${REMOTE_CARGO_QUIET_WAIT:-10}" 6; then
    if [ "$mode" = quiet ]; then fail "refused: quiet window needs an idle host: running jobs hold $quietlock"; fi
    fail "refused: a quiet window is open ($(holder "$quietlock.holder")): $quietlock is held exclusively"
fi
if [ "$mode" != - ]; then
    # Admission is serialized so two launchers cannot both count a free place; it is released
    # once this job's slot file is written.
    exec 7> "$admitlock"
    if ! flock -w 10 7; then fail "admission lock timeout"; fi
    running=0
    for jf in "$pfx"*.job; do
        [ -e "$jf" ] || continue
        j=$(cat "$jf" 2>/dev/null || true)
        if busy "$j" && [ "$jf" != "$slotf" ]; then running=$((running + 1)); fi
    done
    if [ "$mode" = quiet ]; then
        [ "$running" -eq 0 ] || fail "refused: quiet window needs an idle host, $running job(s) running"
    else
        [ "$running" -lt "$max" ] || fail "refused: $running job(s) already run, limit $max"
    fi
    avail=$(df -Pk "$HOME" | awk 'NR == 2 { print $4 }')
    case $avail in '' | *[!0-9]*) fail "refused: cannot read free space for $HOME" ;; esac
    proj=$((avail - running * reserve))
    if [ "$proj" -lt 15728640 ]; then
        fail "refused: $((avail / 1048576)) GiB free on $HOME, $((proj / 1048576)) GiB after reserving for $running running job(s), below the 15 GiB floor"
    fi
    if [ "$proj" -lt 52428800 ]; then echo "warning: $((proj / 1048576)) GiB projected free on $HOME, below the 50 GiB planning target"; fi
    mem=$(awk '/^MemAvailable:/ { print $2 }' "${REMOTE_CARGO_MEMINFO:-/proc/meminfo}" 2>/dev/null || true)
    case $mem in '' | *[!0-9]*) echo "warning: cannot read MemAvailable; the memory check was skipped" ;; *)
        if [ $((mem - running * jobmem)) -lt "$minmem" ]; then
            fail "refused: MemAvailable $((mem / 1024)) MiB, less $((running * jobmem / 1024)) MiB reserved for $running running job(s), is below the floor"
        fi ;;
    esac
fi
exec 9> "${pfx}$slot.lock"
if ! flock -w 10 9; then echo "slot $slot lock timeout"; rm -f "$f.pid"; exit 1; fi
old=$(cat "$slotf" 2>/dev/null || true)
if busy "$old"; then echo "slot $slot busy: $old"; rm -f "$f.pid"; exit 1; fi
# A fence that arrived while this launcher waited: do not touch the slot.
[ ! -e "$f.claim" ] || fail "refused: the start of $name was fenced off"
if [ "$sha" != - ]; then
    # The slot is idle and its lock (descriptor 9) is held from the check above until this job's
    # name is in the slot file, so nothing can move the checkout under a job.
    repo=$HOME/$repo
    [ -e "$repo/.git" ] || fail "no repository at $repo on this host"
    git -C "$repo" cat-file -e "$sha^{commit}" 2>/dev/null || fail "commit $sha is not on this host; push the branch to it first"
    if [ ! -e "$dir/.git" ]; then
        git -C "$repo" worktree add --detach "$dir" "$sha" >/dev/null 2>&1 6>&- 7>&- 8>&- 9>&- || fail "could not create the slot checkout $dir"
    fi
    if [ "$(git -C "$dir" rev-parse HEAD)" != "$(git -C "$repo" rev-parse "$sha^{commit}")" ]; then
        git -C "$dir" checkout --detach --force "$sha" >/dev/null 2>&1 6>&- 7>&- 8>&- 9>&- || fail "could not check out $sha in $dir"
    fi
    mkdir -p "$HOME/Work"
    envf=$HOME/Work/mast-slot-$slot.env
    marker='# managed by mast build_jobs'
    if [ ! -e "$envf" ] || head -n 1 "$envf" | grep -qF "$marker"; then
        { echo "$marker"; echo "CARGO_TARGET_DIR=\$HOME/$target"; echo "CARGO_INCREMENTAL=$incr"; echo "export CARGO_TARGET_DIR CARGO_INCREMENTAL"; } > "$envf.tmp.$$" && mv "$envf.tmp.$$" "$envf"
    else
        echo "note: leaving the unmanaged $envf in place"
    fi
    [ ! -e "$f.claim" ] || fail "refused: the start of $name was fenced off"
fi
echo "$name" > "$slotf"
if [ "$mode" = quiet ]; then echo "$name quiet" > "$quietlock.holder"; fi
exec 9>&- 7>&-
setsid nohup sh -c '
name=$1 slot=$2 dir=$3 mode=$4
shift 4
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
    # The concurrent slots keep incremental compilation to slot a.
    if [ "$mode" != - ] && [ "$slot" != a ]; then CARGO_INCREMENTAL=0; export CARGO_INCREMENTAL; fi
    cd "$dir" && exec "$@"
) 8>&- 6>&- > "/tmp/$name.log" 2>&1
rc=$?
echo "$rc" > "/tmp/$name.exit.tmp" && mv "/tmp/$name.exit.tmp" "/tmp/$name.exit"
' sh "$name" "$slot" "$dir" "$mode" "$@" > /dev/null 2>&1 < /dev/null &
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

# The claim file is the one atomic decision about whether a job ever runs: the runner creates it
# with "run" before it runs anything, and creating it first with "abandoned" stops every runner,
# including one whose start has not reached the host yet (start refuses a claimed name).
FENCE=$(cat <<'EOF'
f=/tmp/$1
if [ -e "$f.exit" ]; then echo "exit $(cat "$f.exit")"; exit 0; fi
if (set -C; echo abandoned > "$f.claim") 2>/dev/null; then echo fenced; exit 0; fi
[ -e "$f.claim" ] || { echo "cannot write $f.claim"; exit 1; }
if [ "$(cat "$f.claim")" = abandoned ]; then echo fenced; else echo started; fi
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

[ "$#" -ge 1 ] || usage
cmd=$1
shift
# --device-serial applies to start only, and comes right after it. Without it a start takes no
# device lock and checks no free space, as before.
device=-
mode=-
repo=- sha=- target=- incr=-
while [ "$cmd" = start ]; do
    case ${1-} in
    --device-serial)
        [ "$#" -ge 2 ] && [ "$device" = - ] || usage
        device=$2
        valid "$device" . && [ "$device" != - ] || usage
        shift 2 ;;
    --admit) [ "$mode" = - ] || usage; mode=admit; shift ;;
    --quiet-window) [ "$mode" = - ] || usage; mode=quiet; shift ;;
    --checkout)
        [ "$#" -ge 5 ] && [ "$sha" = - ] || usage
        repo=$2 sha=$3 target=$4 incr=$5
        for rel in "$repo" "$target"; do
            valid "$rel" ./ || usage
            case $rel in /* | -* | *..*) usage ;; esac
        done
        case $sha in *[!0-9a-fA-F]*) usage ;; esac
        [ "${#sha}" -ge 7 ] || usage
        case $incr in 0 | 1) ;; *) usage ;; esac
        shift 5 ;;
    *) break ;;
    esac
done
[ "$device" = - ] || [ "$mode" = - ] || usage
[ "$#" -ge 1 ] || usage
host=$1
case $host in -* | '') usage ;; esac
shift
[ "$#" -ge 1 ] || usage
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
    run_remote "$host" "$START" "$dir" "$slot" "$name" "$device" "$mode" "$repo" "$sha" "$target" "$incr" "$@"
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
fence)
    [ "$#" -eq 1 ] || usage
    valid "$1" . || usage
    run_remote "$host" "$FENCE" "$1"
    ;;
stop)
    [ "$#" -eq 1 ] || usage
    valid "$1" . || usage
    run_remote "$host" "$STOP" "$1"
    ;;
*) usage ;;
esac
