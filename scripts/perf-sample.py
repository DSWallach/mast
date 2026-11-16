#!/usr/bin/env python3
"""Sample the running Mast app and summarise where its CPU goes.

Usage: scripts/perf-sample.py [--pid PID] [--seconds N] [--top N] [--keep FILE]

Finds the installed Mast GUI process (not its ` mcp` helpers), prints CPU and
RSS, runs macOS `sample`, then attributes every busy (non-waiting) sample to
the innermost `mast_*` frame above it. Output is the busy-sample count per
thread kind and the top Mast frames. Read-only: it never signals the process.
Post-mortem 2026-11-16 (docs/internal/postmortems/2026-11-16-slow-mast.md), P6.
"""

import argparse
import collections
import os
import re
import subprocess
import sys
import tempfile

IDLE = (
    "__psynch_cvwait", "kevent", "mach_msg2_trap", "__workq_kernreturn",
    "__semwait_signal", "__psynch_mutexwait", "semaphore_wait_trap",
    "__select", "__ulock_wait", "poll",
)


def find_pid():
    out = subprocess.run(["ps", "-Ao", "pid=,command="], capture_output=True, text=True).stdout
    for line in out.splitlines():
        pid, _, command = line.strip().partition(" ")
        if "MacOS/mast" in command and " mcp" not in command:
            return int(pid)
    sys.exit("Mast is not running")


def cpu_rss(pid):
    out = subprocess.run(["ps", "-o", "%cpu=,rss=,etime=", "-p", str(pid)], capture_output=True, text=True).stdout.split()
    return f"cpu {out[0]}%  rss {int(out[1]) / 1048576:.1f} GB  up {out[2]}" if len(out) == 3 else "gone"


def short(sym):
    parts = re.findall(r"(?<=[0-9])([a-z_][a-z0-9_]{2,})", sym)
    for i, part in enumerate(parts):
        if part.startswith("mast_"):
            return "::".join(parts[i:i + 4])
    return None


def parse(path):
    text = open(path).read()
    graph = text.split("Call graph:")[1].split("Total number in stack")[0]
    rows = []
    for line in graph.splitlines():
        m = re.match(r"^(\s*[+!:| ]*)(\d+) (.*)$", line)
        if m:
            rows.append((len(m.group(1)), int(m.group(2)), m.group(3).split("  (in ")[0]))
    return rows


def attribute(rows):
    owners, threads = collections.Counter(), collections.Counter()
    stack, thread = [], ""
    for i, (depth, count, sym) in enumerate(rows):
        while stack and stack[-1][0] >= depth:
            stack.pop()
        stack.append((depth, sym))
        if sym.startswith("Thread_"):
            thread = "main" if "main-thread" in sym else sym.split(":")[-1].strip()[:32] or "unnamed"
        child_depth = rows[i + 1][0] if i + 1 < len(rows) else -1
        children = 0
        if child_depth > depth:
            j = i + 1
            while j < len(rows) and rows[j][0] > depth:
                if rows[j][0] == child_depth:
                    children += rows[j][1]
                j += 1
        own = count - children
        if own <= 0 or any(sym.startswith(idle) for idle in IDLE):
            continue
        owner = next((short(s) for _, s in reversed(stack) if short(s)), None)
        owners[owner or "<no mast frame> " + sym[:50]] += own
        threads[thread] += own
    return owners, threads


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int)
    ap.add_argument("--seconds", type=int, default=3)
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--keep", help="save the raw sample here")
    args = ap.parse_args()
    pid = args.pid or find_pid()
    print(f"pid {pid}  {cpu_rss(pid)}")
    path = args.keep or tempfile.mktemp(suffix=".txt", prefix="mast-sample-")
    subprocess.run(["sample", str(pid), str(args.seconds), "-file", path], capture_output=True)
    owners, threads = attribute(parse(path))
    print("busy samples by thread:", ", ".join(f"{k} {v}" for k, v in threads.most_common(6)))
    for owner, count in owners.most_common(args.top):
        print(f"{count:6} {owner}")
    print(f"after: {cpu_rss(pid)}")
    if not args.keep:
        os.unlink(path)


if __name__ == "__main__":
    main()
