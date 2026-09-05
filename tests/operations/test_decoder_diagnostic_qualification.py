from __future__ import annotations

import hashlib
from pathlib import Path
import runpy
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/decoder-diagnostic-qualification"
FIXTURES = ROOT / "tests/playback/decoder-health"
CHECKER = runpy.run_path(str(SCRIPT))
RAWVIDEO_CONTRACT = "ffmpeg-8.0.1-3ubuntu2-rawvideo-v1"
EXPECTED_INVENTORY_IDS = {
    "builder.prepublication_retry",
    "builder.resumable_part",
    "builder.live_hls",
    "process.settings_ffmpeg_version",
    "process.observed_ffmpeg",
    "process.fragmented_ffmpeg",
    "process.vod_generation",
    "process.vod_head_regeneration",
    "process.vod_pipe_consumer",
    "process.pgs_demux",
    "process.fragment_index",
    "process.progressive_remux",
    "process.subtitle_extract",
    "process.subtitle_window",
    "process.pipe_probe",
    "process.ffmpeg_engine_identity",
    "process.ffmpeg_capability_query",
    "process.dovi_reshape_graph_probe",
    "process.dovi_passthrough_probe",
    "process.hdr10_passthrough_probe",
    "process.hdr10_qsv_probe",
    "process.dovi_qsv_probe",
    "process.dovi_pixel_probe",
    "process.pacing_probe",
    "process.media_origin_probe",
    "process.chapter_probe",
    "process.dv_disk_tool",
    "renderer.candidates",
    "renderer.pairing",
    "renderer.residency",
    "renderer.dynamic_range",
    "renderer.decode_arguments",
    "renderer.device_initialization",
    "renderer.filters",
    "renderer.software_requirement",
    "renderer.session_selection",
    "renderer.output_grade",
    "renderer.fallback",
    "renderer.declined",
    "cache.shared_mount_io",
    "cache.shared_root_identity",
    "cache.shared_root_admission",
    "cache.local_publish",
    "cache.manifest_publish",
    "cache.manifest_capability_publish",
    "cache.shared_publish",
    "cache.location_read",
    "cache.local_location_read",
    "cache.speculative_hit",
    "cache.offer_verification",
    "cache.shared_read_prepare",
    "cache.session_serve",
    "cache.playlist_read",
    "cache.object_read",
    "cache.decoded_manifest",
    "offline.prepare",
    "offline.publish_ready",
    "offline.generation_cache",
    "offline.location_validation",
    "offline.playlist",
    "offline.segment",
    "client.web_actions",
    "client.apple_actions",
    "client.android_actions",
}


class DecoderDiagnosticQualificationTests(unittest.TestCase):
    def test_qualified_ffmpeg_8_shape_latches_fifth_record_in_361_ms(self) -> None:
        result = CHECKER["qualify"](
            FIXTURES / "qualified-ffmpeg-8.stderr",
            contract_id=RAWVIDEO_CONTRACT,
        )

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.severity_qualified_primary_records, 5)
        self.assertEqual(result.contract_qualified_primary_records, 5)
        self.assertEqual(result.fault_at_ms, 361)
        self.assertEqual(result.detection_latency_ms, 361)
        self.assertTrue(result.observation_complete)
        self.assertTrue(result.automatic_action_qualified)

    def test_structural_match_without_exact_contract_is_observation_only(self) -> None:
        fixture = FIXTURES / "qualified-ffmpeg-8.stderr"
        without_contract = CHECKER["qualify"](fixture)

        self.assertEqual(without_contract.primary_video_error_records, 5)
        self.assertEqual(without_contract.contract_qualified_primary_records, 0)
        self.assertFalse(without_contract.automatic_action_qualified)

    def test_contract_rejects_addressless_or_different_codec_records(self) -> None:
        cases = {
            "addressless": (
                "[vist#0:0/rawvideo] [dec:rawvideo] [error] "
                "Error submitting packet to decoder: invalid data"
            ),
            "different-codec": (
                "[vist#0:0/h264 @ <address>] [dec:h264 @ <address>] [error] "
                "Error submitting packet to decoder: invalid data"
            ),
        }
        with tempfile.TemporaryDirectory() as directory:
            for name, primary in cases.items():
                fixture = Path(directory) / f"{name}.stderr"
                fixture.write_text(
                    "\n".join(f"{observed}\t{primary}" for observed in range(5))
                    + "\n",
                    encoding="utf-8",
                )
                result = CHECKER["qualify"](
                    fixture,
                    contract_id=RAWVIDEO_CONTRACT,
                )
                with self.subTest(case=name):
                    self.assertEqual(result.primary_video_error_records, 5)
                    self.assertEqual(result.contract_qualified_primary_records, 0)
                    self.assertFalse(result.automatic_action_qualified)

    def test_contract_rejects_unbound_copy_of_qualified_records(self) -> None:
        source = FIXTURES / "qualified-ffmpeg-8.stderr"
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / source.name
            fixture.write_bytes(source.read_bytes() + b"# changed evidence\n")
            result = CHECKER["qualify"](
                fixture,
                contract_id=RAWVIDEO_CONTRACT,
            )

        self.assertEqual(result.primary_video_error_records, 5)
        self.assertEqual(result.contract_qualified_primary_records, 0)
        self.assertFalse(result.automatic_action_qualified)

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

    def test_any_repeat_summary_blocks_action_even_when_unrelated(self) -> None:
        primary = (
            "[vist#0:0/rawvideo @ <address>] "
            "[dec:rawvideo @ <address>] [error] "
            "Error submitting packet to decoder: "
            "Invalid data found when processing input"
        )
        audio = (
            "[aist#0:1/aac @ <address>] [dec:aac @ <address>] [error] "
            "Error submitting packet to decoder: invalid data"
        )
        lines = [f"0\t{audio}", "1\t[error] Last message repeated 3 times"]
        lines.extend(f"{observed}\t{primary}" for observed in range(10, 15))
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "qualified-repeat.stderr"
            fixture.write_text("\n".join(lines) + "\n", encoding="utf-8")
            contract_path = Path(directory) / "contracts.toml"
            contract_path.write_text(
                "\n".join(
                    (
                        "version = 1",
                        "[[contracts]]",
                        f'id = "{RAWVIDEO_CONTRACT}"',
                        'input_codec = "rawvideo"',
                        'decoder = "rawvideo"',
                        "require_context_addresses = true",
                        'error_detail = "Invalid data found when processing input"',
                        f'fixture = "{fixture.name}"',
                        f'fixture_sha256 = "{hashlib.sha256(fixture.read_bytes()).hexdigest()}"',
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            checker_globals = CHECKER["qualify"].__globals__
            original_contracts_path = checker_globals["CONTRACTS_PATH"]
            checker_globals["CONTRACTS_PATH"] = contract_path
            try:
                result = CHECKER["qualify"](
                    fixture,
                    contract_id=RAWVIDEO_CONTRACT,
                )
            finally:
                checker_globals["CONTRACTS_PATH"] = original_contracts_path

        self.assertEqual(result.unrelated_repeat_summaries, 1)
        self.assertEqual(result.contract_qualified_primary_records, 5)
        self.assertIsNotNone(result.fault_at_ms)
        self.assertFalse(result.automatic_action_qualified)

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
        self.assertEqual(set(identifiers), EXPECTED_INVENTORY_IDS)
        self.assertEqual(len(surfaces), 64)
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
        self.assertEqual(set(fleet["capture"]), {
            "host_binary_sha256",
            "host_buildconf_sha256",
            "container_binary_sha256",
            "container_buildconf_sha256",
            "container_decoder_inventory_sha256",
            "container_image_id",
            "canonicalization",
        })
        nodes = {node["name"]: node for node in fleet["nodes"]}
        self.assertEqual(set(nodes), {"nynuc", "m6", "nuc4", "nuc3"})
        for name, node in nodes.items():
            with self.subTest(node=name):
                for field in (
                    "container_image_id",
                    "container_binary_sha256",
                    "container_buildconf_sha256",
                    "container_decoder_inventory_sha256",
                ):
                    self.assertRegex(node[field], r"^[0-9a-f]{64}$")
                if node["host_ffmpeg"] != "not-on-PATH":
                    self.assertRegex(node["host_binary_sha256"], r"^[0-9a-f]{64}$")
                    self.assertRegex(node["host_buildconf_sha256"], r"^[0-9a-f]{64}$")

    def test_diagnostic_contract_is_versioned_and_bound_to_one_binary(self) -> None:
        path = FIXTURES / "diagnostic-contracts.toml"
        with path.open("rb") as document:
            contracts = tomllib.load(document)

        self.assertEqual(contracts["version"], 1)
        self.assertEqual(len(contracts["contracts"]), 1)
        contract = contracts["contracts"][0]
        self.assertEqual(contract["id"], RAWVIDEO_CONTRACT)
        self.assertEqual(len(contract["binary_sha256"]), 64)
        self.assertEqual(len(contract["buildconf_sha256"]), 64)
        fixture = FIXTURES / contract["fixture"]
        self.assertEqual(
            hashlib.sha256(fixture.read_bytes()).hexdigest(),
            contract["fixture_sha256"],
        )
        self.assertEqual(
            contract["error_detail"],
            "Invalid data found when processing input",
        )
        self.assertIn("not deployed producer", contract["scope"])

    def test_actual_media_baseline_is_bound_to_generator_and_output_evidence(self) -> None:
        path = ROOT / "tests/playback/decoder-media-baseline-2026-09-05.toml"
        with path.open("rb") as document:
            baseline = tomllib.load(document)
        generator = ROOT / baseline["generator"]

        self.assertEqual(
            hashlib.sha256(generator.read_bytes()).hexdigest(),
            baseline["generator_sha256"],
        )
        self.assertFalse(baseline["incident_media_available"])
        self.assertEqual(
            {control["id"] for control in baseline["controls"]},
            {"h264-sdr", "mpeg4-simple-avi", "hevc-main10-hdr10"},
        )
        for control in baseline["controls"]:
            with self.subTest(control=control["id"]):
                for field in (
                    "source_sha256",
                    "playlist_sha256",
                    "segment_sha256",
                    "decoded_framemd5_sha256",
                ):
                    self.assertRegex(control[field], r"^[0-9a-f]{64}$")
                self.assertIn("codec_name=", control["source_probe"])
                self.assertIn("codec_name=", control["output_probe"])


if __name__ == "__main__":
    unittest.main()
