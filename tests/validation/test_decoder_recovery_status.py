from __future__ import annotations

import json
import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
STATUS = ROOT / "docs/DECODER_SELECTION_RECOVERY_STATUS.md"
INVENTORY = ROOT / "tests/playback/decoder-selection-m0-inventory.toml"
ARGUMENTS = ROOT / "tests/playback/decoder-selection-m0-args.json"
CONTRACTS = ROOT / "tests/playback/decoder-health/diagnostic-contracts.toml"
FLEET = ROOT / "tests/playback/decoder-health/fleet-ffmpeg-2026-09-05.toml"
MEDIA = ROOT / "tests/playback/decoder-media-baseline-2026-09-05.toml"
HARNESS = ROOT / "scripts/decoder-diagnostic-qualification"
FORGEJO_MAIN_LINEAGE = "4a6a0268bd314ad5587cb3037f12ebd992c0074e"
M1_EFFORT_BASE = "a8bbe574"
M0_QUALIFIED_HEAD = "59d0a4d1"
M0_FORGEJO_PR = "http://192.168.4.7:3000/noirr/plurx/pulls/62"
M1_RECEIPT_HEAD = "628277d1"
M1_REPAIR_HEAD = "07c8f905"


def normalized(text: str) -> str:
    return " ".join(text.split())


class DecoderRecoveryStatusContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.status = STATUS.read_text(encoding="utf-8")
        cls.flat_status = normalized(cls.status)
        with INVENTORY.open("rb") as document:
            cls.inventory = tomllib.load(document)
        cls.arguments = json.loads(ARGUMENTS.read_text(encoding="utf-8"))
        with CONTRACTS.open("rb") as document:
            cls.contracts = tomllib.load(document)
        with FLEET.open("rb") as document:
            cls.fleet = tomllib.load(document)
        with MEDIA.open("rb") as document:
            cls.media = tomllib.load(document)
        cls.harness = HARNESS.read_text(encoding="utf-8")

    def test_frozen_inventory_and_argument_claims_match_retained_artifacts(self) -> None:
        surfaces = self.inventory["surfaces"]
        self.assertEqual(len(surfaces), 73)
        self.assertIn("bring the inventory to 73", self.status)

        names = [case["name"] for case in self.arguments]
        self.assertEqual(len(names), 16)
        self.assertEqual(len(names), len(set(names)))
        argument_section = self.status.split("## M0 argument and media controls", 1)[1]
        argument_section = argument_section.split("## M0 diagnostic contract v1", 1)[0]
        for name in names:
            with self.subTest(argument_case=name):
                self.assertEqual(
                    argument_section.count(f"`{name}`"),
                    1,
                    "every retained argv case must appear once in the M0 status",
                )

    def test_diagnostic_constants_and_action_scope_are_durable(self) -> None:
        constants = {
            "VIDEO_DECODE_ERROR_WINDOW_MS": ("2_000", "Video error window", "2,000 ms"),
            "VIDEO_DECODE_ERROR_LIMIT": ("5", "Video error limit", "5"),
            "DIAGNOSTIC_DRAIN_BUDGET_MS": ("2_000", "Diagnostic drain budget", "2,000 ms"),
            "MAX_DIAGNOSTIC_LINE_BYTES": ("16 * 1024", "Maximum retained line", "16 KiB"),
        }
        for name, (source_value, label, documented_value) in constants.items():
            with self.subTest(constant=name):
                self.assertRegex(
                    self.harness,
                    rf"(?m)^{name} = {re.escape(source_value)}$",
                )
                self.assertIn(f"| {label} | {documented_value} |", self.status)
        self.assertIn("| Automatic recovery limit | 1 |", self.status)
        self.assertRegex(
            self.harness,
            r"(?m)^MAX_RETAINED_COUNTER = \(1 << 64\) - 1$",
        )
        self.assertIn("| Maximum retained counter | `u64::MAX` |", self.status)

        contracts = self.contracts["contracts"]
        self.assertEqual(len(contracts), 1)
        contract = contracts[0]
        self.assertEqual(contract["host"], "nynuc")
        self.assertEqual(contract["input_codec"], "rawvideo")
        self.assertEqual(contract["decoder"], "rawvideo")
        self.assertEqual(
            contract["scope"],
            "host diagnostic grammar only; not deployed producer or MPEG-4 qualification",
        )
        self.assertIn(
            "Its sole action contract is host FFmpeg 8.0.1 `rawvideo`; it is not "
            "deployed-producer or MPEG-4 qualification.",
            self.flat_status,
        )

    def test_status_preserves_unqualified_fleet_media_and_client_boundaries(self) -> None:
        self.assertEqual(self.fleet["qualification"], "advertised-only")
        self.assertEqual(
            {node["name"] for node in self.fleet["nodes"]},
            {"nynuc", "m6", "nuc4", "nuc3"},
        )
        for node in self.fleet["nodes"]:
            self.assertIn(f"| `{node['name']}` |", self.status)
        self.assertIn("no fleet backend class is qualified in M0.", self.flat_status)

        self.assertFalse(self.media["incident_media_available"])
        self.assertEqual(len(self.media["controls"]), 3)
        self.assertIn(
            "original media is not present, so VideoToolbox/software output "
            "comparison remains required",
            self.flat_status,
        )
        self.assertIn("Postpublication recovery is likewise unqualified.", self.status)
        self.assertIn("Live TV Web", self.status)
        self.assertIn(
            "Live TV is limited to prepublication recovery",
            self.flat_status,
        )
        self.assertIn(
            "Unsupported sessions end explicitly; they do not silently run an "
            "unqualified automatic replacement.",
            self.flat_status,
        )

    def test_review_repairs_and_validation_failures_remain_explicit(self) -> None:
        for durable_finding in (
            "Fixture reader allocated the whole input",
            "An unrelated repeat summary still allowed automatic action",
            "Contract metadata was declared but not enforced",
            "No actual media baseline existed",
            "Probe normalization had no direct unit coverage",
        ):
            with self.subTest(finding=durable_finding):
                self.assertIn(durable_finding, self.status)

        for invalid_run in (
            "Invalid environment run",
            "Diagnostic pass",
            "Invalid tool-version run",
        ):
            with self.subTest(run=invalid_run):
                self.assertIn(invalid_run, self.status)

        self.assertIn(
            "python3 -m unittest tests.operations.test_decoder_diagnostic_qualification "
            "tests.validation.test_decoder_recovery_status",
            self.status,
        )
        self.assertNotIn("tests.validation.test_decoder_selection_inventory", self.status)

    def test_current_base_and_receipt_state_cannot_be_confused_with_history(self) -> None:
        self.assertIn(FORGEJO_MAIN_LINEAGE, self.status)
        self.assertIn(
            f"M1 task PR base:** effort head `{M1_EFFORT_BASE}`", self.flat_status
        )
        self.assertIn("Historical pre-rebase head `01368ce1`", self.status)
        self.assertIn(M0_FORGEJO_PR, self.status)
        self.assertIn(
            f"Exact code head `{M0_QUALIFIED_HEAD}` passed `make validate-full`: "
            "23 passed, 0 failed, 2 declared skips",
            self.status,
        )
        self.assertNotIn("has not yet run its final suite", self.status)
        self.assertNotIn("repair head pending push", self.status)
        for falsely_remapped_receipt in (
            "`d528794b` | `make validate-full`",
            "`1ad59932` | Independent",
        ):
            self.assertNotIn(falsely_remapped_receipt, self.status)

    def test_m1_review_repair_distinguishes_exact_and_working_tree_evidence(self) -> None:
        self.assertIn(
            f"Exact receipt | `{M1_RECEIPT_HEAD}`: exact mapped history audit 1,374, "
            "catalog 24/30/1,443",
            self.status,
        )
        self.assertIn("sealed, self-contained Linux FFprobe artifact", self.status)
        self.assertIn("path and second-memfd exec", self.status)
        self.assertIn("pidfd-before-reap", self.status)
        self.assertIn("macOS collector 20/20", self.status)
        self.assertIn("pinned Linux collector 36/36", self.status)
        self.assertIn("one-shot exec supervision", self.status)
        self.assertIn("invalidated first notification", self.status)
        self.assertIn("source-level enforcement", self.status)
        self.assertIn("does not prove arbitrary static parser code trustworthy", self.status)
        self.assertIn(
            "does not make arbitrary malicious parser code safe", self.flat_status
        )
        self.assertIn(
            "does not prevent an already trusted parser from interpreting or "
            "mapping bytes it can read as code",
            self.flat_status,
        )
        self.assertIn(
            "no full unsupported-architecture compile pass is claimed",
            self.flat_status,
        )
        self.assertIn(f"Exact code head `{M1_REPAIR_HEAD}`", self.status)
        self.assertIn("daemon 1,777/1,777", self.status)
        self.assertIn("no exact postcommit history claim", self.status)
        self.assertNotIn("`608dd04d`: history 1,357", self.status)
        self.assertNotIn("Working tree after `0dcbcdf2`", self.status)


if __name__ == "__main__":
    unittest.main()
