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
                "val autoTransfers = AutoTransferEvidence(progressiveMediaOrigin)",
                "transferListener = autoTransfers",
                # The continuous-quality registry and output evidence are built
                # once per pipeline, handed to the builder, and returned with
                # the player, so the owner reads the evidence its own pipeline
                # produced rather than a defaulted, unwired instance.
                "val continuousSources = ContinuousSourceRegistry()",
                "val continuousOutput = ContinuousOutputEvidence()",
                "continuousSources = continuousSources",
                "continuousOutput = continuousOutput",
                "return BuiltPlayer(player, progressiveMediaOrigin, autoTransfers, "
                "continuousSources, continuousOutput)",
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

    def test_screen_on_binder_follows_the_owned_player_and_view_lifecycle(self):
        source = (JAVA / "player/PlayerScreen.kt").read_text()
        self.assertIn("PlayerScreenOn(view = { playerView }, player = { controller.player }, isVideo = !plan.isAudioOnly)", source)
        self.assertEqual(source.count("controller.addPlayerListener(screenOn)"), 1)
        self.assertEqual(source.count("controller.removePlayerListener(screenOn)"), 1)
        self.assertIn("screenOn.sync(this)", source)
        self.assertIn("screenOn.sync(view)", source)
        self.assertNotIn("keepScreenOn =", source)
        binder = (JAVA / "player/PlayerScreenOn.kt").read_text()
        for callback in ("onIsPlayingChanged", "onPlaybackStateChanged", "onPlayWhenReadyChanged"):
            self.assertIn("override fun " + callback, binder)
        self.assertIn("current.isPlaying || (current.playWhenReady && current.playbackState == Player.STATE_BUFFERING)", binder)


if __name__ == "__main__":
    unittest.main()
