"""Static-provider provenance boundaries; no parser/compiler is executed."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


def helper():
    path = Path(__file__).resolve().parents[2] / "scripts/sealed-parser-dependency-offers.py"
    spec = importlib.util.spec_from_file_location("sealed_offers", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SealedParserOffersCase(unittest.TestCase):
    def test_copyright_alias_requires_authenticated_regular_endpoint(self):
        h = helper()
        links = {"usr/share/doc/dev": "usr/share/doc/runtime"}
        text, witness = h.resolve_copyright("dev", {"usr/share/doc/runtime/copyright": b"license"}, links)
        self.assertEqual(text, b"license")
        self.assertEqual(witness["regular_endpoint"], "usr/share/doc/runtime/copyright")
        for invalid in [{}, {"usr/share/doc/dev": "../../host"},
                        {"usr/share/doc/dev": "usr/share/doc/runtime", "usr/share/doc/runtime": "usr/share/doc/dev"}]:
            with self.assertRaises(ValueError):
                h.resolve_copyright("dev", {}, invalid)

    def test_installed_static_archive_must_equal_authenticated_member(self):
        h = helper()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            member = root / "usr/lib/libz.a"
            member.parent.mkdir(parents=True)
            member.write_bytes(b"archive")
            rows = {"zlib": {"installed_members": {"usr/lib/libz.a": {
                "bytes": 7, "sha256": hashlib.sha256(b"archive").hexdigest()}}}}
            h.verify_installed_members(rows, root)
            member.write_bytes(b"replace")
            with self.assertRaises(ValueError):
                h.verify_installed_members(rows, root)
            member.unlink()
            member.symlink_to(root / "outside")
            with self.assertRaises(ValueError):
                h.verify_installed_members(rows, root)

    def test_supplement_preserves_compilation_and_rejects_provenance_tampering(self):
        h = helper()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "parser"
            binary.write_bytes(b"sealed image")
            provenance = root / "provenance"
            provenance.mkdir()
            (provenance / "build-static-ffprobe").write_bytes(b"original recipe")
            (provenance / "config.mak").write_bytes(b"LDEXEFLAGS= -static\n")
            old = {"executable_sha256": h.sha(binary), "build_recipe_sha256": h.sha(provenance / "build-static-ffprobe"), "source": "original"}
            output = provenance / "static-dependencies"
            output.mkdir()
            original = json.dumps(old).encode()
            (output / "original-parser-manifest.json").write_bytes(original)
            (output / "copyright").write_bytes(b"authentic notice")
            facts = {"schema_version": 1, "mode": "supplement-recorded-parser", "parser_sha256": h.sha(binary), "original_compile_recipe_sha256": old["build_recipe_sha256"],
                     "recorded_link_config_sha256": h.sha(provenance / "config.mak"),
                     "original_parser_manifest_sha256": hashlib.sha256(original).hexdigest(),
                     "files": {p.name: {"bytes": p.stat().st_size, "sha256": h.sha(p)} for p in output.iterdir()}}
            (output / "manifest.json").write_text(json.dumps(facts))
            parent = {**old, "static_dependencies": {"path": "static-dependencies/manifest.json", "sha256": h.sha(output / "manifest.json")}}
            (provenance / "manifest.json").write_text(json.dumps(parent))
            h.verify_offer(binary, provenance)
            (output / "copyright").write_bytes(b"substituted notice")
            with self.assertRaises(ValueError):
                h.verify_offer(binary, provenance)
            (output / "copyright").write_bytes(b"authentic notice")
            parent["source"] = "different compilation"
            (provenance / "manifest.json").write_text(json.dumps(parent))
            with self.assertRaises(ValueError):
                h.verify_offer(binary, provenance)
