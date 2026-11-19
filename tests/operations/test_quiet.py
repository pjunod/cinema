"""Concurrent command evidence must never share a quiet log."""
import concurrent.futures
import contextlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


class QuietLogs(unittest.TestCase):
    def test_same_tick_commands_keep_distinct_logs_and_exit_statuses(self):
        path = Path(__file__).resolve().parents[2] / "scripts" / "quiet"
        loader = importlib.machinery.SourceFileLoader("_quiet_regression", str(path))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        quiet = importlib.util.module_from_spec(spec)
        loader.exec_module(quiet)
        script = "import sys; print(sys.argv[1]); sys.exit(int(sys.argv[2]))"
        with tempfile.TemporaryDirectory(prefix="quiet-regression-") as directory:
            with mock.patch.object(quiet, "ROOT", Path(directory)), \
                    mock.patch.object(quiet.time, "strftime", return_value="20000101-000000"), \
                    contextlib.redirect_stdout(io.StringIO()):
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    jobs = [pool.submit(quiet.main, [sys.executable, "-c", script, name, code])
                            for name, code in [("first", "0"), ("second", "7")]]
                    self.assertEqual([job.result() for job in jobs], [0, 7])
            logs = list((Path(directory) / "target" / "quiet").glob("*.log"))
            self.assertEqual(len(logs), 2)
            self.assertEqual({log.read_text() for log in logs}, {"first\n", "second\n"})


if __name__ == "__main__":
    unittest.main()
