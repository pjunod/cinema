"""Metadata-only M1 associations; no fixture acquisition, scoring or playback."""
import copy
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "scripts/codec-qualification-corpus.json"


class CodecQualificationCorpusTests(unittest.TestCase):
    def setUp(self):
        self.bench = runpy.run_path(str(ROOT / "scripts/bench"))
        self.document = json.loads(MANIFEST.read_text())

    def write(self, directory, document):
        path = directory / "corpus.json"
        path.write_text(json.dumps(document))
        return path

    def test_manifest_reuses_generators_without_claiming_measurements(self):
        result = self.bench["load_qualification_corpus"](MANIFEST)
        self.assertEqual({row["generator"] for row in result["fixtures"]}, set(self.bench["FIXTURES"]))
        self.assertEqual(len(result["fixtures"]), 13)
        self.assertEqual(result["manifest_sha256"], hashlib.sha256(MANIFEST.read_bytes()).hexdigest())
        self.assertTrue(result["metadata_valid"])
        for field in ("all_references_associated", "measurement_executed", "qualified"):
            self.assertFalse(result[field])
        self.assertEqual(len(result["unmeasured_associations"]), 16)
        by_name = {row["generator"]: row for row in self.document["fixtures"]}
        self.assertEqual(by_name["grainy"]["source"]["sha256"],
                         "df310256db516c37559f76f0e5f3945609b5c2824b1c548912731de1f49e65e1")
        self.assertIsNone(by_name["sport"]["source"]["sha256"])
        for generator in ("4k-hlg", "dv-p5"):
            self.assertTrue(all(row["domain"] == "unsupported" and row["state"] == "unmeasured"
                                for row in by_name[generator]["references"]))
        with self.assertRaisesRegex(self.bench["BenchError"], "unmeasured"):
            self.bench["load_qualification_corpus"](MANIFEST, require_associated=True)

    def test_reference_associations_bind_domain_parent_and_grade_receipt(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch).resolve()
            document = copy.deepcopy(self.document)
            pq = next(row for row in document["fixtures"] if row["generator"] == "4k-hdr10")
            ref = next(row for row in pq["references"] if row["output_grade"] == "sdr")
            ref.update(state="associated", sha256="a" * 64)
            proof = {"schema": 1, "kind": "independent-PQ-to-BT709-Hable-reference",
                     "parent_source_sha256": pq["source"]["sha256"],
                     "reference_sha256": ref["sha256"], "duration_seconds": 20}
            receipt = directory / "grade.json"
            raw = json.dumps(proof).encode()
            receipt.write_bytes(raw)
            ref["receipt"] = {"path": receipt.name, "sha256": hashlib.sha256(raw).hexdigest()}
            result = self.bench["load_qualification_corpus"](self.write(directory, document))
            checked = next(row for row in result["fixtures"] if row["generator"] == "4k-hdr10")
            self.assertTrue(checked["references"][0]["receipt_binding_verified"])
            self.assertFalse(result["qualified"], "association cannot stand in for decoded/graph/fidelity checks")
            for field, value in (("parent_source_sha256", "b" * 64),
                                 ("reference_sha256", "b" * 64), ("duration_seconds", 19),
                                 ("kind", "PQ-is-not-an-SDR-grade")):
                with self.subTest(receipt_field=field):
                    changed = dict(proof, **{field: value})
                    raw = json.dumps(changed).encode()
                    receipt.write_bytes(raw)
                    ref["receipt"]["sha256"] = hashlib.sha256(raw).hexdigest()
                    with self.assertRaisesRegex(self.bench["BenchError"], "grade receipt.*mismatch"):
                        self.bench["load_qualification_corpus"](self.write(directory, document))
            receipt.write_text(json.dumps(proof))
            ref["receipt"]["sha256"] = "0" * 64
            with self.assertRaisesRegex(self.bench["BenchError"], "SHA-256 mismatch"):
                self.bench["load_qualification_corpus"](self.write(directory, document))
            for field, value in (("domain", "pq-code-values"), ("output_grade", "hdr10"),
                                 ("parent_source_sha256", "b" * 64)):
                with self.subTest(association_field=field):
                    changed = copy.deepcopy(self.document)
                    row = next(row for row in changed["fixtures"] if row["generator"] == "4k-hdr10")
                    row["references"][0][field] = value
                    with self.assertRaises(self.bench["BenchError"]):
                        self.bench["load_qualification_corpus"](self.write(directory, changed))

    def test_cli_refuses_incomplete_or_ambiguous_metadata_without_tools(self):
        with tempfile.TemporaryDirectory() as scratch, \
             patch("subprocess.run", side_effect=AssertionError("no external tools")), \
             patch("urllib.request.urlopen", side_effect=AssertionError("no server")):
            directory = Path(scratch).resolve()
            for args, expected in (([], 0), (["--require-associated"], 1)):
                with self.subTest(args=args), patch("sys.argv", ["bench", "qualification-corpus", *args]), \
                     patch("sys.stdout", new_callable=io.StringIO) as output:
                    self.assertEqual(self.bench["main"](), expected)
                    result = json.loads(output.getvalue())
                    self.assertFalse(result["measurement_executed"])
                    self.assertFalse(result["qualified"])
            path = directory / "corpus.json"
            for raw in ('{"version":1,"version":1}', '{"version":NaN}', '{"version":1e999}',
                        " " * (1024 * 1024 + 1)):
                with self.subTest(raw_kind=raw[:32]):
                    path.write_text(raw)
                    with self.assertRaises(self.bench["BenchError"]):
                        self.bench["load_qualification_corpus"](path)
            path.unlink()
            path.symlink_to(MANIFEST)
            with self.assertRaisesRegex(self.bench["BenchError"], "symlinks"):
                self.bench["load_qualification_corpus"](path)
            path.unlink()
            for field, value in (("generator", []), ("trim", {"start_seconds": True, "duration_seconds": 20}),
                                 ("rung", True), ("source", {"state": "unmeasured", "identity": "fake", "sha256": "a" * 64})):
                with self.subTest(field=field):
                    changed = copy.deepcopy(self.document)
                    changed["fixtures"][0][field] = value
                    with self.assertRaises(self.bench["BenchError"]):
                        self.bench["load_qualification_corpus"](self.write(directory, changed))
