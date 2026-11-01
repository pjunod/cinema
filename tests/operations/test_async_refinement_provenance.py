"""Synthetic transport fixtures; no solver, Rust producer or network execution."""
from __future__ import annotations
import copy
import hashlib
import io
from pathlib import Path
import sqlite3
import tempfile
import unittest
import zipfile

from validation import async_refinement_provenance as p
from validation import async_snapshot_transport as transport
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


ORIGIN = {'repository': 1, 'pr': 2, 'event_base': 'a' * 40, 'workflow_id': p.WORKFLOW,
          'event_name': 'workflow_dispatch', 'mode': 'unit', 'operation': 'execute',
          'run': 11, 'job': 12, 'task': 13, 'attempt': 1, 'dispatch_ordinal': 0}


def pseudo_random(size):
    out = bytearray()
    counter = 0
    while len(out) < size:
        out.extend(hashlib.sha256(counter.to_bytes(8, 'big')).digest())
        counter += 1
    return bytes(out[:size])


class CatalogTransportTests(unittest.TestCase):
    """Snapshot roots are rebuilt from authenticated content-addressed chunks only.

    The former whole-backup packer (a 32-slot inequality over one backup) is retired
    by the sealed capacity design; each fixture below keeps the behavior it pinned.
    """
    def setUp(self):
        scratch = ROOT/'.agent-proof-fixtures/transport'; scratch.mkdir(parents=True,exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch); self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.cp = checkpoint()
        for name in ('work', 'cas', 'restore'):
            (self.directory/name).mkdir(mode=0o700)
        self.backup = self.directory/'work/backup.sqlite'
        db = sqlite3.connect(self.backup)
        db.execute('CREATE TABLE identities(id INTEGER PRIMARY KEY AUTOINCREMENT,value TEXT)')
        db.execute('CREATE TABLE fifo(ticket INTEGER PRIMARY KEY,product INTEGER)')
        db.execute('CREATE TABLE payload(value BLOB)')
        db.execute('INSERT INTO identities(value) VALUES(?)',('exact raw UUID',))
        db.execute('INSERT INTO fifo VALUES(7,1)')
        db.execute('INSERT INTO payload VALUES(?)',(pseudo_random(1300 * 1024),))  # genuinely spans two chunks
        db.commit(); db.close()
        self.backup.chmod(0o600)
        cp = self.cp['record']
        self.descriptor = {'source':SOURCE,'session_id':SESSION,'unit_ordinal':0,
             'checkpoint_sha256':p.digest(p.canonical(self.cp)),'previous_checkpoint_sha256':None,
             'resource_plan_sha256':cp['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id'],
             'catalog_commit':1,'catalog_logical_digest':cp['catalog_logical_digest'],
             'semantic_manifest_root':cp['page_root'],'snapshot_boundary':'post_checkpoint_pre_upload',
             'format':'sqlite3','backup_method':'sqlite_backup_api_v1',
             'catalog_bytes':self.backup.stat().st_size,'catalog_sha256':p.file_sha(self.backup)}
        self.binding = {'source':SOURCE,'session_id':SESSION,'unit_ordinal':0,
                        'resource_plan_sha256':cp['resource_plan_sha256'],'initial_image_sha256':cp['initial_image_id']}
        self.charged = 0
        self.cas = transport.OwnedDirectory(self.directory/'cas', self.directory, lambda: None)
        self.addCleanup(self.cas.close)
        self.work = transport.OwnedDirectory(self.directory/'work', self.directory, lambda: None)
        self.addCleanup(self.work.close)
        self.restore = transport.OwnedDirectory(self.directory/'restore', self.directory, lambda: None)
        self.addCleanup(self.restore.close)
        self.inventory = transport.ContentInventory(sqlite3.connect(':memory:'), self.cas,
                                                    self.charge, self.discharge)

    def charge(self, size):
        self.charged += size

    def discharge(self, size):
        self.charged -= size

    def snapshot(self, cp=None, descriptor=None):
        return transport.snapshot_root(self.inventory, self.binding, ORIGIN, cp or self.cp,
                                       descriptor or self.descriptor, self.work, 'backup.sqlite')

    def content(self):
        rows = self.inventory.db.execute('SELECT kind,sha256,bytes FROM content').fetchall()
        return {sha: self.inventory.bytes({'kind': kind, 'sha256': sha, 'bytes': size}) for kind, sha, size in rows}

    @staticmethod
    def resolver(store):
        def resolve(reference):
            if reference['sha256'] not in store:
                raise transport.SnapshotTransportError('content absent from authenticated parts')
            return store[reference['sha256']]
        return resolve

    def test_multipart_roundtrip_preserves_private_ids_fifo_allocator(self):
        root,_ = self.snapshot()
        self.assertGreaterEqual(root['chunk_count'],2)
        target = transport.restore_image(root,self.resolver(self.content()),self.restore,'staged.sqlite',
                                         self.charge,self.discharge)
        self.assertEqual(p.file_sha(target),p.file_sha(self.backup))
        db = sqlite3.connect('file:'+str(target)+'?mode=ro',uri=True)
        try:
            self.assertEqual(db.execute('SELECT * FROM fifo').fetchall(),[(7,1)])
            self.assertEqual(db.execute('SELECT seq FROM sqlite_sequence').fetchone(),(1,))
        finally: db.close()

    def test_missing_part_truncated_chunk_and_mixed_checkpoint_never_resume(self):
        root,_ = self.snapshot()
        store = self.content()
        chunks = [sha for kind, sha in ((k, s_) for k, s_, _ in self.inventory.db.execute(
            'SELECT kind,sha256,bytes FROM content')) if kind == 'snapshot_chunk']
        for mutate in ('missing','truncated','checkpoint'):
            with self.subTest(mutate=mutate):
                selected = dict(store); name = mutate+'.sqlite'
                before = self.charged
                if mutate == 'missing': selected.pop(chunks[-1])
                elif mutate == 'truncated': selected[chunks[-1]] = selected[chunks[-1]][:-1]
                if mutate == 'checkpoint':
                    cp = copy.deepcopy(self.cp); cp['record']['source']['commit'] = '0'*40
                    with self.assertRaises(transport.SnapshotTransportError):
                        self.snapshot(cp)
                else:
                    with self.assertRaises(transport.SnapshotTransportError):
                        transport.restore_image(root,self.resolver(selected),self.restore,name,
                                                self.charge,self.discharge)
                self.assertFalse((self.directory/'restore'/name).exists())
                self.assertEqual(list((self.directory/'restore').iterdir()),[])  # no partial writer-visible file
                self.assertEqual(self.charged,before)  # the failed restore returned its reservation

    def test_closed_root_rejects_extra_fields_and_old_snapshot_boundary(self):
        root,_ = self.snapshot()
        p.auxiliary(copy.deepcopy(root),ROOT)
        for field,value in (('future_artifact_id',1),('snapshot_boundary','live_copy')):
            bad = dict(root); bad[field] = value
            with self.assertRaises(receipts.ReceiptError):p.auxiliary(bad,ROOT)

    def test_whole_backup_packer_and_part_restorer_stay_retired(self):
        for name in ('pack_catalog','pack_verifier','restore_catalog'):
            self.assertFalse(hasattr(p,name),name)

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
        value={'schema_version':2,'registration_sha256':'a'*64,'source_archive_sha256':'b'*64,
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
