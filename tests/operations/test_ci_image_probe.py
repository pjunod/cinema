"""scripts/ci-image-probe: an unreviewed skip, or a stale review, fails the probe."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci-image-probe"

PASSING = """import unittest
class Case(unittest.TestCase):
    def test_runs(self):
        print("tests write to stdout freely")
"""
TOOL_GATED = """import shutil, unittest
class Case(unittest.TestCase):
    @unittest.skipUnless(shutil.which("no-such-tool-in-any-image"), "tool missing")
    def test_needs_a_tool(self):
        pass
    def test_runs(self):
        pass
"""
FAILING = """import unittest
class Case(unittest.TestCase):
    def test_breaks(self):
        self.fail("broken in the image")
"""
ENTRY = """[[skip]]
suite = "{suite}"
test = "{test}"
reason = "a reason long enough to be one, for the fixture"
"""


class CiImageProbeCase(unittest.TestCase):
    def setUp(self):
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        self.root = Path(self._directory.name)
        (self.root / "scripts").mkdir()
        shutil.copy(SCRIPT, self.root / "scripts/ci-image-probe")
        for suite in ("tests/validation", "tests/operations"):
            (self.root / suite).mkdir(parents=True)
        (self.root / "tests/validation/test_plain.py").write_text(PASSING, encoding="utf-8")
        (self.root / "tests/operations/test_gated.py").write_text(TOOL_GATED, encoding="utf-8")
        self.allow = self.root / "tests/ci-image-skips.toml"
        self.allow.write_text("", encoding="utf-8")

    def probe(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(self.root / "scripts/ci-image-probe")],
            capture_output=True, text=True, timeout=120,
        )

    def test_a_skip_nobody_reviewed_fails_the_probe(self):
        result = self.probe()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(
            "tests/operations: skipped without review: test_gated.Case.test_needs_a_tool (tool missing)",
            result.stdout,
        )
        self.assertIn("tests/validation: ran 1, failures 0, errors 0, skipped 0", result.stdout)

    def test_a_reviewed_skip_passes_and_is_printed_with_its_reason(self):
        self.allow.write_text(
            ENTRY.format(suite="tests/operations", test="test_gated.Case.test_needs_a_tool"),
            encoding="utf-8",
        )
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("reviewed skip: test_gated.Case.test_needs_a_tool (tool missing)", result.stdout)

    def test_a_review_whose_test_no_longer_skips_fails_the_probe(self):
        self.allow.write_text(
            ENTRY.format(suite="tests/operations", test="test_gated.Case.test_needs_a_tool")
            + ENTRY.format(suite="tests/operations", test="test_gated.Case.test_runs"),
            encoding="utf-8",
        )
        result = self.probe()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(
            "tests/operations: listed in ci-image-skips.toml but did not skip: test_gated.Case.test_runs",
            result.stdout,
        )

    def test_a_failing_test_or_an_empty_suite_fails_the_probe(self):
        self.allow.write_text(
            ENTRY.format(suite="tests/operations", test="test_gated.Case.test_needs_a_tool"),
            encoding="utf-8",
        )
        (self.root / "tests/validation/test_plain.py").write_text(FAILING, encoding="utf-8")
        result = self.probe()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("tests/validation: failure: test_plain.Case.test_breaks", result.stdout)
        (self.root / "tests/validation/test_plain.py").unlink()
        result = self.probe()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("tests/validation: no test ran", result.stdout)

    def test_an_entry_without_a_real_reason_is_refused(self):
        self.allow.write_text(
            '[[skip]]\nsuite = "tests/operations"\ntest = "test_gated.Case.test_needs_a_tool"\nreason = "ok"\n',
            encoding="utf-8",
        )
        result = self.probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no real reason", result.stderr)

    def test_the_tree_reviews_no_skip_for_a_missing_tool(self):
        listed = subprocess.run(
            [sys.executable, str(SCRIPT), "--list"], capture_output=True, text=True, check=True
        ).stdout
        for excuse in ("unavailable", "requires autoconf", "root deletes"):
            self.assertNotIn(excuse, listed)


if __name__ == "__main__":
    unittest.main()
