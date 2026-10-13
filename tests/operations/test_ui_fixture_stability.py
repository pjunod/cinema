"""Capture fixtures must retire actual startup work before photographing it."""
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
from unittest.mock import MagicMock, patch
import unittest

ROOT = Path(__file__).resolve().parents[2]


class UiFixtureStabilityCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = runpy.run_path(str(ROOT / 'scripts/ui-baseline'), run_name='ui_fixture_stability')

    def wait(self, process_rows, logs, timeout=4):
        clock = [0.0]
        def sleep(seconds):
            clock[0] += seconds
        def read_processes(budget):
            self.assertGreater(budget, 0)
            return process_rows(clock[0])
        self.fixture['wait_for_fixture_idle'](read_processes, lambda: logs(clock[0]),
            lambda: True, timeout=timeout, quiet_seconds=1,
            now=lambda: clock[0], sleep=sleep)
        return clock[0]

    def test_empty_child_gap_cannot_substitute_boot_audit_completion(self):
        finished = self.wait(lambda t: [],
                             lambda t: '' if t < 2 else 'caption graph probe complete')
        self.assertGreaterEqual(finished, 3)

    def test_new_child_resets_the_quiet_interval(self):
        finished = self.wait(lambda t: [{'purpose': 'Live TV caption probe'}]
                             if 0.5 <= t < 1.5 else [],
                             lambda t: 'caption graph probe complete')
        self.assertGreaterEqual(finished, 2.5)

    def test_timeout_preserves_actual_child_diagnostics(self):
        with self.assertRaisesRegex(self.fixture['Fail'], 'Live TV caption probe'):
            self.wait(lambda t: [{'purpose': 'Live TV caption probe', 'pid': 123}],
                      lambda t: 'caption graph probe complete', timeout=1)

    def test_prior_boot_terminal_record_cannot_satisfy_the_capture_boot(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'daemon.log'
            old = b'caption graph probe complete\n'
            path.write_bytes(old + b'capture boot started\n')
            current = self.fixture['read_current_boot_log'](path, len(old))
            self.assertNotIn('caption graph probe complete', current)
            with self.assertRaises(self.fixture['Fail']):
                self.wait(lambda t: [], lambda t: current, timeout=1)
            with path.open('ab') as source:
                source.write(b'caption graph probe complete\n')
            self.assertIn('caption graph probe complete',
                          self.fixture['read_current_boot_log'](path, len(old)))

    def test_capture_child_forces_its_reserved_port_over_inherited_environment(self):
        with tempfile.TemporaryDirectory() as root:
            cls = self.fixture['SelfHost']
            with patch.dict(cls.__init__.__globals__,
                            {'free_ports': lambda preferred, count: [19000, 19001, 19002, 19003]}):
                host = cls(SimpleNamespace(out=root, port=0))
            host.log.parent.mkdir(parents=True)
            host.server_bin = lambda: Path('test-plurxd')
            child, response = MagicMock(), MagicMock()
            child.poll.return_value = None
            response.__enter__.return_value.status = 200
            globals_ = cls.spawn.__globals__
            with patch.dict(globals_['os'].environ, {'PLURX_JELLYFIN_PORT': '0'}), \
                 patch.object(globals_['subprocess'], 'Popen', return_value=child) as spawn, \
                 patch.object(globals_['urllib'].request, 'urlopen', return_value=response):
                try:
                    host.spawn()
                    self.assertEqual(spawn.call_args.kwargs['env']['PLURX_JELLYFIN_PORT'], '19003')
                finally:
                    if spawn.called:
                        spawn.call_args.kwargs['stdout'].close()
