"""A host reboot challenge carries exact ownership, never a timeout unlock."""

from contextlib import redirect_stdout
import copy
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import uuid


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/source-custody-recovery"
loader = importlib.machinery.SourceFileLoader("source_custody_recovery", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
recovery = importlib.util.module_from_spec(spec)
loader.exec_module(recovery)

NODE = "recovery-node"
OLD_BOOT = "11111111-1111-4111-8111-111111111111"
NEW_BOOT = "22222222-2222-4222-8222-222222222222"
MACHINE = "0123456789abcdef0123456789abcdef"

# Separate processes exercise kernel flock release and real abrupt termination,
# rather than asserting the publication implementation's statement order.
PUBLISHER_WORKER = r"""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import signal
import sys
import time

loader = importlib.machinery.SourceFileLoader("publisher_helper", sys.argv[1])
spec = importlib.util.spec_from_loader(loader.name, loader)
helper = importlib.util.module_from_spec(spec)
loader.exec_module(helper)
directory, name, payload, mode, gate = sys.argv[2:]
if gate:
    deadline = time.monotonic() + 5
    while not Path(gate).exists():
        if time.monotonic() > deadline:
            raise SystemExit("publisher gate timed out")
        time.sleep(0.01)
rename = helper.os.rename
def interrupted_rename(*args, **kwargs):
    if mode == "before":
        os.kill(os.getpid(), signal.SIGKILL)
    result = rename(*args, **kwargs)
    if mode == "after":
        os.kill(os.getpid(), signal.SIGKILL)
    return result
helper.os.rename = interrupted_rename
try:
    with helper.open_directory(Path(directory)) as directory_fd:
        helper.publish_new(directory_fd, name, payload.encode())
except Exception as error:
    print(type(error).__name__, file=sys.stderr)
    raise SystemExit(3)
"""


def foreground(node=NODE):
    return {"owner_id": str(uuid.uuid4()), "lifetime_id": "playback-lifetime",
            "session_incarnation": str(uuid.uuid4()), "producer_attempt": "producer-attempt",
            "node_id": node, "boot_id": OLD_BOOT, "file_id": 12, "source_size": 1234,
            "source_mtime": 5678, "source_generation": "private-generation",
            "policy": "rolling"}


def background(kind, node=NODE):
    if kind == "job":
        return {"kind": kind, "token": {"job_id": str(uuid.uuid4()), "node_id": node,
                "boot_id": OLD_BOOT, "claim_id": str(uuid.uuid4()), "fence": 3,
                "revision": 4, "lease_expires_ms": 1234}}
    if kind == "analysis":
        return {"kind": kind, "request_id": str(uuid.uuid4()), "node_id": node, "fence": 3}
    if kind == "offline":
        return {"kind": kind, "package_id": str(uuid.uuid4()), "node_id": node,
                "claim_generation": 3}
    if kind == "read_attestation":
        return {"kind": kind, "lease": {"resource": f"subtitle-read-attest:{node}:12",
                "owner_node_id": node, "fence": 3, "revision": 4,
                "expires_at_unix_ms": 1234}, "file_id": 12, "source_size": 1234,
                "source_mtime": 5678}
    raise AssertionError(kind)


class RecoveryHelperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.data = self.root / "data"
        self.data.mkdir()
        self.database = self.data / "plurx.db"
        self.connection = sqlite3.connect(self.database)
        self.connection.executescript("""
            CREATE TABLE foreground_source_owners (
                owner_id TEXT PRIMARY KEY, identity_json TEXT, fence INTEGER,
                lease_expires_at_ms INTEGER, settlement_proof TEXT
            );
            CREATE TABLE background_source_custody (
                custody_id TEXT PRIMARY KEY, owner_json TEXT, settlement_proof TEXT
            );
        """)
        self.old_identity = {"machine_id": MACHINE, "boot_id": OLD_BOOT}
        self.new_identity = {"machine_id": MACHINE, "boot_id": NEW_BOOT}

    def tearDown(self):
        self.connection.close()
        self.temporary.cleanup()

    def add_foreground(self, owner=None, proof=None):
        owner = owner or foreground()
        self.connection.execute("INSERT INTO foreground_source_owners VALUES (?, ?, ?, ?, ?)",
                                (owner["owner_id"], json.dumps(owner), 9, 9876, proof))
        self.connection.commit()
        return {"kind": "foreground", "token": {"owner": owner, "fence": 9,
                "lease_expires_at_ms": 9876}}

    def add_background(self, owner=None, proof=None):
        owner = owner or background("job")
        custody_id = str(uuid.uuid4())
        self.connection.execute("INSERT INTO background_source_custody VALUES (?, ?, ?)",
                                (custody_id, json.dumps(owner), proof))
        self.connection.commit()
        return {"kind": "background", "token": {"custody_id": custody_id, "owner": owner}}

    def prepare(self):
        with mock.patch.object(recovery, "local_identity", return_value=self.old_identity):
            return recovery.prepare(self.data, self.database, NODE)

    def challenge_path(self, challenge):
        return self.data / "source-custody-recovery/challenges" / f"{challenge}.json"

    def proof_path(self, challenge):
        return self.data / "source-custody-recovery/v1" / f"legacy-{challenge}.json"

    def publisher(self, directory, payload, mode="normal", gate=""):
        return subprocess.Popen(
            [sys.executable, "-c", PUBLISHER_WORKER, str(SCRIPT), str(directory),
             "proof.json", payload, mode, str(gate)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

    def finish_publishers(self, processes):
        results = []
        try:
            for process in processes:
                stdout, stderr = process.communicate(timeout=10)
                results.append((process.returncode, stdout, stderr))
            return results
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait()

    def rewrite_challenge(self, challenge, value):
        path = self.challenge_path(challenge)
        path.write_text(json.dumps(value))
        path.chmod(0o600)

    def test_prepare_captures_both_families_and_all_independent_owner_variants(self):
        expected = [self.add_foreground()]
        expected.extend(self.add_background(background(kind))
                        for kind in ("job", "analysis", "read_attestation", "offline"))
        self.add_foreground(foreground("other-node"))
        self.add_background(background("analysis", "other-node"))
        self.add_background(proof="joined")
        before = list(self.connection.iterdump())
        challenge, count = self.prepare()
        value = json.loads(self.challenge_path(challenge).read_bytes())
        self.assertEqual(count, 5)
        self.assertEqual(value["records"][0], expected[0])
        self.assertCountEqual(value["records"][1:], expected[1:])
        self.assertEqual(list(self.connection.iterdump()), before)
        self.assertEqual(self.challenge_path(challenge).stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.challenge_path(challenge).parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.challenge_path(challenge).stat().st_uid, self.data.stat().st_uid)
        self.assertEqual(self.challenge_path(challenge).stat().st_gid, self.data.stat().st_gid)
        self.assertFalse(self.proof_path(challenge).exists())

    def test_same_boot_never_publishes_recovery_evidence(self):
        self.add_background()
        challenge, _ = self.prepare()
        with mock.patch.object(recovery, "local_identity", return_value=self.old_identity):
            with self.assertRaisesRegex(recovery.RecoveryError, "boot is unchanged"):
                recovery.apply(self.data, NODE, challenge)
        self.assertFalse(self.proof_path(challenge).exists())
        self.assertTrue(self.challenge_path(challenge).exists())

    def test_verified_reboot_publishes_exact_envelope_and_is_idempotent(self):
        self.add_foreground()
        self.add_background()
        challenge, _ = self.prepare()
        before = list(self.connection.iterdump())
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            self.assertEqual(recovery.apply(self.data, NODE, challenge), 2)
            self.assertEqual(recovery.apply(self.data, NODE, challenge), 2)
        self.assertEqual(self.proof_path(challenge).read_bytes(),
                         self.challenge_path(challenge).read_bytes())
        self.assertEqual(list(self.connection.iterdump()), before)
        self.assertEqual(self.proof_path(challenge).stat().st_mode & 0o777, 0o600)
        self.assertFalse((self.data / "source-custody-settlements").exists())

    def test_other_machine_and_other_node_cannot_use_a_challenge(self):
        self.add_background()
        challenge, _ = self.prepare()
        other_identity = {**self.new_identity, "machine_id": "f" * 32}
        with mock.patch.object(recovery, "local_identity", return_value=other_identity):
            with self.assertRaisesRegex(recovery.RecoveryError, "different machine"):
                recovery.apply(self.data, NODE, challenge)
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            with self.assertRaisesRegex(recovery.RecoveryError, "another node"):
                recovery.apply(self.data, "other-node", challenge)
        self.assertFalse(self.proof_path(challenge).exists())

    def test_readonly_snapshot_includes_uncheckpointed_wal_without_mutation(self):
        self.connection.execute("PRAGMA journal_mode=WAL")
        self.connection.execute("PRAGMA wal_autocheckpoint=0")
        expected = self.add_background()
        files = (self.database, Path(str(self.database) + "-wal"))
        before = [path.read_bytes() for path in files]
        challenge, _ = self.prepare()
        value = json.loads(self.challenge_path(challenge).read_bytes())
        self.assertEqual(value["records"], [expected])
        self.assertEqual([path.read_bytes() for path in files], before)

    def test_missing_or_excessive_owners_refuse_without_challenge(self):
        with self.assertRaisesRegex(recovery.RecoveryError, "1..128"):
            self.prepare()
        self.assertFalse((self.data / "source-custody-recovery").exists())
        for _ in range(recovery.MAX_RECORDS + 1):
            self.add_background(background("analysis"))
        with self.assertRaisesRegex(recovery.RecoveryError, "more than 128"):
            self.prepare()
        self.assertFalse((self.data / "source-custody-recovery").exists())

    def test_malformed_unknown_and_oversized_challenges_are_retained(self):
        self.add_background()
        challenge, _ = self.prepare()
        path = self.challenge_path(challenge)
        original = json.loads(path.read_bytes())
        variants = [b"{broken", b'{"version":1,"version":1}', b"x" * (recovery.MAX_BYTES + 1)]
        future = copy.deepcopy(original)
        future["version"] = 2
        variants.append(json.dumps(future).encode())
        extra = copy.deepcopy(original)
        extra["approved"] = True
        variants.append(json.dumps(extra).encode())
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            for raw in variants:
                with self.subTest(raw_length=len(raw)):
                    path.write_bytes(raw)
                    with self.assertRaises(recovery.RecoveryError):
                        recovery.apply(self.data, NODE, challenge)
                    self.assertEqual(path.read_bytes(), raw)
                    self.assertFalse(self.proof_path(challenge).exists())

    def test_companion_is_not_an_independent_background_recovery_owner(self):
        self.add_background({"kind": "companion", "token": {
            "owner": foreground(), "fence": 1, "lease_expires_at_ms": 100,
        }})
        with self.assertRaisesRegex(recovery.RecoveryError, "companion custody is foreground-owned"):
            self.prepare()

    def test_foreign_token_duplicate_and_bad_counter_are_rejected(self):
        self.add_background()
        challenge, _ = self.prepare()
        original = json.loads(self.challenge_path(challenge).read_bytes())
        foreign = copy.deepcopy(original)
        foreign["records"][0]["token"]["owner"]["token"]["node_id"] = "other-node"
        duplicate = copy.deepcopy(original)
        duplicate["records"].append(copy.deepcopy(duplicate["records"][0]))
        bad_counter = copy.deepcopy(original)
        bad_counter["records"][0]["token"]["owner"]["token"]["fence"] = True
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            for value in (foreign, duplicate, bad_counter):
                self.rewrite_challenge(challenge, value)
                with self.assertRaises(recovery.RecoveryError):
                    recovery.apply(self.data, NODE, challenge)
        self.assertFalse(self.proof_path(challenge).exists())

    def test_symlinks_nonprivate_files_and_hardlinks_are_refused(self):
        self.add_background()
        challenge, _ = self.prepare()
        path = self.challenge_path(challenge)
        saved = path.read_bytes()
        outside = self.root / "outside.json"
        outside.write_bytes(saved)
        outside.chmod(0o600)
        path.unlink()
        path.symlink_to(outside)
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            with self.assertRaises(OSError):
                recovery.apply(self.data, NODE, challenge)
            path.unlink()
            os.link(outside, path)
            with self.assertRaisesRegex(recovery.RecoveryError, "unlinked elsewhere"):
                recovery.apply(self.data, NODE, challenge)
            path.unlink()
            path.write_bytes(saved)
            path.chmod(0o644)
            with self.assertRaisesRegex(recovery.RecoveryError, "owner-only"):
                recovery.apply(self.data, NODE, challenge)
        self.assertEqual(outside.read_bytes(), saved)

    def test_directory_and_database_symlinks_are_refused(self):
        self.add_background()
        link = self.root / "db-link"
        link.symlink_to(self.database)
        with mock.patch.object(recovery, "local_identity", return_value=self.old_identity):
            with self.assertRaises(OSError):
                recovery.prepare(self.data, link, NODE)
        outside = self.root / "outside"
        outside.mkdir()
        (self.data / "source-custody-recovery").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(OSError):
            self.prepare()
        self.assertEqual(list(outside.iterdir()), [])

    def test_proof_conflict_never_overwrites_existing_content(self):
        self.add_background()
        challenge, _ = self.prepare()
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            recovery.apply(self.data, NODE, challenge)
            self.proof_path(challenge).write_bytes(b"other evidence")
            with self.assertRaisesRegex(recovery.RecoveryError, "conflicts"):
                recovery.apply(self.data, NODE, challenge)
        self.assertEqual(self.proof_path(challenge).read_bytes(), b"other evidence")

    def test_publication_lost_ack_retries_one_link_evidence_idempotently(self):
        self.add_background()
        challenge, _ = self.prepare()
        rename = recovery.os.rename

        def lost_ack(*args, **kwargs):
            rename(*args, **kwargs)
            raise OSError("publication acknowledgement lost after rename")

        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            with mock.patch.object(recovery.os, "rename", side_effect=lost_ack):
                with self.assertRaisesRegex(OSError, "acknowledgement lost"):
                    recovery.apply(self.data, NODE, challenge)
            path = self.proof_path(challenge)
            self.assertEqual(path.stat().st_nlink, 1)
            self.assertEqual(path.read_bytes(), self.challenge_path(challenge).read_bytes())
            self.assertEqual(recovery.apply(self.data, NODE, challenge), 1)
            self.assertEqual(path.stat().st_nlink, 1)

    def test_publisher_sigkill_before_or_after_rename_never_strands_two_link_proof(self):
        for mode in ("before", "after"):
            with self.subTest(crash=mode):
                directory = self.root / mode
                directory.mkdir(mode=0o700)
                process = self.publisher(directory, "complete recovery proof", mode=mode)
                result = self.finish_publishers([process])[0]
                self.assertEqual(result[0], -signal.SIGKILL, result)
                path = directory / "proof.json"
                if mode == "after":
                    self.assertEqual(path.read_bytes(), b"complete recovery proof")
                    self.assertEqual(path.stat().st_nlink, 1)
                else:
                    self.assertFalse(path.exists())
                # A fresh process must reacquire the abandoned kernel lock and
                # either publish or acknowledge the exact already-published bytes.
                result = self.finish_publishers([
                    self.publisher(directory, "complete recovery proof"),
                ])[0]
                self.assertEqual(result[0], 0, result)
                self.assertEqual(path.read_bytes(), b"complete recovery proof")
                self.assertEqual(path.stat().st_nlink, 1)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_concurrent_publishers_share_exact_evidence_and_refuse_conflicts(self):
        directory = self.root / "publishers"
        directory.mkdir(mode=0o700)
        gate = self.root / "publishers.start"
        payload = "A" * (64 * 1024)
        processes = [self.publisher(directory, payload, gate=gate) for _ in range(4)]
        gate.touch()
        results = self.finish_publishers(processes)
        self.assertTrue(all(result[0] == 0 for result in results), results)
        path = directory / "proof.json"
        self.assertEqual(path.read_bytes(), payload.encode())
        self.assertEqual(path.stat().st_nlink, 1)

        conflict_dir = self.root / "conflicting-publishers"
        conflict_dir.mkdir(mode=0o700)
        conflict_gate = self.root / "conflicts.start"
        processes = [self.publisher(conflict_dir, value, gate=conflict_gate)
                     for value in ("first complete proof", "second complete proof")]
        conflict_gate.touch()
        results = self.finish_publishers(processes)
        self.assertEqual(sorted(result[0] for result in results), [0, 3], results)
        path = conflict_dir / "proof.json"
        self.assertIn(path.read_bytes(), (b"first complete proof", b"second complete proof"))
        self.assertEqual(path.stat().st_nlink, 1)

    def test_publication_lock_symlink_is_refused_without_touching_target(self):
        self.add_background()
        challenge, _ = self.prepare()
        outside = self.root / "outside-lock"
        outside.write_bytes(b"keep")
        private = self.data / "source-custody-recovery/v1"
        private.mkdir(mode=0o700)
        (private / ".publisher.lock").symlink_to(outside)
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            with self.assertRaises(OSError):
                recovery.apply(self.data, NODE, challenge)
        self.assertEqual(outside.read_bytes(), b"keep")
        self.assertFalse(self.proof_path(challenge).exists())

    def test_validate_is_readonly_and_does_not_expose_tokens(self):
        self.add_foreground()
        challenge, _ = self.prepare()
        output = io.StringIO()
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            with redirect_stdout(output):
                result = recovery.main(["validate", "--data-dir", str(self.data),
                                        "--node-id", NODE, "--challenge", challenge])
        self.assertEqual(result, 0)
        self.assertIn("1 exact owner", output.getvalue())
        self.assertNotIn("private-generation", output.getvalue())
        self.assertFalse(self.proof_path(challenge).exists())

    def test_cli_has_no_identity_override_and_unsupported_platform_refuses(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "prepare", "--help"],
                                check=True, capture_output=True, text=True)
        self.assertNotIn("--machine-id", result.stdout)
        self.assertNotIn("--boot-id", result.stdout)
        with mock.patch.object(recovery.sys, "platform", "unsupported"):
            with self.assertRaisesRegex(recovery.RecoveryError, "only on Linux"):
                recovery.local_identity()

    def test_nonowner_and_existing_wrong_private_modes_are_refused(self):
        self.add_background()
        different_uid = self.data.stat().st_uid + 10000
        with mock.patch.object(recovery.os, "geteuid", return_value=different_uid):
            with self.assertRaisesRegex(recovery.RecoveryError, "data owner or root"):
                self.prepare()
        self.assertFalse((self.data / "source-custody-recovery").exists())
        private = self.data / "source-custody-recovery"
        private.mkdir(mode=0o755)
        with self.assertRaisesRegex(recovery.RecoveryError, "ownership and mode 0700"):
            self.prepare()
        self.assertEqual(private.stat().st_mode & 0o777, 0o755)

    @unittest.skipUnless(os.geteuid() == 0, "requires root to exercise a different actual data owner")
    def test_root_preserves_nonroot_data_owner_for_daemon_replay(self):
        self.add_background()
        owner = (12345, 12345)
        os.chown(self.data, *owner)
        challenge, _ = self.prepare()
        with mock.patch.object(recovery, "local_identity", return_value=self.new_identity):
            recovery.apply(self.data, NODE, challenge)
        for path in (self.challenge_path(challenge), self.proof_path(challenge),
                     self.challenge_path(challenge).parent, self.proof_path(challenge).parent):
            self.assertEqual((path.stat().st_uid, path.stat().st_gid), owner)


if __name__ == "__main__":
    unittest.main()
