"""Check the flow wrapper's manifest paths and status without running Cargo."""

import importlib.util
from pathlib import Path
import json
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("ui_flow", SCRIPTS / "ui-flow.py")
FLOW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FLOW)


class FlowWrapperTest(unittest.TestCase):
    def test_rebases_frame_paths_and_rejects_a_missing_storyboard(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            scenario = root / "pins.yaml"
            scenario.write_text("name: pins\n")
            output = root / "run" / "flow-01"
            report_dir = output / scenario.stem
            report_dir.mkdir(parents=True)
            storyboard = report_dir / "storyboard.png"
            storyboard.touch()
            (report_dir / "report.json").write_text(json.dumps({
                "name": "pins",
                "ok": True,
                "steps": [{"ok": True, "frame": "pins-01.png"}],
                "notes": [],
                "storyboard": storyboard.name,
            }))
            done = subprocess.CompletedProcess([], 0, stdout="")
            with (
                patch.object(FLOW, "ROOT", root),
                patch.object(FLOW, "git_output", return_value=done),
                patch.object(FLOW.subprocess, "run", return_value=done),
            ):
                flow = FLOW.run_flow(scenario, output, "branch @ abcdef0")
                self.assertTrue(flow["ok"])
                self.assertTrue(flow["tracked"])
                self.assertEqual(flow["steps"][0]["frame"], "flow-01/pins/pins-01.png")
                self.assertEqual(flow["storyboard"], "flow-01/pins/storyboard.png")
                storyboard.unlink()
                missing = FLOW.run_flow(scenario, output, "branch @ abcdef0")
                self.assertFalse(missing["ok"])
                self.assertIsNone(missing["storyboard"])
                self.assertIn("No storyboard was produced.", missing["notes"])


if __name__ == "__main__":
    unittest.main()
