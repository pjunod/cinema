"""Keep every Android player on the role-aware construction path."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
JAVA = ROOT / "clients/android/app/src/main/java/tv/plurx/app"


class AndroidPlayerBuilderContract(unittest.TestCase):
    def test_all_player_construction_uses_the_shared_builder(self):
        builders = [
            path.relative_to(JAVA).as_posix()
            for path in JAVA.rglob("*.kt")
            if "ExoPlayer.Builder(" in path.read_text(encoding="utf-8")
        ]
        self.assertEqual(builders, ["player/PlurxPlayerBuilder.kt"])

    def test_each_transport_keeps_its_role_and_source(self):
        expected = {
            "player/Controller.kt": (
                "PlayerRole.Finite",
                "PlayerRole.Successor",
                "Net.dataSourceFactory()",
                "transferListener = progressiveMediaOrigin",
            ),
            "livetv/LiveTvPlayer.kt": (
                "PlayerRole.LiveTv",
                "OkHttpDataSource.Factory(api.mediaClient)",
            ),
            "librarychannels/LibraryChannelPlayer.kt": (
                "PlayerRole.LibraryChannel",
                "OkHttpDataSource.Factory(Net.capabilityClient)",
            ),
            "data/offline/OfflineDownloads.kt": (
                "PlayerRole.Offline",
                "setUpstreamDataSourceFactory(PlaceholderDataSource.FACTORY)",
            ),
        }
        for filename, contract in expected.items():
            with self.subTest(filename=filename):
                source = (JAVA / filename).read_text(encoding="utf-8")
                for required in contract:
                    self.assertIn(required, source)


if __name__ == "__main__":
    unittest.main()
