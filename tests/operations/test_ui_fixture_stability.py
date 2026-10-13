"""Capture fixtures must retire actual startup work before photographing it."""
from pathlib import Path
import runpy
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
