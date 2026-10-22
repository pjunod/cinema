from __future__ import annotations

from contextlib import ExitStack, contextmanager
from concurrent.futures import ThreadPoolExecutor
import fcntl

import json
import errno
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest import mock

from validation import baseline_commands as commands


@contextmanager
def mocked_waitid_capability(**options):
    """Unit-contract capability, including Python builds that omit waitid."""
    defaults = dict(P_PID=1, WEXITED=4, WNOWAIT=0x01000000, WNOHANG=1,
                    CLD_EXITED=1, CLD_KILLED=2, CLD_DUMPED=3)
    with ExitStack() as stack:
        for name, default in defaults.items():
            stack.enter_context(mock.patch.object(commands.os, name, getattr(commands.os, name, default), create=True))
        yield stack.enter_context(mock.patch.object(commands.os, "waitid", create=True, **options))


@contextmanager
def mocked_anchored_process(returncode=0):
    """Signal/error unit tests do not rely on the interpreter's waitid build."""
    process = SimpleNamespace(pid=400, returncode=None)
    def reap(timeout):
        process.returncode = returncode
        return returncode
    process.wait = mock.Mock(side_effect=reap)
    observed = SimpleNamespace(si_pid=400, si_code=getattr(os, "CLD_EXITED", 1), si_status=returncode)
    with mocked_waitid_capability(return_value=observed), mock.patch.object(commands.subprocess, "Popen", return_value=process):
        yield process


class BaselineCommandsCase(unittest.TestCase):
    identity = dict(candidate_sha="a" * 40, candidate_tree="b" * 40)

    def execute(self, root, cells, **overrides):
        manifest = root / "manifest.json"
        manifest.write_text(json.dumps({**self.identity, "cells": cells, **overrides}))
        with mock.patch.object(commands, "source_identity", return_value=self.identity):
            return commands.run_manifest(manifest, root / "results", root, grace=0.2)

    def cell(self, name, code, timeout=5):
        return dict(id=name, argv=[sys.executable, "-c", code], timeout_seconds=timeout)

    def test_failure_does_not_hide_independent_cell_or_original_returncode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.execute(root, [self.cell("bad", "print('original failure');raise SystemExit(7)"),
                                         self.cell("after", "print('independent ran')")])
            self.assertFalse(result["success"])
            self.assertEqual([cell["returncode"] for cell in result["cells"]], [7, 0])
            self.assertIn("original failure", (root / "results/bad.log").read_text())
            self.assertIn("independent ran", (root / "results/after.log").read_text())
            self.assertEqual(result["source"], self.identity)
            self.assertTrue(result["manifest_sha256"])
            self.assertEqual((root / "manifest.json").read_bytes(), (root / "results/input-manifest.json").read_bytes())

    def test_native_leader_inherits_lease_until_success_or_original_failure(self):
        for returncode in (0, 7):
            with self.subTest(returncode=returncode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                lock = root / "admission.lock"
                descriptor = os.open(lock, os.O_CREAT | os.O_RDWR, 0o600)
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                contender = os.open(lock, os.O_RDWR)
                expected_inode = os.fstat(descriptor).st_ino
                child = ("import os,pathlib,sys,time;"
                         f"assert os.fstat({descriptor}).st_ino == {expected_inode};"
                         "pathlib.Path('ready').touch();deadline=time.monotonic()+5;"
                         "\nwhile not pathlib.Path('release').exists() and time.monotonic()<deadline: time.sleep(0.01)"
                         "\nassert pathlib.Path('release').exists(), 'controller did not release native leader'"
                         f"\nraise SystemExit({returncode})")
                try:
                    with ThreadPoolExecutor(max_workers=1) as pool:
                        future = pool.submit(commands.run_process, [sys.executable, "-c", child],
                                             subprocess.DEVNULL, subprocess.DEVNULL, 8,
                                             cwd=root, grace=0.2, pass_fds=(descriptor,))
                        try:
                            deadline = time.monotonic() + 3
                            while not (root / "ready").exists() and time.monotonic() < deadline:
                                time.sleep(0.01)
                            self.assertTrue((root / "ready").exists(), "native child did not inherit the lease")
                            os.close(descriptor)
                            descriptor = None
                            # Only the running native leader now retains the
                            # open-file-description lock; controller close is safe.
                            with self.assertRaises(BlockingIOError):
                                fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        finally:
                            (root / "release").touch()
                        result = future.result(timeout=5)
                    self.assertEqual(result["returncode"], returncode)
                    self.assertEqual(result["success"], returncode == 0)
                    self.assertTrue(result["cleanup_complete"], result)
                    self.assertFalse(result["timed_out"])
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    if descriptor is not None:
                        os.close(descriptor)
                    os.close(contender)

    def test_cancel_terminates_owned_descendant_reaps_leader_and_retains_child_lease(self):
        original_wait = commands.wait_owned_leader
        for use_waitid in (True, False):
            with self.subTest(use_waitid=use_waitid), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                descriptor = os.open(root / "admission.lock", os.O_CREAT | os.O_RDWR, 0o600)
                contender = os.open(root / "admission.lock", os.O_RDWR)
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                canceled = threading.Event()
                descendant = ("import pathlib,signal,time;"
                              "signal.signal(signal.SIGTERM,lambda *_: pathlib.Path('term-seen').touch());"
                              "pathlib.Path('descendant-ready').touch();time.sleep(30)")
                leader = ("import pathlib,subprocess,sys,time;"
                          f"subprocess.Popen([sys.executable,'-c',{descendant!r}],pass_fds=({descriptor},));"
                          "deadline=time.monotonic()+5;"
                          "\nwhile not pathlib.Path('descendant-ready').exists() and time.monotonic()<deadline: time.sleep(0.01)"
                          "\nassert pathlib.Path('descendant-ready').exists()"
                          "\npathlib.Path('ready').touch();time.sleep(30)")
                def wait(process, timeout, **kwargs):
                    return original_wait(process, timeout, use_waitid=use_waitid, **kwargs)
                try:
                    with mock.patch.object(commands, 'wait_owned_leader', side_effect=wait), ThreadPoolExecutor(max_workers=1) as pool:
                        future = pool.submit(commands.run_process, [sys.executable, '-c', leader],
                                             subprocess.DEVNULL, subprocess.DEVNULL, 8, cwd=root,
                                             grace=1, pass_fds=(descriptor,), cancel_event=canceled)
                        try:
                            deadline = time.monotonic() + 3
                            while not (root / 'ready').exists() and time.monotonic() < deadline:
                                time.sleep(0.01)
                            self.assertTrue((root / 'ready').exists(), 'owned leader/descendant not ready')
                            os.close(descriptor)
                            descriptor = None
                            canceled.set()
                            deadline = time.monotonic() + 2
                            while not (root / 'term-seen').exists() and time.monotonic() < deadline:
                                time.sleep(0.01)
                            self.assertTrue((root / 'term-seen').exists(), 'group cancellation did not reach descendant')
                            # The TERM-resistant native descendant still holds
                            # its explicitly inherited lease during cleanup.
                            with self.assertRaises(BlockingIOError):
                                fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        finally:
                            canceled.set()
                        result = future.result(timeout=5)
                    self.assertTrue(result['canceled'])
                    self.assertFalse(result['timed_out'])
                    self.assertFalse(result['success'])
                    self.assertEqual(result['returncode'], -signal.SIGTERM)
                    self.assertIn('SIGKILL', result['cleanup_signals'])
                    self.assertTrue(result['cleanup_complete'], result)
                    self.assertTrue(result['leader_reaped_after_cleanup'])
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    if descriptor is not None:
                        os.close(descriptor)
                    os.close(contender)

    def test_cancellation_before_spawn_never_starts_a_command(self):
        canceled = threading.Event()
        canceled.set()
        with mock.patch.object(commands.subprocess, 'Popen') as spawn:
            result = commands.run_process(['must-not-start'], None, None, 1, cancel_event=canceled)
        spawn.assert_not_called()
        self.assertTrue(result['canceled'])
        self.assertFalse(result['success'])
        self.assertTrue(result['cleanup_complete'])
        self.assertIsNone(result['returncode'])

    def test_cancel_with_already_observed_failure_preserves_original_exit(self):
        canceled = threading.Event()
        def wait(process, timeout, **kwargs):
            canceled.set()
            return dict(held=True, mechanism='waitid-WNOWAIT', timed_out=False,
                        canceled=True, returncode=7)
        with mocked_anchored_process(returncode=7), mock.patch.object(commands, 'wait_owned_leader', side_effect=wait), mock.patch.object(commands, 'observe_group', return_value=dict(status='nonexecuting')):
            result = commands.run_process(['native'], None, None, 1, cancel_event=canceled)
        self.assertTrue(result['canceled'])
        self.assertEqual(result['returncode'], 7)
        self.assertFalse(result['success'])
        self.assertTrue(result['cleanup_complete'])
        self.assertTrue(result['leader_reaped_after_cleanup'])

    def test_invalid_missing_or_duplicate_identity_prevents_every_command(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            good = self.cell("one", "raise RuntimeError('must not run')")
            for cells in ([good, good], [{k:v for k,v in good.items() if k != "id"}], [],
                          [{**good, "cwd": "../"}], [{**good, "timeout_seconds": 0}]):
                with self.subTest(cells=cells):
                    result = self.execute(root, cells)
                    self.assertFalse(result["success"])
                    self.assertEqual(result["cells"], [])
                    self.assertTrue(result["errors"])

    def test_wrong_source_and_symlink_escape_refuse_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cell = self.cell("one", "raise RuntimeError('must not run')")
            result = self.execute(root, [cell], candidate_sha="c" * 40)
            self.assertFalse(result["success"])
            self.assertEqual(result["cells"], [])
            (root / "outside").symlink_to(root.parent, target_is_directory=True)
            result = self.execute(root, [{**cell, "cwd": "outside"}])
            self.assertFalse(result["success"])
            self.assertEqual(result["cells"], [])

    def test_timeout_kills_descendant_even_when_leader_exits_on_term(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child = "import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(30)"
            code = ("import subprocess,sys,time,pathlib;"
                    f"p=subprocess.Popen([sys.executable,'-c',{child!r}]);"
                    "pathlib.Path('child.pid').write_text(str(p.pid));time.sleep(30)")
            result = self.execute(root, [self.cell("timeout", code, timeout=0.4), self.cell("after", "print('after timeout')")])
            self.assertEqual(len(result["cells"]), 2, result.get("errors"))
            outcome = result["cells"][0]
            self.assertTrue(outcome["timed_out"])
            self.assertIn("SIGKILL", outcome["cleanup_signals"])
            self.assertFalse(result["success"])
            self.assertTrue(result["cells"][1]["success"])
            pid = int((root / "child.pid").read_text())
            # A killed orphan may briefly be a zombie; it cannot execute work.
            proc_stat = Path(f"/proc/{pid}/stat")
            if proc_stat.exists():
                self.assertEqual(proc_stat.read_text().split()[2], "Z")
            else:
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    def test_normal_leader_exit_reaps_background_group_before_next_cell(self):
        for returncode in (0, 7):
            with self.subTest(returncode=returncode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                child = ("import pathlib,signal,socket;"
                         "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                         "control=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);"
                         "control.bind('child.control');control.listen(1);control.settimeout(30);"
                         "pathlib.Path('child.ready').touch();"
                         "connection,_=control.accept();connection.recv(16);connection.close();control.close()")
                code = ("import subprocess,sys,time,pathlib;"
                        f"p=subprocess.Popen([sys.executable,'-c',{child!r}]);"
                        "pathlib.Path('child.pid').write_text(str(p.pid));"
                        "deadline=time.monotonic()+3;"
                        "\nwhile not pathlib.Path('child.ready').exists() and time.monotonic()<deadline: time.sleep(0.01)"
                        f"\nraise SystemExit({returncode})")
                try:
                    result = self.execute(root, [self.cell("leaked", code),
                                                 self.cell("after", "print('independent ran')")])
                    self.assertTrue(result["cells"], result.get("errors"))
                    outcome = result["cells"][0]
                    if (sys.platform == "darwin"
                            and outcome.get("cleanup_error_errno") in (errno.EPERM, errno.EACCES)):
                        self.assertEqual(outcome["returncode"], returncode)
                        self.assertFalse(outcome["success"])
                        self.assertFalse(result["success"])
                        self.assertEqual(result["blocked_cells"], ["after"])
                        self.assertEqual(len(result["cells"]), 1)
                        self.skipTest("macOS denied signalling the reparented fixture process group; "
                                      "real orphan-group cleanup capability is unavailable in this sandbox")
                    if outcome["leader_anchor"]["mechanism"] == "reaped-fallback":
                        # Python3.12 on macOS lacks waitid. Once the leader is
                        # reaped, a remaining group has no safe signal anchor.
                        self.assertEqual(outcome["returncode"], returncode)
                        self.assertFalse(outcome["timed_out"])
                        self.assertTrue(outcome["cleanup_observation_unknown"])
                        self.assertFalse(outcome["cleanup_complete"])
                        self.assertFalse(outcome["success"])
                        self.assertEqual(outcome["cleanup_signals"], [])
                        self.assertIn("refusing numeric PGID signal", outcome["cleanup_error"])
                        self.assertEqual(result["blocked_cells"], ["after"])
                        self.assertEqual(len(result["cells"]), 1)
                        continue
                    self.assertEqual(len(result["cells"]), 2, result.get("errors"))
                    self.assertEqual(outcome["returncode"], returncode)
                    self.assertFalse(outcome["timed_out"])
                    self.assertTrue(outcome["leaked_process_group"])
                    self.assertIn("cleanup_signals", outcome, outcome)
                    self.assertIn("SIGKILL", outcome["cleanup_signals"])
                    self.assertFalse(outcome["success"])
                    self.assertFalse(result["success"])
                    self.assertTrue(result["cells"][1]["success"])
                    pid = int((root / "child.pid").read_text())
                    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                           text=True, capture_output=True, check=False)
                    self.assertTrue(state.returncode != 0 or "Z" in state.stdout,
                                    f"child still executing after cleanup: {state.stdout}")
                finally:
                    # Address only this fixture's private endpoint, rather
                    # than risk targeting a reused PID after leader reaping.
                    endpoint = root / "child.control"
                    if endpoint.exists():
                        try:
                            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
                                control.settimeout(1)
                                control.connect(str(endpoint))
                                control.sendall(b"stop")
                        except (ConnectionRefusedError, FileNotFoundError):
                            pass  # Collector already killed/reaped the worker.


    def test_cleanup_refusal_preserves_original_cell_and_blocks_contaminated_followups(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with mocked_anchored_process(), mock.patch.object(commands.os, "killpg", side_effect=PermissionError("group signal refused")):
                result = self.execute(root, [self.cell("unclean", "print('original exit zero')"),
                                             self.cell("after", "print('must not run')")])
            self.assertEqual(len(result["cells"]), 1, result.get("errors"))
            self.assertEqual(result["cells"][0]["returncode"], 0)
            self.assertFalse(result["cells"][0]["success"])
            self.assertIn("group signal refused", result["cells"][0]["cleanup_error"])
            self.assertEqual(result["blocked_cells"], ["after"])
            self.assertFalse((root / "results/after.log").exists())
            self.assertTrue(result["errors"])
            self.assertFalse(result["success"])

    def test_linux_membership_distinguishes_zombies_and_preserves_start_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, state, start in ((41, "Z", 100), (42, "X", 101)):
                entry = root / str(pid)
                entry.mkdir()
                fields = [state] + ["0"] * 19
                fields[1], fields[2], fields[19] = "1", "40", str(start)
                (entry / "stat").write_text(str(pid) + " (worker with (parentheses)) " + " ".join(fields))
            with mock.patch.object(commands.sys, "platform", "linux"), mock.patch.object(commands.os, "killpg"):
                observed = commands.observe_group(40, root)
            self.assertEqual(observed["status"], "nonexecuting")
            self.assertEqual([(m["pid"], m["pgid"], m["state"], m["start_ticks"]) for m in observed["members"]],
                             [(41, 40, "Z", 100), (42, 40, "X", 101)])
            self.assertEqual(observed["errors"], [])

    def test_zombie_only_group_preserves_exit_zero_and_original_failure_separately(self):
        for returncode in (0, 7):
            with self.subTest(returncode=returncode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def zombie(pgid):
                    return dict(pgid=pgid, status="nonexecuting", backend="proc", errors=[],
                                members=[dict(pid=pgid + 1, ppid=1, pgid=pgid, state="Z", start_ticks=100)])
                with mock.patch.object(commands, "observe_group", side_effect=zombie), mock.patch.object(commands.os, "killpg") as signal_group:
                    result = self.execute(root, [self.cell("zombie", f"raise SystemExit({returncode})"),
                                                 self.cell("after", "print('independent ran')")])
                signal_group.assert_not_called()
                self.assertEqual(len(result["cells"]), 2)
                outcome = result["cells"][0]
                self.assertEqual(outcome["returncode"], returncode)
                self.assertEqual(outcome["success"], returncode == 0)
                self.assertFalse(outcome["leaked_process_group"])
                self.assertTrue(outcome["cleanup_complete"])
                self.assertEqual(outcome["group_observations"][0]["members"][0]["state"], "Z")
                self.assertTrue(result["cells"][1]["success"])

    def test_unreadable_linux_identity_cannot_certify_group_absence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "41").mkdir()
            (root / "41/stat").write_text("truncated identity")
            with mock.patch.object(commands.sys, "platform", "linux"), mock.patch.object(commands.os, "killpg"):
                observed = commands.observe_group(40, root)
            self.assertEqual(observed["status"], "unknown")
            self.assertTrue(observed["errors"])

    def test_macos_state_probe_refusal_remains_unknown_with_actual_errno(self):
        with mock.patch.object(commands.sys, "platform", "darwin"), mock.patch.object(commands.os, "killpg"), mock.patch.object(commands.subprocess, "run", side_effect=PermissionError(errno.EPERM, "ps denied")):
            observed = commands.observe_group(40)
        self.assertEqual(observed["status"], "unknown")
        self.assertEqual(observed["error_errno"], errno.EPERM)
        self.assertEqual(observed["backend"], "ps")

    def test_live_or_unknown_group_surviving_cleanup_blocks_followup(self):
        for status in ("live", "unknown"):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def surviving(pgid):
                    return dict(pgid=pgid, status=status, backend="proc", errors=[],
                                members=[dict(pid=pgid + 1, ppid=1, pgid=pgid, state="S", start_ticks=100)])
                with mocked_anchored_process(), mock.patch.object(commands, "observe_group", side_effect=surviving), mock.patch.object(commands.os, "killpg"):
                    result = self.execute(root, [self.cell("survivor", "print('original exit zero')"),
                                                 self.cell("after", "print('must not run')")])
                self.assertEqual(len(result["cells"]), 1)
                outcome = result["cells"][0]
                self.assertEqual(outcome["returncode"], 0)
                self.assertFalse(outcome["success"])
                self.assertFalse(outcome["cleanup_complete"])
                self.assertIn("SIGTERM", outcome["cleanup_signals"])
                self.assertIn("SIGKILL", outcome["cleanup_signals"])
                self.assertEqual(outcome["group_observations"][-1]["status"], status)
                self.assertEqual(result["blocked_cells"], ["after"])
                self.assertFalse((root / "results/after.log").exists())
                self.assertTrue(result["errors"])

    def test_reaped_leader_and_reused_numeric_group_are_never_signalled(self):
        original_wait = commands.wait_owned_leader
        for returncode in (0, 7):
            with self.subTest(returncode=returncode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                def reused_group(pgid):
                    return dict(pgid=pgid, status="live", backend="proc", errors=[],
                                members=[dict(pid=pgid, ppid=1, pgid=pgid, state="S", start_ticks=999999)])
                def fallback(process, timeout):
                    return original_wait(process, timeout, use_waitid=False)
                with mock.patch.object(commands, "wait_owned_leader", side_effect=fallback), mock.patch.object(commands, "observe_group", side_effect=reused_group), mock.patch.object(commands.os, "killpg") as signal_group:
                    result = self.execute(root, [self.cell("old-leader", f"raise SystemExit({returncode})"),
                                                 self.cell("after", "raise RuntimeError('must not run')")])
                signal_group.assert_not_called()
                self.assertEqual(len(result["cells"]), 1)
                outcome = result["cells"][0]
                self.assertEqual(outcome["returncode"], returncode)
                self.assertFalse(outcome["leader_anchor"]["held"])
                self.assertEqual(outcome["leader_anchor"]["mechanism"], "reaped-fallback")
                self.assertFalse(outcome["leaked_process_group"])
                self.assertTrue(outcome["cleanup_observation_unknown"])
                self.assertFalse(outcome["cleanup_complete"])
                self.assertEqual(outcome["cleanup_signals"], [])
                self.assertIn("refusing numeric PGID signal", outcome["cleanup_error"])
                self.assertEqual(result["blocked_cells"], ["after"])

    def test_waitid_anchor_is_not_reaped_until_after_last_cleanup_observation(self):
        events = []
        state = {"value": "live"}
        class OwnedProcess:
            pid = 40
            returncode = None
            def poll(self):
                raise AssertionError("poll would release the owned leader anchor")
            def wait(self, timeout):
                events.append("reap")
                self.returncode = 7
                return 7
        process = OwnedProcess()
        def observation(pgid):
            events.append("observe:" + state["value"])
            return dict(pgid=pgid, status=state["value"], errors=[], members=[])
        def signal_group(_pgid, number):
            self.assertIsNone(process.returncode)
            events.append(signal.Signals(number).name)
            if number == signal.SIGKILL:
                state["value"] = "nonexecuting"
        exit_info = SimpleNamespace(si_pid=40, si_code=getattr(os, "CLD_EXITED", 1), si_status=7)
        with mocked_waitid_capability(return_value=exit_info) as waitid, mock.patch.object(commands, "observe_group", side_effect=observation), mock.patch.object(commands.os, "killpg", side_effect=signal_group):
            anchor = commands.wait_owned_leader(process, 1)
            self.assertEqual(anchor["mechanism"], "waitid-WNOWAIT")
            self.assertEqual(anchor["returncode"], 7)
            self.assertIsNone(process.returncode)
            cleanup = commands.terminate_group(process, 0, anchor)
            self.assertTrue(cleanup["complete"])
            self.assertIsNone(process.returncode)
            self.assertNotIn("reap", events)
            for call in waitid.call_args_list:
                self.assertTrue(call.args[2] & os.WNOWAIT)
            result = {"cleanup_complete": True}
            commands.reap_owned_leader(process, result)
        self.assertEqual(events[-1], "reap")
        self.assertEqual(events[-2], "observe:nonexecuting")
        self.assertEqual(result["returncode"], 7)
        self.assertTrue(result["leader_reaped_after_cleanup"])

    def test_lost_waitid_child_anchor_refuses_every_destructive_signal(self):
        process = SimpleNamespace(pid=40, returncode=None)
        group = dict(pgid=40, status="live", errors=[], members=[])
        anchor = dict(held=True, mechanism="waitid-WNOWAIT")
        with mock.patch.object(commands, "observe_group", return_value=group), mocked_waitid_capability(side_effect=ChildProcessError(errno.ECHILD, "not our child")), mock.patch.object(commands.os, "killpg") as signal_group:
            result = commands.terminate_group(process, 0, anchor)
        signal_group.assert_not_called()
        self.assertFalse(result["complete"])
        self.assertIn("refusing numeric PGID signal", result["error"])

    def test_timeout_without_wnowait_keeps_unreaped_owned_leader_anchor(self):
        process = SimpleNamespace(pid=40, returncode=None,
                                  wait=mock.Mock(side_effect=subprocess.TimeoutExpired("owned", 0.1)))
        anchor = commands.wait_owned_leader(process, 0.1, use_waitid=False)
        self.assertTrue(anchor["held"])
        self.assertTrue(anchor["timed_out"])
        self.assertEqual(anchor["mechanism"], "unreaped-timeout-fallback")
        self.assertTrue(commands.anchor_still_owned(process, anchor))
        process.wait.assert_called_once()

    def test_interrupted_waitid_still_obeys_completion_deadline(self):
        process = SimpleNamespace(pid=40, returncode=None)
        with mocked_waitid_capability(side_effect=InterruptedError(errno.EINTR, "interrupted")), mock.patch.object(commands.time, "monotonic", side_effect=[0, 2]):
            anchor = commands.wait_owned_leader(process, 1)
        self.assertTrue(anchor["held"])
        self.assertTrue(anchor["timed_out"])
        self.assertEqual(anchor["mechanism"], "waitid-WNOWAIT")

    def test_spawn_failure_is_preserved_and_remaining_commands_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing = dict(id="missing", argv=[str(root / "missing")], timeout_seconds=1)
            result = self.execute(root, [missing, self.cell("after", "print('ran')")])
            self.assertFalse(result["success"])
            self.assertIsNone(result["cells"][0]["returncode"])
            self.assertTrue(result["cells"][1]["success"])


if __name__ == "__main__":
    unittest.main()
