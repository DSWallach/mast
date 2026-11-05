#!/usr/bin/env python3
"""Verify packaging refuses a missing Cargo artifact before modifying outputs."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix="mast-package-test-") as directory:
    base = Path(directory)
    bin_dir = base / "bin"
    bin_dir.mkdir()
    target = base / "custom-target"
    reported = target / "release" / "zeron"
    fake_cargo = bin_dir / "cargo"
    fake_cargo.write_text('''#!/bin/sh
if [ "$1" = metadata ]; then
  printf '%s\\n' "$TEST_METADATA"
else
  printf '%s\\n' "$TEST_ARTIFACT"
fi
''')
    fake_cargo.chmod(0o755)
    env = os.environ.copy()
    env.update(PATH=f"{bin_dir}:/usr/bin:/bin:/usr/sbin:/sbin", CARGO_TARGET_DIR=str(target),
               TEST_METADATA=json.dumps({"target_directory": str(target)}),
               TEST_ARTIFACT=json.dumps({"reason": "compiler-artifact", "target": {"name": "zeron"}, "executable": str(reported)}))
    result = subprocess.run([str(root / "scripts/package-macos.sh")], env=env, text=True, capture_output=True)
    assert result.returncode != 0, result.stdout
    assert f"missing zeron executable: {reported}" in result.stderr, result.stderr
    assert not (target / "package").exists()
    env["TEST_ARTIFACT"] = json.dumps({"reason": "build-finished", "success": True})
    result = subprocess.run([str(root / "scripts/package-macos.sh")], env=env, text=True, capture_output=True)
    assert result.returncode != 0
    assert "Cargo did not report a built zeron executable" in result.stderr
print("package target checks passed")
