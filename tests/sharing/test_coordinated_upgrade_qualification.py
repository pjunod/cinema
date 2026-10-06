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

    def test_terminal_cleanup_requires_exact_fixture_lease_without_loosening_rebuild_inventory(self):
        module = load_runner()
        before = {"job_leases": {"columns": ["resource", "revision"],
                                 "rows": [{"resource": module.TERMINAL_FIXTURE_LEASE,
                                           "revision": 3},
                                          {"resource": module.FRESH_FIXTURE_LEASE, "revision": 2},
                                          {"resource": "daemon-worker", "revision": 1}]}}
        after = {"job_leases": {"columns": ["resource", "revision"],
                                "rows": [{"resource": module.FRESH_FIXTURE_LEASE, "revision": 2},
                                         {"resource": "daemon-worker", "revision": 2}]}}
        with self.assertRaisesRegex(RuntimeError, "retained row difference: job_leases"):
            module.compare_retained(before, after)
        module.compare_retained(before, after, terminal_cleanup=True)
        after["job_leases"]["rows"].append(before["job_leases"]["rows"][0])
        with self.assertRaisesRegex(RuntimeError, "lease cleanup was not proven"):
            module.compare_retained(before, after, terminal_cleanup=True)
        with self.assertRaisesRegex(RuntimeError, "lease cleanup was not proven"):
            module.compare_retained({"job_leases": {"columns": ["resource"], "rows": []}},
                                    {"job_leases": {"columns": ["resource"], "rows": []}},
                                    terminal_cleanup=True)

    def test_runtime_cleanup_preserves_fresh_session_and_requires_stale_removal(self):
        module = load_runner()
        before = {"media_sessions": {"columns": ["incarnation_id", "label"],
                                     "rows": [{"incarnation_id": module.FRESH_FIXTURE_INCARNATION,
                                               "label": "retained"},
                                              {"incarnation_id": module.STALE_FIXTURE_INCARNATION,
                                               "label": "expired"}]}}
        after = {"media_sessions": {"columns": ["incarnation_id", "label", "owner_key"],
                                    "rows": [{"incarnation_id": module.FRESH_FIXTURE_INCARNATION,
                                              "label": "retained", "owner_key": "user:1"}]}}
        module.compare_retained(before, after, terminal_cleanup=True)
        after["media_sessions"]["rows"][0]["label"] = "changed"
        with self.assertRaisesRegex(RuntimeError, "retained row difference"):
            module.compare_retained(before, after, terminal_cleanup=True)
        after["media_sessions"]["rows"][0]["label"] = "retained"
        after["media_sessions"]["rows"].append(before["media_sessions"]["rows"][1])
        with self.assertRaisesRegex(RuntimeError, "expected terminal cleanup missing"):
            module.compare_retained(before, after, terminal_cleanup=True)

    def test_active_media_never_sends_fixture_credentials_off_node(self):
        module = load_runner()
        with patch.object(module.urllib.request, "build_opener") as opened:
            with self.assertRaisesRegex(RuntimeError, "escaped its isolated node"):
                module.media_object({"base": "http://127.0.0.1:32400"},
                                    "http://elsewhere.invalid/segment.m4s", "fixture-token")
            opened.assert_not_called()

    def test_active_media_redirect_refuses_before_forwarding_credentials(self):
        module = load_runner()
        with self.assertRaisesRegex(RuntimeError, "redirect refused"):
            module.NoMediaRedirect().redirect_request(None, None, 302, "redirect", {},
                                                      "http://elsewhere.invalid/media")

    def test_active_worker_evidence_only_includes_owned_descendants(self):
        module = load_runner()
        inventory = "100 1 plurxd\n101 100 ffmpeg\n102 101 ffmpeg\n200 1 ffmpeg\n"
        with patch.object(module.subprocess, "check_output", return_value=inventory):
            self.assertEqual(module.owned_workers(100), {101: "ffmpeg", 102: "ffmpeg"})

    def test_active_drain_waits_for_daemon_and_rejects_surviving_encoder(self):
        module = load_runner()
        daemon = Mock()
        calls = []
        daemon.stop.side_effect = lambda: calls.append("signal")
        daemon.wait.side_effect = lambda: calls.append("wait")
        def inventory(*args, **kwargs):
            calls.append("observe")
            return "101\n"
        with patch.object(module.subprocess, "check_output", side_effect=inventory):
            with self.assertRaisesRegex(RuntimeError, "encoder survived"):
                module.drain_active(daemon, {101: "ffmpeg"})
        self.assertEqual(calls, ["signal", "wait", "observe"])
        with patch.object(module.subprocess, "check_output", return_value="200\n"):
            module.drain_active(daemon, {101: "ffmpeg"})

    def test_active_fixture_restores_complete_stopped_historical_directory(self):
        module = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            media = parent / "supplied.mp4"
            media.write_bytes(b"supplied fixture")
            root = parent / "owned-fixture"
            observed = []
            def stage(binary, node, label, file_id=None, previous_session=None):
                state = node["data"] / "whole-state"
                if label == "active-historical":
                    state.write_text("historical stopped state")
                else:
                    observed.append((label, state.read_text(), file_id, previous_session))
                    state.write_text("candidate mutated state")
                return {"file_id": 17, "session_id": label}
            with patch.object(module, "ports", return_value=[1, 2, 3]), patch.object(
                module, "active_local_stage", side_effect=stage
            ):
                result = module.active_local_fixture("old", "new", root, media)
            self.assertEqual(observed, [
                ("active-candidate", "historical stopped state", 17, "active-historical"),
                ("active-restored", "historical stopped state", 17, "active-historical")])
            self.assertEqual((root / "parked-candidate" / "whole-state").read_text(),
                             "candidate mutated state")
            self.assertEqual(result["shared_relay_drain"], "not qualified")
            self.assertEqual(media.read_bytes(), b"supplied fixture")


if __name__ == "__main__":
    unittest.main()
