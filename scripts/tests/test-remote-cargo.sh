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
trap 'rm -rf "$tmp"; rm -f /tmp/$tag-* /tmp/remote-cargo-slot-$tag.*; exit "$failures"' EXIT INT TERM

mkdir -p "$tmp/bin" "$tmp/home/.cargo/bin" "$tmp/wt"
cat > "$tmp/bin/ssh" <<'EOF'
#!/bin/sh
# The remote command is the last argument; stdin carries the script.
for last; do :; done
exec sh -c "$last"
EOF
cat > "$tmp/bin/flock" <<'EOF'
#!/bin/sh
exit 0
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
esac
EOF
chmod +x "$tmp"/bin/* "$tmp/home/.cargo/bin/cargo"
export PATH="$tmp/bin:$PATH" HOME="$tmp/home" FAKE_CARGO_DIR="$tmp"

rc() { sh "$script" "$@"; }
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

[ "$failures" = 0 ] && echo "all passed" || echo "$failures failed"
