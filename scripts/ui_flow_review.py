"""Write a self-contained Mast flow review and check its machine-readable evidence."""

import argparse
import base64
from collections import Counter
import hashlib
from html import escape
import json
import math
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
MAX_ARTIFACT_BYTES = 10 * 1024 * 1024
REVISION = re.compile("[0-9a-f]{40}")


def tracked_scenario(flow, revision):
    path = flow["scenario"]
    if not flow["tracked"] or not path.endswith((".yaml", ".yml")):
        return False
    result = subprocess.run(
        ["git", "show", f"{revision}:{path}"], cwd=ROOT, capture_output=True,
    )
    return result.returncode == 0 and (
        hashlib.sha256(result.stdout).hexdigest() == flow["scenarioSha256"]
    )


def clean_source(manifest, head):
    if not REVISION.fullmatch(head) or manifest["revision"] != head:
        raise ValueError("manifest SHA differs from the gated head")
    if manifest["dirty"] or not manifest["checkoutUnchanged"]:
        raise ValueError("flow evidence requires a clean, unchanged checkout")
    if manifest["theme"] != "mast-light" or not manifest["flows"]:
        raise ValueError("flow evidence requires at least one Mast light flow")
    for flow in manifest["flows"]:
        if not tracked_scenario(flow, head):
            raise ValueError(f'{flow["scenario"]} does not match the scenario at the gated head')


def verify(manifest, head, required):
    clean_source(manifest, head)
    if not manifest["ok"] or not all(flow["ok"] for flow in manifest["flows"]):
        raise ValueError("one or more flows failed")
    criteria = Counter()
    for flow in manifest["flows"]:
        for step in flow["steps"]:
            if criterion := step["criterion"]:
                if not step["ok"] or not step["expect"] or not step["frame"]:
                    raise ValueError(f"{criterion} needs a passing expect and a captured frame")
                criteria[criterion] += 1
    for criterion in required:
        if criteria[criterion] != 1:
            raise ValueError(f"{criterion} must map to exactly one passing expect")


def storyboard(flow, directory):
    if not flow["storyboard"]:
        return None
    path = (directory / flow["storyboard"]).resolve()
    if not path.is_relative_to(directory) or path.suffix != ".png":
        raise ValueError("storyboard must be a PNG in the flow run directory")
    if path.stat().st_size > MAX_ARTIFACT_BYTES:
        raise ValueError("storyboard exceeds Mast's artifact size limit")
    return base64.b64encode(path.read_bytes()).decode("ascii")


def frame_style(step):
    if not (region := step.get("storyboard_region")):
        return None
    if len(region) != 4 or any(not math.isfinite(n) or not 0 <= n <= 1 for n in region):
        raise ValueError("invalid storyboard frame region")
    return ";".join(
        f"{name}:{100 * value:.4f}%"
        for name, value in zip(("left", "top", "width", "height"), region)
    )


def flow_section(flow, index, directory):
    name = escape(flow["name"])
    board = storyboard(flow, directory)
    checks = []
    targets = []
    for step in flow["steps"]:
        if not (criterion := step["criterion"]):
            continue
        step_id = f'flow-{index}-step-{step["index"]}'
        label = escape(step["caption"] or criterion)
        proven = step["ok"] and step["expect"] and step["frame"]
        status = "✓ Pass" if proven else "✗ Failed" if step["frame"] else "✗ Unverified"
        if board and (style := frame_style(step)):
            targets.append(f'<span id="{step_id}" class="flow-frame" style="{style}"></span>')
            label = f'<a href="#{step_id}">{label}</a>'
        checks.append(f"<li><strong>{status}</strong> <code>{escape(criterion)}</code> {label}</li>")
    checklist = "<ul>" + "".join(checks) + "</ul>" if checks else "<p>No acceptance checks reported.</p>"
    sheet = (
        '<figure class="flow-board">'
        f'<img alt="{name} storyboard" src="data:image/png;base64,{board}">'
        + "".join(targets) + "</figure>"
        if board else "<p>No storyboard captured.</p>"
    )
    return (
        f'<section class="mk-card"><h2>{name}</h2>'
        f'<p>{"Passed" if flow["ok"] else "Failed"} · {escape(flow["scenario"])}</p>'
        f'{checklist}{sheet}</section>'
    )


def write_review(manifest, directory, feature):
    clean_source(manifest, manifest["revision"])
    slug = re.sub(r"[^a-zA-Z0-9_-]+", "-", feature).strip("-")[:80] or "feature"
    title = f"{feature} flow review"
    provenance = " · ".join(
        escape(str(manifest[key])) for key in ("branch", "revision", "host", "timestamp")
    )
    verdict = "Pass" if manifest["ok"] else "Fail"
    intro = f"<h1>{escape(title)}</h1><p>{verdict} · Mast light</p><p>{provenance}</p>"
    sections = [
        flow_section(flow, i, directory) for i, flow in enumerate(manifest["flows"], start=1)
    ]
    content = """<style>
.flow-board { position: relative; margin: 1rem 0 0; }
.flow-board img { display: block; width: 100%; height: auto; }
.flow-frame { position: absolute; pointer-events: none; }
.flow-frame:target { outline: 3px solid var(--accent, #6655b8); outline-offset: -3px; }
.mk-sheet { overflow-wrap: anywhere; }
</style>
""" + '<main class="mk-sheet">' + intro + "\n".join(sections) + "</main>\n"
    encoded = content.encode("utf-8")
    if len(encoded) > MAX_ARTIFACT_BYTES:
        raise ValueError("review fragment exceeds Mast's 10 MiB artifact size limit")
    path = directory / f"{slug}.flow-review.fragment.html"
    path.write_bytes(encoded)
    print("artifact_publish " + json.dumps({
        "path": str(path), "title": title, "kind": "html-fragment", "role": "review-evidence",
        "idempotencyKey": "flow-review-" + hashlib.sha256(encoded).hexdigest(),
    }))
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--head", required=True, help="full SHA being reviewed")
    parser.add_argument("--criterion", action="append", required=True, help="confirmed criterion ID")
    args = parser.parse_args()
    try:
        manifest = json.loads(args.manifest.read_text())
        verify(manifest, args.head, args.criterion)
    except (KeyError, TypeError, ValueError, OSError) as error:
        sys.exit(f"Testing: {error}")
    print(f"PASS {len(args.criterion)} confirmed criteria @ {args.head[:7]}")


if __name__ == "__main__":
    main()
