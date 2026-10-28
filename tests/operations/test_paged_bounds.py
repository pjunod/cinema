"""Unrun Init-class fixtures; fixed startup i64s are not Episode horizons."""
from __future__ import annotations

import sqlite3
import unittest

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


if __name__=='__main__':
    unittest.main()
