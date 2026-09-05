from __future__ import annotations

from pathlib import Path
import runpy
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/decoder-diagnostic-qualification"
FIXTURES = ROOT / "tests/playback/decoder-health"
CHECKER = runpy.run_path(str(SCRIPT))


class DecoderDiagnosticQualificationTests(unittest.TestCase):
    def test_qualified_issue_shape_latches_the_fifth_record_in_361_ms(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "qualified-primary-window.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.subordinate_records, 1)
        self.assertEqual(result.fault_at_ms, 361)
        self.assertEqual(result.detection_latency_ms, 361)
        self.assertTrue(result.automatic_action_qualified)

    def test_legacy_issue_capture_is_diagnostic_but_not_action_qualified(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "issue-913-legacy.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.legacy_repeat_summaries, 2)
        self.assertEqual(result.legacy_repeated_messages, 49)
        self.assertEqual(result.detection_latency_ms, 361)
        self.assertFalse(result.automatic_action_qualified)

    def test_tolerant_controls_do_not_latch_or_count_other_stages(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "tolerant-controls.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.subordinate_records, 1)
        self.assertIsNone(result.fault_at_ms)
        self.assertFalse(result.automatic_action_qualified)

    def test_selected_stream_attribution_is_exact(self) -> None:
        result = CHECKER["qualify"](
            FIXTURES / "tolerant-controls.stderr",
            (0, 2),
        )

        self.assertEqual(result.primary_video_error_records, 1)
        self.assertIsNone(result.fault_at_ms)

    def test_retained_fixtures_contain_no_incident_identity(self) -> None:
        for fixture in FIXTURES.glob("*.stderr"):
            text = fixture.read_text(encoding="utf-8")
            with self.subTest(fixture=fixture.name):
                self.assertNotIn("session=", text)
                self.assertNotIn("@ 0x", text)
                self.assertNotIn("/media/", text)


class DecoderSelectionInventoryTests(unittest.TestCase):
    def test_every_shipping_hls_builder_is_in_the_m0_inventory(self) -> None:
        source = (ROOT / "crates/plurxd/src/transcode.rs").read_text(
            encoding="utf-8"
        )
        self.assertEqual(
            source.count("transcode::hls_args("),
            3,
            "update the M0 inventory when a shipping HLS builder is added or migrated",
        )
        for owner in (
            "impl PrepublicationTranscodeRetry",
            "async fn produce_into(",
            "async fn start_with_audio_offset(",
        ):
            self.assertIn(owner, source)

        status = (
            ROOT / "docs/DECODER_SELECTION_RECOVERY_STATUS.md"
        ).read_text(encoding="utf-8")
        for owner in (
            "PrepublicationTranscodeRetry::build",
            "ProducerRunner::produce_into",
            "Manager::start_with_audio_offset",
        ):
            self.assertIn(owner, status)


if __name__ == "__main__":
    unittest.main()
