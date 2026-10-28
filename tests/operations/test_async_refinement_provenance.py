"""Synthetic transport fixtures; no solver, Rust producer or network execution."""
from __future__ import annotations
import copy
import io
from pathlib import Path
import sqlite3
import tempfile
import unittest
import zipfile

from validation import async_refinement_provenance as p
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
SOURCE = {k: 'a' * (40 if k in ('commit', 'tree') else 64) for k in
          ('commit','tree','source_closure_sha256','driver_sha256','finite_inventory_sha256',
           'proof_contract_sha256','toolchain_sha256','workflow_sha256')}
SESSION = '00000000-0000-4000-8000-000000000001'
COUNTS = {k: 0 for k in ('physical_rows','episodes','stores','oracle_edges','products','candidates',
          'demands','product_edges','fair_pairs','enabledness','expansions','frontier',
          'monitor_nodes','monitor_edges','components','boundaries')}


def checkpoint():
    return {'schema_version':6,'kind':'Checkpoint','record':{
        'session_id':SESSION,'sequence':0,'source':SOURCE,'initial_image_id':'b'*64,
        'resource_plan_sha256':'c'*64,'unit_ordinal':0,'previous_checkpoint':None,
        'committed_transaction':1,'oldest_pending_ticket':None,'counts':COUNTS,'contexts':[],
        'page_root':'d'*64,'catalog_logical_digest':'e'*64}}


class CatalogTransportTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT/'.agent-proof-fixtures/transport'; scratch.mkdir(parents=True,exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch); self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.cp = checkpoint()
        self.backup = self.directory/'backup.sqlite'
        db = sqlite3.connect(self.backup)
        db.execute('CREATE TABLE identities(id INTEGER PRIMARY KEY AUTOINCREMENT,value TEXT)')
        db.execute('CREATE TABLE fifo(ticket INTEGER PRIMARY KEY,product INTEGER)')
        db.execute('INSERT INTO identities(value) VALUES(?)',('exact raw UUID',))
        db.execute('INSERT INTO fifo VALUES(7,1)'); db.commit(); db.close()
        cp = self.cp['record']
        self.descriptor = {'source':SOURCE,'session_id':SESSION,'unit_ordinal':0,
             'checkpoint_sha256':p.digest(p.canonical(self.cp)),'previous_checkpoint_sha256':None,
             'resource_plan_sha256':cp['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id'],
             'catalog_commit':1,'catalog_logical_digest':cp['catalog_logical_digest'],
             'semantic_manifest_root':cp['page_root'],'snapshot_boundary':'post_checkpoint_pre_upload',
             'format':'sqlite3','backup_method':'sqlite_backup_api_v1',
             'catalog_bytes':self.backup.stat().st_size,'catalog_sha256':p.file_sha(self.backup)}

    def package(self):
        root, plan = p.pack_catalog(self.backup,self.descriptor,self.cp,self.directory/'package',ROOT)
        self.addCleanup(plan.close)
        receipts_by_part = {}
        parts = {}
        for n in range(plan.preflight()):
            members = plan.manifest(n); parts[n] = plan.directory(n)
            receipts_by_part[n] = {'schema_version':6,'kind':'ArtifactReceipt','record':{
                'artifact_id':n+1,'session_id':SESSION,'unit_ordinal':0,'run':1,'job':2,'task':3,
                'attempt':1,'source':SOURCE,'name':f'part-{n}','expired':False,'zip_sha256':'f'*64,
                'zip_bytes':1,'extracted_bytes':sum(m['bytes'] for m in members),'member_count':len(members),
                'members':members,'upload_log_sha256':'f'*64,'event_payload_sha256':'f'*64,
                'expected_prior_checkpoint':None,'produced_checkpoint':p.digest(p.canonical(self.cp))}}
        return root,parts,receipts_by_part

    def test_multipart_roundtrip_preserves_private_ids_fifo_allocator(self):
        root,parts,rs = self.package()
        self.assertGreaterEqual(len(parts),2)
        target = p.restore_catalog(root,parts,rs,self.cp,self.directory/'staged.sqlite',ROOT)
        self.assertEqual(p.file_sha(target),p.file_sha(self.backup))
        db = sqlite3.connect('file:'+str(target)+'?mode=ro',uri=True)
        try:
            self.assertEqual(db.execute('SELECT * FROM fifo').fetchall(),[(7,1)])
            self.assertEqual(db.execute('SELECT seq FROM sqlite_sequence').fetchone(),(1,))
        finally: db.close()

    def test_missing_part_truncated_chunk_and_mixed_checkpoint_never_resume(self):
        root,parts,rs = self.package()
        for mutate in ('missing','truncated','checkpoint'):
            with self.subTest(mutate=mutate):
                selected = dict(parts); receipts_copy = copy.deepcopy(rs); cp = copy.deepcopy(self.cp)
                if mutate == 'missing': selected.pop(max(selected))
                elif mutate == 'truncated':
                    name = next(pth for pth in selected[max(selected)].rglob('*.bin'))
                    original=name.read_bytes();name.write_bytes(original[:-1])
                else: cp['record']['source']['commit'] = '0'*40
                with self.assertRaises((receipts.ReceiptError,sqlite3.IntegrityError)):
                    p.restore_catalog(root,selected,receipts_copy,cp,self.directory/(mutate+'.sqlite'),ROOT)
                self.assertFalse((self.directory/(mutate+'.sqlite')).exists())
                if mutate=='truncated':name.write_bytes(original)

    def test_closed_root_rejects_extra_fields_and_old_snapshot_boundary(self):
        root,_,_ = self.package()
        for field,value in (('future_artifact_id',1),('snapshot_boundary','live_copy')):
            bad = dict(root); bad[field] = value
            with self.assertRaises(receipts.ReceiptError):p.auxiliary(bad,ROOT)

    def test_digest_collision_never_replaces_full_payload(self):
        index = p.DependencyIndex(self.directory/'objects.sqlite'); self.addCleanup(index.close)
        ref = {'kind':'source_file','id':'a.json','sha256':p.digest(b'one')}
        index.add(ref,b'one',self.backup)
        with self.assertRaises(receipts.ReceiptError):index.add(ref,b'two',self.backup)
        self.assertEqual(index.resolve(ref)[0],b'one')
        missing = {'kind':'source_file','id':'missing.json','sha256':'0'*64}
        index.need(ref,missing)
        with self.assertRaises(receipts.ReceiptError):index.closure([ref])

    def test_duplicate_json_zip_alias_and_unreferenced_member_fail(self):
        with self.assertRaises(receipts.ReceiptError):p.decode(b'{"a":1,"a":2}')
        archive = self.directory/'alias.zip'
        with zipfile.ZipFile(archive,'w') as z:z.writestr('../escape',b'x')
        with self.assertRaises(receipts.ReceiptError):
            p.unpack(archive,self.directory/'extract',[{'path':'safe','bytes':1,'sha256':p.digest(b'x')}])

    def test_atomic_install_requires_private_and_independent_roots(self):
        staged = self.directory/'staged.sqlite'; staged.write_bytes(self.backup.read_bytes())
        old = self.directory/'catalog.sqlite'; old.write_bytes(b'prior valid checkpoint')
        with self.assertRaises(receipts.ReceiptError):p.atomic_install(staged,old,{}, {},self.cp)
        self.assertEqual(old.read_bytes(),b'prior valid checkpoint')
        self.assertTrue(staged.is_file())




class ControlCarrierTests(unittest.TestCase):
    def framed(self):
        value={'registration_sha256':'a'*64,'source_archive_sha256':'b'*64,
               'session_id':SESSION,'unit_ordinal':0,'role':'unit_checkpoint',
               'origin':{'run':7,'job':8,'task':9,'attempt':1},'payload':'x'*4000}
        binding=p.control_binding(value);sha=p.digest(p.canonical(value))
        lines=['ASYNC-PROOF-FINALIZER-BEGIN 1 '+binding]+p.control_frame(value)+[
            'ASYNC-PROOF-FINALIZER-DONE 1 '+binding+' '+sha]
        return value,lines

    def test_canonical_multichunk_control_roundtrip(self):
        value,lines=self.framed()
        actual,sha,begin,done=p.parse_control_frame(lines)
        self.assertEqual(actual,value);self.assertEqual(sha,p.digest(p.canonical(value)))
        self.assertEqual((begin,done),(0,len(lines)-1))

    def test_duplicate_truncated_reordered_or_unbound_frame_refuses(self):
        _,lines=self.framed()
        variants=[lines[:-1],lines+lines,lines[:2]+[lines[3],lines[2]]+lines[4:],
                  [lines[0].replace(' 1 ',' 2 ',1)]+lines[1:],lines[:-2]+lines[-1:]]
        for candidate in variants:
            with self.subTest(candidate=candidate):
                with self.assertRaises(receipts.ReceiptError):p.parse_control_frame(candidate)

    def test_valid_frame_never_overrides_failed_or_retried_actual_producer(self):
        import json
        _,frame=self.framed()
        reg={'source':{'commit':'a'*40},'repository':3}
        event={'repository':{'id':3},'inputs':{'mode':'unit','registration_sha256':p.digest(p.canonical(reg))}}
        class API:
            state='success';attempt=1
            def get(self,path):
                return {'id':7,'workflow_id':p.WORKFLOW,'event':'workflow_dispatch','commit_sha':'a'*40,
                        'repository':{'id':3},'event_payload':json.dumps(event),'status':self.state}
            def pages(self,path):
                return [{'id':8,'task_id':9,'name':p.PRODUCER_JOB,'attempt':self.attempt,
                         'status':self.state,'run_id':7,'repo_id':3,'log':frame}]
        api=API()
        p.producer(api,7,reg,'unit',('success',))
        for state,attempt in (('running',1),('failure',1),('cancelled',1),('success',2)):
            api.state=state;api.attempt=attempt
            with self.subTest(state=state,attempt=attempt),self.assertRaises(receipts.ReceiptError):
                p.producer(api,7,reg,'unit',('success',))

    def test_current_control_cannot_be_a_member_of_its_own_zip(self):
        import tempfile
        scratch=ROOT/'.agent-proof-fixtures/carrier';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as name:
            path=Path(name)
            from validation import async_refinement_session as session
            for kind in ('WorkUnit','ArtifactReceipt','Completion'):
                p.write(path/'competing.json',{'schema_version':6,'kind':kind,'record':{'unit_ordinal':4}})
                with self.subTest(kind=kind),self.assertRaises(receipts.ReceiptError):
                    session.reject_current_control_members({0:path},4)

    def test_native_slice_hash_excludes_later_carrier(self):
        name='part-zero';ident=73;size=100;zip_sha='c'*64
        lines=['With the provided path, there will be 1 file uploaded','Artifact name is valid!',
            'Root directory input is valid!','Beginning upload of artifact content to blob storage',
            'Uploaded bytes 100','Finished uploading artifact content to blob storage!',
            'SHA256 hash of uploaded artifact zip is '+zip_sha,'Finalizing artifact upload',
            f'Artifact {name}.zip successfully finalized. Artifact ID {ident}',
            f'Artifact {name} has been successfully uploaded! Final size is {size} bytes. Artifact ID is {ident}']
        expected=p.digest(b'plurx-async-upload-transaction-v1\0'+('\n'.join(lines)+'\n').encode())
        _,frame=self.framed()
        actual,archive,begin,end=p.upload_slice(lines+frame,name,ident,size)
        self.assertEqual(actual,expected);self.assertEqual(archive,zip_sha)
        self.assertEqual((begin,end),(0,len(lines)-1));self.assertNotEqual(actual,archive)
        p.validate_upload_order(lines+frame,[{'name':name,'id':ident,'size_in_bytes':size}],len(lines))
        with self.assertRaises(receipts.ReceiptError):
            p.validate_upload_order(lines+lines+frame,[{'name':name,'id':ident,'size_in_bytes':size}],2*len(lines))

if __name__ == '__main__':unittest.main()
