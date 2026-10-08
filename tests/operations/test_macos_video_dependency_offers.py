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
