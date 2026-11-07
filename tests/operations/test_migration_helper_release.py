"""Actual archive/description qualification for the separate signed helper cohort."""
import argparse
import copy
import gzip
import hashlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import shutil
import subprocess
import sys
import unittest

from validation import migration_helper_release as HELPER

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("helper_release_manifest", str(ROOT / "scripts/create-desktop-update-manifest"))
spec = importlib.util.spec_from_loader(loader.name, loader)
MANIFEST = importlib.util.module_from_spec(spec)
loader.exec_module(MANIFEST)
sys.path.insert(0, str(ROOT / "tests/desktop"))
from common import check_build_artifacts, check_build_metadata, check_build_asset_inventory
CODE = b"real helper executable fixture\n"


class MigrationHelperReleaseCase(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="optional-helper-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.args = argparse.Namespace(assets=self.root, output=self.root / MANIFEST.MANIFEST,
            version="1.2.3", sequence=123, source_commit="a" * 40, source_tree="b" * 40,
            packaging_commit="c" * 40, created_unix=1792400000, expires_unix=1792486400,
            upgrade_from=[">=1.0.0, <1.2.3"], data_compatibility="native-124",
            cluster_compatibility="auth81-82", minimum_controller="1.0.0", rollback="manual_recovery",
            signing_key=None, inspect_packages=False, migration_helper_description=[])
        for identity in MANIFEST.expected(self.args.version):
            (self.root / identity["name"]).write_bytes(identity["name"].encode())

    def archive(self, target, names=None):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, kind in names or [(HELPER.EXECUTABLE, tarfile.REGTYPE)]:
                header = tarfile.TarInfo(name); header.mode = 0o755; header.type = kind
                data = CODE if kind == tarfile.REGTYPE else b""
                header.size = len(data)
                if kind == tarfile.SYMTYPE:
                    header.linkname = "/bin/sh"
                archive.addfile(header, io.BytesIO(data))
        path = self.root / f"plurx-storage-migrate-1.2.3-{target}.tar.gz"
        path.write_bytes(gzip.compress(stream.getvalue(), mtime=0))
        return path

    def descriptor(self, target):
        self.archive(target)
        description = dict(version=1, build=dict(commit="a" * 40, tree="b" * 40, target=target),
            source_cohorts=[dict(auth_version=version, canonical_layout_fingerprint=str(version - 80) * 64) for version in (81, 82)],
            converter_sha256="d" * 64, native_schema_version=124, manifest_version=1, transition_version=1)
        path = self.root / f"plurx-storage-migrate-1.2.3-{target}.describe.json"
        path.write_text(json.dumps(description))
        self.args.migration_helper_description.append(path)
        return path, description

    def generate(self):
        MANIFEST.create(self.args)
        return json.loads(self.args.output.read_bytes())

    def test_default_v1_never_acquires_unsigned_or_default_helper_assets(self):
        self.archive("x86_64-unknown-linux-gnu")
        value = self.generate()
        self.assertEqual(value["schema_version"], 1)
        self.assertEqual(len(value["artifacts"]), 9)
        self.assertNotIn("migration_helpers", value)
        value["migration_helpers"] = []
        with self.assertRaises(ValueError):
            MANIFEST.validate(value)

    def test_explicit_four_target_v2_binds_actual_bundle_executable_and_descriptions(self):
        for target in HELPER.TARGETS:
            self.descriptor(target)
        value = self.generate()
        self.assertEqual(value["schema_version"], 2)
        self.assertEqual(len(value["artifacts"]), 9)
        self.assertEqual(len(value["migration_helpers"]), 4)
        self.assertTrue(set(a["name"] for a in value["artifacts"]).isdisjoint(h["artifact"]["name"] for h in value["migration_helpers"]))
        for helper in value["migration_helpers"]:
            self.assertEqual(helper["executable_sha256"], hashlib.sha256(CODE).hexdigest())
            self.assertEqual(helper["artifact"]["sha256"], hashlib.sha256((self.root / helper["artifact"]["name"]).read_bytes()).hexdigest())
            self.assertEqual(helper["daemon_build"], helper["helper_build"])
        self.assertIs(MANIFEST.validate(value), value)

    def test_wrong_build_unknown_fields_and_duplicate_targets_refuse(self):
        path, value = self.descriptor("x86_64-unknown-linux-gnu")
        for field, changed in [("commit", "e" * 40), ("tree", "e" * 40), ("target", "x86_64-pc-windows-msvc")]:
            wrong = copy.deepcopy(value); wrong["build"][field] = changed; path.write_text(json.dumps(wrong))
            with self.assertRaises(ValueError):
                self.generate()
        path.write_text(json.dumps(value | {"verified": True}))
        with self.assertRaises(ValueError):
            self.generate()
        path.write_text(json.dumps(value)); self.args.migration_helper_description.append(path)
        with self.assertRaises(ValueError):
            self.generate()
        self.assertFalse(self.args.output.exists())

    def test_closed_archive_rejects_symlink_nested_duplicate_and_extra_companions(self):
        self.descriptor("x86_64-unknown-linux-gnu")
        for rows in [[(HELPER.EXECUTABLE, tarfile.SYMTYPE)], [("nested/" + HELPER.EXECUTABLE, tarfile.REGTYPE)],
                     [(HELPER.EXECUTABLE, tarfile.REGTYPE)] * 2, [(HELPER.EXECUTABLE, tarfile.REGTYPE), ("extra.so", tarfile.REGTYPE)]]:
            self.archive("x86_64-unknown-linux-gnu", rows)
            with self.assertRaises(ValueError):
                self.generate()

    def test_crc_truncation_and_trailing_gzip_members_refuse(self):
        self.descriptor("x86_64-unknown-linux-gnu")
        path = self.archive("x86_64-unknown-linux-gnu")
        original = path.read_bytes()
        bad_crc = bytearray(original); bad_crc[-8] ^= 1
        for data in [bytes(bad_crc), original[:-1], original + b"trailing", original + original]:
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                self.generate()

    def test_description_bound_duplicates_and_symlink_refuse(self):
        path, value = self.descriptor("x86_64-unknown-linux-gnu")
        for data in [b" " * 65537, b'{"version":1,"version":1}', json.dumps(value | {"source_cohorts": []}).encode()]:
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                self.generate()
        other = self.root / "other.json"; other.write_text(json.dumps(value)); path.unlink(); path.symlink_to(other)
        with self.assertRaises(ValueError):
            self.generate()

    def test_qualification_parser_rejects_tampered_signed_tuple_or_unclosed_metadata(self):
        self.descriptor("x86_64-unknown-linux-gnu")
        value = self.generate()
        changes = [("profile", "cluster"), ("helper_version", "1.2.4"), ("helper_protocol", True),
                   ("receipt_format", 2), ("bundle_sha256", "0" * 64), ("source_cohorts", []), ("unexpected", True)]
        for field, mutation in changes:
            wrong = copy.deepcopy(value); wrong["migration_helpers"][0][field] = mutation
            with self.assertRaises(ValueError, msg=field):
                MANIFEST.validate(wrong)
        for helpers in [[], None, value["migration_helpers"] * 2]:
            wrong = copy.deepcopy(value); wrong["migration_helpers"] = helpers
            with self.assertRaises(ValueError):
                MANIFEST.validate(wrong)

    def test_exact_signature_covers_optional_helper_metadata(self):
        openssl = shutil.which("openssl")
        if not openssl:
            self.skipTest("OpenSSL Ed25519 fixture tool unavailable")
        key = self.root / "fixture-key.pem"
        generated = subprocess.run([openssl, "genpkey", "-algorithm", "ED25519", "-out", str(key)], capture_output=True)
        if generated.returncode:
            self.skipTest("host OpenSSL lacks Ed25519")
        public = self.root / "fixture-public.pem"
        subprocess.run([openssl, "pkey", "-in", str(key), "-pubout", "-out", str(public)], check=True, capture_output=True)
        self.args.signing_key = key
        self.descriptor("x86_64-unknown-linux-gnu")
        value = self.generate()
        command = [openssl, "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(public), "-in", str(self.args.output), "-sigfile", str(self.args.output) + ".sig"]
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        value["migration_helpers"][0]["converter_sha256"] = "e" * 64
        self.args.output.write_text(json.dumps(value))
        self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)

    def test_qualification_checks_helper_bytes_and_descriptor_without_executing_code(self):
        path, descriptor = self.descriptor("x86_64-unknown-linux-gnu")
        self.args.packaging_commit = self.args.source_commit
        value = self.generate()
        actual = check_build_metadata(value, b"x" * 64, self.args.source_commit, self.args.source_tree, self.root)
        self.assertEqual(len(actual), 11)
        wrong = copy.deepcopy(descriptor); wrong["converter_sha256"] = "0" * 64
        path.write_text(json.dumps(wrong))
        with self.assertRaises(ValueError):
            check_build_metadata(value, b"x" * 64, self.args.source_commit, self.args.source_tree, self.root)
        path.write_text(json.dumps(descriptor))
        bundle = self.root / value["migration_helpers"][0]["artifact"]["name"]
        bundle.write_bytes(bundle.read_bytes() + b"changed")
        with self.assertRaises(ValueError):
            check_build_metadata(value, b"x" * 64, self.args.source_commit, self.args.source_tree, self.root)

    def test_transport_accepts_historical_v1_or_exact_complete_optional_helper_cohort(self):
        base = ["release-linux-amd64", "release-linux-arm64", "release-macos", "release-windows", "desktop-update-candidate"]
        helpers = [f"release-storage-helper-{target}" for target in HELPER.TARGETS]
        def record(names):
            return {"total_count": len(names), "artifacts": [dict(id=index + 1, name=name, expired=False, size_in_bytes=123) for index, name in enumerate(names)]}
        self.assertEqual(len(check_build_artifacts(record(base))), 5)
        self.assertEqual(len(check_build_artifacts(record(base + helpers))), 9)
        for names in [base + helpers[:1], base + helpers + helpers[:1], base + ["release-storage-helper-unknown"]]:
            with self.assertRaises(ValueError):
                check_build_artifacts(record(names))


    def test_qualification_transport_census_names_manual_extras_without_expanding_defaults(self):
        self.descriptor("x86_64-unknown-linux-gnu")
        self.args.packaging_commit = self.args.source_commit
        value = self.generate()
        observed = check_build_metadata(value, b"x" * 64, self.args.source_commit, self.args.source_tree, self.root)
        assets = self.root / "downloaded"
        assets.mkdir()
        for name in observed:
            shutil.copyfile(self.root / name, assets / name)
        manual = [f"plurxd-{value['version']}-windows-x86_64.zip", *[f"plurx-tray-{value['version']}-linux-{arch}.tar.gz" for arch in ("amd64", "arm64")]]
        for name in manual:
            (assets / name).write_bytes(b"established manual artifact")
        self.assertEqual(len(check_build_asset_inventory(value, observed, assets)), 14)
        self.assertEqual(len(value["artifacts"]), 9)
        extra = assets / "plurx-storage-migrate-unknown.tar.gz"
        extra.write_bytes(b"not a signed component")
        with self.assertRaises(ValueError):
            check_build_asset_inventory(value, observed, assets)
        extra.unlink(); (assets / manual[0]).unlink()
        with self.assertRaises(ValueError):
            check_build_asset_inventory(value, observed, assets)



if __name__ == "__main__":
    unittest.main()
