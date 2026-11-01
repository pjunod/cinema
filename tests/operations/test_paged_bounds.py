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


if __name__=='__main__':
    unittest.main()
