"""Opt-in real root/nonroot filesystem access; no VM or study admission claim."""
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import sys
import tempfile
import time
import unittest

from validation.knowledge.study_driver import guest_stage_helper as helper
from validation.knowledge.study_driver import paired_runtime as paired


@unittest.skipUnless(os.environ.get("PLURX_KNOWLEDGE_ROOT_OWNERSHIP") == "1"
                     and os.environ.get("PLURX_KNOWLEDGE_ROOT_OWNERSHIP_ROOT")
                     and sys.platform.startswith("linux") and os.geteuid() == 0,
                     "requires separately admitted public root/nonroot Linux fixture")
class NativeRoleOwnershipTests(unittest.TestCase):
    def setUp(self):
        root=Path(os.environ["PLURX_KNOWLEDGE_ROOT_OWNERSHIP_ROOT"])
        self.assertTrue(root.is_absolute() and "/tmp" not in str(root))
        self.assertTrue(root.is_dir() and not root.is_symlink())
        self.assertEqual((root.stat().st_uid,stat.S_IMODE(root.stat().st_mode)),(0,0o700))
        directory=tempfile.TemporaryDirectory(prefix="owned-",dir=root)
        self.addCleanup(directory.cleanup);self.base=Path(directory.name)
        self.work=self.base/"work";self.control=self.base/"control"
        self.work.mkdir(mode=0o700);self.control.mkdir(mode=0o700)
        self.fs=helper.GuestStageFilesystem(self.work,self.control,session="a"*32)
        self.addCleanup(self.fs.shutdown)
        self.users={"protected":[10001,10001],"worker":[10002,10002]}

    def entries(self):
        rows=[]
        for role in sorted(helper.ROOTS):
            rows.append({"path":role,"kind":"dir","mode":0o755})
            if role.endswith("runtime"):
                rows.append({"path":role+"/nested","kind":"dir","mode":0o755})
                name=role+"/nested/private-build";mode=0o600
            else:name=role+"/source.txt";mode=0o444 if role in {"grader","worker-export"} else 0o644
            raw=("synthetic "+role).encode()
            rows.append({"path":name,"kind":"file","mode":mode,"size":len(raw),
                         "sha256":hashlib.sha256(raw).hexdigest(),"chunks":iter([raw])})
        return rows

    def as_role(self,cwd,user,action):
        # Root grants only this fixed synthetic working directory, then closes
        # the traversal capability and irreversibly drops all supplementary IDs.
        incoming,outgoing=os.pipe();pid=os.fork()
        if pid==0:
            os.close(incoming)
            try:
                os.chdir(cwd);os.setgroups([]);os.setgid(user[1]);os.setuid(user[0])
                action()
                raw=b'{"ok":true}'
            except BaseException as error:raw=json.dumps({"ok":False,"error":type(error).__name__}).encode()
            try:os.write(outgoing,raw[:4096])
            finally:os.close(outgoing);os._exit(0)
        os.close(outgoing);os.set_blocking(incoming,False);payload=bytearray();deadline=time.monotonic()+10
        try:
            while True:
                try:payload.extend(os.read(incoming,4096))
                except BlockingIOError:pass
                self.assertLessEqual(len(payload),4096)
                observed,status=os.waitpid(pid,os.WNOHANG)
                if observed:
                    self.assertEqual(status,0)
                    try:payload.extend(os.read(incoming,4096))
                    except BlockingIOError:pass
                    break
                if time.monotonic()>=deadline:
                    os.kill(pid,signal.SIGKILL);os.waitpid(pid,0)
                    self.fail("owned role-access child timed out")
                time.sleep(0.01)
        finally:os.close(incoming)
        self.assertEqual(json.loads(payload),{"ok":True})

    def test_root_publication_and_adoption_preserve_nonroot_access_and_private_parents(self):
        baseline="b"*32;current="c"*32
        self.fs.publish(baseline,self.entries(),pinned=True,role_users=self.users)
        roots=self.fs.publish(current,self.entries(),role_users=self.users)
        def source_write():
            Path("preflight-write").write_bytes(b"writable source")
            Path("preflight-write").unlink()
            try:Path("../worker-source/source.txt").read_bytes()
            except PermissionError:pass
            else:raise AssertionError("private stage parent crossed")
        self.as_role(self.work/current/"candidate",self.users["protected"],source_write)
        def private_build():
            if not Path("nested/private-build").read_bytes():raise AssertionError("0600 build unreadable")
            Path("nested/new-build").write_bytes(b"role writes nested build")
            Path("nested/new-build").unlink()
            try:Path("../grader/source.txt").read_bytes()
            except PermissionError:pass
            else:raise AssertionError("controller parent crossed")
        digests={"runtime":paired.measure_tree(self.work/baseline/"runtime"),
                 "worker_runtime":paired.measure_tree(self.work/baseline/"worker-runtime")}
        result=self.fs.copy_qualified_pair(current,baseline,roots["roots"],digests)
        self.assertEqual(result["role_users"],self.users)
        self.assertEqual(set(result["ownership"]),{"runtime","worker-runtime"})
        for role,user in (("runtime",self.users["protected"]),("worker-runtime",self.users["worker"])):
            tree=self.work/current/role;private=tree/"nested/private-build"
            self.assertEqual((private.stat().st_uid,private.stat().st_gid,stat.S_IMODE(private.stat().st_mode)),(*user,0o600))
            self.assertEqual((tree.stat().st_uid,tree.stat().st_gid),tuple(user))
            self.as_role(tree,user,private_build)
        for role in ("grader","worker-export"):
            info=(self.work/current/role/"source.txt").stat()
            self.assertEqual((info.st_uid,stat.S_IMODE(info.st_mode)),(0,0o444))
        for path in (self.work,self.control,self.work/current,self.work/baseline):
            self.assertEqual((path.stat().st_uid,stat.S_IMODE(path.stat().st_mode)),(0,0o700))


if __name__ == "__main__":unittest.main()
