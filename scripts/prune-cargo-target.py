#!/usr/bin/env python3
"""Prune stale workspace build outputs from an idle Cargo target directory.

Usage: scripts/prune-cargo-target.py [--keep N] [--profile debug] [--dry-run] TARGET_DIR

Cargo never deletes superseded artifacts, so every build of a workspace crate
with a new hash leaves its rlib, test binary and, with unpacked macOS debug
info, hundreds of object files behind in deps/. This keeps the newest N hashes
of each workspace target, removes the rest along with the profile's
incremental cache, and leaves third-party dependencies alone so the next build
stays warm. It takes Cargo's own build locks without waiting and holds them
while it deletes, so it refuses to run beside a build and no build can start
halfway through. Cargo releases those locks while test binaries run, so run it
only after the target's user has finished.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ARTIFACT = re.compile(r"^(?:lib)?([A-Za-z0-9_]+)-([0-9a-f]{16})(?:\..*)?$")


def free_kib(path):
    stats = os.statvfs(path)
    return stats.f_bavail * stats.f_frsize // 1024


def size_of(path):
    if path.is_dir() and not path.is_symlink():
        return sum(size_of(child) for child in path.iterdir())
    try:
        return path.lstat().st_blocks * 512
    except FileNotFoundError:
        return 0


def remove(path):
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def workspace_target_names(root):
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=root, check=True, capture_output=True, text=True,
    )
    names = set()
    for package in json.loads(metadata.stdout)["packages"]:
        for target in package["targets"]:
            names.add(target["name"].replace("-", "_"))
    return names


def hold_locks(profile_dir):
    held = []
    for name in (".cargo-lock", ".cargo-build-lock"):
        lock_path = profile_dir / name
        if not lock_path.exists():
            continue
        handle = open(lock_path, "rb")
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            handle.close()
            sys.exit(f"{lock_path} is held: a build is using this target, so nothing was pruned")
        held.append(handle)
    return held


def stale_artifacts(deps, names, keep):
    groups = {}
    for entry in deps.iterdir():
        match = ARTIFACT.match(entry.name)
        if not match or match.group(1) not in names:
            continue
        stem, digest = match.groups()
        files = groups.setdefault(stem, {}).setdefault(digest, [])
        files.append(entry)
    stale = []
    for hashes in groups.values():
        newest_first = sorted(hashes.values(), key=lambda files: max(f.lstat().st_mtime for f in files), reverse=True)
        for files in newest_first[keep:]:
            stale.extend(files)
    return stale


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("target_dir", type=Path)
    parser.add_argument("--profile", default="debug")
    parser.add_argument("--keep", type=int, default=3, help="newest hashes kept per workspace target (default 3)")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.keep < 1:
        parser.error("--keep must be at least 1")

    profile_dir = args.target_dir.resolve() / args.profile
    deps = profile_dir / "deps"
    if not deps.is_dir():
        sys.exit(f"{deps} does not exist")
    names = workspace_target_names(Path(__file__).resolve().parents[1])
    locks = hold_locks(profile_dir)
    try:
        doomed = stale_artifacts(deps, names, args.keep)
        incremental = profile_dir / "incremental"
        if incremental.is_dir():
            doomed.extend(incremental.iterdir())
        freed = sum(size_of(path) for path in doomed)
        before = free_kib(profile_dir)
        if not args.dry_run:
            for path in doomed:
                remove(path)
        after = free_kib(profile_dir)
    finally:
        for handle in locks:
            handle.close()
    verb = "would remove" if args.dry_run else "removed"
    print(f"{verb} {len(doomed)} paths, {freed // 1024} KiB, under {profile_dir}")
    print(f"free KiB before {before} after {after}")


if __name__ == "__main__":
    main()
