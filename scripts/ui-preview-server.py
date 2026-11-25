#!/usr/bin/env python3
"""Serve offscreen preview requests from a directory."""
import json
import fcntl
import os
import shlex
from pathlib import Path
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent


def build_preview():
    result = subprocess.run(
        ["cargo", "test", "-p", "mast-ui", "--features", "snapshot", "--test", "ui_preview", "--no-run", "--message-format=json"],
        stdout=subprocess.PIPE, text=True, check=True, cwd=ROOT,
    )
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == "ui_preview" and message.get("executable"):
            return Path(message["executable"])
    raise RuntimeError("Cargo did not report the preview executable")


def main():
    directory = Path(sys.argv[1]).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    lock = (directory / "server.lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit(f"A preview server already owns {directory}")
    request_path = directory / "request.json"
    inflight_path = directory / "request.inflight.json"
    response_path = directory / "response.json"
    process = None
    identity = None
    print(f"Waiting for {request_path}", file=sys.stderr, flush=True)
    try:
        while True:
            if (directory / "shutdown").exists():
                (directory / "shutdown").unlink()
                break
            if not request_path.exists():
                time.sleep(0.2)
                continue
            request_path.replace(inflight_path)
            try:
                request = json.loads(inflight_path.read_text())
                revision = subprocess.check_output(["sh", "scripts/build-revision.sh"], cwd=ROOT, text=True).strip()
                branch = subprocess.run(
                    ["git", "symbolic-ref", "--quiet", "--short", "HEAD"],
                    cwd=ROOT, capture_output=True, text=True,
                ).stdout.strip() or "detached"
                executable = build_preview()
                stat = executable.stat()
                current = (str(executable), stat.st_mtime_ns, stat.st_size, revision, branch)
                if process is None or process.poll() is not None or current != identity:
                    if process is not None:
                        process.stdin.close()
                        process.wait(timeout=30)
                    environment = os.environ.copy()
                    environment.update(
                        MAST_PREVIEW_SERVE="1", MAST_PREVIEW_WARM="1",
                        MAST_UI_COMMAND="scripts/ui-preview.sh --serve " + shlex.quote(sys.argv[1]),
                    )
                    process = subprocess.Popen([str(executable), "render_ui_preview", "--ignored", "--nocapture", "--test-threads=1"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1, env=environment, cwd=ROOT)
                    identity = current
                process.stdin.write(json.dumps(request) + "\n")
                process.stdin.flush()
                while True:
                    line = process.stdout.readline()
                    if not line:
                        raise RuntimeError("preview renderer exited before responding")
                    marker = "MAST_PREVIEW_RESPONSE="
                    if marker in line:
                        response = json.loads(line.split(marker, 1)[1])
                        break
                response["rendererPid"] = process.pid
            except Exception as error:
                response = {"error": str(error)}
            finally:
                inflight_path.unlink(missing_ok=True)
            temporary = response_path.with_suffix(".json.tmp")
            temporary.write_text(json.dumps(response, indent=2) + "\n")
            temporary.replace(response_path)
    finally:
        if process is not None and process.poll() is None:
            process.stdin.close()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
