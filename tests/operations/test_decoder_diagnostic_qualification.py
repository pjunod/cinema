from __future__ import annotations

from pathlib import Path
import runpy
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/decoder-diagnostic-qualification"
FIXTURES = ROOT / "tests/playback/decoder-health"
CHECKER = runpy.run_path(str(SCRIPT))


class DecoderDiagnosticQualificationTests(unittest.TestCase):
    def test_qualified_ffmpeg_8_shape_latches_fifth_record_in_361_ms(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "qualified-ffmpeg-8.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.severity_qualified_primary_records, 5)
        self.assertEqual(result.fault_at_ms, 361)
        self.assertEqual(result.detection_latency_ms, 361)
        self.assertTrue(result.observation_complete)
        self.assertTrue(result.automatic_action_qualified)

    def test_legacy_issue_capture_is_diagnostic_but_not_action_qualified(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "issue-913-legacy.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.severity_qualified_primary_records, 0)
        self.assertEqual(result.selected_primary_repeat_summaries, 2)
        self.assertEqual(result.selected_primary_repeated_messages, 49)
        self.assertEqual(result.detection_latency_ms, 361)
        self.assertFalse(result.automatic_action_qualified)

    def test_deployed_ffmpeg_5_shape_remains_unqualified(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "unqualified-ffmpeg-5.stderr")

        self.assertEqual(result.primary_video_error_records, 0)
        self.assertIsNone(result.fault_at_ms)
        self.assertTrue(result.observation_complete)
        self.assertFalse(result.automatic_action_qualified)

    def test_tolerant_controls_do_not_latch_or_count_other_stages(self) -> None:
        result = CHECKER["qualify"](FIXTURES / "tolerant-controls.stderr")

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.severity_qualified_primary_records, 5)
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

    def test_repeat_summaries_retain_only_unambiguous_provenance(self) -> None:
        result = CHECKER["qualify"](
            FIXTURES / "repeat-attribution-controls.stderr"
        )

        self.assertEqual(result.selected_primary_repeat_summaries, 1)
        self.assertEqual(result.selected_primary_repeated_messages, 6)
        self.assertEqual(result.unrelated_repeat_summaries, 2)
        self.assertEqual(result.ambiguous_repeat_summaries, 2)

    def test_latency_starts_with_triggering_window_not_stale_error(self) -> None:
        primary = (
            "[vist#0:0/h264 @ <address>] [dec:h264 @ <address>] [error] "
            "Error submitting packet to decoder: corrupt input packet"
        )
        lines = [f"0\t{primary}"] + [
            f"{observed}\t{primary}"
            for observed in (10_000, 10_010, 10_020, 10_030, 10_040)
        ]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "stale.stderr"
            fixture.write_text("\n".join(lines) + "\n", encoding="utf-8")
            result = CHECKER["qualify"](fixture)

        self.assertEqual(result.first_primary_at_ms, 0)
        self.assertEqual(result.triggering_window_started_at_ms, 10_000)
        self.assertEqual(result.fault_at_ms, 10_040)
        self.assertEqual(result.detection_latency_ms, 40)

    def test_coverage_loss_is_bounded_drained_and_never_action_qualified(self) -> None:
        primary = (
            b"[vist#0:0/h264 @ <address>] [dec:h264 @ <address>] [error] "
            b"Error submitting packet to decoder: corrupt input packet"
        )
        records = b"".join(
            str(observed).encode("ascii") + b"\t" + primary + b"\n"
            for observed in range(5)
        )
        cases = {
            "oversized": (
                b"0\t"
                + b"x" * (CHECKER["MAX_DIAGNOSTIC_LINE_BYTES"] + 1)
                + b"\n"
            ),
            "invalid-utf8": b"0\tbad-utf8-\xff\n",
            "giant-comment": (
                b"#" + b"x" * CHECKER["MAX_FIXTURE_RECORD_BYTES"] + b"\n"
            ),
        }
        results = {}
        with tempfile.TemporaryDirectory() as directory:
            for name, prefix in cases.items():
                fixture = Path(directory) / f"{name}.stderr"
                fixture.write_bytes(prefix + records)
                results[name] = CHECKER["qualify"](fixture)

        for name, result in results.items():
            with self.subTest(case=name):
                self.assertEqual(result.primary_video_error_records, 5)
                self.assertEqual(result.fault_at_ms, 4)
                self.assertFalse(result.observation_complete)
                self.assertFalse(result.automatic_action_qualified)
        self.assertEqual(results["oversized"].oversized_lines, 1)
        self.assertEqual(results["invalid-utf8"].invalid_utf8_lines, 1)
        self.assertEqual(results["giant-comment"].oversized_lines, 1)

    def test_many_lines_do_not_require_retaining_the_fixture(self) -> None:
        audio = (
            b"[aist#0:1/aac @ <address>] [dec:aac @ <address>] [error] "
            b"Error submitting packet to decoder: invalid data"
        )
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "many.stderr"
            with fixture.open("wb") as output:
                for observed in range(20_000):
                    output.write(
                        str(observed).encode("ascii") + b"\t" + audio + b"\n"
                    )
            result = CHECKER["qualify"](fixture)

        self.assertEqual(result.primary_video_error_records, 0)
        self.assertTrue(result.observation_complete)

    def test_exact_line_limit_is_accepted_and_repeat_count_is_bounded(self) -> None:
        prefix = b"0\t"
        exact = prefix + b"x" * CHECKER["MAX_DIAGNOSTIC_LINE_BYTES"] + b"\n"
        huge_repeat = (
            b"1\tLast message repeated "
            + b"9" * 21
            + b" times\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "bounds.stderr"
            fixture.write_bytes(exact + huge_repeat)
            result = CHECKER["qualify"](fixture)

        self.assertEqual(result.oversized_lines, 0)
        self.assertEqual(result.malformed_lines, 1)
        self.assertFalse(result.observation_complete)

    def test_retained_fixtures_contain_no_incident_identity(self) -> None:
        for fixture in FIXTURES.glob("*.stderr"):
            text = fixture.read_text(encoding="utf-8")
            with self.subTest(fixture=fixture.name):
                self.assertNotIn("session=", text)
                self.assertNotRegex(text, r"@ 0x[0-9a-fA-F]+")
                self.assertNotIn("/media/", text)


class DecoderSelectionInventoryTests(unittest.TestCase):
    def inventory(self) -> list[dict[str, str]]:
        path = ROOT / "tests/playback/decoder-selection-m0-inventory.toml"
        with path.open("rb") as document:
            inventory = tomllib.load(document)
        self.assertEqual(inventory["version"], 1)
        return inventory["surfaces"]

    def test_every_inventory_anchor_exists_and_identifiers_are_unique(self) -> None:
        surfaces = self.inventory()
        identifiers = [surface["id"] for surface in surfaces]
        self.assertEqual(len(identifiers), len(set(identifiers)))
        for surface in surfaces:
            with self.subTest(surface=surface["id"]):
                source = ROOT / surface["source"]
                self.assertTrue(source.is_file())
                self.assertIn(
                    surface["anchor"],
                    source.read_text(encoding="utf-8"),
                )
                self.assertTrue(surface["obligation"])

    def test_every_shipping_hls_builder_is_in_the_m0_inventory(self) -> None:
        source = (ROOT / "crates/plurxd/src/transcode.rs").read_text(
            encoding="utf-8"
        )
        self.assertEqual(
            source.count("transcode::hls_args("),
            3,
            "update the M0 inventory when a shipping HLS builder is added or migrated",
        )
        builders = {
            surface["id"]
            for surface in self.inventory()
            if surface["kind"] == "hls_builder"
        }
        self.assertEqual(
            builders,
            {
                "builder.prepublication_retry",
                "builder.resumable_part",
                "builder.live_hls",
            },
        )

        status = (
            ROOT / "docs/DECODER_SELECTION_RECOVERY_STATUS.md"
        ).read_text(encoding="utf-8")
        for owner in (
            "PrepublicationTranscodeRetry::build",
            "ProducerRunner::produce_into",
            "Manager::start_with_audio_offset",
        ):
            self.assertIn(owner, status)

    def test_inventory_freezes_each_cross_cutting_contract_class(self) -> None:
        kinds = {surface["kind"] for surface in self.inventory()}
        self.assertTrue(
            {
                "process_owner",
                "process_consumer",
                "support_process",
                "renderer_contract",
                "cache_write",
                "cache_lookup",
                "cache_serve",
                "offline_owner",
                "offline_lookup",
                "offline_serve",
                "client_capability",
            }.issubset(kinds)
        )

    def test_fleet_provenance_is_retained_without_claiming_qualification(self) -> None:
        path = FIXTURES / "fleet-ffmpeg-2026-09-05.toml"
        with path.open("rb") as document:
            fleet = tomllib.load(document)

        self.assertEqual(fleet["qualification"], "advertised-only")
        self.assertEqual(len(fleet["container_binary_sha256"]), 64)
        self.assertEqual(len(fleet["container_buildconf_sha256"]), 64)
        self.assertEqual(len(fleet["relevant_decoder_inventory_sha256"]), 64)
        self.assertEqual(
            {node["name"] for node in fleet["nodes"]},
            {"nynuc", "m6", "nuc4", "nuc3"},
        )


if __name__ == "__main__":
    unittest.main()
