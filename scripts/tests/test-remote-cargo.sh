#!/bin/sh
# Tests scripts/remote-cargo.sh on this machine: a fake `ssh` runs the remote script locally, and
# fake `flock`, `setsid` and `cargo` stand in for the build host's tools. Needs perl (for setsid).
# Run: sh scripts/tests/test-remote-cargo.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
script=$here/../remote-cargo.sh
tmp=$(mktemp -d /tmp/rc-test.XXXXXX)
tag=rctest$$
failures=0
trap 'rm -rf "$tmp"; rm -f /tmp/$tag-* /tmp/remote-cargo-slot-$tag*; exit "$failures"' EXIT INT TERM

mkdir -p "$tmp/bin" "$tmp/home/.cargo/bin" "$tmp/wt"
REAL_FLOCK=$(command -v flock)
export REAL_FLOCK
cat > "$tmp/bin/ssh" <<'EOF'
#!/bin/sh
# The remote command is the last argument; stdin carries the script.
for last; do :; done
exec ${RC_SH:-sh} -c "$last"
EOF
cat > "$tmp/bin/flock" <<'EOF'
#!/bin/sh
# No-op unless FAKE_FLOCK_REAL is set (the device-lock tests need real locks).
if [ -n "${FAKE_FLOCK_REAL:-}" ]; then exec "$REAL_FLOCK" "$@"; fi
exit 0
EOF
cat > "$tmp/bin/df" <<'EOF'
#!/bin/sh
# Fake `df -Pk`: one data line whose Available column is FAKE_DF_AVAIL_KIB.
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "/dev/fake 1 1 ${FAKE_DF_AVAIL_KIB:-104857600} 1% /"
EOF
cat > "$tmp/bin/setsid" <<'EOF'
#!/bin/sh
sleep "${FAKE_SETSID_DELAY:-0}"
exec perl -e 'use POSIX; POSIX::setsid(); exec @ARGV' "$@"
EOF
cat > "$tmp/home/.cargo/bin/cargo" <<'EOF'
#!/bin/sh
echo "$@" >> "$FAKE_CARGO_DIR/ran"
case $1 in
ok) echo "test result: ok. 1 passed" ;;
fail) echo "error: boom"; exit 3 ;;
sleep) sleep 60 & echo $! > "$FAKE_CARGO_DIR/sleep.pid"; wait ;;
env) echo "INC=${CARGO_INCREMENTAL-} TGT=${CARGO_TARGET_DIR-}" ;;
daemon) sleep 30 & echo $! > "$FAKE_CARGO_DIR/daemon.pid"; echo "test result: ok. 1 passed" ;;
esac
EOF
chmod +x "$tmp"/bin/* "$tmp/home/.cargo/bin/cargo"
[ -n "${RC_SH:-}" ] && ln -s "$(command -v "$RC_SH")" "$tmp/bin/sh"
export PATH="$tmp/bin:$PATH" HOME="$tmp/home" FAKE_CARGO_DIR="$tmp"

rc() { ${RC_SH:-sh} "$script" "$@"; }
pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1"; failures=$((failures + 1)); }
# check <description> <expected exit> <output must contain> -- <command...>
check() {
    desc=$1 want=$2 text=$3
    shift 4
    out=$("$@" 2>&1) && got=0 || got=$?
    if [ "$got" = "$want" ] && case $out in *"$text"*) true ;; *) false ;; esac; then pass "$desc"
    else fail "$desc: exit $got (want $want), output: $out"; fi
}

check "start refuses a command that is not cargo" 2 "only a cargo command" -- \
    rc start h "$tmp/wt" $tag $tag-nocargo -- sh -c true
[ ! -e "/tmp/$tag-nocargo.pid" ] && pass "a refused start leaves no files" || fail "a refused start left files"

check "start launches a cargo job" 0 "started $tag-ok" -- rc start h "$tmp/wt" $tag $tag-ok -- cargo ok
check "wait reports the job's success" 0 "exit 0" -- rc wait h $tag-ok 20s
check "status of a finished job" 0 "exit 0" -- rc status h $tag-ok
check "stop of a finished job is a no-op" 0 "already finished" -- rc stop h $tag-ok
check "a name is single-use" 1 "name in use" -- rc start h "$tmp/wt" $tag $tag-ok -- cargo ok

check "start launches a failing job" 0 "started" -- rc start h "$tmp/wt" $tag $tag-fail -- cargo fail
check "wait reports the job's failure" 1 "error: boom" -- rc wait h $tag-fail 20s
check "unknown job" 2 "unknown job" -- rc wait h $tag-nope 5s

# stop reaches the whole job, leaves the runner to write the marker, and frees the slot.
check "start a long job" 0 "started" -- rc start h "$tmp/wt" $tag $tag-long -- cargo sleep
check "the slot is busy while it runs" 1 "slot $tag busy" -- rc start h "$tmp/wt" $tag $tag-other -- cargo ok
check "status of a running job" 0 running -- rc status h $tag-long
sleep 1
check "stop ends the job" 0 "stopped: exit 143" -- rc stop h $tag-long
if kill -0 "$(cat "$tmp/sleep.pid")" 2>/dev/null; then fail "the job's child outlived stop"; kill "$(cat "$tmp/sleep.pid")"
else pass "the job's child is gone"; fi
check "wait after stop reports the stopped exit" 1 "exit 143" -- rc wait h $tag-long 20s
check "the slot is free after stop" 0 "started" -- rc start h "$tmp/wt" $tag $tag-after -- cargo ok
check "wait for the next job" 0 "exit 0" -- rc wait h $tag-after 20s

# A pid the host reused for another process is not the job: wait and status say it died, stop
# refuses and leaves the stranger alone.
sleep 60 & stranger=$!
echo $stranger > "/tmp/$tag-reuse.pid"
echo "Thu Jan  1 00:00:00 1970" | tr -s ' ' > "/tmp/$tag-reuse.start"
check "status ignores a reused pid" 1 died -- rc status h $tag-reuse
check "wait ignores a reused pid" 125 "died without exit marker" -- rc wait h $tag-reuse 5s
check "stop refuses a reused pid" 1 "not running" -- rc stop h $tag-reuse
kill -0 $stranger 2>/dev/null && pass "the reused pid's process was left alone" || fail "stop killed an unrelated process"
kill $stranger 2>/dev/null
# Without a recorded start time (a job from before it was recorded), the pid alone counts.
sleep 60 & stranger=$!
echo $stranger > "/tmp/$tag-old.pid"
check "a job without a start time is judged by its pid" 0 running -- rc status h $tag-old
# stop never signals a pid it cannot tell from a reused one.
check "stop refuses a pid with no recorded start time" 1 "refusing: no recorded start time" -- rc stop h $tag-old
kill -0 $stranger 2>/dev/null && pass "the unverified process was left alone" || fail "stop signalled an unverified process"
kill $stranger 2>/dev/null

# A runner that starts after the launcher gave up must not run the job.
export FAKE_SETSID_DELAY=7
check "a runner that is too late is refused" 1 "runner did not start" -- \
    rc start h "$tmp/wt" $tag $tag-late -- cargo ok
unset FAKE_SETSID_DELAY
sleep 4
if [ -e "/tmp/$tag-late.log" ] || [ -e "/tmp/$tag-late.exit" ] || [ -e "/tmp/$tag-late.pid" ]; then fail "a late runner ran the abandoned job"
else pass "a late runner exits without running"; fi
check "an abandoned name stays used" 1 "name in use" -- rc start h "$tmp/wt" $tag $tag-late -- cargo ok
check "the slot is free after an abandoned start" 0 "started" -- rc start h "$tmp/wt" $tag $tag-final -- cargo ok
check "wait for the final job" 0 "exit 0" -- rc wait h $tag-final 20s

# --device-serial: one job per device (host-wide lock) and a free-space floor. The real flock
# is used here, with a lock path of the test's own so it never touches a real device lock.
export FAKE_FLOCK_REAL=1 REMOTE_CARGO_DEVICE_LOCK="$tmp/device.lock" REMOTE_CARGO_DEVICE_WAIT=2
export FAKE_DF_AVAIL_KIB=$((60 * 1048576))
check "a start without the option ignores free space" 0 "started" -- \
    env FAKE_DF_AVAIL_KIB=$((1 * 1048576)) sh "$script" start h "$tmp/wt" ${tag}x $tag-nodev -- cargo ok
check "the job without the option finishes" 0 "exit 0" -- rc wait h $tag-nodev 20s
check "a bad device serial is a usage error" 2 "Usage" -- rc start --device-serial 'a b' h "$tmp/wt" $tag $tag-badser -- cargo ok
check "the option needs a serial" 2 "Usage" -- rc start --device-serial
check "a serial of - is refused" 2 "Usage" -- rc start --device-serial - h "$tmp/wt" $tag $tag-dash -- cargo ok

export FAKE_DF_AVAIL_KIB=$((10 * 1048576))
check "below 15 GiB free is refused" 1 "below the 15 GiB floor" -- \
    rc start --device-serial SER0 h "$tmp/wt" $tag $tag-lowdisk -- cargo ok
[ ! -e "/tmp/$tag-lowdisk.pid" ] && pass "a refused start leaves no pid file" || fail "a refused disk start left its pid file"
export FAKE_DF_AVAIL_KIB=$((20 * 1048576))
check "between 15 and 50 GiB starts with a warning" 0 "below the 50 GiB planning target" -- \
    rc start --device-serial SER0 h "$tmp/wt" $tag $tag-warn -- cargo ok
check "the warned job finishes" 0 "exit 0" -- rc wait h $tag-warn 20s
export FAKE_DF_AVAIL_KIB=$((60 * 1048576))

check "a device job starts" 0 "started $tag-dev1" -- rc start --device-serial SER1 h "$tmp/wt" $tag $tag-dev1 -- cargo sleep
check "a second job on the same device waits, then is refused" 1 "device SER2 busy" -- \
    rc start --device-serial SER2 h "$tmp/wt" ${tag}y $tag-dev2 -- cargo ok
check "the holder is named while the device lock is held" 1 "$tag-dev1" -- \
    rc start --device-serial SER2 h "$tmp/wt" ${tag}y $tag-dev3 -- cargo ok
check "a job without the option runs beside a device job" 0 "started" -- rc start h "$tmp/wt" ${tag}x $tag-beside -- cargo ok
check "that job finishes" 0 "exit 0" -- rc wait h $tag-beside 20s
check "the device job runs" 0 running -- rc status h $tag-dev1
check "stop ends the device job" 0 "stopped" -- rc stop h $tag-dev1
check "after stop the device is free again" 0 "started" -- rc start --device-serial SER2 h "$tmp/wt" ${tag}y $tag-dev4 -- cargo ok
check "that device job finishes" 0 "exit 0" -- rc wait h $tag-dev4 20s

# A daemon the job leaves behind must not keep the device lock: the job's cargo drops the lock's
# descriptor before it runs.
check "a job that leaves a daemon behind" 0 "started" -- rc start --device-serial SER3 h "$tmp/wt" $tag $tag-daemon -- cargo daemon
check "the daemon's job finishes" 0 "exit 0" -- rc wait h $tag-daemon 20s
check "the device is free although the daemon runs" 0 "started" -- rc start --device-serial SER3 h "$tmp/wt" $tag $tag-after-daemon -- cargo ok
check "the job after the daemon finishes" 0 "exit 0" -- rc wait h $tag-after-daemon 20s
[ -s "$tmp/daemon.pid" ] && kill "$(cat "$tmp/daemon.pid")" 2>/dev/null
unset FAKE_FLOCK_REAL REMOTE_CARGO_DEVICE_LOCK REMOTE_CARGO_DEVICE_WAIT FAKE_DF_AVAIL_KIB

# --admit and --quiet-window: concurrent slots with a slot, checkout and cache policy, an admission
# cap, projected disk and memory checks, and exclusion with --device-serial jobs and quiet windows.
# Lock, slot and conf paths are the test's own, and HOME is the test's home.
W=$tmp/home/Work
mkdir -p $W/mast-wt-build-a $W/mast-wt-build-b $W/mast-wt-build-c $W/mast-wt-build-d $W/other
export FAKE_FLOCK_REAL=1 REMOTE_CARGO_SLOT_PREFIX="$tmp/slot-" REMOTE_CARGO_QUIET_LOCK="$tmp/quiet.lock" \
    REMOTE_CARGO_ADMIT_LOCK="$tmp/admit.lock" REMOTE_CARGO_DEVICE_LOCK="$tmp/device2.lock" \
    REMOTE_CARGO_QUIET_WAIT=2 REMOTE_CARGO_DEVICE_WAIT=2 REMOTE_CARGO_MIN_MEM_KIB=1 REMOTE_CARGO_JOB_MEM_KIB=0
export FAKE_DF_AVAIL_KIB=$((100 * 1048576))
A="rc start --admit h"
check "--admit and --quiet-window together are a usage error" 2 "Usage" -- rc start --admit --quiet-window h $W/mast-wt-build-a a $tag-both -- cargo ok
check "--admit and --device-serial together are a usage error" 2 "Usage" -- rc start --admit --device-serial S h $W/mast-wt-build-a a $tag-both -- cargo ok
check "--device-serial twice is a usage error" 2 "Usage" -- rc start --device-serial S --device-serial T h $W/mast-wt-build-a a $tag-both -- cargo ok

# Resource policy: slot allowlist, checkout and cache mapping, incremental only in slot a.
check "an unlisted slot is refused" 1 "not one of" -- $A $W/mast-wt-build-d d $tag-badslot -- cargo ok
check "a slot in another slot's checkout is refused" 1 "must run in" -- $A $W/mast-wt-build-a b $tag-baddir -- cargo ok
check "a slot in an unrelated checkout is refused" 1 "must run in" -- $A $W/other a $tag-baddir2 -- cargo ok
mkdir -p $tmp/home/Work
echo 'CARGO_TARGET_DIR=$HOME/Work/mast-target-a' > $W/mast-slot-b.env
check "a slot env that points at another slot's cache is refused" 1 "must use CARGO_TARGET_DIR" -- $A $W/mast-wt-build-b b $tag-badtgt -- cargo ok
echo 'export CARGO_INCREMENTAL=1; export CARGO_TARGET_DIR=$HOME/Work/mast-target-b' > $W/mast-slot-b.env
[ ! -e "/tmp/$tag-badtgt.pid" ] && pass "a refused policy start leaves no pid file" || fail "a refused policy start left its pid file"
check "slot b runs admitted with its own cache" 0 "started" -- $A $W/mast-wt-build-b b $tag-inc -- cargo env
check "slot b is forced to CARGO_INCREMENTAL=0 whatever its env says" 0 "INC=0 TGT=$W/mast-target-b" -- rc wait h $tag-inc 20s
check "slot a keeps its own incremental setting" 0 "started" -- $A $W/mast-wt-build-a a $tag-inca -- cargo env
check "slot a is not forced" 0 "INC= TGT=$W/mast-target-a" -- rc wait h $tag-inca 20s
rm -f $W/mast-slot-b.env
check "a plain start keeps the old slot freedom" 0 "started" -- rc start h $W/other zz $tag-free -- cargo ok
check "that plain job finishes" 0 "exit 0" -- rc wait h $tag-free 20s

# Admission cap, aggregate disk and memory, and the serial rollback.
check "three admitted jobs start side by side (1)" 0 "started $tag-ad1" -- $A $W/mast-wt-build-a a $tag-ad1 -- cargo sleep
check "three admitted jobs start side by side (2)" 0 "started $tag-ad2" -- $A $W/mast-wt-build-b b $tag-ad2 -- cargo sleep
check "three admitted jobs start side by side (3)" 0 "started $tag-ad3" -- $A $W/mast-wt-build-c c $tag-ad3 -- cargo sleep
check "a fourth job is refused at the cap" 1 "limit 3" -- env REMOTE_CARGO_SLOTS="a b c d" sh "$script" start --admit h $W/mast-wt-build-d d $tag-ad4 -- cargo ok
[ ! -e "/tmp/$tag-ad4.pid" ] && pass "a refused admission leaves no pid file" || fail "a refused admission left its pid file"
check "an occupied slot is refused" 1 "slot a busy" -- env REMOTE_CARGO_MAX_JOBS=9 sh "$script" start --admit h $W/mast-wt-build-a a $tag-ad5 -- cargo ok
check "a limit of one refuses a second job" 1 "limit 1" -- env REMOTE_CARGO_MAX_JOBS=1 sh "$script" start --admit h $W/mast-wt-build-b b $tag-ad6 -- cargo ok
check "a non-numeric limit is refused, not ignored" 1 "not a number" -- env REMOTE_CARGO_MAX_JOBS=lots sh "$script" start --admit h $W/mast-wt-build-b b $tag-ad7 -- cargo ok
check "a quiet window is refused while admitted jobs run" 1 "refused" -- rc start --quiet-window h $W/mast-wt-build-a a $tag-q0 -- cargo ok
check "a --device-serial start is refused while admitted jobs run" 1 "device SER busy" -- rc start --device-serial SER h $W/other zz $tag-dev0 -- cargo ok
check "a plain start still runs beside admitted jobs" 0 "started" -- rc start h $W/other zz $tag-plain -- cargo ok
check "the plain job finishes" 0 "exit 0" -- rc wait h $tag-plain 20s
for n in ad1 ad2 ad3; do check "stop $n" 0 "stopped" -- rc stop h $tag-$n; done

# The conf file: read for --admit and --quiet-window only, and malformed values are refused.
printf 'export REMOTE_CARGO_MAX_JOBS=oops\n' > $tmp/home/Work/mast-remote-cargo.conf
check "a malformed conf refuses an admitted start" 1 "not a number" -- $A $W/mast-wt-build-a a $tag-cf1 -- cargo ok
check "a malformed conf does not touch a plain start" 0 "started" -- rc start h $W/other zz $tag-cf2 -- cargo ok
check "that plain job finishes" 0 "exit 0" -- rc wait h $tag-cf2 20s
printf 'export REMOTE_CARGO_MAX_JOBS=(\n' > $tmp/home/Work/mast-remote-cargo.conf
check "a conf with a syntax error refuses the start" 1 "refused: cannot read" -- $A $W/mast-wt-build-a a $tag-cf5 -- cargo ok
[ ! -e "/tmp/$tag-cf5.pid" ] && pass "a refused start for a syntax-error conf leaves no pid file" || fail "a syntax-error conf left a pid file"
printf 'false\n' > $tmp/home/Work/mast-remote-cargo.conf
check "a conf with a failing command refuses the start" 1 "refused: cannot read ~/Work/mast-remote-cargo.conf" -- $A $W/mast-wt-build-a a $tag-cf6 -- cargo ok
[ ! -e "/tmp/$tag-cf6.pid" ] && pass "a refused start for a failing conf leaves no pid file" || fail "a failing conf left a pid file"
check "the name of a refused start can be used again" 1 "refused: cannot read" -- $A $W/mast-wt-build-a a $tag-cf6 -- cargo ok
printf 'export REMOTE_CARGO_MAX_JOBS=3\n' > $tmp/home/Work/mast-remote-cargo.conf
printf 'false\n' > $W/mast-slot-b.env
check "a slot env with a failing command refuses the start" 1 "refused: cannot read the slot env" -- $A $W/mast-wt-build-b b $tag-cf7 -- cargo ok
[ ! -e "/tmp/$tag-cf7.pid" ] && pass "a refused start for a failing slot env leaves no pid file" || fail "a failing slot env left a pid file"
rm -f $W/mast-slot-b.env
check "a bad conf does not touch a plain start" 0 "started" -- rc start h $W/other zz $tag-cf8 -- cargo ok
check "that plain job finishes" 0 "exit 0" -- rc wait h $tag-cf8 20s
printf 'export REMOTE_CARGO_MAX_JOBS=1\n' > $tmp/home/Work/mast-remote-cargo.conf
check "the conf sets the serial rollback limit" 0 "started" -- $A $W/mast-wt-build-a a $tag-cf3 -- cargo sleep
check "the conf limit refuses a second admitted job" 1 "limit 1" -- $A $W/mast-wt-build-b b $tag-cf4 -- cargo ok
check "stop cf3" 0 "stopped" -- rc stop h $tag-cf3
rm -f $tmp/home/Work/mast-remote-cargo.conf

# Two launchers racing for the last place: exactly one starts.
rm -f $tmp/race1 $tmp/race2
env REMOTE_CARGO_MAX_JOBS=1 sh "$script" start --admit h $W/mast-wt-build-a a $tag-r1 -- cargo sleep > $tmp/race1 2>&1 &
env REMOTE_CARGO_MAX_JOBS=1 sh "$script" start --admit h $W/mast-wt-build-b b $tag-r2 -- cargo sleep > $tmp/race2 2>&1 &
wait $! ; wait
started=$(cat $tmp/race1 $tmp/race2 | grep -c '^started')
[ "$started" = 1 ] && pass "two racing launchers start exactly one job" || fail "racing launchers started $started jobs"
for n in r1 r2; do rc stop h $tag-$n > /dev/null 2>&1; done

# Projected disk and memory.
export FAKE_DF_AVAIL_KIB=$((30 * 1048576))
check "one running job reserves disk: admitted with a warning" 0 "below the 50 GiB" -- $A $W/mast-wt-build-a a $tag-dk1 -- cargo sleep
check "a second job starts" 0 "started" -- $A $W/mast-wt-build-b b $tag-dk2 -- cargo sleep
check "two running jobs reserve enough to refuse a third" 1 "below the 15 GiB floor" -- $A $W/mast-wt-build-c c $tag-dk3 -- cargo ok
for n in dk1 dk2; do check "stop $n" 0 "stopped" -- rc stop h $tag-$n; done
export FAKE_DF_AVAIL_KIB=$((100 * 1048576))
if [ -r /proc/meminfo ]; then
    check "low memory is refused" 1 "MemAvailable" -- env REMOTE_CARGO_MIN_MEM_KIB=999999999999 sh "$script" start --admit h $W/mast-wt-build-a a $tag-mem -- cargo ok
fi

check "a missing MemAvailable warns and skips the memory check" 0 "cannot read MemAvailable" -- env REMOTE_CARGO_MEMINFO=$tmp/no-meminfo sh "$script" start --admit h $W/mast-wt-build-a a $tag-nomem -- cargo ok
check "that job finishes" 0 "exit 0" -- rc wait h $tag-nomem 20s

# A quiet window excludes every kind of start, and every kind of start excludes a window.
check "a quiet window starts on an idle host" 0 "started $tag-q1" -- rc start --quiet-window h $W/mast-wt-build-a a $tag-q1 -- cargo sleep
check "an admitted start is kept out of the window" 1 "($tag-q1)" -- $A $W/mast-wt-build-b b $tag-q2 -- cargo ok
check "a plain start is kept out of the window" 1 "quiet window is open ($tag-q1)" -- rc start h $W/other zz $tag-q2b -- cargo ok
check "a --device-serial start is kept out of the window" 1 "device SER busy" -- rc start --device-serial SER h $W/other zz $tag-q2c -- cargo ok
check "a second window is refused" 1 "refused" -- rc start --quiet-window h $W/mast-wt-build-b b $tag-q3 -- cargo ok
check "stop ends the window" 0 "stopped" -- rc stop h $tag-q1
check "an admitted job starts after the window" 0 "started" -- $A $W/mast-wt-build-b b $tag-q4 -- cargo ok
check "that job finishes" 0 "exit 0" -- rc wait h $tag-q4 20s
check "a plain job holds the shared lock: a window is refused" 0 "started" -- rc start h $W/other zz $tag-q5 -- cargo sleep
check "the window is refused while a plain job runs" 1 "refused" -- rc start --quiet-window h $W/mast-wt-build-a a $tag-q6 -- cargo ok
check "stop the plain job" 0 "stopped" -- rc stop h $tag-q5
check "a --device-serial job holds the shared lock too" 0 "started" -- rc start --device-serial SER h $W/other zz $tag-q7 -- cargo sleep
check "the window is refused while a serial job runs" 1 "refused" -- rc start --quiet-window h $W/mast-wt-build-a a $tag-q8 -- cargo ok
check "stop the serial job" 0 "stopped" -- rc stop h $tag-q7
check "the window can be taken again once jobs finish" 0 "started" -- rc start --quiet-window h $W/mast-wt-build-a a $tag-q9 -- cargo ok
check "the window job finishes" 0 "exit 0" -- rc wait h $tag-q9 20s
unset FAKE_FLOCK_REAL REMOTE_CARGO_SLOT_PREFIX REMOTE_CARGO_QUIET_LOCK REMOTE_CARGO_ADMIT_LOCK REMOTE_CARGO_DEVICE_LOCK REMOTE_CARGO_QUIET_WAIT REMOTE_CARGO_DEVICE_WAIT REMOTE_CARGO_MIN_MEM_KIB REMOTE_CARGO_JOB_MEM_KIB FAKE_DF_AVAIL_KIB

[ "$failures" = 0 ] && echo "all passed" || echo "$failures failed"
