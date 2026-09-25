"""The caption audit must not report success when Cargo runs no matching test."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
AUDIT = ROOT / "scripts/live-tv-caption-audit"


class LiveTvCaptionAuditCase(unittest.TestCase):
    def test_audit_requires_one_completed_test(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            bin_dir = folder / "bin"
            bin_dir.mkdir()
            cargo = bin_dir / "cargo"
            cargo.write_text(
                '#!/bin/sh\nprintf "%s\\n" "$FAKE_CARGO_OUTPUT"\n'
                'exit "$FAKE_CARGO_EXIT"\n',
                encoding="utf-8",
            )
            cargo.chmod(0o755)
            capture = folder / "capture.ts"
            capture.touch()
            env = os.environ.copy()
            env["PATH"] = f"{bin_dir}:{env['PATH']}"
            env["TMPDIR"] = str(folder)

            def run(args: list[str], passed: int, exit_code: int = 0):
                env["FAKE_CARGO_OUTPUT"] = (
                    f"test result: ok. {passed} passed; 0 failed; 0 ignored; "
                    "0 measured; 0 filtered out"
                )
                env["FAKE_CARGO_EXIT"] = str(exit_code)
                return subprocess.run(
                    [str(AUDIT), *args],
                    cwd=ROOT,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                )

            for args in (["analyze", str(capture)], ["software"]):
                with self.subTest(mode=args[0]):
                    empty = run(args, 0)
                    self.assertNotEqual(empty.returncode, 0)
                    self.assertIn("expected exactly one passing test", empty.stderr)

                    multiple = run(args, 2)
                    self.assertNotEqual(multiple.returncode, 0)

                    success = run(args, 1)
                    self.assertEqual(success.returncode, 0, success.stderr)

                    failed = run(args, 1, exit_code=17)
                    self.assertEqual(failed.returncode, 17)


if __name__ == "__main__":
    unittest.main()
