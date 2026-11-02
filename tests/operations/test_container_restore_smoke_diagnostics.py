"""Failure diagnostics preserve container stderr without dumping configuration."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ContainerRestoreDiagnosticsTests(unittest.TestCase):
    def test_failed_readiness_preserves_stderr_and_safe_state_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            bindir = Path(directory)
            commands = {
                "docker": r'''#!/bin/sh
case "$1" in
  port) echo '127.0.0.1:12345' ;;
  logs) echo 'daemon startup failure sentinel' >&2 ;;
  inspect)
    test "$2" = --format || exit 9
    test "$3" = 'Status={{.State.Status}} ExitCode={{.State.ExitCode}} OOMKilled={{.State.OOMKilled}} Error={{json .State.Error}}' || exit 10
    echo 'Status=exited ExitCode=1 OOMKilled=false Error="startup failure"'
    ;;
esac
''',
                "curl": "#!/bin/sh\nexit 1\n",
                "sleep": "#!/bin/sh\nexit 0\n",
            }
            for name, body in commands.items():
                path = bindir / name
                path.write_text(body)
                path.chmod(0o755)
            result = subprocess.run(
                ["sh", str(ROOT / "scripts/container-restore-smoke"), "fake-image"],
                env={**os.environ, "PATH": str(bindir) + os.pathsep + os.environ["PATH"]},
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("source instance did not become ready", result.stderr)
            self.assertIn("daemon startup failure sentinel", result.stderr)
            self.assertIn('Status=exited ExitCode=1 OOMKilled=false Error="startup failure"', result.stderr)
            self.assertNotIn("startup failure sentinel", result.stdout)
