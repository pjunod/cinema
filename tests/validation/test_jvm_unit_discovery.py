from __future__ import annotations

import base64
import copy
import unittest
from unittest.mock import patch

from validation import jvm_unit_discovery as discovery

# javac --release 17 -cp junit-4.13.2.jar CohortFixture.java, JDK25.0.2.
# package fixture; public class CohortFixture {
#   @org.junit.Test public void neverExecuteThisBody() {
#     throw new AssertionError("Discovery must not execute unit bodies");
#   }
# }
FIXTURE = base64.b64decode('yv66vgAAAD0AFwoAAgADBwAEDAAFAAYBABBqYXZhL2xhbmcvT2JqZWN0AQAGPGluaXQ+AQADKClWBwAIAQAYamF2YS9sYW5nL0Fzc2VydGlvbkVycm9yCAAKAQAmRGlzY292ZXJ5IG11c3Qgbm90IGV4ZWN1dGUgdW5pdCBib2RpZXMKAAcADAwABQANAQAVKExqYXZhL2xhbmcvT2JqZWN0OylWBwAPAQAVZml4dHVyZS9Db2hvcnRGaXh0dXJlAQAEQ29kZQEAD0xpbmVOdW1iZXJUYWJsZQEAFG5ldmVyRXhlY3V0ZVRoaXNCb2R5AQAZUnVudGltZVZpc2libGVBbm5vdGF0aW9ucwEAEExvcmcvanVuaXQvVGVzdDsBAApTb3VyY2VGaWxlAQASQ29ob3J0Rml4dHVyZS5qYXZhACEADgACAAAAAAACAAEABQAGAAEAEAAAAB0AAQABAAAABSq3AAGxAAAAAQARAAAABgABAAAAAwABABIABgACABAAAAAiAAMAAQAAAAq7AAdZEgm3AAu/AAAAAQARAAAABgABAAAABQATAAAABgABABQAAAABABUAAAACABY=')
PATH = "fixture/CohortFixture.class"


class CompiledJvmDiscoveryCase(unittest.TestCase):
    def metadata(self):
        return discovery.native_class_details(FIXTURE)

    def test_real_compiled_throwing_body_is_inventory_only(self):
        result = discovery.native_junit4_inventory({PATH: FIXTURE})
        self.assertEqual(result["ids"], ["fixture.CohortFixture.neverExecuteThisBody"])
        self.assertIsNone(result["unit_runtime_outcomes"])
        self.assertEqual(result["classes"][0]["source_file"], "CohortFixture.java")

    def test_truncated_invalid_magic_unknown_pool_tag_and_wrong_path_refuse(self):
        for raw in [FIXTURE[:8], FIXTURE[:-1], b"nope" + FIXTURE[4:], FIXTURE[:10] + b"\xff" + FIXTURE[11:]]:
            with self.subTest(raw=raw[:12]), self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory({PATH: raw})
        with self.assertRaises(discovery.InventoryError):
            discovery.native_junit4_inventory({"other/CohortFixture.class": FIXTURE})

    def test_invisible_test_annotation_is_not_runtime_discovery(self):
        raw = FIXTURE.replace(b"\x00\x19RuntimeVisibleAnnotations", b"\x00\x1bRuntimeInvisibleAnnotations")
        self.assertNotEqual(raw, FIXTURE)
        with self.assertRaises(discovery.InventoryError):
            discovery.native_junit4_inventory({PATH: raw})

    def test_private_static_abstract_or_parameterized_method_refuses(self):
        for flags, descriptor in [(0, "()V"), (9, "()V"), (0x401, "()V"), (1, "(I)V")]:
            metadata = self.metadata()
            method = next(m for m in metadata["methods"] if m["name"] == "neverExecuteThisBody")
            method.update(flags=flags, descriptor=descriptor)
            with self.subTest(flags=flags, descriptor=descriptor), patch.object(discovery, "native_class_details", return_value=metadata), self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory({PATH: FIXTURE})

    def test_class_and_constructor_must_meet_junit4_preconditions(self):
        header = discovery.class_header(FIXTURE)
        for flags in [0, 0x401, 0x601]:
            bad = dict(header, flags=flags)
            with self.subTest(flags=flags), patch.object(discovery, "class_header", return_value=bad), self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory({PATH: FIXTURE})
        for flags, descriptor in [(0, "()V"), (1, "(I)V")]:
            metadata = self.metadata()
            constructor = next(m for m in metadata["methods"] if m["name"] == "<init>")
            constructor.update(flags=flags, descriptor=descriptor)
            with self.subTest(flags=flags, descriptor=descriptor), patch.object(discovery, "native_class_details", return_value=metadata), self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory({PATH: FIXTURE})

    def test_runner_skip_and_rule_refuse_even_without_direct_tests(self):
        for annotation in ["Lorg/junit/runner/RunWith;", "Lorg/junit/Ignore;", "Lorg/junit/Rule;", "Lorg/junit/ClassRule;"]:
            metadata = self.metadata()
            metadata["methods"] = [m for m in metadata["methods"] if m["name"] != "neverExecuteThisBody"]
            metadata["annotations"].append(("RuntimeVisibleAnnotations", annotation))
            with self.subTest(annotation=annotation), patch.object(discovery, "native_class_details", return_value=metadata), self.assertRaises(discovery.InventoryError):
                discovery.native_junit4_inventory({PATH: FIXTURE})

    def test_generated_helper_cannot_hide_inherited_test_or_unknown_root(self):
        owner = {"name": "fixture/Owner", "super": "java/lang/Object", "interfaces": [], "flags": 1, "junit_descriptors": []}
        helper = dict(owner, name="fixture/Owner$Helper", super="foreign/Unknown")
        with self.assertRaises(discovery.InventoryError):
            discovery.excluded_generated_classes([owner, helper], [owner["name"]])
        test = dict(owner, name="fixture/Ancestor", junit_descriptors=["Lorg/junit/Test;"])
        helper["super"] = test["name"]
        with self.assertRaises(discovery.InventoryError):
            discovery.excluded_generated_classes([owner, helper, test], [owner["name"], test["name"]])
        helper["super"] = "java/lang/Object"
        self.assertEqual(discovery.excluded_generated_classes([owner, helper], [owner["name"]]), [helper["name"]])

    def test_reserved_external_roots_cannot_be_shadowed_by_artifact(self):
        fake = {"name": "okhttp3/ResponseBody", "super": "java/lang/Object", "interfaces": [], "flags": 1, "junit_descriptors": ["Lorg/junit/Test;"]}
        with self.assertRaises(discovery.InventoryError):
            discovery.excluded_generated_classes([fake], [fake["name"]])

    def test_duplicate_roster_missing_owner_and_cyclic_hierarchy_refuse(self):
        owner = {"name": "fixture/Owner", "super": "java/lang/Object", "interfaces": [], "flags": 1, "junit_descriptors": []}
        helper = dict(owner, name="fixture/Owner$Helper", super="fixture/Owner$Helper")
        for headers, selected in [([owner, owner], [owner["name"]]), ([helper], []), ([owner, helper], [owner["name"]])]:
            with self.subTest(headers=headers), self.assertRaises(discovery.InventoryError):
                discovery.excluded_generated_classes(headers, selected)

    def test_modified_utf8_preserves_null_and_supplementary_method_names(self):
        self.assertEqual(discovery.modified_utf8(b"a\xc0\x80b"), "a\0b")
        self.assertEqual(discovery.modified_utf8(b"\xed\xa0\xbd\xed\xb8\x80"), "\U0001f600")
        with self.assertRaises(discovery.InventoryError):
            discovery.modified_utf8(b"\xff")


class CurrentCompiledRosterCase(unittest.TestCase):
    def workspace(self, root):
        from pathlib import Path
        workspace = Path(root)
        classes = workspace / 'clients/android/app/build/intermediates/built_in_kotlinc/debugUnitTest/compileDebugUnitTestKotlin/classes'
        classes.mkdir(parents=True)
        path = classes / PATH
        path.parent.mkdir(parents=True)
        path.write_bytes(FIXTURE)
        return workspace, classes, path

    def test_duplicate_source_sets_and_symlinked_ancestry_refuse(self):
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            workspace, classes, path = self.workspace(root)
            java = workspace / 'clients/android/app/build/intermediates/javac/debugUnitTest/compileDebugUnitTestJavaWithJavac/classes'
            duplicate = java / PATH
            duplicate.parent.mkdir(parents=True)
            duplicate.write_bytes(FIXTURE)
            with self.assertRaises(discovery.InventoryError):
                discovery.compiled_roster(workspace)
            duplicate.unlink()
            path.unlink()
            path.symlink_to(duplicate)
            with self.assertRaises(discovery.InventoryError):
                discovery.compiled_roster(workspace)

    def test_collector_refuses_oversize_before_reading_bytes(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as root:
            workspace, _, path = self.workspace(root)
            with path.open('wb') as stream:
                stream.truncate(8 * 1024 * 1024 + 1)
            with patch.object(Path, 'open', side_effect=AssertionError('Must refuse before reading')):
                with self.assertRaises(discovery.InventoryError):
                    discovery.compiled_roster(workspace)

    def test_current_compiled_identity_and_source_owner_both_bind_adoption(self):
        import hashlib
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            workspace, _, _ = self.workspace(root)
            source = workspace / 'clients/android/app/src/test/java/fixture/CohortFixture.java'
            source.parent.mkdir(parents=True)
            source.write_text('original test source')
            ids = ['fixture.CohortFixture/neverExecuteThisBody']
            owners = [['fixture.CohortFixture', source.relative_to(workspace).as_posix(), hashlib.sha256(source.read_bytes()).hexdigest()]]
            adopted = {'mode': 'retained-source-cohort-v1', 'ids_count': 1,
                       'ids_sha256': hashlib.sha256(json.dumps(ids, separators=(',', ':')).encode()).hexdigest(),
                       'jvm_source_owners_sha256': hashlib.sha256(json.dumps(owners, separators=(',', ':')).encode()).hexdigest()}
            self.assertEqual(discovery.verify_adopted(workspace, adopted), ids)
            source.write_text('changed source')
            with self.assertRaises(discovery.InventoryError):
                discovery.verify_adopted(workspace, adopted)
            adopted['ids_count'] = 2
            with self.assertRaises(discovery.InventoryError):
                discovery.verify_adopted(workspace, adopted)

    def test_streaming_entry_bound_refuses_an_unbounded_directory_walk(self):
        import tempfile
        from contextlib import contextmanager
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as root:
            workspace, classes, _ = self.workspace(root)
            consumed = []
            retired = []
            def entries():
                for index in range(100000):
                    consumed.append(index)
                    yield SimpleNamespace(path=str(classes / ('filler_' + str(index))),
                                          is_symlink=lambda: False, is_dir=lambda **kwargs: True)
            @contextmanager
            def scan(directory):
                try:
                    yield entries()
                finally:
                    retired.append(True)
            with patch('os.scandir', side_effect=scan):
                with self.assertRaises(discovery.InventoryError):
                    discovery.compiled_roster(workspace)
            self.assertEqual(len(consumed), 8193)
            self.assertEqual(retired, [True])

    def test_bounded_read_refuses_growth_after_the_stat_measurement(self):
        import io
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as root:
            workspace, _, _ = self.workspace(root)
            with patch.object(Path, 'open', return_value=io.BytesIO(FIXTURE + b'x')):
                with self.assertRaises(discovery.InventoryError):
                    discovery.compiled_roster(workspace)
