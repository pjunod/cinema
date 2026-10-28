"""Unrun fixtures for source-bound atomic verifier continuation."""
from __future__ import annotations

from pathlib import Path
import sqlite3
import tempfile
import unittest

import test_paged_temporal as fixture
import paged_codec as codec


class PagedContinuationFixtures(unittest.TestCase):
    def setUp(self):
        base=fixture.PACKAGE.parents[1]/'.async-proof-sessions'/'paged-fixtures'
        base.mkdir(parents=True,exist_ok=True)
        self.directory=tempfile.TemporaryDirectory(dir=base)
        self.root=Path(self.directory.name)
        self.index=codec.RecordIndex(self.root/'work.sqlite',source=fixture.SOURCE,
            session_id=fixture.SESSION,budget=fixture.admitted(self.root))
        self.state=codec.ExecutionState(self.index,'6'*64)
        self.index.execution=self.state

    def tearDown(self):
        self.index.close()
        self.directory.cleanup()

    def test_resource_incomplete_rolls_back_rows_cursor_and_credit(self):
        before=self.state.sequence(),self.state.cursor(),self.state.logical_digest()
        with self.assertRaises(codec.Incomplete):
            with self.state.quantum('load_input') as quantum:
                self.index.db.execute("INSERT INTO operation_state VALUES('fixture',?,?)",(codec.encoded(None),codec.encoded({'next':1})))
                quantum.credit('fixture',1)
                raise codec.Incomplete('indivisible operational allocation')
        self.assertEqual((self.state.sequence(),self.state.cursor(),self.state.logical_digest()),before)
        self.assertIsNone(self.index.db.execute("SELECT 1 FROM operation_state WHERE operation='fixture'").fetchone())
        self.assertEqual(self.index.db.execute('SELECT count(*) FROM credit_links').fetchone()[0],0)

    def test_private_quantum_cursor_survives_exact_sqlite_backup(self):
        with self.state.quantum('load_input'):
            self.index.db.execute("INSERT INTO operation_state VALUES('fixture',?,?)",(codec.encoded(None),codec.encoded({'next':2})))
        descriptor=self.state.backup(self.root/'backup.sqlite')
        with sqlite3.connect((self.root/'backup.sqlite').as_uri()+'?mode=ro&immutable=1',uri=True) as restored:
            self.assertEqual(restored.execute('SELECT private_commit,last_result,cursor FROM verifier_meta').fetchone(),
                             self.index.db.execute('SELECT private_commit,last_result,cursor FROM verifier_meta').fetchone())
            self.assertEqual(restored.execute("SELECT state FROM operation_state WHERE operation='fixture'").fetchone(),
                             (codec.encoded({'next':2}),))
        self.assertEqual(descriptor['verifier_logical_digest'],self.state.logical_digest())

    def test_logical_digest_stream_preserves_full_typed_row_preimage(self):
        import hashlib
        with self.state.quantum('load_input'):
            self.index.db.execute("INSERT INTO operation_state VALUES('unicode-fixture',?,?)",(codec.encoded(None),codec.encoded({'text':'é🧪'*9000})))
        order=codec.strict_json(next(line for line in self.state.sql_path.read_text().splitlines() if line.startswith('-- logical-order: ')).split(': ',1)[1])
        expected=hashlib.sha256(b'plurx-independent-verifier-state-v1\0')
        expected.update(bytes.fromhex(self.state.schema_sha256))
        for table,keys in order.items():
            columns=[r[1] for r in self.index.db.execute('PRAGMA table_info("'+table+'")')]
            def cell(value):return {'blob_hex':value.hex()} if type(value) is bytes else value
            for row in self.index.db.execute('SELECT * FROM "'+table+'" ORDER BY '+','.join('"'+key+'"' for key in keys)):
                data=codec.encoded([table,[cell(row[columns.index(key)]) for key in keys],[cell(value) for value in row]])
                expected.update(len(data).to_bytes(8,'big'));expected.update(data)
        self.assertEqual(self.state.logical_digest(),expected.hexdigest())

    def test_unregistered_operation_transition_cannot_commit_progress(self):
        before=self.state.sequence(),self.state.cursor(),self.state.logical_digest()
        with self.assertRaisesRegex(codec.Invalid,'transition'):
            with self.state.quantum('component_tlc'):
                self.fail('illegal transition entered its mutation body')
        self.assertEqual((self.state.sequence(),self.state.cursor(),self.state.logical_digest()),before)

    def test_local_checked_bits_without_uploaded_origin_chain_cannot_resume(self):
        with self.state.quantum('load_input'):
            self.index.db.execute("INSERT INTO operation_state VALUES('forged_checked',?,?)",(codec.encoded(None),codec.encoded(True)))
        auth=self.root/'authenticated.sqlite'
        with sqlite3.connect(auth) as db:
            db.execute('CREATE TABLE verifier_chain(unit_ordinal INTEGER PRIMARY KEY,progress_path TEXT,backup_root_path TEXT,work_unit_json BLOB)')
            db.execute('CREATE TABLE authenticated_members(unit_ordinal INTEGER,part_ordinal INTEGER,member_path TEXT,bytes INTEGER,sha256 TEXT,local_path TEXT)')
        resolver=codec.ContinuationResolver(auth,self.root,source=fixture.SOURCE,session_id=fixture.SESSION,budget=self.index.budget)
        try:
            with self.assertRaisesRegex(codec.Invalid,'missing/latest'):
                resolver.reconcile_chain(self.state,latest_progress_sha256='7'*64,latest_backup_sha256='8'*64,
                                         tlc_reparser=lambda *args:self.fail('no authenticated TLC execution exists'))
        finally:resolver.close()


if __name__=='__main__':
    unittest.main()
