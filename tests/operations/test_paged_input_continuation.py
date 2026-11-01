"""Durable input-preparation fixtures; definitions only until final review."""
from __future__ import annotations

import copy
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import test_paged_codec as transport
import test_paged_temporal as fixture
import paged_codec as codec


class PagedInputContinuationFixtures(unittest.TestCase):
    def setUp(self):
        base=fixture.PACKAGE.parents[1]/'.async-proof-sessions'/'paged-fixtures'
        base.mkdir(parents=True,exist_ok=True)
        self.directory=tempfile.TemporaryDirectory(dir=base)
        self.root=Path(self.directory.name)
        plan=copy.deepcopy(fixture.admitted(self.root).plan)
        plan['unit_record_allowance']=12
        plan['plan_sha256']=codec.digest(codec.envelope('ResourcePlan',plan))
        self.plan=plan
        self.index=codec.RecordIndex(self.root/'work.sqlite',source=fixture.SOURCE,
            session_id=fixture.SESSION,budget=fixture.granted_budget(plan,self.root))
        self.state=codec.ExecutionState(self.index,'6'*64)
        self.index.execution=self.state
        self.auth=self.root/'authenticated.sqlite'
        with sqlite3.connect(self.auth) as db:
            db.execute('CREATE TABLE objects(kind TEXT,key BLOB,digest TEXT,path TEXT,PRIMARY KEY(kind,key))')
            db.execute('CREATE TABLE roots(kind TEXT,key BLOB,digest TEXT,PRIMARY KEY(kind,key))')
            db.execute('CREATE TABLE authenticated_members(unit_ordinal INTEGER,member_path TEXT,bytes INTEGER,sha256 TEXT,local_path TEXT,PRIMARY KEY(unit_ordinal,member_path))')
            db.execute('CREATE TABLE verifier_chain(unit_ordinal INTEGER PRIMARY KEY,progress_path TEXT,backup_root_path TEXT,work_unit_json BLOB,execution_origin_json BLOB)')
        self.index.resolver_path=self.auth

    def tearDown(self):
        self.index.close();self.directory.cleanup()

    def member(self,value,name):
        data=codec.encoded(value);path=self.root/name;path.write_bytes(data)
        reference=codec.ref(value)
        with sqlite3.connect(self.auth) as db:
            db.execute('INSERT INTO objects VALUES(?,?,?,?)',(reference['kind'],codec.encoded(reference['id']),reference['sha256'],str(path)))
            db.execute('INSERT INTO authenticated_members VALUES(0,?,?,?,?)',(name,len(data),codec.sha_bytes(data),str(path)))
        return reference

    def request(self,pages=8,rows_per_page=1):
        maker=transport.PagedCodecFixtures()
        children=[]
        for page in range(pages):
            values=[maker.rows(page*rows_per_page+i+1) for i in range(rows_per_page)]
            children.append(self.member(maker.page(values),f'page-{page}.json'))
        counts={k:0 for k in codec.COUNT_KINDS};counts.update(physical_rows=pages*rows_per_page,frontier=0)
        node=codec.envelope('ManifestNode',{'session_id':fixture.SESSION,'source':fixture.SOURCE,
            'sha256':'0'*64,'level':0,'children':children,'counts':counts,'utf8_bytes':0})
        node['record']['utf8_bytes']=len(codec.transport_bytes(node,measure=True))
        node['record']['sha256']=codec.digest(node)
        root=self.member(node,'manifest.json')
        cp=codec.envelope('Checkpoint',{'session_id':fixture.SESSION,'source':fixture.SOURCE,'sequence':0,
            'initial_image_id':'6'*64,'resource_plan_sha256':self.plan['plan_sha256'],'unit_ordinal':0,
            'previous_checkpoint':None,'committed_transaction':1,'oldest_pending_ticket':None,
            'counts':counts,'contexts':[],'page_root':root['id'],'catalog_logical_digest':'7'*64})
        reference=self.member(cp,'checkpoint.json')
        with sqlite3.connect(self.auth) as db:
            db.execute('INSERT INTO roots VALUES(?,?,?)',(reference['kind'],codec.encoded(reference['id']),reference['sha256']))
        with self.state.quantum('load_input',framing=True):
            before=self.state.cursor()
            self.index.db.execute('INSERT INTO unit_work VALUES(?,?,?,?,?,?,?,?,0)',(1,codec.encoded(None),codec.digest(cp),0,
                '0'*64,codec.encoded(before),self.state.sequence()[1]+1,codec.encoded(cp)))
        return {'checkpoint':cp,'dependency_index':str(self.auth),'initial_image':{'record':{'image_sha256':'6'*64}}}

    def next_quantum_budget(self):
        budget=fixture.granted_budget(copy.deepcopy(self.plan),self.root)
        self.index.budget=self.state.budget=budget

    def finish_dependencies(self,request):
        attempts=0
        while attempts<100:
            attempts+=1
            self.next_quantum_budget()
            try:self.index.prepare_input(request)
            except codec.Incomplete:continue
            return attempts
        self.fail('bounded preparation made no finite progress')

    def test_large_input_commits_finite_loading_and_tally_prefixes(self):
        request=self.request()
        self.assertGreater(self.finish_dependencies(request),1)
        self.assertEqual(self.index.raw_count('PhysicalRows'),8)
        self.assertEqual(self.index.db.execute('SELECT status FROM input_scopes').fetchone(),('dependencies_ready',))
        self.assertEqual(self.index.manifest_counts(request['checkpoint']['record']['page_root'])['physical_rows'],8)
        self.assertGreater(self.index.db.execute("SELECT count(*) FROM input_work WHERE pass='manifest_counts' AND status='done'").fetchone()[0],0)

    def test_second_unit_never_readds_committed_raw_records(self):
        request=self.request()
        self.index.budget.plan['unit_record_allowance']=8
        with self.assertRaises(codec.Incomplete):self.index.prepare_input(request)
        old=set(self.index.db.execute('SELECT kind,key FROM records'))
        original=self.index.add
        def new_only(value):
            self.assertNotIn((value['kind'],codec.canonical(codec.record_key(value))),old)
            return original(value)
        self.next_quantum_budget()
        with patch.object(self.index,'add',side_effect=new_only):
            try:self.index.prepare_input(request)
            except codec.Incomplete:pass
        self.assertTrue(old.issubset(set(self.index.db.execute('SELECT kind,key FROM records'))))

    def test_oversized_whole_page_rolls_back_every_nested_record(self):
        request=self.request(pages=1,rows_per_page=20)
        self.next_quantum_budget()
        with self.assertRaises(codec.Incomplete):self.index.prepare_input(request)
        self.assertEqual(self.index.raw_count('PhysicalRows'),0)
        self.assertEqual(self.index.raw_count('Page'),0)
        self.assertEqual(self.index.db.execute("SELECT count(*) FROM input_members WHERE kind='Page'").fetchone(),(0,))

    def test_dependency_and_tally_progress_never_clear_on_restart(self):
        request=self.request()
        previous=set()
        for _ in range(100):
            self.next_quantum_budget()
            try:self.index.prepare_input(request)
            except codec.Incomplete:pass
            current=set(self.index.db.execute("SELECT scope,pass,owner_key,kind,key FROM input_work WHERE status='done'"))
            self.assertTrue(previous.issubset(current));previous=current
            if self.index.db.execute('SELECT status FROM input_scopes').fetchone()==('dependencies_ready',):break
        else:self.fail('root-bound dependency preparation failed to finish')

    def test_changed_checkpoint_cannot_replace_pending_scope(self):
        request=self.request()
        with self.assertRaises(codec.Incomplete):self.index.prepare_input(request)
        changed=copy.deepcopy(request);changed['checkpoint']['record']['committed_transaction']+=1
        self.next_quantum_budget()
        with self.assertRaisesRegex(codec.Invalid,'frozen checkpoint'):self.index.prepare_input(changed)

    def test_missing_loading_result_or_member_coverage_rejects_restore(self):
        request=self.request(pages=1)
        self.finish_dependencies(request)
        owner=self.index.db.execute('SELECT owner_result FROM input_scopes').fetchone()[0]
        self.index.db.execute("UPDATE input_members SET owner_result=? WHERE kind='Page'",(owner,));self.index.db.commit()
        resolver=codec.ContinuationResolver(self.auth,self.root,source=fixture.SOURCE,session_id=fixture.SESSION,budget=self.index.budget)
        try:
            with self.assertRaisesRegex(codec.Invalid,'loading result'):resolver.reconcile_input_state(self.state)
        finally:resolver.close()

    def test_cross_runner_readonly_view_does_not_materialize_again(self):
        request=self.request(pages=1);self.finish_dependencies(request)
        self.next_quantum_budget()
        descriptor=self.state.backup(self.root/'backup.sqlite')
        view=codec.RecordIndex.readonly_view(self.root/'backup.sqlite',source=fixture.SOURCE,
            session_id=fixture.SESSION,budget=fixture.granted_budget(copy.deepcopy(self.plan),self.root),resolver_path=self.auth)
        try:
            view.bind_scope(self.index.scope)
            self.assertEqual(view.raw_count('PhysicalRows'),1)
            self.assertEqual(view.get('PhysicalRows',1),self.index.get('PhysicalRows',1))
            self.assertEqual(codec.sha_bytes((self.root/'backup.sqlite').read_bytes()),descriptor['work_db_sha256'])
            self.assertEqual(view.budget.records,0)
        finally:view.close()

    def test_historical_raw_cache_does_not_contaminate_current_scope(self):
        request=self.request(pages=1);self.finish_dependencies(request)
        self.next_quantum_budget()
        self.index.add(transport.PagedCodecFixtures().rows(999))
        self.index.db.commit()
        self.assertEqual(self.index.raw_count('PhysicalRows'),2)
        self.assertEqual(self.index.count('PhysicalRows'),1)
        self.assertEqual([x['id'] for x in self.index.find('PhysicalRows')],[1])

    def test_unpublished_proposal_cannot_be_selected_as_authority(self):
        request=self.request(pages=1);self.finish_dependencies(request)
        self.next_quantum_budget()
        self.index.propose(transport.PagedCodecFixtures().rows(999),'check_input')
        with self.index.input_quantum('check_input'):
            self.index.db.execute('INSERT INTO active_selected VALUES(?,?)',('PhysicalRows',codec.canonical(999)))
            self.index.db.execute('INSERT INTO active_authoritative VALUES(?,?)',('PhysicalRows',codec.canonical(999)))
            self.index.save_input_cursor('orphan_check')
        with self.assertRaisesRegex(codec.Invalid,'unpublished'):
            self.index.check_input_scope()

    def test_changed_member_bytes_reject_before_loading_credit(self):
        request=self.request(pages=1)
        path=self.root/'page-0.json';path.write_bytes(path.read_bytes()+b' ')
        with self.assertRaisesRegex(codec.Invalid,'bytes changed'):
            self.index.prepare_input(request)
        self.assertEqual(self.index.raw_count('Page'),0)

    def test_indivisible_restore_authentication_has_no_progress_credit(self):
        request=self.request(pages=1);self.finish_dependencies(request)
        before=self.state.sequence(),self.state.logical_digest(),self.state.cursor()
        member=self.index.db.execute("SELECT member_ref FROM input_members WHERE kind='Page'").fetchone()[0]
        self.index.budget.plan['unit_byte_allowance']=self.index.budget.bytes
        with self.assertRaises(codec.Incomplete):self.index.member_path(codec.strict_json(member))
        # Restore authentication failure cannot create a verifier quantum.
        self.index.budget.plan['unit_byte_allowance']=2**30
        self.assertEqual((self.state.sequence(),self.state.logical_digest(),self.state.cursor()),before)

    def test_missing_lazy_blob_file_confers_no_path_authority(self):
        path=self.root/'source.txt';path.write_bytes(b'exact source')
        member={'unit_ordinal':0,'member_path':path.name,'bytes':path.stat().st_size,'sha256':codec.sha_bytes(path.read_bytes())}
        with sqlite3.connect(self.auth) as db:
            db.execute('INSERT INTO authenticated_members VALUES(0,?,?,?,?)',(path.name,member['bytes'],member['sha256'],str(path)))
        self.index.db.execute('INSERT INTO blob_descriptors VALUES(?,?,?,?,?)',('source_file',codec.canonical(path.name),member['sha256'],member['bytes'],codec.encoded(member)))
        self.index.db.commit();path.unlink()
        with self.assertRaisesRegex(codec.Invalid,'missing lazy'):self.index.blob('source_file',path.name)


if __name__=='__main__':
    unittest.main()
