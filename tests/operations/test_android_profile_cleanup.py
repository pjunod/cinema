"""Journey source wiring, not a claim of physical profile capture or server Stop."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "clients/android/baselineprofile/src/main/java/tv/plurx/profile/ReleaseJourney.kt"


class AndroidReleaseProfileCleanupCase(unittest.TestCase):
    def test_cleanup_confirms_owned_player_exit_and_exact_detail_before_home(self):
        source = SOURCE.read_text()
        cleanup = source.split("fun closeOwnedPlayback() {", 1)[1].split("private fun isOwnedDetail", 1)[0]
        self.assertIn("if (!ownsPlayback) return", cleanup)
        self.assertIn("repeat(4)", cleanup)
        self.assertIn("check(device.currentPackageName == PACKAGE)", cleanup)
        self.assertIn("if (device.hasObject(playerSelector)) {", cleanup)
        self.assertIn("check(device.pressBack())", cleanup)
        exit_guard = cleanup.index("check(device.wait(Until.gone(playerSelector), 5_000L))")
        detail_guard = cleanup.index("check(device.wait(Until.hasObject(titleSelector), 5_000L) && isOwnedDetail())")
        home = cleanup.index("check(device.pressHome())")
        self.assertLess(exit_guard, detail_guard)
        self.assertLess(detail_guard, home)
        self.assertGreater(cleanup.index("ownsPlayback = false"), home)
        self.assertNotIn("forceStop", cleanup)
        self.assertNotIn("killProcess", cleanup)
        self.assertNotIn("executeShellCommand", cleanup)

    def test_owned_fixture_and_pending_player_markers_fence_navigation(self):
        source = SOURCE.read_text()
        self.assertIn('By.pkg(PACKAGE).res(java.util.regex.Pattern.compile("plurx-first-frame-(pending|[0-9]+)"))', source)
        self.assertIn("private val titleSelector = By.pkg(PACKAGE).text(title)", source)
        self.assertIn("private val playSelector = By.pkg(PACKAGE).text", source)
        start = source.split("fun homeDetailPlayFirstFrame(): Long {", 1)[1].split("private fun clickAncestor", 1)[0]
        self.assertIn("check(!ownsPlayback && !device.hasObject(playerSelector))", start)
        self.assertLess(start.index("check(isOwnedDetail())"), start.index("clickAncestor(startOver ?: play)"))
        self.assertLess(start.index("clickAncestor(startOver ?: play)"), start.index("ownsPlayback = true"))
        detail = source.split("private fun isOwnedDetail(): Boolean =", 1)[1]
        for guard in ("device.currentPackageName == PACKAGE", "!device.hasObject(playerSelector)",
                      '!device.hasObject(By.res("plurx-home-ready"))',
                      '!device.hasObject(By.res("plurx-home-loading"))',
                      "device.findObjects(titleSelector).size == 1", "device.hasObject(playSelector)"):
            self.assertIn(guard, detail)


if __name__ == "__main__":
    unittest.main()
