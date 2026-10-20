import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation import baseline_resources as resources


class BaselineResourceTests(unittest.TestCase):
    def test_proc_stat_parses_names_with_spaces_and_parentheses(self):
        tail = ['S'] + ['0'] * 21
        tail[11], tail[12], tail[17], tail[19], tail[21] = '12', '8', '4', '1234', '10'
        value = resources.proc_stat('42 (encoder worker (test)) ' + ' '.join(tail))
        self.assertEqual(value['comm'], 'encoder worker (test)')
        self.assertEqual(value['cpu_ticks'], 20)
        self.assertEqual(value['start_ticks'], 1234)
        self.assertEqual(value['threads'], 4)
        self.assertIsNone(resources.proc_stat('truncated'))

    def test_cgroup_v2_counts_and_missing_values_remain_distinct(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'cpu.stat').write_text('usage_usec 1200\nnr_throttled 4\n')
            (root / 'memory.current').write_text('0\n')
            (root / 'memory.max').write_text('max\n')
            (root / 'io.stat').write_text('8:1 rbytes=12 wbytes=34 rios=1\n')
            values, missing, pids_root = resources.cgroup_sample({'v2': root})
            self.assertEqual(values['memory.current'], 0)
            self.assertNotIn('memory.peak', values)
            self.assertIn('memory.peak', missing)
            self.assertEqual(values['cpu.stat']['nr_throttled'], 4)
            self.assertEqual(values['io.stat']['8:1']['wbytes'], 34)
            self.assertEqual(values['memory.max'], 'max')
            self.assertEqual(pids_root, root)

    def test_cgroup_resolution_uses_job_namespace_root(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root = Path(directory)
            (proc_root / 'self').mkdir()
            (proc_root / 'self/cgroup').write_text('0::/\n')
            (proc_root / 'self/mountinfo').write_text('1 2 0:3 /docker/job /sys/fs/cgroup rw - cgroup2 cgroup rw\n')
            self.assertEqual(resources.discover_cgroup(proc_root), {'v2': Path('/sys/fs/cgroup')})

    def test_cgroup_v1_explicitly_marks_unsampled_io_unavailable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            counters = {'cpuacct.usage': '100', 'cpu.stat': 'nr_throttled 0\n',
                        'memory.usage_in_bytes': '100', 'memory.max_usage_in_bytes': '200',
                        'memory.limit_in_bytes': '1000', 'memory.failcnt': '0',
                        'pids.current': '2', 'pids.max': '100'}
            for name, value in counters.items():
                (root / name).write_text(value)
            values, missing, _ = resources.cgroup_sample({key: root for key in ('cpuacct', 'cpu', 'memory', 'pids')})
            self.assertEqual(values['cpuacct.usage'], '100')
            self.assertEqual(missing, ['cgroup v1 IO counters unavailable (blkio not sampled)'])
            self.assertNotIn('io.stat', values)

    def test_cgroup_resolution_does_not_traverse_parent_path(self):
        with tempfile.TemporaryDirectory() as directory:
            proc_root = Path(directory)
            (proc_root / 'self').mkdir()
            (proc_root / 'self/cgroup').write_text('0::/../host\n')
            (proc_root / 'self/mountinfo').write_text('1 2 0:3 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n')
            self.assertEqual(resources.discover_cgroup(proc_root), {})

    def test_process_sampling_reads_only_explicit_cgroup_members_and_no_cmdline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            group = root / 'group'
            group.mkdir()
            (group / 'cgroup.procs').write_text('42\n999\n')
            process = root / '42'
            process.mkdir()
            tail = ['S'] + ['0'] * 21
            tail[11], tail[12], tail[17], tail[19], tail[21] = '12', '8', '4', '1234', '10'
            (process / 'stat').write_text('42 (ffmpeg) ' + ' '.join(tail))
            (process / 'io').write_text('read_bytes: 12\nwrite_bytes: 34\n')
            (process / 'cmdline').write_text('secret-token-must-not-be-read')
            result = resources.process_sample(group, root)
            self.assertEqual(result['by_comm'], {'ffmpeg': 1})
            self.assertEqual(result['raced_or_unreadable_processes'], 1)
            self.assertEqual(result['write_bytes_live_processes'], 34)
            self.assertNotIn('secret-token', json.dumps(result))
            self.assertFalse(resources.process_sample(None, root)['available'])

    def test_pid_reuse_is_not_treated_as_our_sampler(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '42').mkdir()
            tail = ['S'] + ['0'] * 21
            tail[19] = '5678'
            (root / '42/stat').write_text('42 (python3) ' + ' '.join(tail))
            self.assertFalse(resources.same_process({'pid': 42, 'start_ticks': 1234}, root))
            self.assertTrue(resources.same_process({'pid': 42, 'start_ticks': 5678}, root))
            self.assertFalse(resources.same_process({'pid': 1, 'start_ticks': 0}, root))

    @patch.dict('os.environ', {'GITHUB_TOKEN': 'secret', 'GITHUB_JOB': 'rust-unit', 'RUST_TEST_THREADS': '4'}, clear=True)
    @patch.object(resources, 'command_value', return_value='version-or-sha')
    def test_metadata_uses_environment_allowlist(self, _command):
        result = resources.metadata('rust-unit')
        self.assertEqual(result['environment'], {'GITHUB_JOB': 'rust-unit', 'RUST_TEST_THREADS': '4'})
        self.assertNotIn('secret', json.dumps(result))

    @patch.object(resources, 'command_value', return_value='version-or-sha')
    def test_external_container_metadata_never_claims_actual_test_resources(self, _command):
        result = resources.metadata('android_jvm', 'external-containers')
        self.assertEqual(result['execution_scope'], 'external-containers')
        self.assertEqual(result['observed_counter_scope'], 'current-job cgroup only')
        self.assertFalse(result['actual_test_resources']['available'])
        for counter in ('cpu', 'memory', 'io'):
            self.assertEqual(result['actual_test_resources'][counter], 'unavailable')

    def test_external_container_samples_remain_incomplete_even_with_job_counters(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'stop').touch()
            (root / 'metadata.json').write_text(json.dumps({'execution_scope': 'external-containers'}))
            value = {'at': 'test', 'elapsed_s': 1, 'cgroup': {'memory.current': 100},
                     'missing_counters': [], 'processes': {'available': True, 'rss_bytes_sum': 200},
                     'disk': {'workspace': {'available_bytes': 300}}}
            with patch.object(resources, 'discover_cgroup', return_value={}), patch.object(resources, 'sample_once', return_value=value):
                resources.sample(root, 'test-token')
            summary = json.loads((root / 'summary.json').read_text())
            self.assertEqual(summary['status'], 'incomplete')
            self.assertFalse(summary['actual_test_resources']['available'])
            self.assertIn('actual test CPU/memory/IO', summary['unavailable'][0])
            observation = json.loads((root / 'samples.jsonl').read_text())
            self.assertFalse(observation['actual_test_resources']['available'])
            self.assertEqual(observation['observed_counter_scope'], 'current-job cgroup only')

    @patch.object(resources.sys, 'platform', 'darwin')
    def test_unsupported_platform_preserves_wall_window_and_explicit_unavailable_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(resources, 'command_value', return_value='version-or-sha'), \
                 patch.object(resources.subprocess, 'Popen') as spawn, patch.object(resources.os, 'kill') as signal_process:
                resources.start(root, 'apple-unit')
                original = (root / 'telemetry-incomplete.json').read_bytes()
                resources.finish(root)
                resources.finish(root)
            self.assertEqual((root / 'telemetry-incomplete.json').read_bytes(), original)
            self.assertEqual(json.loads(original)['reason'], resources.UNSUPPORTED_PLATFORM_REASON)
            spawn.assert_not_called()
            signal_process.assert_not_called()
            for filename in ('state.json', 'samples.jsonl', 'summary.json'):
                self.assertFalse((root / filename).exists())
            result = json.loads((root / 'metadata.json').read_text())
            self.assertFalse(result['actual_test_resources']['available'])
            self.assertGreaterEqual(result['observed_telemetry_window_s'], 0)
            self.assertEqual(result['rustc'], 'version-or-sha')

    def test_finish_without_start_writes_incomplete_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            resources.finish(root)
            self.assertEqual(json.loads((root / 'telemetry-incomplete.json').read_text())['status'], 'incomplete')
            self.assertFalse((root / 'summary.json').exists())

    @patch.object(resources.sys, 'platform', 'linux')
    def test_linux_missing_state_still_reports_start_failure_when_metrics_unavailable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            resources.write_json(root / 'metadata.json', dict(platform='linux',
                actual_test_resources=dict(available=False, reason='external containers')))
            resources.incomplete(root, resources.UNSUPPORTED_PLATFORM_REASON)
            resources.finish(root)
            receipt = json.loads((root / 'telemetry-incomplete.json').read_text())
            self.assertEqual(receipt['reason'], 'sampler state missing; start did not complete')
            self.assertFalse((root / 'summary.json').exists())

    @patch.object(resources.sys, 'platform', 'darwin')
    def test_unsupported_platform_without_start_still_reports_missing_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            resources.finish(root)
            receipt = json.loads((root / 'telemetry-incomplete.json').read_text())
            self.assertEqual(receipt['reason'], 'sampler state missing; start did not complete')

    @patch.object(resources, 'metadata', return_value={'schema_version': 1})
    @patch.object(resources.sys, 'platform', 'darwin')
    def test_unsupported_platform_does_not_start_process_or_claim_metrics(self, _metadata):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(resources.subprocess, 'Popen') as spawn:
                resources.start(root, 'test-cell')
            spawn.assert_not_called()
            self.assertTrue((root / 'telemetry-incomplete.json').exists())
            self.assertFalse((root / 'samples.jsonl').exists())

    @patch.object(resources.sys, 'platform', 'linux')
    def test_sampler_writes_final_sample_and_preserves_missing_counter_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'stop').touch()
            value = {'at': 'test', 'elapsed_s': 1, 'cgroup': {'memory.current': 100},
                     'missing_counters': ['memory.peak'],
                     'processes': {'available': True, 'rss_bytes_sum': 200},
                     'disk': {'workspace': {'available_bytes': 300}}}
            with patch.object(resources, 'discover_cgroup', return_value={}), patch.object(resources, 'sample_once', return_value=value):
                resources.sample(root, 'test-token')
            report = json.loads((root / 'summary.json').read_text())
            self.assertEqual(report['samples'], 1)
            self.assertEqual(report['status'], 'incomplete')
            self.assertEqual(report['unavailable'], ['memory.peak'])
            self.assertEqual(report['peak_sampled_cgroup_memory_bytes'], 100)
            self.assertEqual(report['minimum_sampled_disk_available_bytes'], {'workspace': 300})
            self.assertEqual(len((root / 'samples.jsonl').read_text().splitlines()), 1)

    def test_finish_does_not_signal_unverified_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'state.json').write_text(json.dumps({'pid': 42, 'start_ticks': 1, 'token': 't'}))
            with patch.object(resources, 'same_process', return_value=False), patch.object(resources.os, 'kill') as kill:
                resources.finish(root)
            kill.assert_not_called()
            self.assertTrue((root / 'telemetry-incomplete.json').exists())

    def test_cli_telemetry_error_does_not_change_suite_exit_or_expose_error_text(self):
        with tempfile.TemporaryDirectory() as directory:
            arguments = ['baseline_resources', 'start', '--output', directory, '--cell', 'rust-unit']
            with patch.object(resources.sys, 'argv', arguments), patch.object(resources, 'start', side_effect=RuntimeError('private-token-value')):
                self.assertEqual(resources.main(), 0)
            report = json.loads((Path(directory) / 'telemetry-incomplete.json').read_text())
            self.assertIn('RuntimeError', report['reason'])
            self.assertNotIn('private-token-value', json.dumps(report))

    @patch.object(resources, 'command_value', return_value='rust-version')
    def test_finish_accepts_only_own_summary_and_is_idempotent(self, _command):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'state.json').write_text(json.dumps({'pid': 42, 'start_ticks': 1, 'token': 'ours'}))
            (root / 'summary.json').write_text(json.dumps({'token': 'ours', 'status': 'complete', 'samples': 2}))
            resources.finish(root)
            resources.finish(root)
            self.assertEqual(json.loads((root / 'state.json').read_text())['status'], 'finished')
            self.assertNotIn('token', json.loads((root / 'summary.json').read_text()))
            self.assertFalse((root / 'telemetry-incomplete.json').exists())


if __name__ == '__main__':
    unittest.main()
