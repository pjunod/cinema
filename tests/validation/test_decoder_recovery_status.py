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
CORE_DECODE = ROOT / "crates/plurx-core/src/transcode/decode.rs"
CORE_TRANSCODE = ROOT / "crates/plurx-core/src/transcode/mod.rs"
CORE_RECIPE = ROOT / "crates/plurx-core/src/transcode/recipe.rs"
DAEMON_TRANSCODE = ROOT / "crates/plurxd/src/transcode.rs"
LIVE_TV = ROOT / "crates/plurxd/src/live_tv.rs"
DECODER_HEALTH = ROOT / "crates/plurxd/src/decoder_health.rs"
FORGEJO_MAIN_LINEAGE = "4a6a0268bd314ad5587cb3037f12ebd992c0074e"
M1_EFFORT_BASE = "a8bbe574"
M2_TASK_BASE = "f7f98b013ffe9dcc5414e25e0b2e505df3e7beb7"
M3A_TASK_BASE = "773ad4888194ad3b2986b60bd8d1bd4d67595b4a"
M5A_TASK_BASE = "f0f7aec8254ce7c09221bf4462f2f34905f172ef"
M0_QUALIFIED_HEAD = "59d0a4d1"
M0_FORGEJO_PR = "http://192.168.4.7:3000/noirr/plurx/pulls/62"
M1_RECEIPT_HEAD = "81d46577"
M1_RUNTIME_RECEIPT_HEAD = "bc3c3bee"
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
        cls.core_decode = CORE_DECODE.read_text(encoding="utf-8")
        cls.core_transcode = CORE_TRANSCODE.read_text(encoding="utf-8")
        cls.core_recipe = CORE_RECIPE.read_text(encoding="utf-8")
        cls.daemon_transcode = DAEMON_TRANSCODE.read_text(encoding="utf-8")
        cls.live_tv = LIVE_TV.read_text(encoding="utf-8")

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
            f"M5a task base:** M4 candidate `{M5A_TASK_BASE}`", self.flat_status
        )
        self.assertIn(M1_EFFORT_BASE, self.status)
        self.assertIn(
            "| Pre-rebase `01368ce1` | `make validate-full`", self.status
        )
        self.assertIn(M0_FORGEJO_PR, self.status)
        self.assertRegex(
            self.status,
            rf"(?m)^\| `{M0_QUALIFIED_HEAD}` \| .*make validate-full.* \| "
            r"Pass · 23 passed, 0 failed, 2 declared skips;",
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
            f"Exact receipt | `{M1_RECEIPT_HEAD}`: exact mapped history audit 1,387, "
            "catalog 24 points / 30 checks / 1,444 audited files, validation 154/154, "
            "operations 211/211, formatting, and the pinned all-target workspace "
            "compile pass",
            self.status,
        )
        self.assertIn(
            f"Runtime repair receipt `{M1_RUNTIME_RECEIPT_HEAD}` remains the exact "
            "1,378-history mutation-proof receipt",
            self.status,
        )
        self.assertIn(
            "M1 head `22ab9c8d` completed 22 checks, failed only `cluster-auth`, "
            "and declared 2 skips",
            self.status,
        )
        self.assertIn(
            "`81d46577` retains the exact 3,600-second outer ceiling",
            self.status,
        )
        self.assertIn("proves recorded live identities disappeared", self.status)
        self.assertIn("reaps only its owned shell", self.status)
        self.assertIn(
            "Validation checks may not daemonize, double-fork, or deliberately "
            "orphan child sessions",
            self.status,
        )
        self.assertIn(
            "arbitrary detached-session containment is not claimed", self.status
        )
        self.assertIn("sealed, self-contained Linux FFprobe artifact", self.status)
        self.assertIn("path and second-memfd exec", self.status)
        self.assertIn("final kill use exact pidfd signals", self.status)
        self.assertIn("macOS collector 20/20", self.status)
        self.assertIn("collector suite passes 36/36", self.status)
        self.assertIn("observer-activation contract", self.status)
        self.assertIn("one-shot exec supervision", self.status)
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
        self.assertIn(M1_REPAIR_HEAD, self.status)
        self.assertIn("daemon 1,777/1,777", self.status)
        self.assertIn("no exact postcommit history claim", self.status)
        self.assertNotIn("`608dd04d`: history 1,357", self.status)
        self.assertNotIn("Working tree after `0dcbcdf2`", self.status)

    def test_m2_working_tree_binds_arguments_and_identity_to_one_plan(self) -> None:
        self.assertRegex(
            self.core_decode,
            r"(?m)^pub const RESOLVED_TRANSCODE_PLAN_VERSION: u32 = 1;$",
        )
        self.assertRegex(
            self.core_decode,
            r'(?m)^pub const UNQUALIFIED_ARTIFACT_NAMESPACE: &str = '
            r'"decoder-plan-v1-unqualified";$',
        )
        self.assertRegex(
            self.core_recipe,
            r"(?m)^pub const CACHE_RECIPE_VERSION: i64 = 3;$",
        )
        digest_body = self.core_decode.split("pub fn plan_digest", 1)[1].split(
            "pub fn artifact_namespace", 1
        )[0]
        self.assertNotIn("self.decode.reason", digest_body)
        self.assertIn('feed("source_facts", self.source_facts_digest.as_bytes())', digest_body)

        self.assertIn(
            "pub fn hls_args(plan: &ResolvedTranscode, execution: &TranscodeExecution)",
            normalized(self.core_transcode),
        )
        self.assertEqual(self.daemon_transcode.count("transcode::hls_args("), 3)
        self.assertNotIn("transcode::hls_args(&file", self.daemon_transcode)
        self.assertIn("plan: &'a ResolvedTranscode", self.daemon_transcode)
        self.assertEqual(
            self.daemon_transcode.count('std::env::var("PLURX_HWDECODE")'), 1
        )
        self.assertIn("struct LiveTvTranscodePlan", self.live_tv)
        self.assertIn('.args(["-hwaccel", "none"])', self.live_tv)

        builders = [
            surface
            for surface in self.inventory["surfaces"]
            if surface["kind"] == "hls_builder"
        ]
        self.assertEqual(len(builders), 4)
        for surface in builders:
            with self.subTest(surface=surface["id"]):
                source = (ROOT / surface["source"]).read_text(encoding="utf-8")
                self.assertEqual(surface.get("m2_state"), "migrated")
                self.assertEqual(source.count(surface.get("m2_anchor", "")), 1)

        self.assertIn("M5a candidate; M0–M4 merged into the effort", self.status)
        self.assertIn("decoder-plan-v1-unqualified", self.status)
        self.assertIn(
            "No new compile-time feature gate or hidden runtime enable switch is "
            "introduced by M2.",
            self.flat_status,
        )
        # The effort still owes exactly one full qualification; what changed
        # is where it is owed. `AGENTS.md` puts it on the promotion head, not
        # on every task candidate, and the deviation is recorded rather than
        # taken silently.
        self.assertIn(
            "Deferred to the `Main promotion gate`, per `AGENTS.md`", self.status
        )
        self.assertIn(
            "the effort still owes exactly one `make validate-full` on the exact "
            "promotion head",
            self.flat_status,
        )

    def test_one_artifact_name_per_source_however_it_was_measured(self) -> None:
        """The producer and the player must compute one name for one title.

        The artifact key is the catalog identity, which every route derives the
        same way. The descriptor fingerprint answers a different question and is
        kept out of the key: when it was in the key, descriptor-bound and
        catalog-row planning produced disjoint key spaces for identical work and
        the pre-transcode cache could never hit.
        """
        self.assertRegex(
            self.core_decode,
            r"(?m)^pub struct DecodeCacheIdentity\(String\);$",
        )
        self.assertIn("pub fn from_media_file(file: &MediaFile) -> Self", self.core_decode)
        self.assertRegex(
            self.core_decode,
            r"(?m)^pub enum PlanSourceBinding \{$",
        )

        digest_body = self.core_decode.split("pub fn plan_digest", 1)[1].split(
            "pub fn artifact_namespace", 1
        )[0]
        self.assertIn("source_cache_identity", digest_body)
        self.assertNotIn("source_binding", digest_body)
        self.assertNotIn("observed_source_identity", digest_body)

        facts_digest = self.core_decode.split("struct FactsDigest", 1)[1].split("}", 1)[0]
        self.assertIn("input_video_stream", facts_digest)
        self.assertNotIn("source_identity", facts_digest)

        recipe_ctor = self.core_recipe.split("pub fn new<'a>(", 1)[1].split(")", 1)[0]
        self.assertNotIn("MediaFile", recipe_ctor)
        self.assertIn("plan: &'a ResolvedTranscode", recipe_ctor)
        self.assertNotIn("self.file", self.core_recipe)

        self.assertIn(
            "One name per source, however the source was measured", self.status
        )
        self.assertIn(
            "live and offline resolution still\nplan from stored FFprobe JSON and "
            "therefore carry `CatalogRow`",
            self.status,
        )
        self.assertIn(
            "nothing in M2 yet refuses a `CatalogRow` plan", self.status
        )

    def test_provenance_is_kept_out_of_the_artifact_name(self) -> None:
        """How a fact was obtained is not part of what will be produced.

        Three terms forked the key on provenance rather than on bytes: the
        descriptor fingerprint, the stream-selection provenance, and the
        capability evidence class. Each gave two producers of identical work
        two different artifact names.
        """
        digest_body = self.core_decode.split("pub fn plan_digest", 1)[1].split(
            "pub fn artifact_namespace", 1
        )[0]
        # Match the emitting call, not the bare identifier: the identifiers
        # appear in the comment that explains their absence. Allow the line
        # break rustfmt puts after `feed(`.
        def feeds(name: str) -> bool:
            return re.search(rf'feed\(\s*"{name}"', digest_body) is not None

        for absent in ("decode_evidence", "source_binding", "observed_source_identity"):
            self.assertFalse(feeds(absent), absent)
        self.assertTrue(feeds("source_cache_identity"))
        self.assertTrue(feeds("input_video_stream"))

        facts_digest = self.core_decode.split("struct FactsDigest", 1)[1].split("}", 1)[0]
        self.assertIn("input_video_stream", facts_digest)
        self.assertNotIn("selection_provenance", facts_digest)
        self.assertNotIn("source_identity", facts_digest)

        # One selection rule, reached by both planning routes.
        daemon_facts = (ROOT / "crates/plurxd/src/decode_facts.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("pub(crate) fn legacy_ordinal_facts(", daemon_facts)
        self.assertEqual(self.daemon_transcode.count("legacy_ordinal_facts("), 1)
        self.assertNotIn(
            "DecodeFacts::from_ffprobe_json_with_catalog(", self.daemon_transcode
        )

        self.assertIn(
            "What the whole-PR review found, and what it changed", self.status
        )

    def test_m3a_grammar_is_the_qualified_one(self) -> None:
        """The Rust grammar and the M0 harness are one policy, not two.

        Both files hold the same four constants and the same primary-record
        rule. Nothing but this test stops the Rust copy from drifting away from
        the numbers and the wording the retained fixtures qualified, and a
        grammar that has drifted is a grammar that acts on evidence it does not
        have.
        """
        health = DECODER_HEALTH.read_text(encoding="utf-8")
        for name, value in (
            ("VIDEO_DECODE_ERROR_WINDOW", "Duration = Duration::from_millis(2_000)"),
            ("VIDEO_DECODE_ERROR_LIMIT", "usize = 5"),
            ("DIAGNOSTIC_DRAIN_BUDGET", "Duration = Duration::from_millis(2_000)"),
            ("MAX_DIAGNOSTIC_LINE_BYTES", "usize = 16 * 1024"),
            ("AUTOMATIC_PRODUCER_RECOVERY_LIMIT", "u32 = 1"),
        ):
            with self.subTest(constant=name):
                self.assertIn(f"pub const {name}: {value};", health)

        # The message is the harness's literal, matched as a prefix. A
        # `contains` here is what let a filter-graph failure carrying the
        # contract's own detail text read as a decode failure.
        self.assertIn(
            'const PRIMARY_MESSAGE: &str = "Error submitting packet to decoder:";',
            health,
        )
        self.assertIn(
            "Error submitting packet to decoder:",
            self.harness,
            "the harness and the Rust grammar name the same message",
        )
        # Both halves of the stream selector, and every contract field that
        # changes what a line means.
        for field in (
            "ffmpeg_version",
            "binary_sha256",
            "buildconf_sha256",
            "stderr_mode",
            "input_codec",
            "decoder",
        ):
            with self.subTest(covered_field=field):
                self.assertIn(f"build.{field}", health)
        self.assertIn("selected_input", health)
        self.assertIn("selected_stream", health)

        # No retained contract qualifies a fatal family, so the grammar cannot
        # emit a backend fault on the builds this repository has evidence for.
        self.assertNotIn("backend_fault_detail", CONTRACTS.read_text(encoding="utf-8"))
        self.assertIn("backend_fault_detail", health)

        m3a = self.status.split("## M3a working tree", 1)[1].split("\n## ", 1)[0]
        self.assertIn("a tolerant structural match", " ".join(m3a.split()))
        self.assertIn("22/22", m3a)


if __name__ == "__main__":
    unittest.main()
