"""Coordinator controls using real native parsing and tiny executable fixtures."""
import copy
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from validation import rust_parallel as parallel
from validation.rust_parallel_manifest import build_plan

SOURCE = dict(commit="a" * 40, tree="b" * 40, tracked_clean=True)


def footer(passed=0, failed=0, ignored=0, filtered=0):
    return (f"test result: {'FAILED' if failed else 'ok'}. {passed} passed; {failed} failed; "
            f"{ignored} ignored; 0 measured; {filtered} filtered out; finished in 0.01s")


def docs(*, zero=False, failed=False):
    return "\n".join(["Doc-tests plurx_core", f"running {0 if zero else 1} tests",
                      *([] if zero else [f"test src/lib.rs - contract (line 1) ... {'FAILED' if failed else 'ok'}"]),
                      footer(0 if zero or failed else 1, int(failed)),
                      "Doc-tests empty_target", "running 0 tests", footer()])


class RustParallelCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        lease = self.root / "lease"
        self.fd = os.open(lease, os.O_CREAT | os.O_RDWR, 0o600)
        self.addCleanup(os.close, self.fd)
        self.binaries = [self.binary("plurxd", [dict(name="daemon", ignored=False),
                                              *[dict(name=name, ignored=True) for name in parallel.SERIAL]]),
                         self.binary("plurx_core", [dict(name="shared_name", ignored=False)]),
                         self.binary("store_contract", [dict(name="shared_name", ignored=False)]),
                         self.binary("empty_target", [])]
        self.inventory = dict(discovery_complete=True, errors=[], source=copy.deepcopy(SOURCE), binaries=self.binaries)
        self.metadata = dict(version=1, workspace_members=["core", "empty", "excluded"], packages=[
            dict(id="core", name="plurx-core", targets=[dict(name="plurx_core", kind=["lib"], doctest=True)]),
            dict(id="empty", name="empty", targets=[dict(name="empty_target", kind=["lib"], doctest=True)]),
            dict(id="excluded", name="plurx-cluster-check", targets=[dict(name="excluded", kind=["lib"], doctest=True)])])
        self.identity = patch.object(parallel, "git_identity", return_value=copy.deepcopy(SOURCE))
        self.identity.start()
        self.addCleanup(self.identity.stop)
        self.actual_command = parallel.command

    def binary(self, name, cases, mode="normal"):
        row = dict(executable=str(self.root / (name + "-abcdef")), sha256="c" * 64,
                   package_id="package-" + name, features=["hiqlite-store"],
                   target=dict(name=name, kind=["bin" if name == "plurxd" else "lib"]),
                   profile=dict(test=True, opt_level="0", debuginfo=0),
                   cases=sorted(cases, key=lambda case: case["name"]), listing_complete=True)
        self.native_script(row, mode)
        return row

    def native_script(self, row, mode):
        path = Path(row["executable"])
        path.write_text(f'''#!{sys.executable}
import sys
CASES = {row['cases']!r}
MODE = {mode!r}
args = sys.argv[1:]
selected = [case for case in CASES if '--exact' not in args or case['name'] in args]
if '--ignored' in args:
    selected = [case for case in selected if case['ignored']]
if MODE == 'zero' or MODE == 'zero_serial' and '--ignored' in args:
    selected = []
print('running %d tests' % len(selected))
passed = failed = ignored = 0
for case in selected:
    status = 'ignored' if case['ignored'] and '--ignored' not in args else 'ok'
    if status == 'ok' and (MODE == 'fail' or MODE == 'fail_serial' and '--ignored' in args):
        status = 'FAILED'
    name = 'wrong_binary_case' if MODE == 'wrong_case' else case['name']
    print('test %s ... %s' % (name, status))
    passed += status == 'ok'; failed += status == 'FAILED'; ignored += status == 'ignored'
print('test result: %s. %d passed; %d failed; %d ignored; 0 measured; %d filtered out; finished in 0.01s' % ('FAILED' if failed else 'ok', passed, failed, ignored, len(CASES)-len(selected)))
raise SystemExit(101 if failed else 0)
''')
        path.chmod(0o700)
        row["sha256"] = parallel.sha256(path)

    def cargo_route(self, *, doc_output=None):
        cargo = self.root / "fake-cargo"
        cargo.write_text(f'''#!{sys.executable}
import subprocess, sys
BINARIES = {[(b['executable'], b['target']['name']) for b in self.binaries]!r}
DOCS = {docs() if doc_output is None else doc_output!r}
status = 0
if '--doc' not in sys.argv:
    for executable, name in BINARIES:
        print('Running unittests src/lib.rs (%s)' % executable, flush=True)
        result = subprocess.run([executable], text=True, capture_output=True)
        print(result.stdout, end='', flush=True)
        print(result.stderr, end='', flush=True)
        status = status or result.returncode
print(DOCS, flush=True)
raise SystemExit(status)
''')
        cargo.chmod(0o700)
        def route(argv, *args, **kwargs):
            return self.actual_command([str(cargo), *argv[1:]] if argv[0] == "cargo" else argv, *args, **kwargs)
        return patch.object(parallel, "command", side_effect=route)

    def test_exact_legacy_and_parallel_preserve_zero_case_binary_ignored_cases_and_serial_bodies(self):
        for mode in ("legacy", "parallel"):
            with self.subTest(mode=mode), self.cargo_route():
                report = parallel.execute(mode, self.inventory, self.metadata, self.root / mode, self.fd)
            self.assertTrue(report["success"], report["errors"])
            self.assertEqual(len(report["serial"]), 2)
            for serial in report["serial"]:
                suite = serial["evidence"]["observed_suites"][0]
                self.assertEqual(suite["result"]["passed"], 1)
                self.assertEqual(suite["result"]["ignored"], 0)
            if mode == "parallel":
                self.assertEqual(report["native"]["binary_count"], 4)
                self.assertEqual(report["native"]["counts"], dict(total=5, passed=3, failed=0, ignored=2))
                self.assertEqual(sum(s["result"]["passed"] for s in report["doctests"]["suites"]), 1)
            else:
                suites = report["evidence"]["observed_suites"]
                empty = next(s for s in suites if s["binary"] == "empty_target-abcdef")
                self.assertEqual(empty["declared_tests"], 0)
                self.assertEqual(sum(s["result"]["ignored"] for s in suites), 2)

    def test_actual_native_zero_match_or_wrong_identity_cannot_pass(self):
        for mode in ("zero", "wrong_case"):
            self.native_script(self.binaries[1], mode)
            plan = build_plan(self.inventory)
            receipt = parallel.run_shard(plan["shards"][1], plan, self.inventory, self.root / mode, self.fd, threading.Event())
            self.assertFalse(receipt["binaries"][0]["success"])
            self.assertTrue(receipt["binaries"][0]["cleanup_complete"])
            self.assertTrue(receipt["binaries"][0]["parser_failure"])

    def test_all_36_binary_receipts_include_every_prelisted_empty_target(self):
        self.binaries.extend(self.binary("empty_" + str(index), []) for index in range(32))
        plan = build_plan(self.inventory)
        receipts = [parallel.run_shard(shard, plan, self.inventory, self.root / "many-empty", self.fd,
                                      threading.Event()) for shard in plan["shards"]]
        result = parallel.validate_receipts(plan, receipts)
        self.assertTrue(result["success"])
        self.assertEqual(result["binary_count"], 36)
        zero_suites = [record["evidence"]["observed_suites"][0] for receipt in receipts
                       for record in receipt["binaries"] if not record["outcomes"]]
        self.assertEqual(len(zero_suites), 33)
        self.assertTrue(all(suite["declared_tests"] == 0 and suite["status"] == "passed"
                            for suite in zero_suites))

    def test_native_assertion_failure_stays_red_but_safe_lane_collects_remaining_binaries(self):
        self.native_script(self.binaries[1], "fail")
        plan = build_plan(self.inventory, workers=1)
        stop = threading.Event()
        receipt = parallel.run_shard(plan["shards"][0], plan, self.inventory, self.root / "failed", self.fd, stop)
        self.assertEqual(len(receipt["binaries"]), 4)
        failed = next(b for b in receipt["binaries"] if b["target"]["name"] == "plurx_core")
        self.assertEqual(failed["returncode"], 101)
        self.assertEqual(failed["outcomes"], [dict(name="shared_name", status="FAILED")])
        self.assertFalse(failed["success"])
        self.assertFalse(stop.is_set())

    def test_doctest_target_union_accepts_legitimate_zero_target_but_requires_actual_body(self):
        targets = parallel.doctest_targets(self.metadata)
        suites = parallel.parse_doctests(docs(), targets)
        self.assertEqual([suite["result"]["passed"] for suite in suites], [1, 0])
        with self.assertRaises(ValueError):
            parallel.parse_doctests(docs(zero=True), targets)

    def test_missing_duplicate_failed_and_unknown_doctests_are_rejected(self):
        targets = parallel.doctest_targets(self.metadata)
        for text in [docs().split("Doc-tests empty_target")[0], docs() + "\n" + docs(),
                     docs(failed=True), docs().replace("Doc-tests empty_target", "Doc-tests unrelated")]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                parallel.parse_doctests(text, targets)

    def test_unsupported_execution_mode_remains_failed_and_persists_error(self):
        report = parallel.execute("unsupported", self.inventory, self.metadata, self.root / "unsupported", self.fd)
        self.assertFalse(report["success"])
        self.assertTrue(any("unknown execution mode" in error for error in report["errors"]))
        self.assertFalse(parallel.read(self.root / "unsupported/results.json")["success"])

    def test_source_or_executable_drift_is_rejected_before_native_launch(self):
        with patch.object(parallel, "git_identity", return_value=dict(SOURCE, tree="d" * 40)):
            with self.assertRaisesRegex(ValueError, "source changed"):
                parallel.check_inputs(self.inventory)
        Path(self.binaries[0]["executable"]).write_text("changed executable bytes")
        with self.assertRaisesRegex(ValueError, "executable changed"):
            parallel.check_inputs(self.inventory)

    def test_after_execution_binary_drift_is_red_even_when_native_parser_and_cleanup_pass(self):
        plan = build_plan(self.inventory)
        def mutate(argv, *args, **kwargs):
            result = self.actual_command(argv, *args, **kwargs)
            with Path(argv[0]).open("a") as stream:
                stream.write("\n# changed after execution\n")
            return result
        with patch.object(parallel, "command", side_effect=mutate):
            receipt = parallel.run_shard(plan["shards"][1], plan, self.inventory, self.root / "drift", self.fd, threading.Event())
        observed = receipt["binaries"][0]
        self.assertTrue(observed["evidence"]["native_passed"])
        self.assertTrue(observed["cleanup_complete"])
        self.assertFalse(observed["success"])
        self.assertIn("executable changed", observed["error"])

    def test_missing_or_failed_serial_execution_cannot_be_replaced_by_ignored_ordinary_verdict(self):
        for mode in ("zero_serial", "fail_serial"):
            self.native_script(self.binaries[0], mode)
            with self.subTest(mode=mode), self.cargo_route():
                report = parallel.execute("parallel", self.inventory, self.metadata, self.root / mode, self.fd)
            self.assertFalse(report["success"])
            self.assertTrue(report["native"]["success"])
            self.assertEqual(len(report["serial"]), 1)
            self.assertFalse(report["serial"][0]["evidence"]["native_passed"])

    def test_serial_case_must_remain_prelisted_ignored_before_explicit_body(self):
        self.binaries[0]["cases"] = [case for case in self.binaries[0]["cases"] if case["name"] != parallel.SERIAL[0]]
        self.native_script(self.binaries[0], "normal")
        with self.cargo_route():
            report = parallel.execute("parallel", self.inventory, self.metadata, self.root / "missing-serial", self.fd)
        self.assertFalse(report["success"])
        self.assertTrue(report["native"]["success"])
        self.assertEqual(report["serial"], [])
        self.assertTrue(any("serial regression missing/active" in error for error in report["errors"]))

    def test_preexisting_global_stop_blocks_every_native_launch(self):
        plan = build_plan(self.inventory)
        stop = threading.Event(); stop.set()
        with patch.object(parallel, "command") as command:
            receipt = parallel.run_shard(plan["shards"][0], plan, self.inventory, self.root / "stopped", self.fd, stop)
        command.assert_not_called()
        self.assertEqual(receipt["binaries"], [])
        self.assertIn("canceled", receipt["blocked"])

    def test_unsafe_cleanup_blocks_remaining_lane_and_sets_global_stop(self):
        plan = build_plan(self.inventory, workers=1)
        stop = threading.Event()
        def unsafe(argv, *args, **kwargs):
            process, text = self.actual_command(argv, *args, **kwargs)
            process.update(success=False, cleanup_complete=False)
            return process, text
        with patch.object(parallel, "command", side_effect=unsafe) as command:
            receipt = parallel.run_shard(plan["shards"][0], plan, self.inventory, self.root / "unsafe", self.fd, stop)
        self.assertEqual(command.call_count, 1)
        self.assertEqual(len(receipt["binaries"]), 1)
        self.assertFalse(receipt["binaries"][0]["cleanup_complete"])
        self.assertTrue(stop.is_set())
        with patch.object(parallel, "command") as following:
            parallel.run_shard(plan["shards"][0], plan, self.inventory, self.root / "following", self.fd, stop)
        following.assert_not_called()

    @unittest.skipUnless(sys.platform == "linux", "real cancellation ownership control uses Linux /proc")
    def test_global_cancellation_drains_actual_running_process_before_return(self):
        marker = self.root / "started"
        stop = threading.Event()
        def cancel_when_started():
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            if marker.exists():
                stop.set()
        thread = threading.Thread(target=cancel_when_started)
        thread.start()
        try:
            result, _ = self.actual_command([sys.executable, "-c",
                "from pathlib import Path; import time; Path(" + repr(str(marker)) + ").write_text('ready'); time.sleep(30)"],
                self.root / "cancel", "native", 8, self.fd, cancel_event=stop)
        finally:
            stop.set(); thread.join(timeout=6)
        self.assertTrue(marker.exists(), "native process did not reach the cancellation boundary")
        self.assertTrue(result["cleanup_complete"], result)
        self.assertFalse(result["success"])
        self.assertFalse(result["timed_out"])
        self.assertLess(result["seconds"], 8)
        self.assertTrue(result["leader_reaped_after_cleanup"])


if __name__ == "__main__":
    unittest.main()
