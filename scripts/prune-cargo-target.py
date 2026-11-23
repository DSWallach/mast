#!/usr/bin/env python3
"""Prune stale workspace build outputs from an idle Cargo target directory.

Usage: scripts/prune-cargo-target.py [--keep N] [--profile debug] [--dry-run] TARGET_DIR

Cargo never deletes superseded artifacts, so every build of a workspace crate
with a new hash leaves its rlib, test binary and, with unpacked macOS debug
info, hundreds of object files behind in deps/. This keeps the newest N builds
of each workspace target in deps/ and examples/, counting check-only builds
separately, and removes the rest along with the profile's incremental cache.
Dep-info decides what is workspace code, so third-party dependencies stay and
the next build stays warm. It takes Cargo's own build locks without waiting and holds them
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
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
        cwd=root, capture_output=True, text=True,
    )
    if metadata.returncode != 0:
        sys.exit(f"cargo metadata failed:\n{metadata.stderr}")
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


def built_from(directory, stem, digest, source_root):
    """Whether this build's crate root is in the repository.

    A crates.io crate can share a name with a workspace target, so the name
    alone does not make a build ours. Rustc lists workspace sources relative
    to the workspace and registry sources as absolute paths.
    """
    try:
        with open(directory / f"{stem}-{digest}.d", encoding="utf-8", errors="replace") as dep_info:
            _, _, sources = dep_info.readline().partition(": ")
    except FileNotFoundError:
        return False
    crate_root = sources.split(" ", 1)[0].strip()
    return bool(crate_root) and (not crate_root.startswith("/") or crate_root.startswith(source_root))


def stale_artifacts(directory, names, keep, source_root):
    groups = {}
    for entry in directory.iterdir():
        match = ARTIFACT.match(entry.name)
        if match and match.group(1) in names:
            groups.setdefault(match.groups(), []).append(entry)
    newest_first = {}
    for (stem, digest), files in groups.items():
        if not built_from(directory, stem, digest, source_root):
            continue
        check_only = all(f.suffix in (".d", ".rmeta") for f in files)
        newest_first.setdefault((stem, check_only), []).append(files)
    stale = []
    for builds in newest_first.values():
        builds.sort(key=lambda files: max(f.lstat().st_mtime for f in files), reverse=True)
        for files in builds[keep:]:
            stale.extend(files)
    return stale


def repository_root(script_dir):
    common = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=script_dir, capture_output=True, text=True,
    )
    if common.returncode != 0:
        sys.exit(f"git rev-parse failed:\n{common.stderr}")
    return Path(common.stdout.strip()).parent


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("target_dir", type=Path)
    parser.add_argument("--profile", default="debug")
    parser.add_argument("--keep", type=int, default=3,
                        help="newest builds kept per workspace target, counting check-only builds separately (default 3)")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.keep < 1:
        parser.error("--keep must be at least 1")

    profile_dir = args.target_dir.resolve() / args.profile
    deps = profile_dir / "deps"
    if not deps.is_dir():
        sys.exit(f"{deps} does not exist")
    checkout = Path(__file__).resolve().parents[1]
    names = workspace_target_names(checkout)
    source_root = f"{repository_root(checkout)}/"
    locks = hold_locks(profile_dir)
    try:
        doomed = []
        for directory in (deps, profile_dir / "examples"):
            if directory.is_dir():
                doomed.extend(stale_artifacts(directory, names, args.keep, source_root))
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
