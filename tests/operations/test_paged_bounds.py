"""Unrun Init-class fixtures; fixed startup i64s are not Episode horizons."""
from __future__ import annotations

import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

import test_paged_temporal as fixture
import paged_bounds as bounds
import paged_codec as codec


class PagedInitClassFixtures(unittest.TestCase):
    def initial(self):
        db=sqlite3.connect(':memory:')
        db.executescript('CREATE TABLE settings(key TEXT,value TEXT,updated_at INTEGER);'
                         'CREATE TABLE libraries(id INTEGER,name TEXT,kind TEXT,paths TEXT,anime INTEGER,created_at INTEGER);'
                         'CREATE TABLE background_storage_domains(library_id INTEGER,root_path TEXT,domain_id TEXT);')
        for table in ('background_jobs','background_library_requests','background_job_waiters'):
            db.execute('CREATE TABLE '+table+'(id INTEGER)')
            db.executemany('INSERT INTO '+table+' VALUES(?)',[(1,),(2,)])
        for table in ('background_job_attempts','background_job_reservations','background_job_domain_leases','background_job_commands','job_leases'):
            db.execute('CREATE TABLE '+table+'(id INTEGER)')
        db.executemany('INSERT INTO settings VALUES(?,?,?)',[
            ('hdr_format_backfilled_v1','1',-(2**63)),
            ('instance.id','00000000-0000-4000-8000-000000000003',2**63-1)])
        db.execute('INSERT INTO libraries VALUES(?,?,?,?,?,?)',(1,'async-store-domain','movies','["/async-store-root"]',0,2**63-1))
        db.execute('INSERT INTO background_storage_domains VALUES(?,?,?)',(1,'/async-store-root','shared-root'))
        raw={'jobs':['00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002'],
             'library_id':1,'omitted':{},'initial_projection':{},'initial_inputs':[]}
        return db,raw

    def test_fixed_startup_i64_samples_survive_without_episode_clipping(self):
        db,raw=self.initial()
        try:
            before=list(db.execute('SELECT key,value,updated_at FROM settings ORDER BY key'))
            self.assertTrue(bounds.validate_initial_class(db,raw))
            self.assertEqual(list(db.execute('SELECT key,value,updated_at FROM settings ORDER BY key')),before)
        finally:db.close()

    def test_extra_preexisting_setting_refuses_fresh_constructor_class(self):
        db,raw=self.initial()
        try:
            db.execute('INSERT INTO settings VALUES(?,?,?)',('preexisting','arbitrary',0))
            with self.assertRaisesRegex(codec.Invalid,'settings inventory'):
                bounds.validate_initial_class(db,raw)
        finally:db.close()

    def test_raw_non_v4_job_identity_is_refused_without_normalization(self):
        db,raw=self.initial()
        try:
            raw['jobs'][0]='00000000-0000-1000-8000-000000000001'
            with self.assertRaisesRegex(codec.Invalid,'UUID source class'):
                bounds.validate_initial_class(db,raw)
            self.assertEqual(raw['jobs'][0],'00000000-0000-1000-8000-000000000001')
        finally:db.close()

    def test_changed_storage_mapping_refuses_the_fixed_source_class(self):
        db,raw=self.initial()
        try:
            db.execute("UPDATE background_storage_domains SET domain_id='other'")
            with self.assertRaisesRegex(codec.Invalid,'domain constructor'):
                bounds.validate_initial_class(db,raw)
        finally:db.close()


class PagedPhysicalChargeInventory(unittest.TestCase):
    """The nonsemantic inventory names physical policy pools/families, never aggregate multiples."""
    SPEC = bounds.PACKAGE / 'source-bound-formulas.json'

    def refused(self, mutate, pattern):
        spec = json.loads(self.SPEC.read_bytes())
        mutate(spec['inventory_mapping'])
        base = bounds.ROOT / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as scratch:
            path = Path(scratch) / 'spec.json'
            path.write_bytes(codec.canonical(spec).encode())
            with self.assertRaisesRegex(codec.Invalid, pattern):
                bounds.validate_specification(path, expected_sha256=codec.sha_bytes(path.read_bytes()))

    def row(self, mapping, kind):
        return next(r for r in mapping if r['kind'] == kind)

    def test_retired_overhead_fields_are_refused(self):
        for kind, retired in (('backup_and_chunks', 'duplicate_backup_bytes'), ('downloads_and_restores', 'duplicate_backup_bytes'),
                              ('logs', 'retained_bytes'), ('source_archive', 'scratch_bytes'),
                              ('sql_indexes', 'index_bytes'), ('sql_wal', 'wal_bytes')):
            self.refused(lambda m, kind=kind, retired=retired: self.row(m, kind).update(overhead_field=retired), 'physical policy pool/family')

    def test_each_peak_workspace_class_has_exactly_its_own_physical_charge(self):
        self.refused(lambda m: self.row(m, 'sql_wal').update(overhead_field='family_main_bytes'), 'physical policy pool/family')
        self.refused(lambda m: self.row(m, 'backup_and_chunks').update(overhead_field='restore_staging_bytes'), 'physical policy pool/family')

    def test_dropping_a_physical_charge_class_is_refused(self):
        self.refused(lambda m: m.remove(self.row(m, 'sql_wal')), 'charge inventory|missing/extra|order')

    def test_the_registered_mapping_names_only_policy_pools_and_families(self):
        spec = json.loads(self.SPEC.read_bytes())
        charges = {r['overhead_field'] for r in spec['inventory_mapping'] if r['overhead_field'] is not None}
        self.assertEqual(charges, set(bounds.PHYSICAL_CHARGES.values()))
        self.assertFalse(charges & {'index_bytes', 'wal_bytes', 'scratch_bytes', 'retained_bytes', 'duplicate_backup_bytes'})


class PagedRealSpecificationAdmission(unittest.TestCase):
    """validate_specification on the REAL source-bound-formulas.json and the REAL pinned schemas.

    N2: stale record_schemas / primitive_bindings pins and a mapped kind that no pinned schema declares were both
    present in the tree while every other fixture used synthetic specifications. These drive the real files, so a
    pin that drifts or a kind that is mapped but undeclared (the `validation_receipt` gap) refuses here.
    """
    SPEC = bounds.PACKAGE / 'source-bound-formulas.json'

    def check(self, mutate=None):
        spec = json.loads(self.SPEC.read_bytes())
        if mutate is not None:
            mutate(spec)
        base = bounds.ROOT / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as scratch:
            path = Path(scratch) / 'spec.json'
            path.write_bytes(self.SPEC.read_bytes() if mutate is None else codec.canonical(spec).encode())
            return bounds.validate_specification(path, expected_sha256=codec.sha_bytes(path.read_bytes()))

    def declared_kinds(self):
        """Every `properties.kind.const` of every pinned record schema, found by an independent walk."""
        spec = json.loads(self.SPEC.read_bytes())
        found = set()

        def walk(node):
            if isinstance(node, dict):
                kind = node.get('properties', {}).get('kind', {}).get('const') if isinstance(node.get('properties'), dict) else None
                if isinstance(kind, str):
                    found.add(kind)
                for child in node.values():
                    walk(child)
            elif isinstance(node, list):
                for child in node:
                    walk(child)
        for binding in spec['record_schemas']:
            walk(json.loads((bounds.ROOT / binding['path']).read_bytes()))
        return found

    def test_the_real_specification_is_admitted_by_the_real_pins_and_schemas(self):
        spec, graph = self.check()
        self.assertTrue(spec['complete'])
        self.assertTrue(graph.nodes)

    def test_every_mapped_record_kind_is_declared_by_a_pinned_schema(self):
        spec = json.loads(self.SPEC.read_bytes())
        mapped = {row['kind'] for row in spec['inventory_mapping'] if row['role'] in ('source_global', 'operational_only')}
        self.assertEqual(mapped - self.declared_kinds(), set(), 'a kind is charged by the inventory but no pinned schema declares it')
        self.assertIn('validation_receipt', self.declared_kinds())

    def test_a_mapped_kind_that_no_pinned_schema_declares_is_refused(self):
        def add_ghost(spec):
            spec['inventory_mapping'].append({'kind': 'zz_undeclared_record', 'role': 'operational_only',
                                              'lifetime': 'session_retained_unit_member', 'source_record_kind': None, 'overhead_field': None})
            spec['inventory_mapping'].sort(key=lambda row: row['kind'])
        with self.assertRaisesRegex(codec.Invalid, 'missing/extra registered source/control/auxiliary kind'):
            self.check(add_ghost)

    def test_dropping_a_mapped_kind_a_schema_declares_is_refused(self):
        def drop_receipt(spec):
            spec['inventory_mapping'] = [row for row in spec['inventory_mapping'] if row['kind'] != 'validation_receipt']
        with self.assertRaisesRegex(codec.Invalid, 'missing/extra registered source/control/auxiliary kind'):
            self.check(drop_receipt)

    def test_a_stale_record_schema_pin_is_refused(self):
        for index in range(len(json.loads(self.SPEC.read_bytes())['record_schemas'])):
            with self.assertRaisesRegex(codec.Invalid, 'record schema premise drift', msg=str(index)):
                self.check(lambda spec, index=index: spec['record_schemas'][index].update(sha256='0' * 64))

    def test_a_stale_source_input_or_primitive_pin_is_refused(self):
        with self.assertRaisesRegex(codec.Invalid, 'source-bound premise source drift'):
            self.check(lambda spec: spec['source_inputs'][0].update(sha256='0' * 64))
        spec = json.loads(self.SPEC.read_bytes())
        paths = sorted({row['source_path'] for row in spec['primitive_bindings']})
        self.assertTrue(paths)
        for path in paths:  # every distinct pinned source, not just the first binding
            def stale(value, path=path):
                row = next(r for r in value['primitive_bindings'] if r['source_path'] == path)
                row['source_sha256'] = '0' * 64
            with self.assertRaisesRegex(codec.Invalid, 'primitive source hash', msg=path):
                self.check(stale)

    def test_a_missing_primitive_anchor_is_refused(self):
        def lose_anchor(spec):
            spec['primitive_bindings'][0]['anchor'] = 'ANCHOR-THAT-IS-NOT-IN-THE-SOURCE-0f3a'
        with self.assertRaisesRegex(codec.Invalid, 'primitive source anchor missing'):
            self.check(lose_anchor)


if __name__=='__main__':
    unittest.main()
