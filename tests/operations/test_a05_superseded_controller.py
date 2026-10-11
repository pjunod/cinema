"""Source-only receipt for removal of the superseded A05 height controller.

This protects the narrowed PR from reintroducing its old duplicate adapter;
it is not physical acceptance or proof of PR669's newer controller behavior.
"""

from pathlib import Path
import unittest


PLAYER = Path(__file__).resolve().parents[2] / "clients/android/app/src/main/java/tv/plurx/app/player"


class SupersededA05ControllerTests(unittest.TestCase):
    def test_removed_height_controller_does_not_reappear(self):
        self.assertFalse((PLAYER / "AutoQualityAdapter.kt").exists())
        self.assertFalse((PLAYER / "AutoBandwidth.kt").exists())
        controller = (PLAYER / "Controller.kt").read_text()
        intent = (PLAYER / "PlaybackIntent.kt").read_text()
        self.assertIn("class Controller internal constructor(", controller)
        for removed_owner in ("nativeAutoCause", "AutoQualityAdapter", "automaticClaimInFlight"):
            self.assertNotIn(removed_owner, controller)
        self.assertNotIn("data class Auto(val height:", intent)
