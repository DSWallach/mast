"""The reviewer must reject missing, failed, stale and ambiguous criteria."""

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "ui_flow_review.py"
SPEC = importlib.util.spec_from_file_location("ui_flow_review", SCRIPT)
REVIEW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REVIEW)


class FlowReviewTests(unittest.TestCase):
    def setUp(self):
        self.head = "a" * 40
        self.step = {
            "criterion": "pins.1",
            "ok": True,
            "expect": {"visible": "#weekly"},
            "frame": "pins-01.png",
        }
        self.flow = {"ok": True, "scenario": "pins.yaml", "steps": [self.step]}
        self.manifest = {
            "revision": self.head,
            "dirty": False,
            "checkoutUnchanged": True,
            "theme": "mast-light",
            "ok": True,
            "flows": [self.flow],
        }
        stub = patch.object(REVIEW, "tracked_scenario", return_value=True)
        self.tracked = stub.start()
        self.addCleanup(stub.stop)

    def check(self, head=None, criterion="pins.1"):
        REVIEW.verify(self.manifest, head or self.head, [criterion])

    def test_requires_exact_clean_source(self):
        self.check()
        with self.assertRaisesRegex(ValueError, "gated head"):
            self.check(head="b" * 40)
        self.tracked.return_value = False
        with self.assertRaisesRegex(ValueError, "does not match"):
            self.check()
        self.tracked.return_value = True
        self.manifest["dirty"] = True
        with self.assertRaisesRegex(ValueError, "clean, unchanged"):
            self.check()

    def test_criterion_needs_one_passing_expect_and_frame(self):
        with self.assertRaisesRegex(ValueError, "pins.2"):
            self.check(criterion="pins.2")
        self.flow["steps"].append(self.step)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.check()
        self.flow["steps"].pop()
        self.step["expect"] = None
        with self.assertRaisesRegex(ValueError, "passing expect"):
            self.check()


if __name__ == "__main__":
    unittest.main()
