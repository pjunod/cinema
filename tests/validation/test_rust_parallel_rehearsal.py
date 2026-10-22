"""One-lease orchestration controls; native work and resource sampling mocked."""
from contextlib import contextmanager
import copy
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

from validation import rust_parallel_rehearsal as rehearsal

SOURCE = dict(commit='a' * 40, tree='b' * 40, tracked_clean=True)
SAFE = dict(returncode=0, success=True, timed_out=False, cleanup_complete=True, canceled=False)


class RehearsalCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = self.base / 'source'
        self.root.mkdir()
        (self.root / 'Cargo.toml').write_text('[workspace]\n')
        self.output, self.target = self.base / 'evidence', self.base / 'cargo-cold'
        before = Path.cwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, before)
        environment = patch.dict(os.environ, CARGO_TARGET_DIR=str(self.target), PLURX_CI_IMAGE_DIGEST='sha256:observed-input')
        environment.start()
        self.addCleanup(environment.stop)
        self.stop = threading.Event()
        self.events = []
        self.lease_active = False
        self.lease_count = 0
        self.discover_count = 0
        self.arm_names = []
        self.fail_arm = self.unsafe_arm = self.cancel_arm = self.resource_failure = None
        self.after_arm = None
        self.during_acquire = None
        self.sampler_still_alive = False
        self.resource_summary = dict(status='complete', unavailable=[], samples=2)
        self.command_process = {}
        self.prepare_process = copy.deepcopy(SAFE)
        self.current_source = copy.deepcopy(SOURCE)
        self.bundle = None
        patches = [
            patch.object(rehearsal, 'source_identity', side_effect=lambda bundle: copy.deepcopy(self.current_source)),
            patch.object(rehearsal.parallel, 'normalized_runner_name', side_effect=lambda root, name: name),
            patch.object(rehearsal, 'acquire', side_effect=self.acquire),
            patch.object(rehearsal, 'inspect_environment', return_value=dict(cpu_limit=12, memory_limit_bytes=15*1024**3,
                                                                           swap_limit_bytes=0)),
            patch.object(rehearsal, 'quarantine', side_effect=self.quarantine),
            patch.object(rehearsal.parallel, 'discover', side_effect=self.discover),
            patch.object(rehearsal.parallel, 'command', side_effect=self.command),
            patch.object(rehearsal.parallel, 'check_inputs'),
            patch.object(rehearsal.parallel, 'execute', side_effect=self.execute),
            patch.object(rehearsal.resources, 'start', side_effect=self.start),
            patch.object(rehearsal.resources, 'finish', side_effect=self.finish),
            patch.object(rehearsal, 'sampler_exit', side_effect=self.sampler_exit),
            patch.object(rehearsal, 'compare', side_effect=self.compare),
        ]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    @contextmanager
    def acquire(self, directory, **kwargs):
        self.assertFalse(self.lease_active)
        self.lease_count += 1
        self.lease_active = True
        self.events.append('lease-enter')
        lease = dict(lock_fd=17, cleanup_qualified=True)
        try:
            if self.during_acquire:
                self.during_acquire()
            yield lease
        finally:
            self.events.append('lease-exit')
            self.lease_active = False

    def quarantine(self, lease, reason):
        self.assertTrue(self.lease_active)
        self.events.append('quarantine')
        lease['cleanup_qualified'] = False

    def discover(self, output, command, **kwargs):
        self.assertTrue(self.lease_active)
        self.assertEqual(kwargs['pass_fds'], (17,))
        self.assertIs(kwargs['cancel_event'], self.stop)
        self.assertEqual(kwargs['source_bundle'], self.bundle)
        self.assertEqual(rehearsal.parallel.SOURCE_BUNDLE, self.bundle)
        self.assertTrue((self.output / 'pre-run-manifest.json').exists())
        self.discover_count += 1
        self.target.mkdir(exist_ok=True)
        self.binary = self.target / 'native-aabbccdd'
        self.binary.write_bytes(b'one compiled native payload')
        inventory = dict(discovery_complete=True, errors=[], source=copy.deepcopy(SOURCE),
                         compile=copy.deepcopy(self.prepare_process), compiler=dict(version='rustc 1.97.1'),
                         binaries=[dict(executable=str(self.binary), sha256=rehearsal.parallel.sha256(self.binary),
                                        listing_complete=True, listing=dict(all=copy.deepcopy(SAFE), ignored=copy.deepcopy(SAFE)))])
        if self.prepare_process.get('success') is not True:
            inventory.update(discovery_complete=False, errors=['original compiler failure'])
        rehearsal.parallel.write(output / 'inventory.json', inventory)
        self.events.append('discover')
        return inventory

    def command(self, argv, output, name, timeout, fd, **kwargs):
        self.assertTrue(self.lease_active)
        self.assertEqual(fd, 17)
        self.assertIs(kwargs['cancel_event'], self.stop)
        self.events.append(name)
        process = copy.deepcopy(self.command_process.get(name, SAFE))
        return process, json.dumps(dict(version=1)) if name == 'metadata' else 'cargo 1.97.1\n'

    def execute(self, mode, inventory, metadata, output, fd, *, stop_event):
        self.assertTrue(self.lease_active)
        self.assertIs(stop_event, self.stop)
        self.assertEqual(fd, 17)
        label = output.name
        self.arm_names.append(label)
        self.events.append(label)
        output.mkdir()
        result = dict(schema=1, mode=mode, source=copy.deepcopy(SOURCE),
                      success=label != self.fail_arm, safe_to_release=label != self.unsafe_arm,
                      canceled=label == self.cancel_arm,
                      errors=['original assertion failure'] if label == self.fail_arm else [])
        rehearsal.parallel.write(output / 'results.json', result)
        if self.after_arm:
            self.after_arm(label)
        return result

    def start(self, output, cell):
        self.assertTrue(self.lease_active)
        self.events.append('resource-start:' + output.name)
        rehearsal.parallel.write(output / 'state.json', dict(status='sampling'))

    def finish(self, output):
        self.assertTrue(self.lease_active)
        self.events.append('resource-finish:' + output.name)
        rehearsal.parallel.write(output / 'state.json', dict(status='finished', pid=1234, start_ticks=99))
        rehearsal.parallel.write(output / 'summary.json', self.resource_summary)
        if output.name == self.resource_failure:
            rehearsal.parallel.write(output / 'telemetry-incomplete.json', dict(reason='original sampler failure'))

    def sampler_exit(self, state):
        self.assertEqual(state['pid'], 1234)
        if self.sampler_still_alive:
            raise ValueError('Resource sampler remains alive after finish')
        return dict(pid=1234, start_ticks=99, disposition='gone')

    def compare(self, output):
        self.assertTrue(self.lease_active)
        labels = [label for label, _ in rehearsal.ARMS if (output / label).exists()]
        complete = bool(labels) and all(rehearsal.parallel.read(output / label / 'results.json').get('success') is True
                                        for label in labels)
        return dict(qualification_complete_for_runtime=complete, adoption_ready=False)

    def run_campaign(self, **kwargs):
        return rehearsal.rehearse(self.output, self.base / 'admission', 'public-rostered-runner',
                                  stop_event=self.stop, **kwargs)

    def test_complete_sequence_builds_once_holds_one_lease_and_finishes_every_observer(self):
        result = self.run_campaign()
        self.assertTrue(result['success'])
        self.assertFalse(result['adoption_ready'])
        self.assertEqual(self.arm_names, [label for label, _ in rehearsal.ARMS])
        self.assertEqual((self.lease_count, self.discover_count), (1, 1))
        self.assertEqual(self.events[0], 'lease-enter')
        self.assertEqual(self.events[-1], 'lease-exit')
        self.assertEqual(sum(event.startswith('resource-start:') for event in self.events), 7)
        self.assertEqual(sum(event.startswith('resource-finish:') for event in self.events), 7)
        manifest = rehearsal.parallel.read(self.output / 'pre-run-manifest.json')
        self.assertIn('OS page cache and registry cache state unqualified', manifest['cold_definition'])
        self.assertEqual(manifest['arms'], [dict(label=label, mode=mode) for label, mode in rehearsal.ARMS])
        self.assertEqual(result['pre_run_manifest_sha256'], rehearsal.parallel.sha256(self.output / 'pre-run-manifest.json'))
        self.assertEqual(result['source_binding_sha256'], rehearsal.parallel.sha256(self.output / 'source-binding.json'))
        binding = rehearsal.parallel.read(self.output / 'source-binding.json')
        self.assertEqual(binding['source'], SOURCE)
        self.assertIn('not independently inspected', binding['image']['provenance'])
        self.assertIn('no authenticated CI', binding['authentication'])

    def test_assertion_failure_remains_original_red_and_does_not_retry(self):
        self.fail_arm = 'cold-parallel'
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertTrue(result['safe_to_release'])
        self.assertEqual(self.arm_names, [label for label, _ in rehearsal.ARMS])
        original = rehearsal.parallel.read(self.output / 'cold-parallel/results.json')
        self.assertEqual(original['errors'], ['original assertion failure'])
        self.assertEqual(self.arm_names.count('cold-parallel'), 1)
        self.assertNotIn('quarantine', self.events)

    def test_unsafe_arm_quarantines_and_blocks_every_remaining_arm(self):
        self.unsafe_arm = 'cold-parallel'
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, ['cold-parallel'])
        self.assertEqual(self.events.count('quarantine'), 1)
        self.assertFalse(rehearsal.parallel.read(self.output / 'legacy/results.json')['executed'])

    def test_canceled_arm_stops_without_quarantining_qualified_cleanup(self):
        self.cancel_arm = 'legacy'
        result = self.run_campaign()
        self.assertTrue(result['canceled'])
        self.assertFalse(result['success'])
        self.assertTrue(result['safe_to_release'])
        self.assertEqual(self.arm_names, ['cold-parallel', 'legacy'])
        self.assertNotIn('quarantine', self.events)

    def test_compile_failure_stops_before_metadata_or_native_bodies(self):
        self.prepare_process.update(success=False, returncode=1)
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertTrue(result['safe_to_release'])
        self.assertEqual(self.arm_names, [])
        self.assertNotIn('metadata', self.events)
        self.assertEqual(rehearsal.parallel.read(self.output / 'inventory/inventory.json')['errors'],
                         ['original compiler failure'])

    def test_prepare_unsafe_cleanup_or_explicit_cancellation_stops_all(self):
        self.prepare_process['cleanup_complete'] = False
        result = self.run_campaign()
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, [])
        self.assertIn('quarantine', self.events)

    def test_prepare_cancellation_with_cleanup_proof_does_not_start_metadata(self):
        self.prepare_process['canceled'] = True
        result = self.run_campaign()
        self.assertTrue(result['canceled'])
        self.assertTrue(result['safe_to_release'])
        self.assertNotIn('metadata', self.events)
        self.assertEqual(self.arm_names, [])

    def test_sampler_failure_is_red_without_retry_and_preserves_native_result(self):
        self.resource_failure = 'cold-parallel'
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, ['cold-parallel'])
        self.assertTrue(rehearsal.parallel.read(self.output / 'cold-parallel/results.json')['success'])
        self.assertIn('quarantine', self.events)

    def test_superseded_source_stops_before_another_arm(self):
        self.after_arm = lambda label: self.current_source.update(tree='c' * 40)
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertEqual(self.arm_names, ['cold-parallel'])
        self.assertIn('superseded', ' '.join(result['errors']))

    def test_same_bytes_recompiled_native_payload_refuses_comparison(self):
        def replace(label):
            stamp = self.binary.stat()
            os.utime(self.binary, ns=(stamp.st_atime_ns, stamp.st_mtime_ns + 1000))
        self.after_arm = replace
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertEqual(self.arm_names, ['cold-parallel'])
        self.assertIn('rebuilt', ' '.join(result['errors']))

    def test_pre_run_policy_mutation_is_refused_even_during_last_arm(self):
        def mutate(label):
            if label == 'nextest':
                (self.output / 'pre-run-manifest.json').write_text('{}')
        self.after_arm = mutate
        self.assertFalse(self.run_campaign()['success'])

    def test_source_binding_mutation_during_last_arm_is_retained_and_refused(self):
        def mutate(label):
            if label == 'nextest':
                (self.output / 'source-binding.json').write_text('{"tampered":true}')
        self.after_arm = mutate
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertEqual(rehearsal.parallel.read(self.output / 'source-binding.json'), {'tampered': True})
        self.assertNotEqual(result['source_binding_sha256'], rehearsal.parallel.sha256(self.output / 'source-binding.json'))
        self.assertTrue(rehearsal.parallel.read(self.output / 'nextest/results.json')['success'])

    def test_cargo_input_mutation_during_last_arm_is_refused(self):
        def mutate(label):
            if label == 'nextest':
                (self.root / 'Cargo.toml').write_text('[workspace] # changed')
        self.after_arm = mutate
        self.assertFalse(self.run_campaign()['success'])

    def test_finished_but_live_sampler_quarantines_before_native_arms(self):
        self.sampler_still_alive = True
        result = self.run_campaign()
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, [])
        self.assertIn('quarantine', self.events)

    def test_incomplete_counter_summary_is_not_green_without_incomplete_marker(self):
        self.resource_summary.update(status='incomplete', unavailable=['cpu counters'])
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, [])

    def test_metadata_cancellation_stops_before_version_and_native_arms(self):
        self.command_process['metadata'] = dict(SAFE, canceled=True)
        result = self.run_campaign()
        self.assertTrue(result['canceled'])
        self.assertTrue(result['safe_to_release'])
        self.assertNotIn('cargo-version', self.events)
        self.assertEqual(self.arm_names, [])

    def test_version_cleanup_failure_quarantines_before_native_arms(self):
        self.command_process['cargo-version'] = dict(SAFE, cleanup_complete=False)
        result = self.run_campaign()
        self.assertFalse(result['safe_to_release'])
        self.assertEqual(self.arm_names, [])
        self.assertIn('quarantine', self.events)

    def test_explicit_without_nextest_preserves_owed_comparison(self):
        result = self.run_campaign(without_nextest=True)
        self.assertTrue(result['success'])
        self.assertEqual(self.arm_names, [label for label, _ in rehearsal.ARMS[:-1]])
        self.assertFalse(result['adoption_ready'])
        self.assertIn('qualified nextest comparison', result['owed'])

    def test_target_filled_while_waiting_for_lease_is_retained_without_cold_claim(self):
        self.during_acquire = lambda: (self.target / 'other-campaign-payload').write_text('preserve')
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertTrue(result['safe_to_release'])
        self.assertFalse(result['preparation_started'])
        self.assertEqual(self.discover_count, 0)
        self.assertEqual(self.arm_names, [])
        self.assertNotIn('metadata', self.events)
        self.assertFalse((self.output / 'pre-run-manifest.json').exists())
        self.assertEqual((self.target / 'other-campaign-payload').read_text(), 'preserve')
        self.assertEqual(self.events[-1], 'lease-exit')
        self.assertNotIn('quarantine', self.events)
        self.assertEqual(rehearsal.parallel.read(self.output / 'rehearsal.json')['cold_target_validation'], 'refused')

    def test_empty_target_replaced_while_waiting_is_not_the_frozen_target(self):
        def replace():
            self.target.rename(self.base / 'original-target')
            self.target.mkdir(mode=0o700)
        self.during_acquire = replace
        result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertIn('identity changed', ' '.join(result['errors']))
        self.assertEqual(self.discover_count, 0)
        self.assertFalse((self.output / 'pre-run-manifest.json').exists())

    def test_compile_environment_is_recorded_and_cannot_drift_in_final_arm(self):
        def mutate(label):
            if label == 'nextest':
                os.environ['CARGO_INCREMENTAL'] = '1'
        self.after_arm = mutate
        with patch.dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', CARGO_INCREMENTAL='0'):
            result = self.run_campaign()
        self.assertFalse(result['success'])
        self.assertIn('environment changed', ' '.join(result['errors']))
        manifest = rehearsal.parallel.read(self.output / 'pre-run-manifest.json')
        for key in ('CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL'):
            self.assertEqual(manifest['environment'][key], '0')
        self.assertTrue(rehearsal.parallel.read(self.output / 'nextest/results.json')['success'])

    def test_nonempty_target_is_never_deleted_or_reused(self):
        self.target.mkdir()
        retained = self.target / 'retained-payload'
        retained.write_text('preserve')
        with self.assertRaisesRegex(ValueError, 'nonempty'):
            self.run_campaign()
        self.assertEqual(retained.read_text(), 'preserve')
        self.assertEqual(self.lease_count, 0)

    def test_source_internal_and_existing_outputs_refuse_before_execution(self):
        for path in (self.root / 'forbidden', self.base):
            with self.assertRaises(ValueError):
                rehearsal.rehearse(path, self.base / 'admission', 'runner')
        self.assertEqual(self.lease_count, 0)

    def test_source_bundle_remains_fixed_across_preparation_and_all_arms(self):
        self.bundle = self.base / 'source-bundle.json'
        self.bundle.write_text('external cryptographically verified fixture manifest')
        self.assertTrue(self.run_campaign(source_bundle=self.bundle)['success'])


class SamplerExitCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.proc = Path(self.temp.name)
        (self.proc / '1234').mkdir()
        self.state = dict(pid=1234, start_ticks=99)

    def stat(self, state='S', ticks=99):
        fields = ['0'] * 22
        fields[0], fields[19] = state, str(ticks)
        (self.proc / '1234' / 'stat').write_text('1234 (sampler with ) name) ' + ' '.join(fields))

    def test_gone_original_and_reused_pid_are_distinguished_without_signals(self):
        self.assertEqual(rehearsal.sampler_exit(self.state, proc_root=self.proc)['disposition'], 'gone')
        self.stat(ticks=100)
        self.assertEqual(rehearsal.sampler_exit(self.state, proc_root=self.proc)['disposition'],
                         'original-exited-pid-reused')

    def test_zombie_is_exited_but_live_sampler_cannot_qualify(self):
        self.stat(state='Z')
        self.assertEqual(rehearsal.sampler_exit(self.state, proc_root=self.proc)['disposition'], 'exited-Z')
        self.stat(state='S')
        with self.assertRaisesRegex(ValueError, 'remains alive'):
            rehearsal.sampler_exit(self.state, proc_root=self.proc, timeout=0)

    def test_live_sampler_can_exit_during_bounded_wait(self):
        self.stat()
        with patch.object(rehearsal.time, 'sleep', side_effect=lambda delay: (self.proc / '1234/stat').unlink()) as wait:
            self.assertEqual(rehearsal.sampler_exit(self.state, proc_root=self.proc)['disposition'], 'gone')
        self.assertEqual(wait.call_count, 1)

    def test_malformed_unreadable_and_missing_procfs_fail_closed(self):
        (self.proc / '1234/stat').write_text('malformed')
        with self.assertRaisesRegex(ValueError, 'malformed'):
            rehearsal.sampler_exit(self.state, proc_root=self.proc)
        with patch.object(Path, 'read_text', side_effect=PermissionError('not observable')):
            with self.assertRaisesRegex(ValueError, 'unobservable'):
                rehearsal.sampler_exit(self.state, proc_root=self.proc)
        with self.assertRaisesRegex(ValueError, 'procfs unavailable'):
            rehearsal.sampler_exit(self.state, proc_root=self.proc / 'absent')
        with self.assertRaisesRegex(ValueError, 'identity unavailable'):
            rehearsal.sampler_exit(dict(pid=1234), proc_root=self.proc)


if __name__ == '__main__':
    unittest.main()
