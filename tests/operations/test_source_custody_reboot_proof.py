"""Host reboot migration binds the exact expired job to a trusted local boot."""
import contextlib
import copy
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader('source_custody_reboot_proof', str(ROOT / 'scripts/source-custody-reboot-proof'))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
helper = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(helper)
NODE = '11111111-1111-4111-8111-111111111111'
CUSTODY = '22222222-2222-4222-8222-222222222222'
OLD = '33333333-3333-4333-8333-333333333333'
CURRENT = '44444444-4444-4444-8444-444444444444'
ACTIVATION = '55555555-5555-4555-8555-555555555555'
MACHINE = 'abcdef0123456789abcdef0123456789'
AT = 20_000


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def witnesses():
    record = {'kind': 'background', 'token': {'custody_id': CUSTODY, 'owner': {'kind': 'job', 'token': {
        'job_id': ACTIVATION, 'node_id': NODE, 'boot_id': OLD, 'claim_id': CUSTODY,
        'fence': 1, 'revision': 2, 'lease_expires_ms': 10_000}}}}
    witness = {'version': 2, 'reader_model': 'daemon_mediated_v1', 'kernel_instance': 'linux:' + OLD,
               'activation_id': ACTIVATION, 'node_id': NODE, 'anchor_nonce': CUSTODY,
               'anchor_device': 1, 'anchor_inode': 2, 'record': record}
    legacy = {'version': 1, 'machine_id': '', 'boot_id': '', 'node_id': NODE, 'records': [copy.deepcopy(record)]}
    return witness, legacy


def entry(**fields):
    value = {'_BOOT_ID': OLD.replace('-', ''), '_MACHINE_ID': MACHINE,
             '__REALTIME_TIMESTAMP': '5000000', '__CURSOR': 's=fixture', '__MONOTONIC_TIMESTAMP': '200',
             '__SEQNUM': '12', '__SEQNUM_ID': 'abcdef0123456789abcdef0123456789'}
    value.update(fields)
    return encoded(value)


class WitnessValidationTests(unittest.TestCase):
    def validate(self, witness=None, legacy=None, armed=ACTIVATION.encode(), current=CURRENT, at=AT):
        original, old = witnesses()
        return helper.validate_witnesses(encoded(original if witness is None else witness),
            encoded(old if legacy is None else legacy), armed, NODE, CUSTODY, current, at)

    def test_exact_expired_job_with_old_optional_fields_absent_is_supported(self):
        self.assertEqual(self.validate(), (ACTIVATION, OLD))
        witness, _ = witnesses()
        witness.update(anchor_ctime=[1, 2], daemon_namespace={'pid_namespace': 3, 'init': False})
        self.assertEqual(self.validate(witness=witness), (ACTIVATION, OLD))

    def test_same_kernel_boot_never_establishes_reboot(self):
        with self.assertRaisesRegex(helper.Refused, 'current kernel'):
            self.validate(current=OLD)

    def test_missing_wrong_or_whitespace_armed_nonce_is_refused(self):
        for armed in (b'', CURRENT.encode(), ACTIVATION.encode() + b'\n'):
            with self.subTest(armed=armed), self.assertRaisesRegex(helper.Refused, 'armed'):
                self.validate(armed=armed)

    def test_foreign_or_disagreeing_owner_and_non_job_records_are_refused(self):
        for change in ('legacy-owner', 'foreign-owner', 'foreign-custody', 'non-job', 'live-lease', 'nonempty-v1'):
            witness, legacy = witnesses()
            if change == 'legacy-owner':
                legacy['records'][0]['token']['owner']['token']['claim_id'] = CURRENT
            elif change == 'foreign-owner':
                witness['record']['token']['owner']['token']['node_id'] = CURRENT
                legacy['records'] = [copy.deepcopy(witness['record'])]
            elif change == 'foreign-custody':
                witness['record']['token']['custody_id'] = CURRENT
                legacy['records'] = [copy.deepcopy(witness['record'])]
            elif change == 'non-job':
                witness['record']['token']['owner']['kind'] = 'session'
                legacy['records'] = [copy.deepcopy(witness['record'])]
            elif change == 'live-lease':
                witness['record']['token']['owner']['token']['lease_expires_ms'] = AT + 1
                legacy['records'] = [copy.deepcopy(witness['record'])]
            else:
                legacy['machine_id'] = MACHINE
            with self.subTest(change=change), self.assertRaises(helper.Refused):
                self.validate(witness, legacy)

    def test_boolean_integer_and_noncanonical_uuid_are_refused(self):
        for field, value in (('anchor_inode', True), ('activation_id', '{' + ACTIVATION + '}'),
                             ('kernel_instance', 'linux:not-a-uuid'), ('version', True)):
            witness, _ = witnesses()
            witness[field] = value
            with self.subTest(field=field), self.assertRaises((helper.Refused, ValueError)):
                self.validate(witness=witness)

    def test_v1_boolean_or_float_cannot_compare_equal_to_integer_v2_token(self):
        for value in (True, 1.0):
            witness, legacy = witnesses()
            legacy['records'][0]['token']['owner']['token']['fence'] = value
            with self.subTest(value=value), self.assertRaisesRegex(helper.Refused, 'same record'):
                self.validate(witness, legacy)

    def test_duplicate_fields_and_all_nonfinite_json_numbers_are_refused(self):
        for raw in (b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":Infinity}', b'{"x":1e999}'):
            with self.subTest(raw=raw), self.assertRaises(helper.Refused):
                helper.strict_json(raw)

    def test_procfs_boot_time_is_unique_positive_and_not_future(self):
        with mock.patch.object(helper.time, 'time_ns', return_value=AT * 1_000_000):
            self.assertEqual(helper.proc_boot_started_at_ms(b'cpu 1 2 3\nbtime 10\n'), 10_000)
            for raw in (b'', b'btime 10\nbtime 11\n', b'btime 0\n', b'btime -1\n', b'btime 21\n'):
                with self.subTest(raw=raw), self.assertRaises(helper.Refused):
                    helper.proc_boot_started_at_ms(raw)

    def test_journal_requires_exact_local_boot_and_sane_timestamp(self):
        self.assertEqual(helper.journal_entry(entry(), MACHINE, OLD, AT), 5000)
        for raw in (b'', entry(_MACHINE_ID='0' * 32), entry(_BOOT_ID=CURRENT.replace('-', '')),
                    entry(__REALTIME_TIMESTAMP='0'), entry(__REALTIME_TIMESTAMP=str((AT + 1) * 1000)),
                    entry(__REALTIME_TIMESTAMP=['5000000']), entry() + entry(),
                    b'{"_BOOT_ID":"x","_BOOT_ID":"y"}', entry(unknown='x')):
            with self.subTest(raw=raw), self.assertRaises((helper.Refused, ValueError)):
                helper.journal_entry(raw, MACHINE, OLD, AT)


class RebootProofFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.data = self.root / 'data'; self.data.mkdir(mode=0o700)
        (self.data / 'node.id').write_text(NODE)
        base = self.data / 'source-custody-recovery'; base.mkdir(mode=0o700)
        for version in ('v1', 'v2'):
            (base / version).mkdir(mode=0o700)
        witness, legacy = witnesses()
        self.witness = base / 'v2' / ('background-' + CUSTODY + '.json')
        self.legacy = base / 'v1' / self.witness.name
        self.armed = self.witness.with_suffix('.armed')
        for path, raw in ((self.witness, encoded(witness)), (self.legacy, encoded(legacy)),
                          (self.armed, ACTIVATION.encode())):
            path.write_bytes(raw); path.chmod(0o600)
        self.enrollment = self.root / 'host.json'
        self.enrollment.write_bytes(encoded({'version': 1, 'node_id': NODE, 'data_dir': str(self.data),
            'machine_id_sha256': hashlib.sha256(MACHINE.encode()).hexdigest(), 'boot_id': CURRENT,
            'initial_pid_namespace_inode': 1, 'initial_user_namespace_inode': 2,
            'host_root_device_inode': [1, 2]}))
        self.enrollment.chmod(0o600)
        self.ancestor_check = helper.trusted_data_ancestors
        self.admin_check = helper.admin_directory
        self.journal_ancestor_check = helper.journal_ancestor
        self.stack = contextlib.ExitStack()
        self.stack.enter_context(mock.patch.object(helper, 'ADMIN_UID', os.getuid()))
        self.stack.enter_context(mock.patch.object(helper, 'admin_directory'))
        self.stack.enter_context(mock.patch.object(helper, 'journal_ancestor'))
        self.filesystem = self.stack.enter_context(mock.patch.object(helper, 'local_filesystem', return_value=True))
        # Isolated test temp ancestors may be shared; the production path walker
        # is tested separately, while the actual private recovery files stay pinned.
        self.stack.enter_context(mock.patch.object(helper, 'trusted_data_ancestors'))
        self.stack.enter_context(mock.patch.object(helper.os, 'getuid', return_value=0))
        self.stack.enter_context(mock.patch.object(helper.os, 'geteuid', return_value=0))
        self.stack.enter_context(mock.patch.object(helper.sys, 'platform', 'linux'))
        # Model an actual host reboot after the original fixture files existed.
        self.boot_started_at_ms = max(path.stat().st_ctime_ns for path in
                                     (self.witness, self.legacy, self.armed)) // 1_000_000 + 1000
        self.observed_at_ms = self.boot_started_at_ms + 1000
        self.host = self.stack.enter_context(mock.patch.object(helper, 'actual_host',
            return_value=(MACHINE, CURRENT, self.boot_started_at_ms)))
        self.journal = self.stack.enter_context(mock.patch.object(helper, 'prior_boot_entry', return_value=5000))
        self.publisher = self.stack.enter_context(mock.patch.object(helper, 'publish_fixed'))
        self.stack.enter_context(mock.patch.object(helper.time, 'time_ns', return_value=self.observed_at_ms * 1_000_000))

    def tearDown(self):
        self.stack.close(); self.temporary.cleanup()

    def verify(self, publish=False):
        return helper.verify(self.enrollment, CUSTODY, publish)

    def test_check_only_hashes_exact_worker_bytes_without_mutation(self):
        before = [path.read_bytes() for path in (self.witness, self.legacy, self.armed)]
        proof = self.verify()
        self.assertEqual(set(proof), {'version', 'model', 'node_id', 'custody_id', 'activation_id',
            'machine_id', 'previous_boot_id', 'current_boot_id', 'witness_sha256', 'legacy_sha256',
            'at_ms', 'previous_boot_entry_ms'})
        self.assertEqual(proof['model'], 'host_journal_reboot_v1')
        self.assertEqual(proof['witness_sha256'], hashlib.sha256(before[0]).hexdigest())
        self.assertEqual(proof['legacy_sha256'], hashlib.sha256(before[1]).hexdigest())
        self.assertEqual([path.read_bytes() for path in (self.witness, self.legacy, self.armed)], before)
        self.publisher.assert_not_called()

    def test_only_publish_option_publishes_exact_proof(self):
        proof = self.verify(True)
        custody, raw = self.publisher.call_args.args
        self.assertEqual(custody, CUSTODY); self.assertEqual(json.loads(raw), proof)

    def test_any_worker_byte_change_changes_digest_even_if_same_json(self):
        original = self.verify()
        self.witness.write_bytes(self.witness.read_bytes() + b' ')
        changed = self.verify()
        self.assertNotEqual(original['witness_sha256'], changed['witness_sha256'])
        self.assertEqual(original['legacy_sha256'], changed['legacy_sha256'])

    def test_nonlocal_recovery_directories_cannot_supply_historical_timestamps(self):
        base = self.data / 'source-custody-recovery'
        for path in (base, base / 'v1', base / 'v2'):
            target = (path.stat().st_dev, path.stat().st_ino)
            def local(fd):
                s = os.fstat(fd)
                return (s.st_dev, s.st_ino) != target
            self.filesystem.side_effect = local
            with self.subTest(path=path), self.assertRaisesRegex(helper.Refused, 'recovery directory.*local filesystem'):
                self.verify(True)
        self.publisher.assert_not_called(); self.journal.assert_not_called()
        self.filesystem.side_effect = None
        self.assertEqual(self.verify()['model'], 'host_journal_reboot_v1')

    def test_each_nonlocal_worker_file_is_refused_even_inside_local_private_directories(self):
        for path in (self.witness, self.legacy, self.armed):
            self.assertLessEqual(path.stat().st_mtime_ns, self.boot_started_at_ms * 1_000_000)
            self.assertLessEqual(path.stat().st_ctime_ns, self.boot_started_at_ms * 1_000_000)
            target = (path.stat().st_dev, path.stat().st_ino)
            def local(fd):
                s = os.fstat(fd)
                return (s.st_dev, s.st_ino) != target
            self.filesystem.side_effect = local
            with self.subTest(path=path), self.assertRaisesRegex(helper.Refused, 'worker input.*local filesystem'):
                self.verify(True)
        self.publisher.assert_not_called(); self.journal.assert_not_called()
        self.filesystem.side_effect = None
        self.assertEqual(self.verify()['model'], 'host_journal_reboot_v1')

    def test_inputs_modified_after_boot_cannot_receive_a_proof(self):
        for path in (self.witness, self.legacy, self.armed):
            before = path.stat()
            future = (self.boot_started_at_ms + 1) * 1_000_000
            os.utime(path, ns=(before.st_atime_ns, future))
            with self.subTest(path=path), self.assertRaisesRegex(helper.Refused, 'after the actual host boot'):
                self.verify(True)
            os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.publisher.assert_not_called(); self.journal.assert_not_called()

    def test_new_or_metadata_changed_postboot_input_is_refused_even_with_old_mtime(self):
        original_stamp = helper.fd_stamp
        target = self.witness.stat().st_ino
        def stamp(fd):
            value = original_stamp(fd)
            if value[1] == target:
                return value[:4] + ((self.boot_started_at_ms * 1_000_000) + 1,)
            return value
        with mock.patch.object(helper, 'fd_stamp', side_effect=stamp):
            with self.assertRaisesRegex(helper.Refused, 'after the actual host boot'):
                self.verify(True)
        self.publisher.assert_not_called(); self.journal.assert_not_called()

    def test_change_during_journal_query_refuses_publication(self):
        def journal(*args):
            before = self.armed.stat()
            os.utime(self.armed, ns=(before.st_atime_ns, (self.boot_started_at_ms + 1) * 1_000_000))
            return 5000
        self.journal.side_effect = journal
        with self.assertRaisesRegex(helper.Refused, 'pinned input changed'):
            self.verify(True)
        self.publisher.assert_not_called()

    def test_missing_marker_or_unsafe_private_file_never_publishes(self):
        self.armed.unlink()
        with self.assertRaises(FileNotFoundError): self.verify(True)
        self.armed.write_bytes(ACTIVATION.encode()); self.armed.chmod(0o644)
        with self.assertRaisesRegex(helper.Refused, 'private'): self.verify(True)
        self.publisher.assert_not_called()

    def test_changed_host_or_missing_local_prior_boot_never_publishes(self):
        self.host.side_effect = [(MACHINE, CURRENT, self.boot_started_at_ms),
                                 ('0' * 32, CURRENT, self.boot_started_at_ms)]
        with self.assertRaisesRegex(helper.Refused, 'host changed'): self.verify(True)
        self.host.side_effect = None
        self.journal.side_effect = helper.Refused('no matching local prior boot')
        with self.assertRaisesRegex(helper.Refused, 'prior boot'): self.verify(True)
        self.publisher.assert_not_called()

    def test_nonroot_and_worker_writable_enrollment_are_refused(self):
        with mock.patch.object(helper.os, 'geteuid', return_value=1000):
            with self.assertRaisesRegex(helper.Refused, 'root'): self.verify(True)
        self.enrollment.chmod(0o666)
        with self.assertRaisesRegex(helper.Refused, 'protected by host root'): self.verify(True)
        self.publisher.assert_not_called()

    def test_symlink_and_hardlinked_inputs_are_refused(self):
        link = self.root / 'linked.json'; os.link(self.witness, link)
        with self.assertRaisesRegex(helper.Refused, 'single-link'): self.verify(True)
        link.unlink(); raw = self.witness.read_bytes(); self.witness.unlink()
        link.write_bytes(raw); link.chmod(0o600); self.witness.symlink_to(link)
        with self.assertRaises(OSError): self.verify(True)
        self.publisher.assert_not_called()

    def test_untrusted_data_ancestor_and_proof_parent_are_refused(self):
        unsafe = self.root / 'unsafe'; unsafe.mkdir(); unsafe.chmod(0o777)
        child = unsafe / 'data'; child.mkdir(mode=0o700)
        fd = os.open(child, helper.DIRECTORY_FLAGS)
        try:
            with self.assertRaisesRegex(helper.Refused, 'untrusted'):
                self.ancestor_check(child, fd, os.fstat(fd).st_uid)
        finally:
            os.close(fd)
        fd = os.open(unsafe, helper.DIRECTORY_FLAGS)
        try:
            with self.assertRaisesRegex(helper.Refused, 'writable'):
                self.admin_check(fd)
        finally:
            os.close(fd)

    def test_journal_subprocess_has_fixed_binary_filters_and_bounded_output(self):
        for output, accepted in ((entry(), True), (b'x' * (helper.MAX_PROOF + 1), False)):
            readfd, writefd = os.pipe()
            os.write(writefd, output); os.close(writefd)
            process = mock.Mock(stdout=os.fdopen(readfd, 'rb'))
            process.wait.return_value = 0; process.poll.return_value = 0
            with mock.patch.object(helper.subprocess, 'Popen', return_value=process) as spawned:
                if accepted:
                    self.assertEqual(helper.bounded_journalctl(99, MACHINE, OLD), output)
                else:
                    with self.assertRaisesRegex(helper.Refused, 'bound'):
                        helper.bounded_journalctl(99, MACHINE, OLD)
                command = spawned.call_args.args[0]
                self.assertEqual(command[0], '/usr/bin/journalctl')
                self.assertIn('--directory=/proc/self/fd/99', command)
                self.assertIn('_MACHINE_ID=' + MACHINE, command)
                self.assertIn('_BOOT_ID=' + OLD.replace('-', ''), command)
                self.assertEqual(spawned.call_args.kwargs['pass_fds'], (99,))
                self.assertNotIn('shell', spawned.call_args.kwargs)

    def test_standard_group_writable_log_ancestor_keeps_journal_boundaries_strict(self):
        log = self.root / 'log'; log.mkdir(); log.chmod(0o775)
        journal_root = log / 'journal'; journal_root.mkdir(); journal_root.chmod(0o2755)
        machine = journal_root / MACHINE; machine.mkdir(); machine.chmod(0o2755)
        fd = os.open(log, helper.DIRECTORY_FLAGS)
        try:
            self.journal_ancestor_check(fd, allow_group_write=True)
            with self.assertRaisesRegex(helper.Refused, 'untrusted'):
                self.journal_ancestor_check(fd)
            log.chmod(0o777)
            with self.assertRaisesRegex(helper.Refused, 'untrusted'):
                self.journal_ancestor_check(fd, allow_group_write=True)
            log.chmod(0o775)
        finally:
            os.close(fd)
        with mock.patch.object(helper, 'JOURNAL_ROOT', journal_root), \
                mock.patch.object(helper, 'local_filesystem', return_value=True), \
                mock.patch.object(helper, 'admin_directory', side_effect=self.admin_check), \
                mock.patch.object(helper, 'journal_ancestor') as ancestor:
            with helper.journal_directory(MACHINE) as directory:
                self.assertEqual(os.fstat(directory).st_ino, machine.stat().st_ino)
            self.assertEqual(sum(call.kwargs.get('allow_group_write', False)
                                 for call in ancestor.call_args_list), 1)
            for boundary in (journal_root, machine):
                boundary.chmod(0o2775)
                with self.subTest(boundary=boundary), self.assertRaisesRegex(helper.Refused, 'writable'):
                    with helper.journal_directory(MACHINE):
                        self.fail('writable journal boundary was accepted')
                boundary.chmod(0o2755)

    def test_persistent_journal_query_binds_fixed_directory_and_filters(self):
        # Exercise the real query boundary independently of setUp's proof stub.
        journal_helper = importlib.util.module_from_spec(SPEC)
        LOADER.exec_module(journal_helper)
        journal_root = self.root / 'journal'; journal_root.mkdir(mode=0o755)
        machine_dir = journal_root / MACHINE; machine_dir.mkdir(mode=0o755)
        journal_file = machine_dir / 'system.journal'
        journal_file.write_bytes(b'fixture'); journal_file.chmod(0o640)
        with mock.patch.object(journal_helper, 'ADMIN_UID', self.root.stat().st_uid), \
                mock.patch.object(journal_helper, 'journal_ancestor'), \
                mock.patch.object(journal_helper, 'JOURNAL_ROOT', journal_root), \
                mock.patch.object(journal_helper, 'local_filesystem', return_value=True), \
                mock.patch.object(journal_helper, 'bounded_journalctl', return_value=entry()) as query:
            self.assertEqual(journal_helper.prior_boot_entry(MACHINE, OLD, AT), 5000)
            self.assertEqual(query.call_args.args[1:], (MACHINE, OLD))
            journal_file.chmod(0o660)
            with self.assertRaisesRegex(helper.Refused, 'untrusted'):
                journal_helper.prior_boot_entry(MACHINE, OLD, AT)
            journal_file.chmod(0o640); journal_file.unlink(); journal_file.symlink_to(self.witness)
            with self.assertRaisesRegex(helper.Refused, 'untrusted'):
                journal_helper.prior_boot_entry(MACHINE, OLD, AT)

    def test_publication_preserves_existing_different_proof_and_refuses_symlink(self):
        with mock.patch.object(helper, 'local_filesystem', return_value=True):
            fd = os.open(self.root, helper.DIRECTORY_FLAGS)
            try:
                helper.publish_at(fd, CUSTODY, b'{"proof":1}\n')
                path = self.root / (CUSTODY + '.json')
                self.assertEqual(path.stat().st_nlink, 1)
                self.assertEqual(path.stat().st_mode & 0o777, 0o644)
                with self.assertRaisesRegex(helper.Refused, 'nothing overwritten'):
                    helper.publish_at(fd, CUSTODY, b'{"proof":2}\n')
                self.assertEqual(path.read_bytes(), b'{"proof":1}\n')
                path.unlink(); path.symlink_to(self.witness)
                with self.assertRaises(OSError): helper.publish_at(fd, CUSTODY, b'{}\n')
            finally:
                os.close(fd)


if __name__ == '__main__':
    unittest.main()
