"""Run UI driver scenarios and record their light storyboards and provenance."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import socket
import subprocess
import sys
import tempfile

from ui_flow_review import write_review


ROOT = Path(__file__).resolve().parent.parent


def git_output(*args):
    return subprocess.run(["git", *args], cwd=ROOT, text=True, capture_output=True)


def checkout():
    stamp = subprocess.check_output(["sh", "scripts/build-revision.sh"], cwd=ROOT, text=True).strip()
    revision = stamp.removesuffix("-dirty")
    if not re.fullmatch("[0-9a-f]{40}", revision):
        raise ValueError("could not read the UI flow checkout SHA")
    branch = git_output("symbolic-ref", "--quiet", "--short", "HEAD").stdout.strip() or "detached"
    return revision, branch, stamp != revision


def provenance(argv):
    revision, branch, dirty = checkout()
    env = {}
    for key, value in sorted(os.environ.items()):
        if key.startswith("MAST_"):
            env[key] = value if key.startswith("MAST_PREVIEW_") else "[redacted]"
    env["MAST_SCENARIO_RENDER"] = "1"
    env["MAST_SCENARIO_FLOW"] = "1"
    return {
        "revision": revision,
        "branch": branch,
        "dirty": dirty,
        "host": socket.gethostname(),
        "command": shlex.join(["scripts/ui-flow.sh", *argv]),
        "env": env,
        "theme": "mast-light",
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }


def run_flow(scenario, output, stamp):
    source = scenario.resolve()
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    relpath = source.relative_to(ROOT) if source.is_relative_to(ROOT) else source.name
    tracked = source.is_relative_to(ROOT) and git_output(
        "ls-files", "--error-unmatch", "--", str(relpath),
    ).returncode == 0
    result = subprocess.run(
        [str(ROOT / "scripts/ui-drive"), "run", str(source), "--flow", "--out", str(output)],
        cwd=ROOT, env=dict(os.environ, MAST_SCENARIO_FLOW=stamp),
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    report_dir = output / source.stem
    report_path = report_dir / "report.json"
    if report_path.is_file():
        report = json.loads(report_path.read_text())
    else:
        report = {"name": source.stem, "steps": [], "notes": [result.stdout[-2500:]]}
    storyboard = report.get("storyboard")
    has_storyboard = bool(storyboard and (report_dir / storyboard).is_file())
    notes = report["notes"]
    source_unchanged = source.is_file() and hashlib.sha256(source.read_bytes()).hexdigest() == digest
    if not source_unchanged:
        notes.append("The scenario changed while it was running.")
    if not has_storyboard:
        notes.append("No storyboard was produced.")
    for step in report["steps"]:
        if step["frame"]:
            step["frame"] = str(Path(output.name, source.stem, step["frame"]))
    return {
        "name": report["name"],
        "scenario": str(relpath),
        "scenarioSha256": digest,
        "tracked": tracked,
        "ok": result.returncode == 0 and bool(report.get("ok")) and has_storyboard and source_unchanged,
        "steps": report["steps"],
        "notes": notes,
        "storyboard": str(Path(output.name, source.stem, storyboard)) if has_storyboard else None,
    }


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--publish", action="store_true", help="prepare a Mast flow review artifact")
    parser.add_argument("--feature", help="name the feature in the review artifact")
    parser.add_argument("scenarios", nargs="+", type=Path)
    args = parser.parse_args(argv)
    for path in args.scenarios:
        if not path.is_file() or path.suffix not in {".yaml", ".yml"}:
            parser.error(f"not a scenario YAML file: {path}")
    manifest = provenance(argv)
    sha = manifest["revision"][:7]
    branch_slug = re.sub("[^A-Za-z0-9_-]", "-", manifest["branch"])[:80]
    target = Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target")
    target = (ROOT / target).resolve()
    parent = target / "ui-flow"
    parent.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    directory = Path(tempfile.mkdtemp(prefix=f"{branch_slug}-{sha}-{timestamp}-", dir=parent))
    stamp = f'{manifest["branch"]} @ {sha}{"-dirty" if manifest["dirty"] else ""} · {manifest["host"]}'
    manifest["flows"] = [
        run_flow(scenario, directory / f"flow-{index:02}", stamp)
        for index, scenario in enumerate(args.scenarios, start=1)
    ]
    stable = checkout() == (manifest["revision"], manifest["branch"], manifest["dirty"])
    manifest["checkoutUnchanged"] = stable
    manifest["ok"] = stable and all(flow["ok"] for flow in manifest["flows"])
    path = directory / "flow-manifest.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n")
    steps = [step for flow in manifest["flows"] for step in flow["steps"]]
    count = sum(step["ok"] for step in steps)
    verdict = "PASS" if manifest["ok"] else "FAIL"
    print(f"{verdict} {count}/{len(steps)} steps @ {sha} {path}")
    if not stable:
        print("Checkout changed during the run.", file=sys.stderr)
    for flow in manifest["flows"]:
        if not flow["ok"]:
            print(f'{flow["name"]}:', file=sys.stderr)
            for step in flow["steps"]:
                if not step["ok"]:
                    ident = step["criterion"] or f'step {step["index"]}'
                    print(f'  {ident}: {step["error"]}', file=sys.stderr)
            for note in flow["notes"]:
                print(f"  {note}", file=sys.stderr)
    if args.publish:
        feature = args.feature or (
            manifest["flows"][0]["name"] if len(manifest["flows"]) == 1 else manifest["branch"]
        )
        try:
            write_review(manifest, directory, feature)
        except (ValueError, OSError) as error:
            print(f"Could not prepare flow review: {error}", file=sys.stderr)
            return 1
    return 0 if manifest["ok"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
