"""The historical qualification runner must never overlay user source."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/qualify-sharing-old-store.py"


class HistoricalStoreQualificationTests(unittest.TestCase):
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

    def test_unpinned_compiler_refuses_before_archiving_or_extracting(self):
        spec = importlib.util.spec_from_file_location("sharing_old_store_runner", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
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
            self.assertIn("repository-pinned Rust 1.97.1 required", error.getvalue())
            self.assertFalse(source.exists())


if __name__ == "__main__":
    unittest.main()
