"""Synthetic guest-stage filesystem boundaries; no VM or study inputs."""
import hashlib
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation.knowledge.study_driver import guest_stage_helper as helper
from validation.knowledge.study_driver import paired_runtime
from validation.knowledge.study_driver import runtime_bindings


class GuestStageFilesystemTests(unittest.TestCase):
    def setUp(self):
        fixtures = Path(__file__).resolve().parents[2] / "target/agent-fixtures/.hermes/hdr-collaboration/workspaces/guest-stage-helper"
        fixtures.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="owned-", dir=fixtures)
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.work = self.base / "work"
        self.control = self.base / "control"
        self.work.mkdir(mode=0o700)
        self.control.mkdir(mode=0o700)
        self.fs = helper.GuestStageFilesystem(self.work, self.control, session="a" * 32)
        self.addCleanup(self.fs.shutdown)

    @staticmethod
    def file_entry(path, body, *, mode=0o644, digest=None):
        return {"path": path, "kind": "file", "mode": mode, "size": len(body),
                "sha256": digest or hashlib.sha256(body).hexdigest(), "chunks": iter([body]) if body else iter(())}

    @staticmethod
    def entries(*, paired=False, runtime=b"old-runtime", worker_runtime=b"old-worker"):
        rows = [
            {"path": "candidate", "kind": "dir", "mode": 0o700},
            {"path": "grader", "kind": "dir", "mode": 0o700},
            {"path": "runtime", "kind": "dir", "mode": 0o700},
            {"path": "candidate/main.rs", "kind": "file", "mode": 0o644, "size": 5,
             "sha256": hashlib.sha256(b"main\n").hexdigest(), "chunks": iter([b"main\n"])},
            {"path": "grader/check.py", "kind": "file", "mode": 0o644, "size": 8,
             "sha256": hashlib.sha256(b"check()\n").hexdigest(), "chunks": iter([b"check()\n"])},
            {"path": "runtime/state", "kind": "file", "mode": 0o600, "size": len(runtime),
             "sha256": hashlib.sha256(runtime).hexdigest(), "chunks": iter([runtime]) if runtime else iter(())},
        ]
        if paired:
            rows.extend([
                {"path": "worker-source", "kind": "dir", "mode": 0o700},
                {"path": "worker-export", "kind": "dir", "mode": 0o700},
                {"path": "worker-runtime", "kind": "dir", "mode": 0o700},
                {"path": "worker-source/main.rs", "kind": "file", "mode": 0o644, "size": 5,
                 "sha256": hashlib.sha256(b"main\n").hexdigest(), "chunks": iter([b"main\n"])},
                {"path": "worker-export/answer", "kind": "file", "mode": 0o644, "size": 1,
                 "sha256": hashlib.sha256(b"x").hexdigest(), "chunks": iter([b"x"])},
                {"path": "worker-runtime/state", "kind": "file", "mode": 0o600, "size": len(worker_runtime),
                 "sha256": hashlib.sha256(worker_runtime).hexdigest(), "chunks": iter([worker_runtime]) if worker_runtime else iter(())},
            ])
        return rows

    @staticmethod
    def role_users(*, paired=False):
        identity = [65534, 65534] if os.geteuid() == 0 else [os.getuid(), os.getgid()]
        users = {"protected": identity}
        if paired:
            users["worker"] = list(identity)
        return users

    def new_stage(self, *, paired=False, runtime=b"old-runtime", worker_runtime=b"old-worker",
                  pinned=False, entries=None):
        stage_id = os.urandom(16).hex()
        result = self.fs.publish(stage_id, self.entries(paired=paired, runtime=runtime,
                                                        worker_runtime=worker_runtime) if entries is None else entries,
                                 pinned=pinned, role_users=self.role_users(paired=paired))
        return stage_id, result

    def test_publish_and_observe_keep_equal_relative_names_in_separate_tree_objects(self):
        decoy = self.base / "host-mirror" / "candidate"
        decoy.mkdir(parents=True)
        (decoy / "main.rs").write_bytes(b"host copy")
        stage_id, published = self.new_stage()

        observed = self.fs.observe(stage_id, "candidate", relative="main.rs")

        self.assertEqual(observed["entries"][0]["sha256"], hashlib.sha256(b"main\n").hexdigest())
        guest_root = published["roots"]["candidate"]
        host_stat = decoy.stat()
        self.assertNotEqual((guest_root["device"], guest_root["inode"]), (host_stat.st_dev, host_stat.st_ino))
        self.assertEqual(published["session"], "a" * 32)
        self.assertEqual(observed["root"], guest_root)
        self.assertEqual(observed["bytes"], 5)

    def test_publish_rejects_traversal_links_duplicates_bad_digest_and_unbounded_members(self):
        cases = []
        traversal = self.entries()
        traversal[3] = self.file_entry("candidate/../escape", b"x")
        cases.append(("traversal", traversal))
        link = self.entries()
        link.append({"path": "candidate/link", "kind": "symlink", "mode": 0o777, "target": "outside"})
        cases.append(("link-kind", link))
        duplicate = self.entries()
        duplicate.append(dict(duplicate[0]))
        cases.append(("duplicate", duplicate))
        bad_digest = self.entries()
        bad_digest[3] = self.file_entry("candidate/main.rs", b"main\n", digest="0" * 64)
        cases.append(("digest", bad_digest))
        special = self.entries()
        special[3]["mode"] = 0o4755
        cases.append(("special-mode", special))
        too_large = self.entries()
        too_large[3] = {"path": "candidate/huge", "kind": "file", "mode": 0o600,
                        "size": helper.SOURCE_BYTES + 1, "sha256": "0" * 64, "chunks": iter(())}
        cases.append(("file-bound", too_large))

        for name, manifest in cases:
            with self.subTest(case=name), self.assertRaises(helper.GuestStageError):
                self.fs.publish(os.urandom(16).hex(), manifest, role_users=self.role_users())

    def test_real_immutable_protected_and_worker_exports_publish_as_0444(self):
        protected = self.base / "protected-export"
        worker = self.base / "worker-export-source"
        protected.mkdir(mode=0o700)
        worker.mkdir(mode=0o700)
        runtime_bindings.write_grader(protected, "protected-driver.py", b"assert True\n")
        runtime_bindings.write_grader(worker, "rpc-contract.json", b"{\"ok\":true}\n")
        paired_runtime.readable_tree(protected)
        paired_runtime.readable_tree(worker)

        protected_file = protected / "protected-driver.py"
        worker_file = worker / "rpc-contract.json"
        self.assertEqual(os.stat(protected_file).st_mode & 0o777, 0o444)
        self.assertEqual(os.stat(worker_file).st_mode & 0o777, 0o444)
        manifest = self.entries(paired=True)
        manifest.extend((
            self.file_entry("grader/protected-driver.py", protected_file.read_bytes(),
                            mode=os.stat(protected_file).st_mode & 0o777),
            self.file_entry("worker-export/rpc-contract.json", worker_file.read_bytes(),
                            mode=os.stat(worker_file).st_mode & 0o777),
        ))

        stage_id, published = self.new_stage(paired=True, entries=manifest)
        self.assertEqual(set(published["roots"]), set(helper.ROOTS))
        for role, filename, source in (
            ("grader", "protected-driver.py", protected_file),
            ("worker-export", "rpc-contract.json", worker_file),
        ):
            with self.subTest(role=role):
                observed = self.fs.observe(stage_id, role)
                member = next(row for row in observed["entries"] if row["path"] == filename)
                self.assertEqual(member["mode"], 0o444)
                self.assertEqual(member["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())

    def test_observe_refuses_guest_symlinks_and_special_files(self):
        stage_id, _ = self.new_stage()
        candidate = self.work / stage_id / "candidate"
        (candidate / "escape").symlink_to(self.base, target_is_directory=True)
        with self.assertRaises(helper.GuestStageError):
            self.fs.observe(stage_id, "candidate")

        (candidate / "escape").unlink()
        os.mkfifo(candidate / "pipe", 0o600)
        with self.assertRaises(helper.GuestStageError):
            self.fs.observe(stage_id, "candidate")

    def test_outside_sentinel_requires_original_host_challenge_and_closes_before_stage(self):
        stage_id, _ = self.new_stage()
        name = "outside-" + os.urandom(16).hex()
        challenge = b"c" * 32
        forged = {"scope": "outside-all-role-mounts", "stage": stage_id, "name": name,
                  "device": self.work.stat().st_dev, "inode": 1, "sha256": hashlib.sha256(challenge).hexdigest(),
                  "bytes": 32, "mode": 0o600, "present": True}
        with self.assertRaises(helper.GuestStageError):
            self.fs.outside_sentinel(stage_id, name, expected=forged)

        issued = self.fs.outside_sentinel(stage_id, name, content=challenge)
        self.assertEqual(issued["scope"], "outside-all-role-mounts")
        self.assertEqual(issued["sha256"], hashlib.sha256(challenge).hexdigest())
        self.assertEqual(issued["mode"], 0o600)
        self.assertEqual([row["path"] for row in self.fs.observe(stage_id, "candidate")["entries"]], ["main.rs"])
        with self.assertRaises(helper.GuestStageError):
            self.fs.close(stage_id)
        for key, value in (("sha256", "0" * 64), ("inode", issued["inode"] + 1)):
            changed = dict(issued, **{key: value})
            with self.subTest(forged_field=key), self.assertRaises(helper.GuestStageError):
                self.fs.outside_sentinel(stage_id, name, expected=changed)
        removed = self.fs.outside_sentinel(stage_id, name, expected=issued)
        self.assertEqual(removed, dict(issued, present=False, removed=True))
        self.fs.close(stage_id)

    def test_outside_sentinel_refuses_missing_replaced_inode_and_changed_digest(self):
        stage_id, _ = self.new_stage()
        name = "outside-" + os.urandom(16).hex()
        issued = self.fs.outside_sentinel(stage_id, name, content=b"m" * 32)
        sentinel = self.work / stage_id / ".isolation" / name
        sentinel.unlink()
        with self.assertRaises(FileNotFoundError):
            self.fs.outside_sentinel(stage_id, name, expected=issued)

        other_stage, _ = self.new_stage()
        other_name = "outside-" + os.urandom(16).hex()
        inode_issued = self.fs.outside_sentinel(other_stage, other_name, content=b"i" * 32)
        inode_path = self.work / other_stage / ".isolation" / other_name
        replacement = inode_path.with_name(other_name + "-replacement")
        inode_path.rename(replacement)
        inode_path.write_bytes(b"i" * 32)
        inode_path.chmod(0o600)
        with self.assertRaises(helper.GuestStageError):
            self.fs.outside_sentinel(other_stage, other_name, expected=inode_issued)

        digest_stage, _ = self.new_stage()
        digest_name = "outside-" + os.urandom(16).hex()
        digest_issued = self.fs.outside_sentinel(digest_stage, digest_name, content=b"d" * 32)
        digest_path = self.work / digest_stage / ".isolation" / digest_name
        digest_path.write_bytes(b"x" * 32)
        with self.assertRaises(helper.GuestStageError):
            self.fs.outside_sentinel(digest_stage, digest_name, expected=digest_issued)

    def test_qualified_copy_refuses_a_late_guest_root_swap_and_poisons_stage(self):
        baseline_id, _ = self.new_stage(runtime=b"qualified-runtime", pinned=True)
        stage_id, published = self.new_stage(runtime=b"old-runtime")
        original = self.work / stage_id / "runtime"
        captured = self.work / stage_id / "runtime-captured-by-test"
        real_copy = helper._copy_runtime

        def swap_then_copy(source, target, *args, **kwargs):
            target = Path(target)
            if target == original:
                target.rename(captured)
                target.mkdir(mode=0o700)
            return real_copy(source, target, *args, **kwargs)

        with patch.object(helper, "_copy_runtime", side_effect=swap_then_copy):
            with self.assertRaises(RuntimeError):
                self.fs.copy_qualified_pair(stage_id, baseline_id, published["roots"],
                    {"runtime": paired_runtime.measure_tree(self.work / baseline_id / "runtime")})
        self.assertEqual(self.fs.stages[stage_id]["phase"], "poisoned")
        self.assertEqual((captured / "state").read_bytes(), b"old-runtime")
        with self.assertRaises(helper.GuestStageError):
            self.fs.observe(stage_id, "candidate")
        self.assertEqual((self.work / baseline_id / "runtime" / "state").read_bytes(), b"qualified-runtime")

    def test_closed_unpinned_stage_retirement_removes_only_owned_stage_and_keeps_receipts(self):
        stage_id, published = self.new_stage()
        sibling = self.work / "unrelated-sibling"
        sibling.write_bytes(b"preserve")

        closed = self.fs.close(stage_id)
        authorization = {
            "receipt_sha256": "a" * 64,
            "checkpoint_sha256": "b" * 64,
            "event": "acceptance_finished",
        }
        archive_sha256 = "c" * 64
        retired = self.fs.retire(stage_id, authorization, expected_roots=published["roots"],
                                 archive_sha256=archive_sha256)

        session_receipts = self.control / ("a" * 32)
        self.assertEqual(retired["retired"], True)
        self.assertEqual(retired["authorization"], authorization)
        self.assertEqual(retired["archive_sha256"], archive_sha256)
        self.assertFalse((self.work / stage_id).exists())
        self.assertEqual(sibling.read_bytes(), b"preserve")
        self.assertTrue((session_receipts / (stage_id + "-closed.json")).is_file())
        self.assertTrue((session_receipts / (stage_id + "-retirement-intent.json")).is_file())
        self.assertTrue((session_receipts / (stage_id + "-retired.json")).is_file())
        self.assertEqual(closed["stage"], stage_id)
        with self.assertRaises(helper.GuestStageError):
            self.fs.observe(stage_id, "candidate")
        with self.assertRaises(helper.GuestStageError):
            self.fs.retire(stage_id, authorization, expected_roots=published["roots"],
                           archive_sha256=archive_sha256)

    def test_partial_paired_copy_failure_poisoned_before_roots_can_be_reused(self):
        baseline_id, _ = self.new_stage(paired=True, runtime=b"new-runtime", worker_runtime=b"new-worker", pinned=True)
        stage_id, published = self.new_stage(paired=True, runtime=b"old-runtime", worker_runtime=b"old-worker")
        real_copy = helper._copy_runtime

        def fail_worker_copy(source, target, *args, **kwargs):
            if Path(target) == self.work / stage_id / "worker-runtime":
                raise OSError("synthetic second-copy failure")
            return real_copy(source, target, *args, **kwargs)

        digests = {
            "runtime": paired_runtime.measure_tree(self.work / baseline_id / "runtime"),
            "worker_runtime": paired_runtime.measure_tree(self.work / baseline_id / "worker-runtime"),
        }
        with patch.object(helper, "_copy_runtime", side_effect=fail_worker_copy):
            with self.assertRaises(OSError):
                self.fs.copy_qualified_pair(stage_id, baseline_id, published["roots"], digests)
        self.assertEqual((self.work / stage_id / "runtime" / "state").read_bytes(), b"new-runtime")
        with self.assertRaises(helper.GuestStageError):
            self.fs.observe(stage_id, "candidate")
        self.assertEqual(self.fs.observe(baseline_id, "runtime")["entries"][0]["sha256"],
                         hashlib.sha256(b"new-runtime").hexdigest())


if __name__ == "__main__":
    unittest.main()
