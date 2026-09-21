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
PLAYBACK_CONTROL = ROOT / "crates/plurxd/src/playback_control.rs"
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
M3E_MERGED_HEAD = "c54fb05276c31a5a39e91157f35f1bdb6b049a01"
M3C5_TASK_BASE = M3E_MERGED_HEAD
M3C5_MERGED_HEAD = "5e0f7f1f51d4476fabee81d278c47ccad6baf1a2"
M5A_CENSUS_TASK_BASE = M3C5_MERGED_HEAD
M5A_CENSUS_MERGED_HEAD = "b2dcf458500e210032197b3a3c73abbbe89092b8"
M3F_TASK_BASE = M5A_CENSUS_MERGED_HEAD
M3F_MERGED_HEAD = "02831892f096c047d5300ac0c89cbff2ed7216a3"
M5B_TASK_BASE = M3F_MERGED_HEAD
M5B_MERGED_HEAD = "7f2cb598c6ed89dd24996e06783077f1e29c0678"
M5C1_TASK_BASE = M5B_MERGED_HEAD
M5C1_MERGED_HEAD = "67216de972c1e39f1ef6aefb2541cf542619cb0c"
# The head after M5c3, M7a and the merge of current `main`. Named here rather
# than derived because "the next task base" is a claim the document makes and
# a reader acts on, and a stale one sends a task branch at a tree that no
# longer exists.
MAIN_MERGED_HEAD = "08086371180ae9ae89063ec0946e42bb355a8d60"
PROMOTION_TASK_BASE = "990bf334"
CORE_INVENTORY = ROOT / "crates/plurx-core/src/transcode/decoder_inventory.rs"
CORE_STORE = ROOT / "crates/plurx-core/src/store/mod.rs"
SQLITE_CACHE = ROOT / "crates/plurx-core/src/store/sqlite/cache.rs"
HIQLITE_DURABLE = ROOT / "crates/plurx-core/src/store/hiqlite_durable.rs"
SQLITE_FENCED = ROOT / "crates/plurx-core/src/store/sqlite/publication.rs"
HIQLITE_FENCED = ROOT / "crates/plurx-core/src/store/hiqlite_publication.rs"
STORE_PUBLICATION = ROOT / "crates/plurx-core/src/store/publication.rs"
STORE_CONTRACT = ROOT / "crates/plurx-core/tests/store_contract.rs"
REPLICATED = ROOT / "crates/plurx-core/src/store/replicated.rs"
HIQLITE = ROOT / "crates/plurx-core/src/store/hiqlite.rs"
DV_CONVERSION = ROOT / "crates/plurx-core/src/store/sqlite/dv_conversion.rs"
SQLITE_STORE = ROOT / "crates/plurx-core/src/store/sqlite/mod.rs"
HTTP_SYSTEM = ROOT / "crates/plurxd/src/http/system.rs"
WEB = ROOT / "crates/plurxd/src/web"
# The seven sidecars keep their own routes and are not part of the split shell.
WEB_SIDECARS = frozenset({
    "cluster-panel.js", "playback-policy.js", "playback-control.js", "reader.js",
    "hls.min.js", "live-tv.js", "library-channels.js",
})


def web_body_script() -> str:
    """The split shell's body rows, joined in served order.

    This is what `crates/plurxd/src/web/index.html` used to be: the Developer
    tab's cards are in `pages/settings-developer.js` and the save handlers they
    post through are in `pages/settings-playback.js`, but the assertions below
    include absences, and an absence checked against one file is not an absence.
    See `docs/clients/WEB-SHELL-LAYOUT.md`.
    """
    _, _, body = (WEB / "index.html").read_text(encoding="utf-8").partition("</head>")
    rows = re.findall(r'<script src="/assets/([^"?]+)"></script>', body)
    return "".join(
        (WEB / row).read_text(encoding="utf-8")
        for row in rows
        if row not in WEB_SIDECARS
    )
WEB_SETTINGS_TEST = ROOT / "tests/web/settings-sections.test.js"
DAEMON_MAIN = ROOT / "crates/plurxd/src/main.rs"
HTTP_HLS = ROOT / "crates/plurxd/src/http/hls.rs"
SESSIONS_SQLITE = ROOT / "crates/plurx-core/src/store/sqlite/sessions.rs"
SESSIONS_HIQLITE = ROOT / "crates/plurx-core/src/store/hiqlite_sessions.rs"
M0_QUALIFIED_HEAD = "59d0a4d1"
M0_FORGEJO_PR = "http://forge.lan:3000/noirr/plurx/pulls/62"
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
        self.assertEqual(len(surfaces), 74)
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
        media1 = by_host["media1"]
        self.assertEqual(media1["input_codec"], "rawvideo")
        self.assertEqual(media1["decoder"], "rawvideo")
        self.assertEqual(
            media1["scope"],
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
            {"media1", "lab6", "lab4", "lab3"},
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
            f"Task base | Effort head `{PROMOTION_TASK_BASE}`", self.flat_status
        )
        # Each merged head is named, not only the pull request that carried it.
        self.assertIn(M3C1_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C2_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C3_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C4_MERGED_HEAD[:8], self.status)
        self.assertIn(M3D_MERGED_HEAD[:8], self.status)
        self.assertIn(M3E_MERGED_HEAD[:8], self.status)
        self.assertIn(M3C5_MERGED_HEAD[:8], self.status)
        self.assertIn(M5A_CENSUS_MERGED_HEAD[:8], self.status)
        self.assertIn(M5C1_MERGED_HEAD[:8], self.status)
        self.assertIn(M3F_MERGED_HEAD[:8], self.status)
        self.assertIn(M5B_MERGED_HEAD[:8], self.status)
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
            r"(?m)^pub const CACHE_RECIPE_VERSION: i64 = 4;$",
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
            "M0–M5 complete · M6 server/client implementation landed · M7 "
            "complete · frozen promotion candidate integrated with current `main` · "
            "broader fleet qualification continues after merge",
            self.flat_status,
        )
        self.assertIn("decoder-plan-v1-unqualified", self.status)
        self.assertIn("one replicated cluster-wide kill-switch transaction", self.status)
        self.assertIn("there is no additional task-review loop", self.status)
        self.assertIn(
            "No new compile-time feature gate or hidden runtime enable switch is "
            "introduced by M2.",
            self.flat_status,
        )
        # Task PRs use the effort lane. Promotion remains subject to the
        # repository's exact-tree gate; broader qualification continues after
        # merge rather than delaying M7 task integration.
        self.assertIn(
            "Task PRs into the effort do not run the full unit suite",
            self.flat_status,
        )
        self.assertIn(
            "functionality smoke evidence and a green fast lane before promotion",
            self.flat_status,
        )
        self.assertIn(
            "owner explicitly approved watching the remaining promotion jobs",
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

        # Until M3f nothing selected the qualified identity at all, and this
        # asserted the document said so. M3f adds the selector, so the claim
        # worth pinning is the rule that replaced it: the request is honoured,
        # while only an exactly measured and uniquely covered path changes
        # identity. Missing prerequisites remain visible advice.
        self.assertIn("path-scoped policy", self.status)
        self.assertNotIn("Nothing selects it in production yet", self.status)
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
        self.assertIn(
            "self.measured_decoders.implementation( &codec, "
            "plurx_core::transcode::DecodeBackend::Software, )",
            normalized(naming),
        )
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

    def test_m3c5_every_qualified_path_files_its_receipt_and_no_other_row_moves(
        self,
    ) -> None:
        """A receipt written nowhere certifies nothing tomorrow.

        M3c4 enforced a receipt the manifest carries, and only the queue
        published a manifest — so speculative warming and offline preparation
        settled a clean receipt and were refused for having filed it nowhere.
        This closes that, and the interesting half is what it refuses to
        change: a manifest is what makes a row eligible for shared-cache
        fanout, for cluster placement, for the integrity scrub, and for fatal
        rather than lenient offline segment serving. None of those may move for
        a row that exists on a deployed node today.
        """
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")

        # The publication decision, stated once and gated on the identity.
        self.assertIn(
            "let manifest = if queue_job.is_some() || plan.enforces_receipt() {", daemon
        )

        # Off the queue the generation id is the final directory's own name,
        # which is what makes it stable across the resume passes of one
        # production — and that stability is the digest checkpoint's key.
        self.assertIn("fn identity_for(relative: &str)", daemon)
        self.assertEqual(daemon.count("fn identity_for(relative: &str)"), 1)
        self.assertIn("None => identity_for(&relative)?.to_owned(),", daemon)
        # One derivation, not two spellings of it.
        self.assertNotIn("cache publication has no safe final-directory identity", daemon)

        # Fanout stays queue-owned. Carrying a receipt must not enrol a
        # speculative or offline generation in a second full copy onto a shared
        # mount, in other nodes routing work to that copy, or in cluster quota.
        fanout = daemon.split("if let (Some(shared_cache), Some(manifest), true) = (", 1)[
            1
        ].split(") {", 1)[0]
        self.assertIn("queue_job.is_some()", fanout)

        # The manifest's own bytes are charged wherever one is written, or the
        # cache budget under-counts every generation the other paths made.
        self.assertIn(
            "published.bytes = published.bytes.saturating_add(", daemon
        )

        # The digest reaches durable storage through both arms of the
        # publication wrapper. A parameter added only to the fenced one would
        # be silently dropped for every unfenced caller, which is most of them.
        publication = STORE_PUBLICATION.read_text(encoding="utf-8")
        self.assertIn(
            ".complete_cache_entry(recipe_hash, node_id, bytes, manifest_digest)",
            publication,
        )

        # `None` never blanks a digest an earlier write recorded. Four
        # implementations answer this — sqlite and hiqlite, each fenced and
        # unfenced — and a `COALESCE` dropped from any one of them loses a
        # qualified artifact's identity with no error anywhere.
        for path in (SQLITE_CACHE, HIQLITE_DURABLE, SQLITE_FENCED, HIQLITE_FENCED):
            with self.subTest(backend=f"{path.parent.name}/{path.name}"):
                self.assertIn(
                    "manifest_digest = COALESCE(", path.read_text(encoding="utf-8")
                )
        # And the behaviour itself is proved through `dyn Store` on every
        # backend, rather than inferred from the SQL text above.
        self.assertIn(
            "async fn a_cache_completion_without_a_digest_never_clears_the_one_on_the_row",
            STORE_CONTRACT.read_text(encoding="utf-8"),
        )

        # Every call site's digest argument, as a whole multiset. Counting
        # only the `None`s would move in one direction and not the other: a new
        # caller that starts writing a digest adds a non-`None` element and
        # leaves that count untouched, which is the change this most needs to
        # catch.
        self.assertEqual(
            sorted(self._manifest_digest_arguments()),
            sorted(
                ["None"] * 27
                + ["digest", "digest", "manifest_digest", "manifest_digest"]
                + ['Some("d1")', 'Some("d2")', "written"]
            ),
        )
        # The two production writers are the off-queue completion arms, and
        # both take their value from `digest` — which is `None` unless the
        # identity enforces a receipt. Reading it off the manifest instead
        # would work today and would put the safety in an invariant four
        # hundred lines away: `queue_job` is `Some` exactly when
        # `pretranscode_fence` is, so a queue job can never reach that arm.
        self.assertIn("let digest = plan\n                .enforces_receipt()", daemon)
        self.assertIn(
            "a queue completion must settle through its own fence", daemon
        )

        # The trait says what the two states mean, so a future implementer does
        # not default the parameter.
        store = CORE_STORE.read_text(encoding="utf-8")
        self.assertIn(
            "`manifest_digest` is `None` for a generation published without one",
            store,
        )

        # And the test that discriminates is the unqualified arm, because on a
        # host no contract covers both arms refuse.
        self.assertIn(
            "fn a_manifest_is_written_off_the_queue_only_where_a_receipt_is_enforced",
            daemon,
        )
        self.assertIn(
            "an unqualified row records no digest, so nothing that keys on one changes",
            daemon,
        )
        # The qualified arm keys on a counter, not on the outcome: the refusal
        # quarantines the generation and takes the manifest with it, so every
        # on-disk observation afterwards is identical to the one a build
        # without this milestone would leave.
        self.assertIn(
            "the qualified identity publishes a manifest off the queue", daemon
        )
        self.assertIn(
            "the unqualified identity publishes none off the queue", daemon
        )
        self.assertIn("fn test_manifests_published(&self) -> usize", daemon)
        self.assertIn(
            "fn the_generation_identity_is_the_final_directory_and_survives_a_fence",
            daemon,
        )
        # A derived generation id is checked where it is derived. Publication
        # rejects a bad one too, but as an ordinary retryable production error
        # — so a name that can never be accepted would retry the title on a
        # backoff forever and report it as an encoder fault.
        self.assertIn(
            "plurx_core::transcode::manifest::is_safe_generation_id(identity)", daemon
        )
        self.assertIn(
            "pub fn is_safe_generation_id(generation_id: &str) -> bool",
            CORE_MANIFEST.read_text(encoding="utf-8"),
        )

        # Both background lanes owe the idle courtesy — the queue and
        # speculative warming exist to be interrupted by people pressing play.
        # An offline package does not: a user is waiting on that download, and
        # the predicate is false while one is waiting, so it would yield on
        # every check and the package would never become ready.
        self.assertIn("let owes_idle_courtesy = offline_package_id.is_none();", daemon)
        self.assertIn(
            "|| (owes_idle_courtesy && !self.pretranscode_worker_idle())", daemon
        )

        # The stale claim M3c4 made about where the operator control lands is
        # corrected in place rather than left to mislead.
        self.assertIn("*Amended after M3c5.*", self.status)
        self.assertIn("The control is M3f, immediately after", self.status)

    @staticmethod
    def _manifest_digest_arguments() -> list[str]:
        """The digest argument at every `complete_cache_entry` call site."""

        def split_args(text: str) -> list[str]:
            parts: list[str] = []
            depth = 0
            current = ""
            for character in text:
                if character in "([{":
                    depth += 1
                elif character in ")]}":
                    depth -= 1
                if character == "," and depth == 0:
                    parts.append(current.strip())
                    current = ""
                else:
                    current += character
            if current.strip():
                parts.append(current.strip())
            return parts

        found: list[str] = []
        for source in sorted((ROOT / "crates").rglob("*.rs")):
            text = source.read_text(encoding="utf-8")
            for match in re.finditer(
                r"\.complete_cache_entry(_fenced)?\s*\(", text
            ):
                index = match.end() - 1
                depth = 0
                while index < len(text):
                    if text[index] == "(":
                        depth += 1
                    elif text[index] == ")":
                        depth -= 1
                        if depth == 0:
                            break
                    index += 1
                arguments = split_args(text[match.end() : index])
                # The fenced method takes `relative_dir` before `bytes`; the
                # publication wrapper takes it too, so position is read off the
                # argument count rather than the method name.
                position = 4 if len(arguments) > 4 else 3
                found.append(
                    arguments[position] if len(arguments) > position else "??"
                )
        return found

    def test_m5a_census_repair_answers_every_gate_and_says_what_it_cannot(self) -> None:
        """Five gates, and the two things that still have no gate at all.

        M5a shipped a correct table and answered none of the five assertions
        that exist to notice one. Repairing them is arithmetic; the part worth
        pinning is what the repair refuses to claim — that the practice which
        found them is now enforced, and that the second downgrade fixture is
        guarded by anything but a sentence in another test's failure message.
        """
        replicated = REPLICATED.read_text(encoding="utf-8")
        hiqlite = HIQLITE.read_text(encoding="utf-8")
        dv_conversion = DV_CONVERSION.read_text(encoding="utf-8")
        sqlite_store = SQLITE_STORE.read_text(encoding="utf-8")
        store_contract = STORE_CONTRACT.read_text(encoding="utf-8")

        # Both new boundaries are classified, and the third store method is
        # not — because it opens none, which the source says at the site.
        for method in ("reserve_producer_recovery", "settle_producer_recovery"):
            with self.subTest(method=method):
                self.assertIn(f'method: "{method}"', replicated)
        self.assertNotIn('method: "producer_recovery_for_epoch"', replicated)
        self.assertIn(
            "// A read, so no transaction: the connection mutex already",
            (ROOT / "crates/plurx-core/src/store/sqlite/sessions.rs").read_text(
                encoding="utf-8"
            ),
        )
        self.assertEqual(replicated.count("TransactionShape::WriteReadBack"), 2)
        # 97 includes seven recording-event boundaries, subject_write, and two
        # content-analysis repair boundaries added to the previous 87. Each is
        # explicitly classified in replicated.rs; its census assertion remains
        # a deliberate count rather than a value derived from the same table.
        self.assertIn("assert_eq!(methods.len(), 97);", replicated)

        # The shape records a real difference between the backends rather than
        # a promise about future work: the replicated twin cannot hold it, and
        # M5a already said why at its own call site.
        self.assertIn("The replicated twin cannot hold this shape", replicated)
        self.assertIn(
            "txn` cannot carry the read",
            (ROOT / "crates/plurx-core/src/store/hiqlite_sessions.rs").read_text(
                encoding="utf-8"
            ),
        )

        # The migration count, and the additive chain with the paired step
        # assertion every earlier version already has.
        # Shape, not value. These numbers belong to whichever migration
        # shipped last, and pinning them here made every later milestone edit
        # a test named after M5a — which is how the M5a census defect happened
        # in the first place. What this gate owns is that the assertions exist.
        self.assertRegex(sqlite_store, r"version, \d+,")
        self.assertRegex(hiqlite, r"AUTH_SCHEMA_MIGRATION_SOURCE \+ \d+,")
        self.assertRegex(hiqlite, r"every additive v5→v\d+ step")
        self.assertRegex(
            # The recovery pair now sits behind main's drain-deadline step.
            # What is pinned is that the final chain remains contiguous.
            hiqlite,
            r"PRODUCER_RECOVERY_SCHEMA_MIGRATION_SOURCE: i64 =\s+DRAIN_DEADLINE_SCHEMA_VERSION",
        )

        # Both hand-written downgrade fixtures give the v48 table back.
        self.assertRegex(dv_conversion, r"DROPPED_BY_THE_FIXTURE: \[&str; \d+\]")
        self.assertIn("media_session_producer_recovery", dv_conversion)
        self.assertIn(
            "DROP TABLE media_session_producer_recovery", store_contract
        )
        # A table drop is silent if forgotten, because the migration is
        # `CREATE TABLE IF NOT EXISTS`. Saying the replay would catch it is the
        # comment that lets the next author skip the count.
        self.assertIn("Leaving a *table* behind is silent", dv_conversion)

        # What the repair does not claim.
        self.assertIn("That is a practice, not a gate", self.status)
        self.assertIn(
            "Run the full `cargo test --workspace` coverage after promotion",
            self.status,
        )
        self.assertIn(
            "Give `populated_v14_import_fixture` in `tests/store_contract.rs` "
            "arithmetic of its own",
            self.flat_status,
        )
        self.assertIn("five gates M5a tripped and nobody read", self.status)
        self.assertNotIn("four gates M5a tripped", self.status)

    def test_m3f_the_control_is_path_scoped_and_prerequisites_are_advisory(self) -> None:
        """A covered path rotates identity without gating the operator request.

        The identity this control selects is a content-addressed key space.
        Enabling the policy rotates only exact paths with one covering
        contract; uncovered or ambiguous paths retain their current identity.
        The surface therefore reports the enabled policy, the conservative
        legacy whole-node fact, and the path coverage separately.
        """
        daemon = DAEMON_TRANSCODE.read_text(encoding="utf-8")
        system = HTTP_SYSTEM.read_text(encoding="utf-8")
        store = CORE_STORE.read_text(encoding="utf-8")
        web = web_body_script()
        main = DAEMON_MAIN.read_text(encoding="utf-8")

        self.assertIn(
            '"playback.decoder_health_qualified_artifacts"', store
        )
        # The rule takes immutable boot facts and selectable paths, so it is
        # testable without a manager, a store, a cache or an FFmpeg.
        self.assertIn(
            "pub fn artifact_qualification_readiness(\n    requested: bool,", daemon
        )
        # Each prerequisite is its own refusal. A single "not eligible" would
        # be a refusal an operator cannot act on, which is the same as none.
        for refusal in (
            "NotRequested",
            "BuildUnmeasured",
            "NoDecoderMeasured",
            "NoContractCoversThisBuild",
            "IncompleteCoverage",
            # Two covering contracts read downstream exactly like none, and the
            # fix for one is the opposite of the fix for the other.
            "AmbiguousContract",
            # A store that cannot be read keeps the identity every deployed
            # node has, rather than failing the boot of a media server over an
            # off-by-default preview control.
            "SettingUnreadable",
        ):
            with self.subTest(refusal=refusal):
                self.assertIn(f"QualificationRefusal::{refusal}", daemon)
        self.assertIn("pub fn explanation(self) -> &'static str", daemon)

        # Coverage is checked as the node will run it: the decoder the probe
        # measured, under the qualified log flags. A contract qualified under
        # different flags describes a different log.
        self.assertIn(
            "crate::decoder_health::QUALIFIED_STDERR_MODE", daemon
        )

        # Published once, at start, and nowhere else. The value can change the
        # key of each covered path, so applying it to a live node would move
        # affected key spaces under work already running — a session
        # publishing where the next lookup will not look, a resumable
        # production restarting from parts that carry no receipt.
        self.assertIn("state.transcode.publish_artifact_qualification().await;", main)
        self.assertNotIn("publish_artifact_qualification", system)
        self.assertEqual(daemon.count("pub async fn publish_artifact_qualification"), 1)
        # And a store that cannot be read does not take the daemon down.
        self.assertNotIn(
            "state.transcode.publish_artifact_qualification().await?;", main
        )

        # The surface reports what the node published, never a recomputation:
        # a recomputation would say "enforcing" on a node planning unqualified.
        self.assertIn("pub fn published_artifact_qualification(", daemon)
        self.assertIn("state.transcode.published_artifact_qualification()", system)

        # The policy is handed to the manager rather than reached for, which is
        # what makes the publisher testable at all.
        self.assertIn("pub fn with_diagnostic_policy(", daemon)
        self.assertIn(
            "fn the_published_identity_is_the_request_this_node_can_honour_and_moves_its_cache_keys",
            daemon,
        )
        # And the direct setter stays test-only: every test of the enforcement
        # behind this control runs on a host no contract covers.
        self.assertIn(
            "#[cfg(test)]\n    pub(crate) fn test_publish_artifact_qualification(", daemon
        )

        # The stored request, conservative legacy state and exact coverage are
        # separate facts. Collapsing them would either hide an enabled policy
        # or claim every transcode on the node is verified.
        self.assertIn("pub decoder_health_qualified_artifacts: bool,", system)
        self.assertIn(
            "pub decoder_health_qualification: DecoderHealthQualification,", system
        )
        self.assertIn("pub struct DecoderHealthQualification", system)
        for field in (
            "namespace",
            "enforcing",
            "policy_enabled",
            "requested_namespace",
            "path_scoped",
            "eligible",
            "measured_decoders",
            "covered_decoders",
            "measured_paths_v2",
            "covered_paths_v2",
            "refusal",
            "explanation",
            # The third fact. Without it the surface either hides a saved
            # change or claims one that has not happened.
            "pending_restart",
        ):
            with self.subTest(field=field):
                self.assertIn(f"pub {field}:", system)

        # The enable section states the cost before the switch, shows what this
        # node measured, and saves its own single field.
        self.assertIn("function verifiedDecodeCard(", web)
        self.assertIn("verifiedDecodeCard(settings)", web)
        # Conditional, because an uncovered path pays no cache rename yet.
        # The prerequisites advise; they never disable the operator control.
        self.assertIn("You may still enable the policy", web)
        self.assertIn("advisory and never disable this control", web)
        self.assertIn("renames cached transcodes that use those paths", web)
        self.assertIn("paths pay the same rename a second time", web)
        self.assertNotIn("every cache key the node computes", web)
        # FFmpeg's banner and decoder names are somebody else's strings.
        self.assertIn("esc(q.measured_build)", web)
        # Saving replaces this card, never the panel: the Live TV card beside
        # it stages a whole configuration before its own Save.
        self.assertIn('document.getElementById("vdcard")', web)
        self.assertNotIn("#/settings/developer\") render()", web)
        self.assertIn("drop every frame of a file and still exit successfully", web)
        self.assertIn("What this node measured", web)

        # The recovery's own enable section. The effort was required to explain
        # what safe enablement depends on, and the honest answer today opens
        # with the prerequisite that is not met — so the section is checked for
        # saying that, not merely for existing.
        self.assertIn("function decodeRecoveryCard(", web)
        self.assertIn("decodeRecoveryCard(settings)", web)
        self.assertIn("Enable automatic decoder recovery", web)
        self.assertIn("This checkbox is the enable path", web)
        self.assertIn("missing measurements or retained contracts never turn it back off", web)
        self.assertIn("One recovery per playback, and it is never given back", web)
        self.assertIn("hardware and software paths", web)
        # Saving replaces its own card, like the one above it.
        self.assertIn("drcard", web)
        # And the status document carries the finding rather than only the card.
        self.assertIn(
            "M7a — direct recovery enablement with advisory readiness",
            self.status,
        )
        # M7b is the milestone that closes M7a's finding, and the one claim in
        # its specification that a later reader must not lose is the measured
        # one: the accelerated and the software decode of a codec print the
        # same decoder name, so the contract cannot be keyed on that name.
        self.assertIn(
            "M7b specification — the hardware decoder contract, measured before it is written",
            self.status,
        )
        self.assertIn("**The lines are identical.**", self.status)
        self.assertIn(
            "Selecting decoder 'h264' because of requested hwaccel method videotoolbox",
            self.status,
        )
        self.assertIn("This is toolchain evidence, not\nfleet evidence", self.status)
        inventory = CORE_INVENTORY.read_text(encoding="utf-8")
        self.assertIn("pub fn selected_hardware_decoder(", inventory)
        self.assertIn(
            "Selecting decoder '<name>' because of requested hwaccel method <backend>",
            inventory,
        )
        self.assertIn('"-hwaccel_output_format",', inventory)
        self.assertIn("hardware_runtime_evidence(stderr, backend)", inventory)
        self.assertIn("const INVENTORY_TIMEOUT: std::time::Duration", inventory)
        self.assertIn("by_codec_and_backend_v2", inventory)
        self.assertIn(
            "selected_hardware_decoder(&stderr, codec, backend, output.status.success())",
            inventory,
        )
        observation = daemon.split("fn for_plan_against(", 1)[1].split(
            "fn resolve(", 1
        )[0]
        self.assertIn(
            "measured.implementation(codec, plan.decode().backend())", observation
        )
        readiness = daemon.split("pub fn artifact_qualification_readiness(", 1)[
            1
        ].split("impl TranscodeManager", 1)[0]
        self.assertIn("backend.name()", readiness)
        self.assertNotIn("DecodeBackend::Software.name()", readiness)
        self.assertIn(
            'format!("{codec}/{}/{decoder}", backend.name())', system
        )
        self.assertIn("measured_paths_v2", system)
        self.assertIn("covered_paths_v2", system)
        self.assertIn("q.measured_paths_v2||q.measured_decoders", web)
        self.assertIn("q.covered_paths_v2||q.covered_decoders", web)
        self.assertIn(
            "const covered=q.covered_paths_v2||q.covered_decoders||[];", web
        )
        self.assertIn(
            "const policyEnabled=q.policy_enabled===undefined?!!q.enforcing:!!q.policy_enabled;",
            web,
        )
        self.assertIn("const enabled=!!s.automatic_decoder_recovery;", web)
        self.assertIn("saveAutomaticDecoderRecovery", web)
        self.assertIn("automatic_decoder_recovery:document.getElementById", web)
        self.assertIn("The checks below are advisory only", web)
        self.assertIn("Recovery remains available when explicitly enabled", web)
        self.assertIn("Only some selectable decode paths", daemon)
        self.assertIn("missing measurements or contracts", daemon)
        self.assertNotIn(
            "!delivered.enforces_receipt() || !alternate.enforces_receipt()",
            daemon,
        )
        self.assertIn("async function saveVerifiedDecode(", web)
        self.assertIn(
            "decoder_health_qualified_artifacts:document.getElementById"
            '("dhqa").checked',
            web,
        )
        self.assertIn(
            "Verified decode states its cost, its prerequisites, and what this "
            "node measured",
            WEB_SETTINGS_TEST.read_text(encoding="utf-8"),
        )
        self.assertIn(
            "Automatic recovery is directly enabled and coverage remains advisory",
            WEB_SETTINGS_TEST.read_text(encoding="utf-8"),
        )

        # And the document says what is still owed, which is the reason every
        # fleet node will be refused on day one.
        self.assertIn("no_contract_covers_this_build", self.status)

    def test_m5c1_the_decode_evidence_names_only_the_answer_a_client_gets(
        self,
    ) -> None:
        """The seam that ends the reopen loop, and the three bounds on it.

        A source the decoder cannot decode answers a retry with the identical
        failure, and every reason outside `is_permanent` tells a client to
        retry. M5c1 lets a contract-qualified decode fault rewrite that reason.
        Everything asserted here is a way that rewrite could reach a decision
        it has no business naming: a retry the client can see, a producer that
        demonstrably decoded, or an outage on this node.
        """
        control = PLAYBACK_CONTROL.read_text(encoding="utf-8")
        health = DECODER_HEALTH.read_text(encoding="utf-8")

        # The reason exists, is permanent, and says so in prose rather than
        # handing the client a raw status token.
        self.assertIn("SourceDecodeFailed,", control)
        self.assertIn('Self::SourceDecodeFailed => "source_decode_failed",', control)
        # Four permanent reasons after main merged in, so the match is no
        # longer one line. What this pins is that the decode verdict is one
        # of them, which is the fact M5c1 exists to establish.
        self.assertIn("pub(crate) fn is_permanent(self) -> bool {", control)
        self.assertIn("| Self::SourceDecodeFailed", control)
        self.assertIn(
            "this source did not decode, and trying again will not change that",
            control,
        )

        # The gate is a named list, not a negation. `!is_permanent()` also
        # swept up ExecutorLost and the flow and install deadlines, which are
        # facts about this node rather than about the file.
        self.assertIn("fn yields_to_decode_evidence(self) -> bool", control)
        self.assertIn(
            "if !producer_media_published && reason.yields_to_decode_evidence() {",
            control,
        )

        # And it is asked on the failing branch only. The reason on a Retry
        # reaches DeliveryView, and resolve_action turns any permanent reason
        # into a terminal answer, so a rewritten Retry ends the session while
        # the node is still bringing up a fallback that would have played it.
        for name in (
            "fn a_qualified_decode_fault_replaces_the_process_verdict_a_client_would_retry",
            "fn an_unqualified_decode_fault_leaves_the_process_verdict_alone",
            "fn a_qualified_decode_fault_does_not_overwrite_a_verdict_that_already_knows_more",
            "fn a_retry_is_never_handed_a_permanent_reason_while_the_fallback_is_still_coming",
            "fn a_fault_on_a_producer_that_published_is_recorded_and_not_acted_on",
            "fn a_server_side_loss_is_never_relabelled_as_a_verdict_about_the_source",
        ):
            with self.subTest(regression=name):
                self.assertIn(name, control)

        # The latch asks the same question as the settle. A weaker method for
        # the latch was written and deleted; the reasoning stays where the
        # next person will read it.
        self.assertNotIn("latched_action_qualified", health)
        self.assertIn("accumulator.automatic_action_allowed(),", health)
        self.assertIn("Asked at the latch as well as at the settle", health)

        # The status document records that the specification's three
        # candidates were not chosen between, and what M5c2 still owes.
        self.assertIn(
            "M5c1, merged: the ordering question is answered, and by a fourth option",
            self.status,
        )
        self.assertIn("5, 6, 7 and 9 are all still open.", self.status)

    def test_m5b_a_budget_is_minted_by_a_new_play_and_inherited_by_everything_else(
        self,
    ) -> None:
        """The ledger M5a shipped had a key nobody could present.

        `media_session_producer_recovery` is keyed by the epoch, so until a
        session carries one the budget is unaddressable — which is why no
        daemon code called it. This is the key. Everything asserted here is a
        way one playback could end up with two budgets, which is one automatic
        retry per attempt instead of per playback: an unbounded loop against a
        decoder that will never succeed.
        """
        hls = HTTP_HLS.read_text(encoding="utf-8")
        store = CORE_STORE.read_text(encoding="utf-8")
        sqlite_sessions = SESSIONS_SQLITE.read_text(encoding="utf-8")
        hiqlite_sessions = SESSIONS_HIQLITE.read_text(encoding="utf-8")

        # The column, spelled once and shared by both backends.
        self.assertIn("MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA", store)
        self.assertIn("ADD COLUMN recovery_epoch TEXT NOT NULL DEFAULT ''", store)
        self.assertIn(
            "super::MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA",
            SQLITE_STORE.read_text(encoding="utf-8"),
        )
        self.assertIn(
            # v32 after the final main freeze: main's drain deadline owns v31,
            # so the recovery ledger and epoch follow it.
            "RECOVERY_EPOCH_SCHEMA_MIGRATION_SOURCE, 32,",
            HIQLITE.read_text(encoding="utf-8"),
        )

        # The decision, in one named place rather than inline at the seam.
        self.assertIn(
            "fn recovery_epoch_for(predecessor: Option<&MediaSessionRoute>) -> String",
            hls,
        )
        # Minted exactly once per start, and read from that one binding by
        # both the session identity and the durable activation.
        #
        # M5b pinned the opposite — read at the point of use, never a local —
        # on the reasoning that the store decides the row's epoch and a local
        # copy could be believed after the store had ruled otherwise. That
        # reasoning is about a *read* and it still holds; it was never about a
        # mint. M5c3 needed the epoch before placement, because the session
        # that carries it is built by the task placement spawns, and calling
        # the function twice mints twice: `recovery_epoch_for` draws a fresh
        # UUID when there is no predecessor, which is the whole point of it.
        # A new play would have given the live session one budget and the
        # durable row another.
        production, _, _ = hls.partition("\nmod tests {")
        self.assertEqual(
            production.count("recovery_epoch_for(activation_predecessor.as_ref())"),
            1,
            "exactly one mint per start; a second call mints a second budget",
        )
        self.assertIn(
            "let recovery_epoch = recovery_epoch_for(activation_predecessor.as_ref());",
            production,
        )
        self.assertEqual(
            production.count("recovery_epoch: recovery_epoch.clone(),"),
            2,
            "the session identity and the durable activation, and nothing else",
        )
        self.assertIn(
            "fn one_start_mints_one_recovery_epoch_for_both_the_session_and_the_row",
            hls,
        )
        # And the claim about what the epoch buys is the one that holds.
        self.assertIn("It does not\n/// bound a client that varies", hls)
        self.assertIn(
            "fn a_new_play_mints_a_budget_and_every_continuation_inherits_one", hls
        )

        # A successor reads its predecessor's epoch rather than being handed
        # one, on both backends, so no caller can get it wrong.
        for name, source in (
            ("sqlite", sqlite_sessions),
            ("hiqlite", hiqlite_sessions),
        ):
            with self.subTest(backend=name):
                self.assertIn(
                    "COALESCE((SELECT recovery_epoch FROM media_sessions", source
                )
                # A replay refreshes the lease and the response and never the
                # epoch: re-minting on replay is two budgets for one playback.
                self.assertIn("The epoch is written once, with the row", source)

        # Proved through `dyn Store` on every backend rather than inferred
        # from the SQL text above.
        self.assertIn(
            "async fn a_recovery_epoch_is_written_once_and_inherited_by_a_successor",
            STORE_CONTRACT.read_text(encoding="utf-8"),
        )

        # Nothing reads it yet, and the document says so rather than implying
        # the recovery path works.
        self.assertIn("Nothing reads the epoch yet", self.status)
        # And M5c's specification records the ordering discovery that decides
        # where the recovery can honestly be committed: the receipt is settled
        # after the process terminal is published, so an exit-time decision
        # could only act on a fault whose qualification it has not established.
        self.assertIn("the diagnostics-complete barrier is the decision point", self.status)
        self.assertIn(
            "a process can exit long before its stderr reaches EOF",
            self.flat_status,
        )
        # And the second finding, which came from building the first answer
        # and watching it lose the race: the deadline decision commits before
        # the receipt settles, so moving to the barrier is necessary and not
        # sufficient. Three candidate resolutions are named; none is assumed.
        self.assertIn("The second ordering finding", self.status)
        self.assertIn("was reverted rather than merged", self.status)
        # The case no in-session decision can help is stated rather than
        # quietly folded into the ones that can.
        self.assertIn("The live session is not recoverable", self.status)
        # The claim about what the epoch buys is the corrected one: it does not
        # bound a client that varies `playback_id`, because the ledger key
        # contains `playback_id`.
        self.assertIn("It does **not** bound a client that varies", self.status)
        # The old claim survives only where the review ledger quotes it as the
        # thing that was wrong, which is the opposite of asserting it.
        self.assertIn(
            "would mint itself an unlimited supply of budgets\" was false",
            self.flat_status,
        )
        # A takeover inherits by construction — there is no `INSERT INTO
        # media_sessions` in any owner-transition path — and the reaped-pointer
        # gap is stated rather than left to be discovered.
        self.assertIn("A takeover inherits by construction", self.status)
        self.assertIn("The gap is the reaped pointer, and it is real", self.status)
        self.assertIn("Empty means three different things", self.status)

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
