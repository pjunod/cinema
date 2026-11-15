"""Finite legacy migration requires real reader-class absence, never a stop boolean."""
import contextlib
import fcntl
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT=Path(__file__).resolve().parents[2]
SCRIPT=ROOT/'scripts/source-custody-reader-stop'
loader=importlib.machinery.SourceFileLoader('source_custody_reader_stop',str(SCRIPT))
spec=importlib.util.spec_from_loader(loader.name,loader)
helper=importlib.util.module_from_spec(spec)
loader.exec_module(helper)
NODE='11111111-1111-4111-8111-111111111111'
CUSTODY='22222222-2222-4222-8222-222222222222'
JOB='33333333-3333-4333-8333-333333333333'
BOOT='44444444-4444-4444-8444-444444444444'
CLAIM='55555555-5555-4555-8555-555555555555'


def activation():
    return {'version':1,'machine_id':'','boot_id':'','node_id':NODE,'records':[
        {'kind':'background','token':{'custody_id':CUSTODY,'owner':{'kind':'job','token':{
            'job_id':JOB,'node_id':NODE,'boot_id':BOOT,'claim_id':CLAIM,
            'fence':3,'revision':4,'lease_expires_ms':1}}}}]}


def encoded(value):
    return (json.dumps(value,sort_keys=True)+'\n').encode()


def measurement():
    row={'tasks':2,'executables':1,'matched_readers':0,'snapshot_sha256':'1'*64}
    return {'backend':'linux_initial_pidns_executable_markers_v1','passes':[row,dict(row)],
            'images':[{'device':1,'inode':2,'bytes':300,'sha256':'2'*64,'matched_roles':[]}]}


class ReaderStopHelperTests(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory()
        self.root=Path(self.temporary.name).resolve()
        self.data=self.root/'data';self.data.mkdir(mode=0o700)
        (self.data/'node.id').write_text(NODE)
        self.lock=self.data/'.plurxd.lock';self.lock.write_text('1\n');self.lock.chmod(0o600)
        self.journal=self.data/'source-custody-recovery'/'v1';self.journal.mkdir(parents=True,mode=0o700)
        self.active=self.journal/('background-'+CUSTODY+'.json')
        self.raw=encoded(activation());self.active.write_bytes(self.raw);self.active.chmod(0o600)
        self.manifest=self.root/'manifest.json'
        manifest={'source_sha256':helper.CLOSURE['native_source_sha256'],'tools':{
            'scanner':{'sha256':helper.CLOSURE['scanner_sha256']},
            'renderer':{'sha256':helper.CLOSURE['renderer_sha256']}}}
        self.manifest.write_bytes(encoded(manifest));self.manifest.chmod(0o600)
        self.enrollment=self.root/'host.json'
        self.enrollment.write_bytes(encoded({'version':1,'node_id':NODE,'data_dir':str(self.data),
            'machine_id_sha256':'a'*64,'boot_id':BOOT,'initial_pid_namespace_inode':1,
            'initial_user_namespace_inode':2,'host_root_device_inode':[1,2]}))
        self.enrollment.chmod(0o600)
        self.stack=contextlib.ExitStack()
        # Test directories are owned by the test account. Production keeps the
        # literal UID0 policy; this only models trusted files in isolated fixtures.
        self.stack.enter_context(mock.patch.object(helper,'ADMIN_UID',os.getuid()))
        self.stack.enter_context(mock.patch.object(helper,'admin_directory'))
        self.stack.enter_context(mock.patch.object(helper.os,'geteuid',return_value=0))
        self.stack.enter_context(mock.patch.object(helper.sys,'platform','linux'))
        self.stack.enter_context(mock.patch.object(helper,'ACTIVATION_SHA256',hashlib.sha256(self.raw).hexdigest()))
        closure=dict(helper.CLOSURE);closure['manifest_sha256']=hashlib.sha256(self.manifest.read_bytes()).hexdigest()
        self.stack.enter_context(mock.patch.object(helper,'CLOSURE',closure))
        self.census_impl=helper.perform_census
        self.census=self.stack.enter_context(mock.patch.object(helper,'perform_census',return_value=({'boot_id':BOOT},measurement())))
        self.publisher=self.stack.enter_context(mock.patch.object(helper,'publish_fixed'))

    def tearDown(self):
        self.stack.close();self.temporary.cleanup()

    def verify(self,publish=False):
        return helper.verify(self.data,NODE,CUSTODY,self.enrollment,self.manifest,publish)

    def test_check_holds_launch_fence_but_does_not_publish_or_change_activation(self):
        def inspect(_):
            fd=os.open(self.lock,os.O_RDWR)
            try:
                with self.assertRaises(BlockingIOError):fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            finally:os.close(fd)
            return {'boot_id':BOOT},measurement()
        self.census.side_effect=inspect
        proof=self.verify()
        self.assertEqual(proof['owner']['claim_id'],CLAIM)
        self.assertEqual(proof['activation_sha256'],hashlib.sha256(self.raw).hexdigest())
        self.assertEqual(self.active.read_bytes(),self.raw)
        self.assertEqual(self.lock.read_text(),'1\n')
        self.publisher.assert_not_called()

    def test_only_explicit_publish_uses_the_actual_census(self):
        proof=self.verify(True)
        self.census.assert_called_once()
        published=json.loads(self.publisher.call_args.args[0])
        self.assertEqual(published,proof)
        self.assertEqual(published['census'],measurement())

    def test_unknown_census_cannot_publish(self):
        self.census.side_effect=helper.Refused('unreadable live executable')
        with self.assertRaisesRegex(helper.Refused,'unreadable'):self.verify(True)
        self.publisher.assert_not_called()

    def test_incomplete_census_cannot_publish(self):
        original_open=os.open
        def opened(path,*args,**kwargs):
            return original_open(self.root if path=='/proc' else path,*args,**kwargs)
        fake=mock.Mock();fake.collect.side_effect=[[],[]];fake.cache={};fake.images=[]
        self.census.side_effect=self.census_impl
        with mock.patch.object(helper.os,'open',side_effect=opened),mock.patch.object(helper,'host_context',return_value={}),mock.patch.object(helper,'Census',return_value=fake):
            with self.assertRaisesRegex(helper.Refused,'accounting'):self.verify(True)
        self.publisher.assert_not_called()

    def test_worker_writable_manifest_cannot_publish(self):
        self.manifest.chmod(0o666)
        with self.assertRaisesRegex(helper.Refused,'protected by host root'):self.verify(True)
        self.census.assert_not_called();self.publisher.assert_not_called()

    def test_unreviewed_activation_is_rejected_before_census(self):
        self.active.write_bytes(self.raw+b'\n')
        with self.assertRaisesRegex(helper.Refused,'reviewed finite'):self.verify(True)
        self.census.assert_not_called();self.publisher.assert_not_called()

    def test_foreign_enrollment_and_manifest_are_rejected(self):
        for target,change in [(self.enrollment,lambda d:d.update(node_id=BOOT)),
                              (self.manifest,lambda d:d.update(source_sha256='0'*64))]:
            original=target.read_bytes();value=json.loads(original);change(value);target.write_bytes(encoded(value))
            with self.subTest(target=target.name),self.assertRaises(helper.Refused):self.verify(True)
            target.write_bytes(original)
        self.census.assert_not_called();self.publisher.assert_not_called()

    def test_activation_path_substitution_after_census_refuses_publication(self):
        def replace(_):
            self.active.rename(self.active.with_suffix('.old'))
            self.active.write_bytes(self.raw);self.active.chmod(0o600)
            return {'boot_id':BOOT},measurement()
        self.census.side_effect=replace
        with self.assertRaisesRegex(helper.Refused,'changed|substituted'):self.verify(True)
        self.publisher.assert_not_called()

    def test_symlink_and_hardlinked_activation_are_rejected(self):
        saved=self.journal/'saved.json';self.active.rename(saved)
        self.active.symlink_to(saved)
        with self.assertRaises(OSError):self.verify(True)
        self.active.unlink();os.link(saved,self.active)
        with self.assertRaisesRegex(helper.Refused,'single-link'):self.verify(True)
        self.census.assert_not_called()

    def test_existing_launch_holder_prevents_census(self):
        fd=os.open(self.lock,os.O_RDWR)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with self.assertRaises(BlockingIOError):self.verify(True)
        finally:os.close(fd)
        self.census.assert_not_called();self.publisher.assert_not_called()

    def test_no_prior_census_json_option_exists(self):
        with open(os.devnull,'w') as sink,contextlib.redirect_stderr(sink):
            with self.assertRaises(SystemExit):helper.main(['--census-json','prior.json'])
        self.census.assert_not_called();self.publisher.assert_not_called()

    def test_publish_never_overwrites_a_different_proof(self):
        directory=self.root/'proofs';directory.mkdir();fd=os.open(directory,helper.DIRECTORY_FLAGS)
        try:
            with mock.patch.object(helper,'local_filesystem',return_value=True):
                helper.publish_at(fd,b'first')
                helper.publish_at(fd,b'first')
                with self.assertRaisesRegex(helper.Refused,'different proof'):helper.publish_at(fd,b'second')
            self.assertEqual((directory/helper.PROOF_NAME).read_bytes(),b'first')
            self.assertEqual((directory/helper.PROOF_NAME).stat().st_nlink,1)
        finally:os.close(fd)


class ExecutableCensusTests(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory();self.root=Path(self.temporary.name).resolve()
    def tearDown(self):self.temporary.cleanup()

    def test_streamed_markers_cross_chunks_and_dedup_keeps_the_executable_pinned(self):
        executable=self.root/'exe'
        executable.write_bytes(b'x'*(helper.CHUNK-5)+b' '.join(helper.MARKERS.values()))
        directory=os.open(self.root,helper.DIRECTORY_FLAGS);census=helper.Census()
        try:
            image=census.image(directory,1,1)
            self.assertEqual(image['matched_roles'],sorted(helper.MARKERS))
            self.assertIs(census.image(directory,1,1),image)
            self.assertEqual(len(census.cache),1)
            held=next(iter(census.cache.values()))[0]
            executable.unlink()
            self.assertEqual(os.fstat(held).st_size,image['bytes'])
        finally:census.close();os.close(directory)

    def test_unknown_live_executable_does_not_become_an_absence(self):
        (self.root/'status').write_text('Tgid:\t1\nPid:\t1\nState:\tS\n')
        directory=os.open(self.root,helper.DIRECTORY_FLAGS);census=helper.Census()
        try:
            with self.assertRaisesRegex(helper.Refused,'no inspectable executable'):census.image(directory,1,1)
        finally:census.close();os.close(directory)

    def test_zombie_leader_with_live_threads_fails_closed(self):
        for pid in {1,os.getpid()}:(self.root/str(pid)).mkdir()
        directory=self.root/'1';fields=['0']*22;fields[0]='Z';fields[19]='1'
        (directory/'stat').write_text('1 (fixture) '+' '.join(fields))
        (directory/'status').write_text('Tgid:\t1\nPid:\t1\nThreads:\t2\nKthread:\t0\n')
        fd=os.open(self.root,helper.DIRECTORY_FLAGS);census=helper.Census()
        try:
            with self.assertRaisesRegex(helper.Refused,'zombie/exiting group'):census.collect(fd)
        finally:census.close();os.close(fd)

    @unittest.skipUnless(sys.platform=='linux','Linux kernel executable handle control')
    def test_real_subprocess_executable_is_read_through_the_kernel_handle(self):
        process=subprocess.Popen([sys.executable,'-I','-B','-c','import time;time.sleep(20)'])
        census=helper.Census();directory=None
        try:
            directory=os.open('/proc/'+str(process.pid),helper.DIRECTORY_FLAGS)
            with open('/proc/'+str(process.pid)+'/exe','rb') as executable:marker=executable.read(32)
            with mock.patch.object(helper,'MARKERS',{'owned_fixture':marker}):
                image=census.image(directory,process.pid,process.pid)
            self.assertEqual(image['matched_roles'],['owned_fixture'])
        finally:
            census.close()
            if directory is not None:os.close(directory)
            process.terminate();process.wait(timeout=5)


if __name__=='__main__':unittest.main()
