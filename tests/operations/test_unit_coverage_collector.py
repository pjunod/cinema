"""Keep the offline collector guards in the ordinary operations CI suite."""
from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CoverageCollectorContract(unittest.TestCase):
    def test_offline_source_object_feature_and_publication_guards(self):
        env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        result = subprocess.run(
            [sys.executable, "-m", "unittest", "discover", "-s",
             "scripts/unit_coverage", "-p", "test_*.py", "-q"],
            cwd=ROOT, env=env, capture_output=True, text=True, timeout=30,
        )
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, output[-6000:])
        summary = re.search(r"Ran (\d+) tests? in", output)
        self.assertIsNotNone(summary, output[-6000:])
        self.assertGreaterEqual(int(summary.group(1)), 28,
                                "the existing collector guards must remain exercised")
