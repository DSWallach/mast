#!/usr/bin/env python3
"""Verify target pruning keeps recent workspace builds and refuses a busy target."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
script = root / "scripts/prune-cargo-target.py"
ours = "crates/engine/src/lib.rs"
ours_absolute = f"{root}/crates/engine/examples/opencode_probe.rs"
registry = "/cargo-home/registry/src/index/leak-0.1.2/src/lib.rs"


def touch(path, mtime, text="x" * 4096):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    os.utime(path, (mtime, mtime))


def build(directory, stem, digest, mtime, outputs, source):
    for name in outputs:
        touch(directory / name.format(stem=stem, digest=digest), mtime)
    touch(directory / f"{stem}-{digest}.d", mtime, f"{directory}/{stem}-{digest}: {source}\n")


with tempfile.TemporaryDirectory(prefix="mast-prune-test-") as directory:
    base = Path(directory)
    bin_dir = base / "bin"
    bin_dir.mkdir()
    fake_cargo = bin_dir / "cargo"
    fake_cargo.write_text('#!/bin/sh\nprintf \'%s\\n\' "$TEST_METADATA"\n')
    fake_cargo.chmod(0o755)
    names = ["mast-engine", "engine", "leak", "opencode_probe"]
    metadata = {"packages": [{"targets": [{"name": name} for name in names]}]}
    env = os.environ.copy()
    env.update(PATH=f"{bin_dir}:/usr/bin:/bin", TEST_METADATA=json.dumps(metadata))

    target = base / "target"
    debug = target / "debug"
    deps = debug / "deps"
    full = [f"{n:016x}" for n in range(1, 6)]
    checks = [f"{n:016x}" for n in range(10, 14)]
    leaks = [f"{n:016x}" for n in range(20, 24)]
    examples = [f"{n:016x}" for n in range(30, 34)]
    for age, digest in enumerate(full):
        build(deps, "mast_engine", digest, 1_000_000 - age * 100,
              ["lib{stem}-{digest}.rlib", "{stem}-{digest}.{stem}.abc-cgu.007.rcgu.o"], ours)
    for age, digest in enumerate(checks):
        build(deps, "mast_engine", digest, 2_000_000 - age * 100, ["lib{stem}-{digest}.rmeta"], ours)
    for digest in leaks:
        build(deps, "leak", digest, 1, ["lib{stem}-{digest}.rlib"], registry)
    build(deps, "leak", "00000000000000ff", 3_000_000, ["{stem}-{digest}"], ours)
    build(deps, "engine", "00000000000000ee", 3_000_000, ["{stem}-{digest}"], ours)
    for age, digest in enumerate(examples):
        build(debug / "examples", "opencode_probe", digest, 1_000_000 - age * 100, ["{stem}-{digest}"], ours_absolute)
    touch(debug / "incremental" / "mast_engine-1" / "s-1" / "query-cache.bin", 1)
    (debug / ".cargo-lock").touch()
    (debug / ".cargo-build-lock").touch()

    def run(*extra):
        return subprocess.run(["python3", "-I", str(script), *extra, str(target)], env=env, text=True, capture_output=True)

    def listing():
        return {path.name for path in deps.iterdir()} | {path.name for path in (debug / "examples").iterdir()}

    before = listing()
    result = run("--dry-run")
    assert result.returncode == 0, result.stderr
    assert "would remove 11 paths" in result.stdout, result.stdout
    assert listing() == before

    for lock_name, mode in ((".cargo-build-lock", fcntl.LOCK_EX), (".cargo-lock", fcntl.LOCK_SH)):
        lock = open(debug / lock_name, "rb")
        fcntl.flock(lock, mode)
        result = run()
        assert result.returncode != 0 and "is held" in result.stderr, (lock_name, result.stderr)
        assert listing() == before
        lock.close()

    result = run()
    assert result.returncode == 0, result.stderr
    remaining = listing()
    for digest in full[:3]:
        assert {f"libmast_engine-{digest}.rlib", f"mast_engine-{digest}.d"} <= remaining
        assert f"mast_engine-{digest}.mast_engine.abc-cgu.007.rcgu.o" in remaining
    for digest in full[3:]:
        assert not any(digest in name for name in remaining), digest
    assert all(f"libmast_engine-{digest}.rmeta" in remaining for digest in checks[:3])
    assert not any(checks[3] in name for name in remaining)
    assert all(f"libleak-{digest}.rlib" in remaining for digest in leaks)
    assert {"leak-00000000000000ff", "engine-00000000000000ee"} <= remaining
    assert all(f"opencode_probe-{digest}" in remaining for digest in examples[:3])
    assert f"opencode_probe-{examples[3]}" not in remaining
    assert not any((debug / "incremental").iterdir())
    assert "free KiB before" in result.stdout

    env["TEST_METADATA"] = "not json"
    fake_cargo.write_text("#!/bin/sh\necho 'lock file needs to be updated' >&2\nexit 101\n")
    result = run()
    assert result.returncode != 0 and "lock file needs to be updated" in result.stderr, result.stderr
print("prune target checks passed")
