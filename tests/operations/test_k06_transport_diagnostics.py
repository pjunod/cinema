"""New diagnostic contracts: local fake transport, never SSH or lab mutation."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from .test_k06_owned_lab import LAB, manifest
else:
    from test_k06_owned_lab import LAB, manifest


class ClockTransportDiagnosticsTests(unittest.TestCase):
    def invoke(self, root, stderr, stdout=b"", code=2, timeout=False):
        def transport(args, **kwargs):
            self.assertEqual(kwargs["timeout"], 30)
            self.assertIsNot(kwargs["stdout"], kwargs["stderr"])
            self.assertNotIn("fixture-secret", " ".join(args))
            kwargs["stderr"].write(stderr)
            kwargs["stdout"].write(stdout)
            if timeout:
                raise subprocess.TimeoutExpired(args, 30)
            return subprocess.CompletedProcess(args, code)
        with patch.object(LAB.subprocess, "run", transport), \
             patch("tempfile.mkdtemp", return_value=str(root)):
            return LAB.remote(Path("/fixture/key"), manifest(), manifest()["nodes"][0],
                              "sample", "fixture-secret")

    def test_remote_failure_retains_exact_policy_but_never_echoed_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = b"fixture-secret\nk06-owned-lab: active swap traffic\nk06-owned-lab: host not idle fixture-secret\n"
            with self.assertRaises(ValueError) as failure:
                self.invoke(root, raw, b'{"token":"fixture-secret"}')
            self.assertIn("active swap traffic", str(failure.exception))
            self.assertNotIn("fixture-secret", str(failure.exception))
            path = root / "stderr-evidence.json"
            evidence = json.loads(path.read_text())
            self.assertEqual(evidence["stderr_sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(evidence["stderr_bytes"], len(raw))
            self.assertEqual(evidence["policy_reasons"], ["active swap traffic"])
            self.assertNotIn("fixture-secret", path.read_text())
            self.assertFalse(evidence["stdout_retained"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_remote_stderr_tail_is_bounded_and_timeout_is_safe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = b"fixture-secret\n" * 100000
            with self.assertRaises(ValueError) as failure:
                self.invoke(root, raw, timeout=True)
            self.assertIn("remote transport refused", str(failure.exception))
            self.assertNotIn("fixture-secret", str(failure.exception))
            evidence = json.loads((root / "stderr-evidence.json").read_text())
            self.assertTrue(evidence["timed_out"])
            self.assertTrue(evidence["tail_truncated"])
            self.assertEqual(evidence["stderr_sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(evidence["stderr_bytes"], len(raw))
            self.assertLessEqual(len(evidence["redacted_tail"].encode()), 65536)
            self.assertNotIn("fixture-secret", evidence["redacted_tail"])

    def test_successful_remote_json_remains_separate_without_failure_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.invoke(root, b"fixture-secret\n", b'{"actual":true}', code=0)
            self.assertEqual(result, {"actual": True})
            self.assertEqual(list(root.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
