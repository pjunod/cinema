"""Source-offer identity and historical lineage fences; no native build launched."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("dependency_offers", ROOT / "scripts/macos-video-dependency-offers.py")
TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOL)


class MacosVideoDependencyOffersCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.inputs = self.root / "dependency-source-inputs"
        self.inputs.mkdir()
        self.policy = self.root / "policy.json"
        self.archive = self.inputs / "demo-pinned.tar.gz"
        source = self.root / "original.c"
        source.write_text("original source\n")
        with tarfile.open(self.archive, "w:gz") as archive:
            archive.add(source, arcname="original.c")
        self.facts = {"demo": {"commit": "pinned", "kind": "git-source-archive", "sha256": TOOL.digest(self.archive)}}
        self.policy.write_text(json.dumps(self.facts))
        (self.inputs / "manifest.json").write_text(json.dumps(self.facts))
        self.override = patch.object(TOOL, "POLICY", self.policy)
        self.override.start()
        self.addCleanup(self.override.stop)
        self.recipe = "builder/scripts.d/50-demo.sh"
        path = self.root / "source" / self.recipe
        path.parent.mkdir(parents=True)
        path.write_text('archive-source --recipe builder/scripts.d/50-demo.sh demo pinned demo\n')
        self.bindings = {self.recipe: {"roles": ["demo"], "sha256": TOOL.digest(path)}}
        (self.root / "dependency-recipe-bindings.json").write_text(json.dumps(self.bindings))
        self.preparation = {"dependency_source_inputs": {
            "manifest_sha256": TOOL.digest(self.inputs / "manifest.json"),
            "recipe_bindings_sha256": TOOL.digest(self.root / "dependency-recipe-bindings.json"),
            "helper_sha256": TOOL.digest(Path(TOOL.__file__)), "policy_sha256": TOOL.digest(self.policy)}}
        (self.root / "prepared-source.json").write_text(json.dumps(self.preparation))

    def test_missing_tampered_or_wrong_role_sources_are_rejected(self):
        self.assertEqual(TOOL.verify_offers(self.inputs), self.facts)
        self.archive.write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "archive mismatch"):
            TOOL.verify_offers(self.inputs)
        self.archive.unlink()
        with self.assertRaisesRegex(ValueError, "archive mismatch"):
            TOOL.verify_offers(self.inputs)
        (self.inputs / "manifest.json").write_text('{}')
        with self.assertRaises(ValueError):
            TOOL.verify_offers(self.inputs)

    def test_consumption_binds_role_recipe_revision_helper_and_input_manifest(self):
        TOOL.extract_source(self.root, "demo", "pinned", self.root / "demo", self.recipe)
        self.assertEqual(TOOL.verify_consumption(self.root), self.facts)
        with self.assertRaisesRegex(ValueError, "recipe revision"):
            TOOL.extract_source(self.root, "demo", "wrong", self.root / "wrong", self.recipe)
        with self.assertRaisesRegex(ValueError, "bound"):
            TOOL.verify_recipe(self.root, "builder/other.sh", "demo")
        receipt = self.root / "dependency-source-consumption.jsonl"
        row = json.loads(receipt.read_text())
        row["extraction_helper_sha256"] = "stale"
        receipt.write_text(json.dumps(row) + "\n")
        with self.assertRaisesRegex(ValueError, "helper identity"):
            TOOL.verify_consumption(self.root)
        (self.inputs / "manifest.json").write_text('{}')
        with self.assertRaisesRegex(ValueError, "prepared dependency input"):
            TOOL.verify_consumption(self.root)

    def test_historical_mode_cannot_bless_unexplained_original_source_changes(self):
        target = self.root / "source/builder/build/demo"
        target.mkdir(parents=True)
        (target / "original.c").write_text("original source\n")
        self.assertEqual(TOOL.audit_historical_sources(self.root, self.inputs, "archive-source"), [])
        (target / "original.c").write_text("changed source\n")
        with self.assertRaisesRegex(ValueError, "unexplained historical"):
            TOOL.audit_historical_sources(self.root, self.inputs, "archive-source")

    def test_historical_lineage_requires_actual_hash_bound_logs_and_helper(self):
        lineage = self.root / "lineage.json"
        lineage.write_text(json.dumps({"mode": "historical-archive-build", "offers": str(self.inputs), "executed_files": []}))
        evidence = self.root / "evidence"
        evidence.mkdir()
        with self.assertRaisesRegex(ValueError, "actual logs"):
            TOOL.retain_historical_build(self.root, evidence, lineage)

    def test_logged_generation_cannot_authorize_arbitrary_generated_source(self):
        archive = self.inputs / "lame-pinned.tar.gz"
        original = self.root / "configure"
        original.write_text("original generated configure\n")
        with tarfile.open(archive, "w:gz") as output:
            output.add(original, arcname="configure")
        facts = {"lame": {"commit": "pinned", "kind": "git-source-archive", "sha256": TOOL.digest(archive)}}
        self.policy.write_text(json.dumps(facts))
        (self.inputs / "manifest.json").write_text(json.dumps(facts))
        target = self.root / "source/builder/build/lame"
        target.mkdir(parents=True)
        (target / "configure").write_text("arbitrary changed generated code\n")
        with self.assertRaisesRegex(ValueError, "exact audited transformation"):
            TOOL.audit_historical_sources(self.root, self.inputs, "autoreconf -fi")

    def test_reviewed_lame_alternate_is_bound_to_role_revision_and_canonical_identity(self):
        approved = TOOL.SOURCE_ALTERNATIVES["lame"]
        self.assertTrue(TOOL.source_identity_matches("lame", approved["alternate"], approved["canonical"]))
        for field, value in [("revision", "6532"), ("kind", "git-source-archive"), ("sha256", "unreviewed")]:
            altered = {**approved["alternate"], field: value}
            self.assertFalse(TOOL.source_identity_matches("lame", altered, approved["canonical"]))
        self.assertFalse(TOOL.source_identity_matches("other", approved["alternate"], approved["canonical"]))
        self.assertFalse(TOOL.source_identity_matches("lame", approved["alternate"], {**approved["canonical"], "revision": "other"}))

    def test_reviewed_retained_helper_is_verified_without_relabelling_consumption(self):
        TOOL.extract_source(self.root, "demo", "pinned", self.root / "demo", self.recipe)
        retained = self.root / "extraction-helpers/macos-video-dependency-offers.py"
        retained.parent.mkdir()
        retained.write_text("reviewed earlier extractor\n")
        identity = TOOL.digest(retained)
        self.preparation["dependency_source_inputs"]["helper_sha256"] = identity
        (self.root / "prepared-source.json").write_text(json.dumps(self.preparation))
        receipt = self.root / "dependency-source-consumption.jsonl"
        row = json.loads(receipt.read_text())
        row["extraction_helper_sha256"] = identity
        receipt.write_text(json.dumps(row) + "\n")
        original_receipt = receipt.read_bytes()
        with patch.object(TOOL, "REVIEWED_EXTRACTION_HELPERS", {identity}):
            self.assertEqual(TOOL.verify_consumption(self.root), self.facts)
            self.assertEqual(receipt.read_bytes(), original_receipt)
            with self.assertRaisesRegex(ValueError, "prepared dependency input"):
                TOOL.verify_preparation(self.root)
            retained.write_text("tampered extractor\n")
            with self.assertRaisesRegex(ValueError, "prepared dependency input"):
                TOOL.verify_consumption(self.root)
        with self.assertRaisesRegex(ValueError, "unreviewed retained"):
            TOOL.verify_consumption(self.root)

    def test_retained_policy_requires_approved_identities_even_if_receipt_is_rehashed(self):
        TOOL.extract_source(self.root, "demo", "pinned", self.root / "demo", self.recipe)
        retained = self.root / "extraction-helpers/macos-video-dependency-sources.json"
        retained.parent.mkdir()
        retained.write_text(json.dumps(self.facts, indent=4))
        self.preparation["dependency_source_inputs"]["policy_sha256"] = TOOL.digest(retained)
        (self.root / "prepared-source.json").write_text(json.dumps(self.preparation))
        self.assertEqual(TOOL.verify_consumption(self.root), self.facts)
        retained.write_text(json.dumps({"demo": {**self.facts["demo"], "commit": "unreviewed"}}))
        self.preparation["dependency_source_inputs"]["policy_sha256"] = TOOL.digest(retained)
        (self.root / "prepared-source.json").write_text(json.dumps(self.preparation))
        with self.assertRaisesRegex(ValueError, "policy is not approved"):
            TOOL.verify_consumption(self.root)

    def test_external_support_link_requires_exact_member_of_pinned_tool_source(self):
        tools = self.root / "tools"
        (tools / "archives").mkdir(parents=True)
        installed = self.root / "installed/install-sh"
        installed.parent.mkdir()
        installed.write_text("approved support source\n")
        tool_archive = tools / "archives/automake.tar.gz"
        with tarfile.open(tool_archive, "w:gz") as output:
            output.add(installed, arcname="automake-pinned/lib/install-sh")
        target = self.root / "source/install-sh"
        target.symlink_to(installed)
        evidence = self.root / "evidence"
        evidence.mkdir()
        with patch.object(TOOL, "GENERATION_TOOL_ARCHIVES", {"automake": {TOOL.digest(tool_archive)}}):
            TOOL.retain_compiled_snapshot(self.root, evidence, tools)
            with tarfile.open(evidence / "compiled-source.tar.gz") as snapshot:
                self.assertTrue(snapshot.getmember("install-sh").isfile())
                self.assertEqual(snapshot.extractfile("install-sh").read(), installed.read_bytes())
            index = json.loads((evidence / "compiled-source-index.json").read_text())
            self.assertEqual(index["install-sh"]["member"], "automake-pinned/lib/install-sh")
            installed.write_text("unapproved support source\n")
            altered = self.root / "altered-evidence"
            altered.mkdir()
            with self.assertRaisesRegex(ValueError, "escapes verified"):
                TOOL.retain_compiled_snapshot(self.root, altered, tools)
            tool_archive.write_bytes(b"tampered archive")
            tampered = self.root / "tampered-evidence"
            tampered.mkdir()
            with self.assertRaisesRegex(ValueError, "archive mismatch"):
                TOOL.retain_generation_tools(tools, tampered)
