from __future__ import annotations

import hashlib
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
CORE_HEALTH = ROOT / "crates/plurx-core/src/transcode/health.rs"
CORE_MANIFEST = ROOT / "crates/plurx-core/src/transcode/manifest.rs"
FORGEJO_MAIN_LINEAGE = "4a6a0268bd314ad5587cb3037f12ebd992c0074e"
M1_EFFORT_BASE = "a8bbe574"
M2_TASK_BASE = "f7f98b013ffe9dcc5414e25e0b2e505df3e7beb7"
M3A_TASK_BASE = "773ad4888194ad3b2986b60bd8d1bd4d67595b4a"
M5A_TASK_BASE = "f0f7aec8254ce7c09221bf4462f2f34905f172ef"
M3B_TASK_BASE = "de05be0494d846bc3b77e462505f1d3ecdb21b46"
M3C1_MERGED_HEAD = "bf75c62cf642ecac7671456aa88e48ed57fd46fd"
M3C2_MERGED_HEAD = "ff10c2dbfcf74d6e78d51b69378fd40a540b3ff4"
M3C3_TASK_BASE = M3C2_MERGED_HEAD
M3C3_MERGED_HEAD = "b602b9f2add7c14861265f7d384c1d9910b0c742"
M3C4_MERGED_HEAD = "86647b37cb9e91d3c043f3e3a0ef4fb5330ef34e"
M3D_TASK_BASE = M3C4_MERGED_HEAD
M3D_MERGED_HEAD = "a8403e104f8be8a31dba09d184bfa7da983d73da"
M3E_TASK_BASE = M3D_MERGED_HEAD
CORE_INVENTORY = ROOT / "crates/plurx-core/src/transcode/decoder_inventory.rs"
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
        # Every retained contract is a host grammar qualification, and none of
        # them is a deployed-producer one. The count is not pinned — a build
        # measured is a contract added — but the boundary is.
        self.assertGreaterEqual(len(contracts), 1)
        for contract in contracts:
            with self.subTest(contract=contract["id"]):
                self.assertIn("host diagnostic grammar only", contract["scope"])
                # Every one of them disclaims being a producer or fleet
                # qualification, however it words it.
                self.assertRegex(contract["scope"], r"not (a )?deployed")
        by_host = {contract["host"]: contract for contract in contracts}
        nynuc = by_host["nynuc"]
        self.assertEqual(nynuc["input_codec"], "rawvideo")
        self.assertEqual(nynuc["decoder"], "rawvideo")
        self.assertEqual(
            nynuc["scope"],
            "host diagnostic grammar only; not deployed producer or MPEG-4 qualification",
        )
        self.assertIn(
            "Its sole *deployed-build* action contract is host FFmpeg 8.0.1 `rawvideo`; "
            "it is not deployed-producer or MPEG-4 qualification.",
            self.flat_status,
        )
        # And the only other one is explicit that it is a workstation, so no
        # reader can mistake it for fleet evidence.
        self.assertNotIn("pauls-macbook-pro-2", {node["name"] for node in self.fleet["nodes"]})

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
        self.assertIn(f"M3e task base:** effort head `{M3E_TASK_BASE}`", self.flat_status)
        # Each merged head is named, not only the pull request that carried it.
        self.assertIn(M3C1_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C2_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C3_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C4_MERGED_HEAD[:8], self.status)
        self.assertIn(M3D_MERGED_HEAD[:8], self.status)
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

        self.assertIn(
            "M3e candidate; M0–M3d, M4 and M5a merged into the effort", self.status
        )
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

    def test_m3c1_receipt_is_authenticated_and_unattributed_bytes_carry_none(self) -> None:
        """A receipt the manifest digest does not cover is worse than none.

        A reader would believe it, and anyone who can write the generation
        directory could edit it. These four pins are what stop the field from
        drifting out of the digested body, out of its backward-compatible
        encoding, or into a publication path that did not observe the bytes it
        would be describing.
        """
        manifest = CORE_MANIFEST.read_text(encoding="utf-8")
        health = CORE_HEALTH.read_text(encoding="utf-8")
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")

        # The receipt is inside `ManifestBody`, which is what `body_digest`
        # hashes — not merely a field beside the digest.
        body = manifest.split("struct ManifestBody<'a> {", 1)[1].split("\n}", 1)[0]
        self.assertIn("producer_health: Option<&'a ProducerHealthReceipt>", body)
        self.assertIn('#[serde(skip_serializing_if = "Option::is_none")]', body)
        # And absent still encodes exactly as it did before the field existed.
        self.assertIn(
            '#[serde(default, skip_serializing_if = "Option::is_none")]', manifest
        )

        # `publish` is the entry point for callers assembling bytes they did
        # not watch being produced, and it cannot claim otherwise.
        self.assertIn(
            "publish_controlled(root, generation_id, ordered_names, None, || false)",
            normalized(manifest),
        )

        # Every attempt is recorded, produced or not: an attempt that decodes
        # nothing and exits zero writes no segment, and its receipt is the one
        # that says the film is truncated.
        self.assertIn("struct GenerationObservation {", daemon)
        self.assertEqual(daemon.count("generation_health.record(receipt, produced);"), 1)
        self.assertNotIn("part_receipts", daemon)

        # One bound for the contract identifier, enforced where the identifier
        # is authored as well as where it is stored.
        self.assertIn("pub const MAX_DIAGNOSTIC_CONTRACT_BYTES: usize = 128;", health)
        self.assertIn("pub fn safe_diagnostic_contract_id(", health)
        self.assertIn(
            "ContractLoadError::UnsafeId", DECODER_HEALTH.read_text(encoding="utf-8")
        )
        for contract in tomllib.loads(CONTRACTS.read_text(encoding="utf-8"))["contracts"]:
            with self.subTest(contract=contract["id"]):
                self.assertLessEqual(len(contract["id"].encode("utf-8")), 128)
                self.assertRegex(contract["id"], r"\A[A-Za-z0-9._-]+\Z")

        # The status document does not claim a reader consults one yet.
        self.assertIn("Nothing selects it in production yet", self.status)
        # And the document does not name two different milestones for it.
        self.assertNotIn("That is M3c3, and it now has something", self.status)

    def test_m3c2_a_resumed_part_recovers_only_a_record_still_bound_to_it(self) -> None:
        """Reading a record back may recover a conclusion, never invent one.

        Every way of failing to read one has to land on `unobserved`, because
        the alternative — treating an unreadable record as clean — is the same
        false certificate the effort exists to prevent, reached by a new route.
        """
        health = CORE_HEALTH.read_text(encoding="utf-8")
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")

        # The record binds a receipt to the bytes it was settled over.
        self.assertIn("pub struct RetainedPartReceipt {", health)
        self.assertIn("pub fn part_shape_digest(", health)
        self.assertIn("pub const RETAINED_PART_RECEIPT_VERSION: u32 = 1;", health)
        # And opening it checks the version, the binding and its own digest.
        opened = health.split("impl RetainedPartReceipt {", 1)[1]
        opened = opened.split("pub fn opened(", 1)[1].split("\n    }", 1)[0]
        self.assertIn("self.record_version == RETAINED_PART_RECEIPT_VERSION", opened)
        self.assertIn("self.part_shape == part_shape", opened)
        self.assertIn("actual == self.record_digest", opened)

        # The shape covers the plan and every listed segment's name, size and
        # duration — not its content, which the manifest already hashes.
        shape = health.split("pub fn part_shape_digest(", 1)[1].split("\n}", 1)[0]
        for field in ('"plan"', '"segments"', '"segment"', '"bytes"', '"duration_ms"'):
            self.assertIn(field, shape)

        # Anything that is not a record still bound to these bytes reads as
        # unobserved, and the digest is taken under this pass's plan.
        resumed = daemon.split("async fn resumed_part_health(", 1)[1].split("\n}", 1)[0]
        self.assertIn("part_shape_digest(plan_digest, shape)", resumed)
        self.assertIn("record.opened(&shape_digest).cloned()", resumed)
        # The fallback is in `resume_parts`, and it is the one line that decides
        # what an unreadable record means.
        resume = daemon.split("async fn resume_parts(", 1)[1].split("\n}\n", 1)[0]
        self.assertIn("resumed_part_health(&dir, plan_digest, &shape)", resume)
        self.assertIn("ProducerHealthReceipt::unobserved(", resume)
        self.assertNotIn("Qualification::Qualified", resume)

        # An attempt that left no part is carried at the staging root, because
        # there is no part for its receipt to be sealed beside.
        self.assertIn('GENERATION_HEALTH_FILE: &str = ".generation-health.json"', daemon)
        self.assertIn("pub struct RetainedGenerationHealth {", health)
        record = daemon.split("fn record(&mut self,", 1)[1].split("\n    }", 1)[0]
        self.assertIn("if !produced {", record)
        self.assertIn("self.attempts.push(receipt);", record)

        # The records are staging-local: the manifest inventories objects the
        # part playlists list, and a dotted name is not one of them.
        self.assertIn('PART_HEALTH_FILE: &str = ".part-health.json"', daemon)
        names = daemon.split('std::iter::once("index.m3u8".to_owned())', 1)[1].split(";", 1)[0]
        self.assertNotIn("HEALTH_FILE", names)

    def test_m3c3_the_artifact_identity_is_a_fleet_decision_not_a_node_accident(self) -> None:
        """A namespace derived from what a node knows splits a cluster's cache.

        Contract coverage is matched on the FFmpeg binary's own digest, so two
        distribution builds of one version would compute two cache keys for the
        same encode. The identity has to come from the requested mode, and the
        unqualified name has to stay byte-identical or shipping the mechanism
        is itself a cache flush.
        """
        decode = CORE_DECODE.read_text(encoding="utf-8")
        selection = (ROOT / "crates/plurx-core/tests/decoder_selection.rs").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            'pub const UNQUALIFIED_ARTIFACT_NAMESPACE: &str = "decoder-plan-v1-unqualified";',
            decode,
        )
        # The receipt schema version is part of the name; the assertion beside
        # it is what keeps the two from drifting apart.
        self.assertIn(
            "pub const HEALTH_QUALIFIED_ARTIFACT_NAMESPACE: &str = "
            '"decoder-plan-v1-health-qualified-r1";',
            decode,
        )
        self.assertIn("super::health::PRODUCER_HEALTH_RECEIPT_VERSION == 1", decode)
        # Unqualified is the default, so a fleet that has not turned it on
        # computes the names it always computed.
        qualification = decode.split("pub enum ArtifactQualification {", 1)[1].split(
            "\n}", 1
        )[0]
        self.assertIn("#[default]\n    Unqualified,", qualification)

        # The identity is read from the policy snapshot at resolution, and from
        # nothing observed afterwards.
        self.assertIn(
            "artifact_qualification: policy.artifact_qualification(),", decode
        )
        self.assertIn(
            "self.artifact_qualification.namespace().as_bytes(),", decode
        )
        # Never from the evidence class, which is the same trap one level down.
        self.assertIn("Deliberately absent: `decode.evidence`", decode)

        # And the two properties are asserted, not merely described.
        self.assertIn(
            "fn the_qualified_identity_is_a_separate_key_space_and_costs_nothing_until_it_is_used",
            selection,
        )
        self.assertIn(
            "fn how_well_a_node_knows_its_decoder_does_not_move_the_artifact_identity",
            selection,
        )

        # The document says why enforcement is not here yet, rather than
        # leaving a control that rotates a key space for no benefit.
        self.assertIn("the control never exists without the behaviour behind it", self.status)

        # The recorded constraints a later milestone will implement literally.
        # `verified_cache_hit` reads like a reuse decision and is not: its only
        # caller feeds cluster offer eligibility.
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")
        self.assertEqual(daemon.count("self.verified_cache_hit("), 1)
        self.assertIn(
            "and so\n  does `verified_cache_hit`, which reads like a reuse decision and is not",
            self.status,
        )

    def test_m3c4_a_refusal_declines_to_keep_and_is_terminal(self) -> None:
        """Refusing to keep a generation must not refuse to serve it, or loop.

        Two failure modes sit either side of this rule. Route it through
        invalidation and it deletes bytes and fails a download a user already
        has; make it a yield and the same plan re-encodes the same title on
        every discovery pass forever.
        """
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")
        state = (ROOT / "crates/plurxd/src/state.rs").read_text(encoding="utf-8")
        offline = (ROOT / "crates/plurxd/src/offline.rs").read_text(encoding="utf-8")

        # One rule, both directions, reading the manifest rather than any
        # in-memory value.
        rule = daemon.split("fn generation_permits_reuse(", 1)[1].split("\n}", 1)[0]
        self.assertIn("if !plan.enforces_receipt() {\n        return true;", rule)
        self.assertIn("manifest.is_some_and(", rule)
        self.assertIn("producer_health", rule)
        self.assertIn("permits_reuse", rule)
        self.assertEqual(daemon.count("generation_permits_reuse(plan,"), 2)

        # The refusal is its own outcome, and it never invalidates.
        self.assertIn("OfflineProduceOutcome::HealthRefused", daemon)
        refusal = daemon.split(
            "if !generation_permits_reuse(plan, manifest.as_ref()) {", 1
        )[1].split("return Ok(OfflineProduceOutcome::HealthRefused);", 1)[0]
        self.assertNotIn("invalidate_cache", refusal)
        self.assertIn("quarantine_remove_cache_tree", refusal)

        # Terminal at every caller: a cancelled queue job with a code that is
        # not one of the re-enqueueable ones, and a failed package.
        self.assertIn('cancel_job(self.store.as_ref(), "health_refused"', state)
        # And the code it cancels with is not one the queue treats as
        # provisional, which is the difference between a terminal refusal and a
        # title re-encoded on every discovery pass.
        enqueue = (ROOT / "crates/plurx-core/src/store/sqlite/pretranscode.rs").read_text(
            encoding="utf-8"
        )
        suppression = enqueue.split("fn enqueue_pretranscode_job", 1)[1][:8000]
        self.assertIn("policy_changed", suppression)
        self.assertNotIn("health_refused", suppression)
        self.assertIn('"decode_unhealthy"', offline)
        # With its own metric label: the encoder did not fail, and an operator
        # cannot see that if it is bucketed as `other`.
        codes = offline.split("const FAILURE_CODES", 1)[1].split("];", 1)[0]
        self.assertIn('"decode_unhealthy"', codes)
        health_arm = offline.split("Ok(OfflineProduceOutcome::HealthRefused) => {", 1)[1].split(
            "\n            }", 1
        )[0]
        self.assertNotIn("self.requeue(", health_arm)

        # The qualified namespace names the receipt version it can read, so a
        # build that cannot read one never computes these keys.
        decode = CORE_DECODE.read_text(encoding="utf-8")
        self.assertIn('"decoder-plan-v1-health-qualified-r1"', decode)
        self.assertIn(
            "super::health::PRODUCER_HEALTH_RECEIPT_VERSION == 1", decode
        )

        # Nothing turns it on: the only writer of the effective identity is
        # test-only until the enable path lands.
        self.assertIn(
            "#[cfg(test)]\n    pub(crate) fn test_publish_artifact_qualification(", daemon
        )
        self.assertEqual(daemon.count("fn test_publish_artifact_qualification("), 1)
        self.assertIn("Nothing turns this on", self.status)

    def test_m3d_the_decoder_is_measured_and_only_named_where_enforced(self) -> None:
        """A family is not an implementation, and a guess is worse than silence.

        A contract is qualified against a *named* decoder, so a plan naming
        none can never be matched to one. But a plan naming the wrong one is
        worse: the grammar it yields matches nothing, and a grammar that
        matches nothing reports every stream as clean.
        """
        inventory = CORE_INVENTORY.read_text(encoding="utf-8")
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")

        # Both halves of the context are required, so a context belonging to
        # another stream of another codec cannot answer for this one.
        matcher = inventory.split("fn decoder_for_codec<'a>(", 1)[1].split("\n}", 1)[0]
        self.assertIn('strip_prefix("vist#")', matcher)
        self.assertIn("line_codec != codec", matcher)
        self.assertIn('strip_prefix("dec:")', matcher)

        # Disagreement across lines is refused rather than resolved.
        read = inventory.split("pub fn selected_decoder(", 1)[1].split("\n}", 1)[0]
        self.assertIn("Some(previous) if previous != decoder => return None", read)

        # An unmeasured codec stays unnamed. There is no fallback to the
        # family anywhere in the module.
        self.assertNotIn("unwrap_or(codec)", inventory)
        self.assertNotIn("unwrap_or_else(|| codec", inventory)
        # And what a plan can carry is asked of the plan, not restated here:
        # a name this accepted and the plan refused would make
        # `DecodeCapabilities::new` refuse every plan on the node.
        self.assertIn("crate::transcode::plan_can_name_decoder(name)", inventory)

        # Each probe is bounded and killed on drop. These run before the node
        # opens a listener.
        self.assertIn("const PROBE_TIMEOUT:", inventory)
        # Every spawn, including the two the probe test makes for its own
        # comparison: a probe process that outlives a dropped future is the
        # same leak whether it is production or a test.
        self.assertEqual(
            inventory.count("tokio::process::Command::new("),
            inventory.count(".kill_on_drop(true)"),
        )
        self.assertIn("tokio::time::timeout(PROBE_TIMEOUT, work)", inventory)

        # And the measurement reaches a plan only under the enforced identity.
        naming = daemon.split(".map(|codec| SoftwareDecoder {", 1)[1].split("})", 1)[0]
        self.assertIn("qualifying", naming)
        self.assertIn("self.measured_decoders.implementation(&codec)", naming)
        self.assertIn(
            "let qualifying = qualification.enforces_receipt();", daemon
        )

        # The measurement this host actually made is recorded, including the
        # one codec whose answer is not its own name.
        self.assertIn("| av1 | **libdav1d** |", self.status)
        self.assertIn("ffmpeg version 9.0.1", self.status)

        # And the document does not let this read as the unblocking step: the
        # only retained contract covers a codec no plan can name.
        self.assertIn("### What this does not do", self.status)
        self.assertIn(
            "`rawvideo` is not a codec this node advertises for", self.flat_status
        )

    def test_m3e_the_diagnostic_wording_is_a_property_of_the_build(self) -> None:
        """A constant here would have turned the effort off on an upgrade.

        `Error submitting packet to decoder` does not exist anywhere in the
        FFmpeg 9 binary. A grammar carrying it matches nothing on that build,
        and a grammar that matches nothing reports every stream as clean —
        the failure this project was created from, reached by a package update.
        """
        health = DECODER_HEALTH.read_text(encoding="utf-8")
        contracts = self.contracts

        self.assertEqual(contracts["version"], 2)
        by_id = {contract["id"]: contract for contract in contracts["contracts"]}
        ffmpeg9 = by_id["ffmpeg-9.0.1-homebrew-h264-v1"]
        self.assertEqual(ffmpeg9["primary_message"], "Decoding error:")
        self.assertNotIn("subordinate_message", ffmpeg9)
        self.assertFalse(ffmpeg9["attributes_every_failure"])

        # The gate is on the action, never the latch. Blocking the latch would
        # cost a burst-failing stream its fault, its barrier and the strongest
        # statement its receipt had to make.
        self.assertIn("windowed_action_qualified: bool,", health)
        action = health.split("pub fn automatic_action_allowed(", 1)[1].split(
            "\n    }", 1
        )[0]
        self.assertIn("self.windowed_action_qualified", action)
        latch = health.split("if self.window.len() >= VIDEO_DECODE_ERROR_LIMIT", 1)[1][
            :200
        ]
        self.assertNotIn("windowed_action_qualified", latch)
        # And the harness applies the same gate, because it is what qualifies a
        # contract before it is retained.
        self.assertIn("and windowed_action_qualified", self.harness)

        # An empty prefix would match every line ever printed, so it cannot be
        # spelled at all.
        self.assertIn("UnusableMatcher(String)", health)

        # The capture is real, hashed, and its host is a workstation kept out
        # of the fleet survey.
        fixture = (
            ROOT / "tests/playback/decoder-health" / ffmpeg9["fixture"]
        ).read_bytes()
        self.assertEqual(
            hashlib.sha256(fixture).hexdigest(), ffmpeg9["fixture_sha256"]
        )
        self.assertIn(b"Decoding error: Invalid data found", fixture)
        # Only the address form is redacted: the other hex values are what the
        # build printed, and rewriting them would misstate the evidence. The
        # header comment names the redacted form, so read the records only.
        records = [
            line
            for line in fixture.decode().splitlines()
            if line.strip() and not line.startswith("#")
        ]
        self.assertTrue(records)
        for record in records:
            with self.subTest(record=record[:60]):
                self.assertNotIn("@ 0x", record)
        self.assertTrue(any("first byte 0x" in record for record in records))
        hosts = tomllib.loads(
            (
                ROOT / "tests/playback/decoder-health/qualifying-hosts-2026-09-07.toml"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(
            {node["name"] for node in hosts["nodes"]}, {"pauls-macbook-pro-2"}
        )
        self.assertIn("not deployed nodes", hosts["scope"])
        # Enforced, not described: the harness refuses a contract whose scope
        # disagrees with the file its host came from.
        self.assertIn('(provenance == "workstation") != ("workstation" in contract.scope)', self.harness)

        # And the document says what is still owed rather than claiming the
        # fleet is covered.
        self.assertIn("What is still owed", self.status)
        self.assertIn("not fleet evidence", self.status)

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

        # The message is matched as a prefix, and it is the *build's* message
        # rather than a constant. A `contains` here is what let a filter-graph
        # failure carrying the contract's own detail text read as a decode
        # failure; a constant is what would have let an FFmpeg upgrade read
        # every broken stream as clean.
        self.assertNotIn("const PRIMARY_MESSAGE:", health)
        self.assertIn("self.contract.primary_message.as_str()", health)
        self.assertIn("pub primary_message: String,", health)
        # And the harness reads it from the contract too, so a capture from one
        # build cannot be replayed under another build's wording.
        self.assertIn("re.escape(contract['primary_message'])", self.harness)
        self.assertNotIn(
            'r"(?P<severity>\\[error\\]\\s+)?Error submitting packet to decoder:"',
            self.harness,
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
