#!/usr/bin/env python3
"""Perf-gate stamp: written by run_release_tests.sh and perf-soak.sh, required by
install-macos-app.sh (HANDOFF "Perf gate before every install").

  perf-gate.py write  --section release_tests|soak --json '<object or "pass">'
  perf-gate.py check  [--binary PATH]
  perf-gate.py override --reason TEXT

A stamp is target/perf-gate/<commit>.json:
  {"commit", "time", "release_tests": "pass", "release_selection", "budget_tests": [...],
   "soak": {"pass": true, "mode": "real", "candidate_sha256", cpu_pct, steady_cpu_pct,
            "rss_mb", "git_spawns_per_min", "baseline": {...}}}

`check` passes when a stamp for HEAD has release_tests == "pass" and a passing real-data soak,
or when the newest such stamp is for an ancestor commit and `git diff --name-only <stamp>..HEAD`
touches only docs/** or crates/*/tests/** (test-only fixes after the run). With --binary the
soaked candidate must be byte-identical to the binary being installed.
Exit: 0 ok, 1 gate not satisfied (reasons printed), 2 usage/environment error, 3 write skipped
(dirty tree: a stamp for HEAD would not describe the tested code).
"""
import argparse
import datetime
import fnmatch
import hashlib
import json
import os
import subprocess
import sys

ROOT = os.environ.get("MAST_PERF_GATE_REPO") or subprocess.run(
    ["git", "-C", os.path.dirname(os.path.abspath(__file__)), "rev-parse", "--show-toplevel"],
    capture_output=True, text=True).stdout.strip()
STAMP_DIR = os.environ.get("MAST_PERF_GATE_DIR") or os.path.join(ROOT, "target", "perf-gate")
OVERRIDES = os.environ.get("MAST_PERF_GATE_OVERRIDES_FILE") or os.path.join(
    ROOT, "docs", "internal", "perf-gate-overrides.md")
TEST_ONLY = ("docs/**", "crates/*/tests/**")


def git(*args):
    out = subprocess.run(["git", "-C", ROOT, *args], capture_output=True, text=True)
    return out.returncode, out.stdout.strip()


def head():
    code, sha = git("rev-parse", "HEAD")
    if code:
        sys.exit("perf-gate: not a git checkout")
    return sha


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load(commit):
    try:
        with open(os.path.join(STAMP_DIR, commit + ".json")) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def test_only(paths):
    return all(any(fnmatch.fnmatchcase(p, pat) for pat in TEST_ONLY) for p in paths)


def problems(stamp, binary_hash):
    """Why this stamp does not satisfy the gate (empty list = satisfied)."""
    missing = []
    if stamp.get("release_tests") != "pass":
        missing.append("release tests did not pass (run scripts/run_release_tests.sh)")
    elif not stamp.get("budget_tests"):
        missing.append("no budget-test results in the stamp")
    soak = stamp.get("soak")
    if not isinstance(soak, dict) or not soak.get("pass"):
        missing.append("no passing soak (run scripts/perf-soak.sh <candidate> --baseline <installed> --real)")
    elif soak.get("mode") != "real":
        missing.append(f"soak ran on {soak.get('mode')!r} data; the gate needs --real")
    elif not soak.get("baseline"):
        missing.append("soak had no baseline binary")
    elif binary_hash and soak.get("candidate_sha256") != binary_hash:
        missing.append("the binary being installed is not the one that was soaked (hash differs); re-soak it")
    return missing


def cmd_write(args):
    code, dirty = git("status", "--porcelain", "--untracked-files=no")
    if dirty:
        print("perf-gate: tracked changes present; not writing a stamp for HEAD", file=sys.stderr)
        return 3
    commit = head()
    value = args.json
    try:
        value = json.loads(value)
    except ValueError:
        pass
    os.makedirs(STAMP_DIR, exist_ok=True)
    stamp = load(commit) or {}
    stamp.update({"commit": commit, "time": now()})
    if args.section == "release_tests":
        stamp["release_tests"] = value["result"]
        stamp["release_selection"] = value.get("selection")
        stamp["budget_tests"] = value.get("budget_tests", [])
    else:
        stamp["soak"] = value
    tmp = os.path.join(STAMP_DIR, f".{commit}.tmp")
    with open(tmp, "w") as f:
        json.dump(stamp, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, os.path.join(STAMP_DIR, commit + ".json"))
    print(f"perf-gate: stamped {commit[:9]} ({args.section})")
    return 0


def cmd_check(args):
    commit = head()
    binary_hash = None
    if args.binary:
        try:
            binary_hash = sha256(args.binary)
        except OSError as error:
            print(f"perf-gate: cannot read {args.binary}: {error}", file=sys.stderr)
            return 2
    stamp = load(commit)
    if stamp is not None:
        missing = problems(stamp, binary_hash)
        if not missing:
            print(f"perf-gate: ok (stamp for HEAD {commit[:9]})")
            return 0
        report = [f"stamp for HEAD {commit[:9]}:"] + ["  - " + m for m in missing]
    else:
        report = [f"no stamp for HEAD {commit[:9]} in {STAMP_DIR}"]
    # An earlier stamp is acceptable when only tests/docs changed since.
    candidates = []
    if os.path.isdir(STAMP_DIR):
        for name in os.listdir(STAMP_DIR):
            if not name.endswith(".json") or name.startswith(".") or name[:-5] == commit:
                continue
            sha = name[:-5]
            if git("merge-base", "--is-ancestor", sha, commit)[0] != 0:
                continue
            other = load(sha)
            if other and not problems(other, binary_hash):
                candidates.append((other.get("time", ""), sha))
    for _, sha in sorted(candidates, reverse=True):
        _, changed = git("diff", "--name-only", f"{sha}..{commit}")
        paths = [p for p in changed.splitlines() if p]
        if test_only(paths):
            print(f"perf-gate: ok (stamp for {sha[:9]}; {len(paths)} file(s) since are docs/tests only)")
            return 0
        report.append(f"earlier stamp {sha[:9]} is stale: {len(paths)} file(s) changed since, "
                      f"e.g. {next(p for p in paths if not test_only([p]))}")
    print("perf-gate: REFUSED. Missing:\n" + "\n".join(report), file=sys.stderr)
    return 1


def cmd_override(args):
    reason = " ".join(args.reason.split())
    if not reason:
        print("perf-gate: --skip-perf-gate needs a reason", file=sys.stderr)
        return 2
    os.makedirs(os.path.dirname(OVERRIDES), exist_ok=True)
    new = not os.path.exists(OVERRIDES)
    with open(OVERRIDES, "a") as f:
        if new:
            f.write("# Perf-gate overrides\n\nOne line per install that skipped the gate: time, HEAD, reason.\n\n")
        f.write(f"- {now()} {head()} {reason}\n")
    print(f"perf-gate: override recorded in {OVERRIDES}; commit it")
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("write")
    w.add_argument("--section", choices=["release_tests", "soak"], required=True)
    w.add_argument("--json", required=True)
    c = sub.add_parser("check")
    c.add_argument("--binary")
    o = sub.add_parser("override")
    o.add_argument("--reason", required=True)
    args = ap.parse_args()
    return {"write": cmd_write, "check": cmd_check, "override": cmd_override}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
