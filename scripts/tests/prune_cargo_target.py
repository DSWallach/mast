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


def touch(path, mtime):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x" * 4096)
    os.utime(path, (mtime, mtime))


with tempfile.TemporaryDirectory(prefix="mast-prune-test-") as directory:
    base = Path(directory)
    bin_dir = base / "bin"
    bin_dir.mkdir()
    fake_cargo = bin_dir / "cargo"
    fake_cargo.write_text('#!/bin/sh\nprintf \'%s\\n\' "$TEST_METADATA"\n')
    fake_cargo.chmod(0o755)
    metadata = {"packages": [{"targets": [{"name": "mast-engine"}, {"name": "engine"}]}]}
    env = os.environ.copy()
    env.update(PATH=f"{bin_dir}:/usr/bin:/bin", TEST_METADATA=json.dumps(metadata))

    target = base / "target"
    deps = target / "debug" / "deps"
    hashes = [f"{n:016x}" for n in range(1, 6)]
    for age, digest in enumerate(hashes):
        mtime = 1_000_000 - age * 100
        touch(deps / f"libmast_engine-{digest}.rlib", mtime)
        touch(deps / f"mast_engine-{digest}.mast_engine.abc-cgu.007.rcgu.o", mtime)
    touch(deps / f"engine-{hashes[4]}", 2_000_000)
    for digest in hashes:
        touch(deps / f"libserde-{digest}.rlib", 1)
    touch(target / "debug" / "incremental" / "mast_engine-1" / "s-1" / "query-cache.bin", 1)
    (target / "debug" / ".cargo-lock").touch()

    def run(*extra):
        return subprocess.run(["python3", "-I", str(script), *extra, str(target)], env=env, text=True, capture_output=True)

    result = run("--dry-run")
    assert result.returncode == 0, result.stderr
    assert "would remove 5 paths" in result.stdout, result.stdout
    assert len(list(deps.iterdir())) == 16

    lock = open(target / "debug" / ".cargo-lock", "rb")
    fcntl.flock(lock, fcntl.LOCK_EX)
    result = run()
    assert result.returncode != 0 and "is held" in result.stderr, result.stderr
    assert len(list(deps.iterdir())) == 16
    lock.close()

    result = run()
    assert result.returncode == 0, result.stderr
    remaining = {path.name for path in deps.iterdir()}
    for digest in hashes[:3]:
        assert f"libmast_engine-{digest}.rlib" in remaining
        assert f"mast_engine-{digest}.mast_engine.abc-cgu.007.rcgu.o" in remaining
    for digest in hashes[3:]:
        assert f"libmast_engine-{digest}.rlib" not in remaining
        assert f"mast_engine-{digest}.mast_engine.abc-cgu.007.rcgu.o" not in remaining
    assert f"engine-{hashes[4]}" in remaining
    assert all(f"libserde-{digest}.rlib" in remaining for digest in hashes)
    assert not any((target / "debug" / "incremental").iterdir())
    assert "free KiB before" in result.stdout
print("prune target checks passed")
