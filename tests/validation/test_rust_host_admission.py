"""Admission evidence and kernel ownership must fail closed."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from validation import rust_host_admission as admission


CHILD = '''
import sys,time
from pathlib import Path
from validation.rust_host_admission import acquire
with acquire(Path(sys.argv[1]), timeout=float(sys.argv[4]), repository=Path(sys.argv[2]), proc_root=Path(sys.argv[3])) as lease:
    print(lease['lock_inode'], flush=True)
    time.sleep(float(sys.argv[5]))
'''


class RustHostAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'repo'
        (self.repo / 'validation').mkdir(parents=True)
        self.directory = self.root / 'lease'
        self.directory.mkdir()
        self.proc = self.root / 'proc'
        (self.proc / 'self').mkdir(parents=True)
        self.cg = self.root / 'cgroup'
        self.cg.mkdir()
        self.mount = f'7 1 8:1 /var/lib/docker/volumes/{admission.VOLUME}/_data {self.directory} rw - ext4 /dev/example rw\n'
        self.write_mount()
        (self.proc / 'self/cgroup').write_text('0::/\n')
        (self.cg / 'cpu.max').write_text('1200000 100000')
        (self.cg / 'memory.max').write_text(str(15 * admission.GIB))
        (self.cg / 'memory.swap.max').write_text('0')
        (self.cg / 'pids.max').write_text('4096')
        (self.proc / 'meminfo').write_text(f'MemAvailable: {40 * admission.GIB // 1024} kB\n')
        self.roster()

    def roster(self, *, host='lab5', labels=None):
        labels = labels or ['lab', 'general', 'high-cpu', 'baremetal']
        (self.repo / 'validation/runner-fleet.toml').write_text(
            'voter_hosts = ["voter"]\n[[runners]]\nname = "runner"\n'
            f'host = "{host}"\nos = "Linux"\narch = "X64"\nlabels = {json.dumps(labels)}\n')

    def write_mount(self, mount=None):
        (self.proc / 'self/mountinfo').write_text((self.mount if mount is None else mount) +
            f'9 1 0:1 / {self.cg} rw - cgroup2 cgroup rw\n')

    def inspect(self):
        return admission.inspect_environment('runner', self.repo, self.directory, proc_root=self.proc)

    def lease(self, timeout=0):
        return admission.acquire(self.directory, timeout, repository=self.repo, proc_root=self.proc)

    def child(self, seconds=60, timeout=1):
        process = subprocess.Popen([sys.executable, '-c', CHILD, str(self.directory), str(self.repo),
                                    str(self.proc), str(timeout), str(seconds)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop, process)
        return process

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)

    def test_current_caps_and_roster_are_observations_not_host_reservation(self):
        receipt = self.inspect()
        self.assertEqual(receipt['cpu_limit'], 12)
        self.assertEqual(receipt['memory_limit_bytes'], 15 * admission.GIB)
        self.assertEqual(receipt['swap_limit_bytes'], 0)
        self.assertEqual(len(receipt['roster_sha256']), 64)
        self.assertFalse(receipt['other_workflows_reserved'])
        self.assertFalse(receipt['live_fleet_verified'])

    def test_missing_or_ephemeral_mount_is_refused(self):
        for mount in ('', self.mount.replace(' - ext4 ', ' - tmpfs '),
                      self.mount.replace(admission.VOLUME, 'different-volume'),
                      self.mount.replace(' rw - ', ' ro - '), self.mount + self.mount):
            with self.subTest(mount=mount):
                self.write_mount(mount)
                with self.assertRaises(admission.AdmissionError):
                    self.inspect()

    def test_directory_inside_checkout_is_refused_even_if_mount_claims_shared(self):
        with self.assertRaises(admission.AdmissionError):
            admission.validate_directory(self.repo / 'validation', repository=self.repo, proc_root=self.proc)

    def test_symlink_mount_is_refused(self):
        link = self.root / 'alias'
        link.symlink_to(self.directory)
        with self.assertRaises(admission.AdmissionError):
            admission.validate_directory(link, repository=self.repo, proc_root=self.proc)

    def test_unlimited_missing_malformed_or_oversized_caps_are_refused(self):
        for filename, invalid in [('cpu.max', 'max 100000'), ('cpu.max', '1 0'),
                                  ('cpu.max', '1300000 100000'), ('memory.max', 'max'),
                                  ('memory.max', str(16 * admission.GIB)),
                                  ('memory.swap.max', 'max'), ('memory.swap.max', '1'),
                                  ('memory.swap.max', '-1'), ('pids.max', 'max'),
                                  ('pids.max', '4097'), ('pids.max', '0')]:
            path = self.cg / filename
            original = path.read_text()
            with self.subTest(filename=filename, invalid=invalid):
                path.write_text(invalid)
                with self.assertRaises(admission.AdmissionError):
                    self.inspect()
                path.write_text(original)
        (self.cg / 'memory.max').unlink()
        with self.assertRaises(admission.AdmissionError):
            self.inspect()

    def test_reserve_is_checked_against_actual_cap_and_available_memory(self):
        (self.proc / 'meminfo').write_text(f'MemAvailable: {22 * admission.GIB // 1024} kB\n')
        with self.assertRaises(admission.AdmissionError):
            self.inspect()
        (self.cg / 'memory.max').write_text(str(14 * admission.GIB))
        self.assertEqual(self.inspect()['memory_limit_bytes'], 14 * admission.GIB)

    def test_ineligible_voter_android_vm_and_unknown_runner_are_refused(self):
        for host, labels in [('voter', ['lab', 'general', 'high-cpu', 'baremetal']),
                             ('lab5', ['lab', 'general', 'high-cpu', 'baremetal', 'android-kvm']),
                             ('lab5', ['lab', 'general', 'high-cpu', 'incus'])]:
            with self.subTest(host=host, labels=labels):
                self.roster(host=host, labels=labels)
                with self.assertRaises(admission.AdmissionError):
                    self.inspect()
        with self.assertRaises(admission.AdmissionError):
            admission.inspect_environment('unknown', self.repo, self.directory, proc_root=self.proc)

    def test_real_process_contention_times_out_and_controller_death_fences_reentry(self):
        child = self.child()
        inode = int(child.stdout.readline().strip())
        with self.assertRaisesRegex(admission.AdmissionError, 'timed out'):
            with self.lease(timeout=0.1):
                self.fail('contending process must not acquire')
        self.stop(child)
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('controller loss must not automatically readmit')
        path = self.directory / admission.LOCK
        self.assertEqual(path.stat().st_ino, inode)
        self.assertEqual(json.loads(path.read_text())['state'], 'ACTIVE')

    def test_explicitly_inherited_child_lease_outlives_controller_context(self):
        with self.lease() as lease:
            child = subprocess.Popen(
                [sys.executable, '-c', 'import time; print("held", flush=True); time.sleep(60)'],
                pass_fds=(lease['lock_fd'],), stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
            self.addCleanup(self.stop, child)
            self.assertEqual(child.stdout.readline().strip(), 'held')
        with self.assertRaisesRegex(admission.AdmissionError, 'timed out'):
            with self.lease(timeout=0.1):
                self.fail('child lease released with controller context')
        self.stop(child)
        with self.lease():
            pass

    def test_exception_retains_active_fence_and_original_exception(self):
        with self.assertRaisesRegex(RuntimeError, 'original'):
            with self.lease() as first:
                raise RuntimeError('original')
        path = self.directory / admission.LOCK
        self.assertEqual(first['lock_inode'], path.stat().st_ino)
        self.assertEqual(json.loads(path.read_text())['state'], 'ACTIVE')
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('exception must retain fence')

    def test_normal_qualified_return_clears_marker_and_reuses_same_inode(self):
        with self.lease() as first:
            self.assertEqual(json.loads((self.directory / admission.LOCK).read_text())['state'], 'ACTIVE')
        self.assertEqual((self.directory / admission.LOCK).read_bytes(), b'')
        with self.lease() as second:
            self.assertEqual(first['lock_inode'], second['lock_inode'])

    def test_unqualified_cleanup_quarantines_and_blocks_even_normal_return(self):
        with self.lease() as lease:
            admission.quarantine(lease, 'owned process cleanup unqualified')
            self.assertFalse(lease['cleanup_qualified'])
        marker = json.loads((self.directory / admission.LOCK).read_text())
        self.assertEqual(marker['state'], 'QUARANTINED')
        self.assertEqual(marker['reason'], 'owned process cleanup unqualified')
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('unsafe context readmitted')

    def test_any_existing_nonempty_or_invalid_marker_refuses_without_erasure(self):
        path = self.directory / admission.LOCK
        for marker in (b'garbage', b' ', b'{}', b'ACTIVE'):
            with self.subTest(marker=marker):
                path.write_bytes(marker)
                with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
                    with self.lease():
                        self.fail('invalid existing fence accepted')
                self.assertEqual(path.read_bytes(), marker)

    def test_failed_healthy_clear_restores_active_fence(self):
        with self.assertRaises(admission.AdmissionError):
            with self.lease():
                original = admission.os.fsync
                calls = [0]
                def fail_once(fd):
                    calls[0] += 1
                    if calls[0] == 1:
                        raise OSError('injected healthy-clear fsync failure')
                    return original(fd)
                patcher = mock.patch.object(admission.os, 'fsync', side_effect=fail_once)
                patcher.start()
                self.addCleanup(patcher.stop)
        self.assertEqual(json.loads((self.directory / admission.LOCK).read_text())['state'], 'ACTIVE')
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('failed healthy-clear readmitted')

    def test_active_marker_must_be_durable_before_body_starts(self):
        with mock.patch.object(admission.os, 'fsync', side_effect=OSError('injected active-marker failure')):
            with self.assertRaises(admission.AdmissionError):
                with self.lease():
                    self.fail('lease body started without durable ACTIVE fence')
        self.assertTrue((self.directory / admission.LOCK).read_bytes())
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('failed ACTIVE marker was automatically recovered')

    def test_unqualified_flag_alone_preserves_fence_without_quarantine_io(self):
        with self.lease() as lease:
            lease['cleanup_qualified'] = False
        with self.assertRaisesRegex(admission.AdmissionError, 'fenced'):
            with self.lease():
                self.fail('unqualified flag was ignored')

    def test_symlink_hardlink_or_unwritable_lock_is_refused(self):
        path = self.directory / admission.LOCK
        path.symlink_to(self.root / 'other')
        with self.assertRaises(admission.AdmissionError):
            with self.lease():
                self.fail('symlink accepted')
        path.unlink()
        path.write_text('')
        os.link(path, self.root / 'hardlink')
        with self.assertRaises(admission.AdmissionError):
            with self.lease():
                self.fail('hardlink accepted')
        path.unlink()
        with mock.patch.object(admission.os, 'open', side_effect=PermissionError('denied')):
            with self.assertRaises(admission.AdmissionError):
                with self.lease():
                    self.fail('unwritable mount accepted')

    def test_invalid_timeout_and_policy_are_refused(self):
        for timeout in (-1, float('inf'), float('nan')):
            with self.subTest(timeout=timeout), self.assertRaises(admission.AdmissionError):
                with self.lease(timeout):
                    self.fail('invalid timeout accepted')
        with self.assertRaises(admission.AdmissionError):
            admission.inspect_environment('runner', self.repo, self.directory, proc_root=self.proc, max_cpu=0)


if __name__ == '__main__':
    unittest.main()
