#!/bin/sh
# Rebuild and restart the gallery when its source changes.
set -eu
cd "$(dirname "$0")/.."
exec python3 - "$@" <<'PY'
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

root = Path.cwd()
watched = [root / "crates/ui/src", root / "apps/mast-gallery"]
target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
if not target.is_absolute():
    target = root / target
binary = target / "debug" / ("mast-gallery.exe" if os.name == "nt" else "mast-gallery")


def snapshot():
    result = {}
    for directory in watched:
        for path in directory.rglob("*"):
            try:
                if path.is_file():
                    stat = path.stat()
                    result[str(path)] = (stat.st_mtime_ns, stat.st_size)
            except FileNotFoundError:
                pass
    return result


with tempfile.TemporaryDirectory(prefix="mast-gallery-") as state_directory:
    environment = os.environ.copy()
    environment["MAST_GALLERY_STATE"] = str(Path(state_directory) / "state")
    process = None
    previous = None
    try:
        while True:
            current = snapshot()
            if current != previous:
                previous = current
                print("Building mast-gallery...", flush=True)
                result = subprocess.run(["cargo", "build", "-p", "mast-gallery"], env=environment)
                if result.returncode == 0:
                    if process is not None and process.poll() is None:
                        process.terminate()
                        process.wait()
                    print("Starting mast-gallery", flush=True)
                    process = subprocess.Popen([str(binary)], env=environment)
                else:
                    print("Build failed; waiting for another change.", file=sys.stderr, flush=True)
            time.sleep(1)
    except KeyboardInterrupt:
        pass
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait()
PY
