"""Coordinator controls using real native parsing and tiny executable fixtures."""
import copy
import base64
from contextlib import redirect_stdout
import io
import json
import os
import stat
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch, call

from validation import rust_parallel as parallel
from validation import rust_source_bundle as bundle
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
    def test_capacity_serializes_real_counter_shape_without_claiming_test_execution(self):
        from validation import baseline_resources
        read_text = Path.read_text
        def memory(path, *args, **kwargs):
            if str(path) == '/proc/meminfo':
                return 'MemTotal: 32000000 kB\nMemAvailable: 24000000 kB\n'
            return read_text(path, *args, **kwargs)
        with tempfile.TemporaryDirectory() as directory, redirect_stdout(io.StringIO()), \
                patch.object(baseline_resources, 'metadata', return_value={'cell': 'm3-capacity'}), \
                patch.object(baseline_resources, 'discover_cgroup', return_value={}), \
                patch.object(baseline_resources, 'cgroup_sample', return_value=(
                    {'memory.max': 'max'}, ['io.stat'], Path('/sys/fs/cgroup'))), \
                patch.object(Path, 'read_text', memory):
            self.assertEqual(parallel.main(['capacity', '--output', directory]), 0)
            report = json.loads((Path(directory) / 'capacity.json').read_text())
        self.assertEqual(report['cgroup'], {'memory.max': 'max'})
        self.assertEqual(report['missing_counters'], ['io.stat'])
        self.assertEqual(report['counter_root'], '/sys/fs/cgroup')
        self.assertIs(report['qualified'], False)
        self.assertIn('no native test execution', report['limitation'])

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
        # Native fixtures live below the test runner's world-writable /tmp.
        # Only this unit boundary is stubbed; production protection is tested
        # independently below, without this fixture patch.
        protection = patch.object(parallel, 'validate_native_working_directory')
        self.protection = protection.start()
        self.addCleanup(protection.stop)

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
            self.assertIn('not established', report['working_directory_policy']['environment_equivalence'])
            native_cwds = [row['process']['cwd'] for row in report['serial']]
            for row in report['serial']:
                self.assertEqual(row['process']['cwd_policy'], 'explicit-native-private')
            if mode == 'parallel':
                native_cwds.extend(row['process']['cwd'] for shard in report['shards'] for row in shard['binaries'])
                self.assertEqual(report['doctests']['process']['cwd'], str(Path.cwd().resolve()))
                self.assertEqual(report['doctests']['process']['cwd_policy'], 'inherited-controller-cargo')
            else:
                self.assertEqual(report['process']['cwd'], str(Path.cwd().resolve()))
            self.assertEqual(len(native_cwds), len(set(native_cwds)))
            for directory in native_cwds:
                self.assertTrue(Path(directory).is_relative_to(self.root / mode))
                self.assertEqual(stat.S_IMODE(Path(directory).stat().st_mode), 0o700)
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

    def test_shared_stop_preserves_cancellation_and_unsafe_cleanup_across_arms(self):
        stop = threading.Event()
        stop.set()
        stop.unsafe_cleanup = True
        with self.cargo_route():
            report = parallel.execute("parallel", self.inventory, self.metadata,
                                      self.root / "shared-stop", self.fd, stop_event=stop)
        self.assertFalse(report["success"])
        self.assertTrue(report["canceled"])
        self.assertFalse(report["safe_to_release"])
        self.assertTrue(stop.is_set())
        self.assertTrue(stop.unsafe_cleanup)
        self.assertTrue(all(not shard["binaries"] for shard in report["shards"]))

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

    def test_command_passes_cwd_only_when_explicit_and_records_inherited_cargo_boundary(self):
        process = dict(returncode=0, success=True, cleanup_complete=True, timed_out=False)
        with patch.object(parallel, 'run_process', side_effect=lambda *args, **kwargs: dict(process)) as run:
            result, _ = self.actual_command(['cargo', '--version'], self.root / 'command', 'inherited', 2, self.fd)
            self.assertNotIn('cwd', run.call_args.kwargs)
            self.assertEqual(result['cwd'], str(Path.cwd().resolve()))
            self.assertEqual(result['cwd_policy'], 'inherited-controller-cargo')
            cwd = parallel.native_working_directory(self.root / 'command', 'explicit')
            result, _ = self.actual_command(['/bin/true'], self.root / 'command', 'explicit', 2, self.fd, cwd=cwd)
            self.assertEqual(run.call_args.kwargs['cwd'], cwd)
            self.assertEqual(result['cwd'], str(cwd))
            self.assertEqual(result['cwd_policy'], 'explicit-native-private')

    def test_native_working_directory_is_fresh_and_never_created_in_source(self):
        output = self.root / 'fresh-runtime'
        path = parallel.native_working_directory(output, 'binary-00')
        self.assertEqual(self.protection.call_args_list, [call(path, existing_only=True), call(path)])
        retained = path / 'retained'
        retained.write_text('do not delete')
        with self.assertRaises(FileExistsError):
            parallel.native_working_directory(output, 'binary-00')
        self.assertEqual(retained.read_text(), 'do not delete')
        with self.assertRaisesRegex(ValueError, 'outside frozen source'):
            parallel.native_working_directory(Path.cwd(), 'forbidden-runtime')
        with self.assertRaisesRegex(ValueError, 'Invalid native'):
            parallel.native_working_directory(output, '../escape')

    def test_output_ancestor_link_into_source_refuses_without_creating_source_artifacts(self):
        source = self.root / 'frozen-source'
        source.mkdir()
        (source / 'original.rs').write_text('// frozen source')
        alias = self.root / 'source-alias'
        alias.symlink_to(source, target_is_directory=True)
        with patch.object(Path, 'cwd', return_value=source):
            with self.assertRaisesRegex(ValueError, 'outside frozen source'):
                parallel.native_working_directory(alias / 'not-created', 'binary-00')
        self.assertEqual([path.name for path in source.iterdir()], ['original.rs'])
        self.assertFalse((source / 'not-created').exists())
        self.protection.assert_not_called()

    def test_real_shell_runtime_write_keeps_strict_source_identity_and_source_write_refuses(self):
        source = self.root / 'immutable-source'
        source.mkdir()
        content = b'// source bound fixture\n'
        (source / 'fixture.rs').write_bytes(content)
        blob = bundle._object('blob', content)
        tree = bundle._tree({'fixture.rs': ('100644', blob)})
        raw_commit = ('tree ' + tree + '\nauthor Fixture <fixture@example.invalid> 0 +0000\n'
                      'committer Fixture <fixture@example.invalid> 0 +0000\n\nFixture\n').encode()
        manifest = dict(schema=1, commit=bundle._object('commit', raw_commit), tree=tree,
                        raw_commit_base64=base64.b64encode(raw_commit).decode(),
                        entries=[dict(path='fixture.rs', mode='100644', blob=blob)])
        manifest_path = self.root / 'source-bundle.json'
        parallel.write(manifest_path, manifest)
        inventory = copy.deepcopy(self.inventory)
        inventory['source'] = bundle.identity(source, manifest_path)
        output = self.root / 'shell-runtime'
        ready, release = self.root / 'shell-ready', self.root / 'shell-release'
        result, errors = [], []
        with patch.object(parallel, 'SOURCE_BUNDLE', manifest_path), patch.object(Path, 'cwd', return_value=source):
            cwd = parallel.native_working_directory(output, 'binary-00')
            # Positional shell arguments avoid interpreting paths as shell code.
            script = ('mkdir -p .tmp-fixture/data; printf database > .tmp-fixture/data/plurx.db; '
                      'touch "$1"; while [ ! -f "$2" ]; do sleep 0.02; done')
            def run():
                try:
                    result.append(self.actual_command(['/bin/sh', '-c', script, 'fixture', str(ready), str(release)],
                                                      output, 'native', 8, self.fd, cwd=cwd))
                except BaseException as error:
                    errors.append(error)
            thread = threading.Thread(target=run)
            thread.start()
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), 'shell did not create its real runtime fixture')
                self.assertTrue((cwd / '.tmp-fixture/data/plurx.db').is_file())
                parallel.check_inputs(inventory)
                (source / 'unexpected-source-write').write_text('must refuse')
                with self.assertRaisesRegex(bundle.BundleError, 'unexpected source file'):
                    parallel.check_inputs(inventory)
            finally:
                release.touch()
                thread.join(timeout=9)
        self.assertFalse(thread.is_alive(), 'owned shell fixture did not finish')
        self.assertEqual(errors, [])
        self.assertEqual(len(result), 1)
        process = result[0][0]
        self.assertTrue(process['cleanup_complete'], process)
        self.assertTrue(process['success'], process)
        self.assertEqual(process['cwd'], str(cwd))
        self.assertEqual(process['cwd_policy'], 'explicit-native-private')

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


class NativeWorkingDirectoryProtectionCase(unittest.TestCase):
    """Hermetic ancestor metadata; production validator is never stubbed here."""
    def setUp(self):
        self.path = Path('/owned/runtime/binary-00')
        self.records = {path: SimpleNamespace(st_mode=stat.S_IFDIR | (0o700 if index == 0 else 0o755),
                                             st_uid=1000 if index < 2 else 0)
                        for index, path in enumerate((self.path, *self.path.parents))}

    def validate(self):
        with patch.object(os, 'getuid', return_value=1000), \
                patch.object(Path, 'lstat', autospec=True, side_effect=lambda path: self.records[path]):
            parallel.validate_native_working_directory(self.path)

    def test_real_symlink_ancestor_is_refused_before_mkdir_mutates_its_target(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            target = base / 'symlink-target'
            target.mkdir()
            alias = base / 'alias'
            alias.symlink_to(target, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'real directory'):
                parallel.native_working_directory(alias / 'not-created', 'binary-00')
            self.assertEqual(list(target.iterdir()), [])

    def test_preflight_allows_missing_leaf_but_preserves_unsafe_ancestor_refusal(self):
        records = dict(self.records)
        del records[self.path]
        def lookup(path):
            if path not in records:
                raise FileNotFoundError(str(path))
            return records[path]
        with patch.object(os, 'getuid', return_value=1000), \
                patch.object(Path, 'lstat', autospec=True, side_effect=lookup):
            parallel.validate_native_working_directory(self.path, existing_only=True)
            records[self.path.parent] = SimpleNamespace(st_mode=stat.S_IFDIR | 0o777, st_uid=1000)
            with self.assertRaisesRegex(ValueError, 'permissions are unsafe'):
                parallel.validate_native_working_directory(self.path, existing_only=True)

    def test_private_leaf_and_real_root_or_owner_ancestors_are_accepted(self):
        self.validate()

    def test_shared_write_ancestors_and_nonprivate_leaf_are_refused(self):
        for path, mode in ((self.path, 0o755), (self.path.parent, 0o777),
                           (self.path.parent.parent, 0o775), (Path('/'), 0o777)):
            with self.subTest(path=path):
                original = self.records[path].st_mode
                self.records[path].st_mode = stat.S_IFDIR | mode
                with self.assertRaisesRegex(ValueError, 'permissions are unsafe'):
                    self.validate()
                self.records[path].st_mode = original

    def test_symlink_or_foreign_owner_anywhere_in_chain_is_refused(self):
        for path in self.records:
            with self.subTest(path=path):
                original = self.records[path]
                self.records[path] = SimpleNamespace(st_mode=stat.S_IFLNK | 0o777, st_uid=1000)
                with self.assertRaisesRegex(ValueError, 'real directory'):
                    self.validate()
                self.records[path] = SimpleNamespace(st_mode=original.st_mode, st_uid=2000)
                with self.assertRaisesRegex(ValueError, 'ownership is unsafe'):
                    self.validate()
                self.records[path] = original


if __name__ == "__main__":
    unittest.main()
