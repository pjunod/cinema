"""Adversarial schema6 transport fixtures; defined before execution approval."""
from __future__ import annotations

import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

PACKAGE = Path(__file__).resolve().parents[2] / 'verification' / 'async-driver-refinement'
sys.path.insert(0, str(PACKAGE))
import paged_codec as codec
from test_paged_temporal import SOURCE, SESSION, admitted


class PagedCodecFixtures(unittest.TestCase):
    def setUp(self):
        base = PACKAGE.parents[1] / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=base)
        self.root = Path(self.directory.name)
        self.budget = admitted(self.root)
        self.index = codec.RecordIndex(self.root / 'index.sqlite', source=SOURCE, session_id=SESSION, budget=self.budget)

    def tearDown(self):
        self.index.close()
        self.directory.cleanup()

    def rows(self, identity=1, value=None):
        raw = codec.canonical({'x': [identity] if value is None else value})
        return codec.envelope('PhysicalRows', {'id': identity, 'canonical_utf8': raw,
            'utf8_bytes': len(raw.encode()), 'sha256': codec.sha_bytes(raw.encode())})

    def page(self, values, dependencies=None):
        keys = [codec.record_key(x) for x in values]
        record = {'session_id': SESSION, 'source': SOURCE, 'page_id': '0' * 64,
            'kind': values[0]['kind'], 'first_key': keys[0], 'last_key': keys[-1],
            'record_count': len(values), 'utf8_bytes': 0, 'records': values, 'dependencies': dependencies or [],
            'previous_page': None, 'transaction_high_water': 1, 'previous_checkpoint': None}
        value = codec.envelope('Page', record)
        record['utf8_bytes'] = len(codec.transport_bytes(value, measure=True))
        record['page_id'] = codec.digest(value)
        return value

    def write_page(self, value, name='page.json'):
        path = self.root / name
        path.write_bytes(codec.encoded(value))
        return path

    def test_duplicate_json_keys_unknown_fields_bool_ids_and_nonfinite_rejected(self):
        with self.assertRaisesRegex(codec.Invalid, 'duplicate JSON'):
            codec.strict_json('{"a":1,"a":2}')
        for update in ({'unknown': 1}, {'id': True}, {'utf8_bytes': -1}):
            value = self.rows()
            value['record'].update(update)
            with self.subTest(update=update), self.assertRaises(codec.Invalid):
                self.index.add(value)
        with self.assertRaisesRegex(codec.Invalid, 'nonfinite'):
            codec.strict_json('{"unused":NaN}')

    def test_forced_digest_collision_never_aliases_distinct_canonical_bytes(self):
        # A forced hash collision is allowed to occupy two exact identity keys;
        # it never permits replacing bytes under an existing key.
        with patch.object(codec, 'sha_bytes', return_value='0' * 64):
            first, second = self.rows(1), self.rows(2)
            self.index.add(first)
            self.index.add(second)
            self.assertNotEqual(self.index.get('PhysicalRows', 1)['canonical_utf8'], self.index.get('PhysicalRows', 2)['canonical_utf8'])
            changed = self.rows(1, ['different'])
            with self.assertRaisesRegex(codec.Invalid, 'conflicting duplicate'):
                self.index.add(changed)

    def test_overlapping_page_copies_require_identical_full_bytes(self):
        value = self.page([self.rows()])
        self.index.load_member(self.write_page(value))
        self.index.load_member(self.write_page(value, 'second.json'))
        self.assertEqual(self.index.count('PhysicalRows'), 1)
        changed = self.rows(1, ['new'])
        with self.assertRaisesRegex(codec.Invalid, 'conflicting duplicate'):
            self.index.load_member(self.write_page(self.page([changed]), 'conflict.json'))

    def test_tampered_hash_byte_count_kind_and_key_range_fail(self):
        for field, value in (('page_id', 'a' * 64), ('record_count', 9), ('utf8_bytes', 3), ('first_key', 9), ('kind', 'Episode')):
            page = self.page([self.rows()])
            page['record'][field] = value
            if field not in ('page_id', 'utf8_bytes'):
                page['record']['utf8_bytes'] = len(codec.transport_bytes(page, measure=True))
                page['record']['page_id'] = codec.digest(page)
            with self.subTest(field=field), self.assertRaises(codec.Invalid):
                self.index.load_member(self.write_page(page, field + '.json'))

    def test_root_filenames_are_valid_and_aliases_symlinks_traversal_rejected(self):
        for name in ('driver-graph.json', 'initial-image.sqlite', 'catalog.sqlite'):
            (self.root / name).write_bytes(b'x')
            self.assertEqual(codec.safe_path(self.root, name), self.root / name)
        (self.root / 'alias').symlink_to(self.root / 'catalog.sqlite')
        for name in ('../catalog.sqlite', './catalog.sqlite', '/catalog.sqlite', 'a//b', 'alias'):
            with self.subTest(name=name), self.assertRaises(codec.Invalid):
                codec.safe_path(self.root, name)

    def test_resource_exhaustion_is_incomplete_and_preserves_prior_records(self):
        self.index.add(self.rows(1))
        self.budget.plan['unit_record_allowance'] = self.budget.records
        with self.assertRaises(codec.Incomplete):
            self.index.add(self.rows(2))
        self.assertEqual(self.index.count('PhysicalRows'), 1)
        self.assertEqual(self.index.get('PhysicalRows', 1)['id'], 1)

    def test_mixed_source_and_premature_singletons_are_rejected(self):
        node = {'id': 1, 'context': 'base', 'product': 1, 'seen_goal': False, 'capacity_ok': True, 'pending_attempts': []}
        self.index.add(codec.envelope('MonitorNode', node))
        root = codec.envelope('ContextRoot', {'context': 'base', 'initial_product': 1,
            'reachable_products': 2, 'expanded_products': 1, 'frontier': 1})
        with self.assertRaisesRegex(codec.Invalid, 'premature'):
            self.index.add(root)
        page = self.page([self.rows()])
        page['record']['source'] = dict(SOURCE, tree='f' * 40)
        page['record']['utf8_bytes'] = len(codec.transport_bytes(page, measure=True))
        page['record']['page_id'] = codec.digest(page)
        with self.assertRaisesRegex(codec.Invalid, 'mixed source'):
            self.index.load_member(self.write_page(page))

    def test_text_cursor_keys_do_not_acquire_numeric_aliases(self):
        key = codec.canonical(['base', 12, 14])
        value = codec.envelope('FairPair', {'context': 'base', 'from_product': 12, 'to_product': 14, 'labels': [], 'nonstuttering': True})
        self.index.add(value)
        self.assertEqual(codec.record_key(value), key)
        self.assertEqual(self.index.get('FairPair', key)['to_product'], 14)
        with self.assertRaisesRegex(codec.Invalid, 'missing record'):
            self.index.get('FairPair', 1)


if __name__ == '__main__':
    unittest.main()
