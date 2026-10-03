"""Safety and retained-data contracts for the isolated upgrade runner."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/qualify-sharing-coordinated-upgrade.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("sharing_coordinated_runner", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CoordinatedUpgradeQualificationTests(unittest.TestCase):
    def test_existing_source_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "existing"
            source.mkdir()
            marker = source / "user-source.txt"
            marker.write_text("keep this source\n")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--source-dir", str(source),
                 "--target-dir", str(Path(directory) / "target")],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn("source-dir must not exist", result.stderr)
            self.assertEqual(marker.read_text(), "keep this source\n")
            self.assertEqual(list(source.iterdir()), [marker])

    def test_unpinned_compiler_refuses_before_source_extraction(self):
        module = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            arguments = [str(SCRIPT), "--source-dir", str(source),
                         "--target-dir", str(Path(directory) / "target")]
            error = io.StringIO()
            with patch.object(sys, "argv", arguments), patch.object(
                module.subprocess, "check_output", return_value="rustc 1.98.0 (different)\n"
            ) as command, contextlib.redirect_stderr(error):
                with self.assertRaises(SystemExit) as refusal:
                    module.main()
            self.assertEqual(refusal.exception.code, 2)
            command.assert_called_once_with(["rustc", "--version"], text=True)
            self.assertFalse(source.exists())

    def test_daemon_readiness_accepts_plain_text_ready_response(self):
        module = load_runner()
        daemon = object.__new__(module.Daemon)
        daemon.node = {"base": "http://127.0.0.1:1"}
        daemon.log = Path("unused-test-log")
        daemon.process = Mock()
        daemon.process.poll.return_value = None
        response = io.BytesIO(b"ready\n")
        response.status = 200
        with patch.object(module.urllib.request, "urlopen", return_value=response) as request:
            daemon.ready()
        request.assert_called_once_with("http://127.0.0.1:1/readyz", timeout=5)

    def test_retention_detects_changed_old_values_and_allows_new_owner_columns(self):
        module = load_runner()
        before = {"media_sessions": {"columns": ["incarnation", "user_id"],
                                      "rows": [{"incarnation": "old", "user_id": 1}]}}
        rebuilt = {"media_sessions": {"columns": ["incarnation", "user_id", "owner_key"],
                                       "rows": [{"incarnation": "old", "user_id": 1,
                                                 "owner_key": "user:1"}]}}
        module.compare_retained(before, rebuilt)
        rebuilt["media_sessions"]["rows"][0]["user_id"] = 2
        with self.assertRaisesRegex(RuntimeError, "retained row difference: media_sessions"):
            module.compare_retained(before, rebuilt)


if __name__ == "__main__":
    unittest.main()
