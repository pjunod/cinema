"""Provisioned public roots remain auditable in never-published fixture overlays."""
import copy
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


common = load('fixture_roots_common', ROOT / 'tests/desktop/common.py')
with mock.patch.dict(sys.modules, {'common': common}):
    overlay = load('fixture_roots_overlay', ROOT / 'tests/desktop/create-overlay.py')


class DesktopFixtureRootTests(unittest.TestCase):
    def test_provisioned_candidate_is_preserved_in_provenance_and_only_fixture_keys_replace_it(self):
        raw = (ROOT / 'crates/plurx-desktop-control/production-trust-roots.json').read_bytes()
        original = json.loads(raw)
        self.assertTrue(original['keys'])
        fixture, provenance = overlay.fixture_root_overlay(raw, 'cd' * 32)
        self.assertEqual(provenance['candidate_trust_roots'], original)
        self.assertEqual(provenance['candidate_trust_roots_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(fixture['keys'], [{'id': 'qualification-local-only', 'public_key_hex': 'cd' * 32}])
        self.assertEqual((ROOT / 'crates/plurx-desktop-control/production-trust-roots.json').read_bytes(), raw)

    def test_empty_roots_and_distinct_rotation_keys_remain_supported(self):
        for keys in ([], [{'id': 'one', 'public_key_hex': 'ab' * 32}, {'id': 'two', 'public_key_hex': 'ef' * 32}]):
            original = {'schema_version': 1, 'repository': 'pjunod/cinema', 'keys': keys}
            _, provenance = overlay.fixture_root_overlay(json.dumps(original).encode(), 'cd' * 32)
            self.assertEqual(provenance['candidate_trust_roots'], original)

    def test_malformed_candidate_documents_and_reused_production_key_are_rejected(self):
        original = {'schema_version': 1, 'repository': 'pjunod/cinema', 'keys': [{'id': 'production', 'public_key_hex': 'ab' * 32}]}
        variants = []
        for field, value in [('repository', 'elsewhere/project'), ('schema_version', True), ('keys', {}), ('extra', True)]:
            item = copy.deepcopy(original)
            item[field] = value
            variants.append(item)
        for key in ({'id': 'bad/key', 'public_key_hex': 'ab' * 32}, {'id': 'one', 'public_key_hex': '00' * 32}, {'id': 'one', 'public_key_hex': 'AB' * 32}):
            item = copy.deepcopy(original)
            item['keys'] = [key]
            variants.append(item)
        item = copy.deepcopy(original)
        item['keys'] *= 2
        variants.append(item)
        for item in variants:
            with self.subTest(item=item), self.assertRaises(ValueError):
                overlay.fixture_root_overlay(json.dumps(item).encode(), 'cd' * 32)
        with self.assertRaisesRegex(ValueError, 'distinct disposable'):
            overlay.fixture_root_overlay(json.dumps(original).encode(), 'ab' * 32)
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            overlay.fixture_root_overlay(b'{"schema_version":1,"schema_version":1}', 'cd' * 32)
