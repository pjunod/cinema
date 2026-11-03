"""Reviewed short controls extend the experiment, never its acceptance verdict."""
import importlib.util
from importlib.machinery import SourceFileLoader
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "playback-startup-lab"
SPEC = importlib.util.spec_from_loader("startup_short_arms", SourceFileLoader("startup_short_arms", str(SCRIPT)))
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)


class ShortStartupArmTests(unittest.TestCase):
    def test_reviewed_short_arms_are_explicit_closed_gop_controls(self):
        for name, pair in LAB.SHORT_ARMS.items():
            self.assertEqual(LAB.resolve_arm(name), pair)
            self.assertEqual(LAB.validate_arm(*pair, 1.05)[:2], pair)
        self.assertEqual(LAB.DECISION_ARMS, ("a", "b"))

    def test_other_under_two_target_pairs_remain_rejected(self):
        for pair in [(4, 6), (8, 8), (8, 12), (16, 24)]:
            with self.assertRaises(LAB.ArmError):
                LAB.validate_arm(*pair, 1.0)
        self.assertEqual(LAB.validate_arm(8, 24, 1.0), (8, 24, 1.0))

    def test_short_controls_cannot_waive_segment_or_fixture_bounds(self):
        for target, bootstrap, segment in [(2, 2, 4), (4, 4, 4), (4, 4, 1)]:
            with self.assertRaises(LAB.ArmError):
                LAB.validate_arm(target, bootstrap, 1.0, segment=segment)
        with self.assertRaises(LAB.ArmError):
            LAB.validate_arm(4, 3, 1.0)
