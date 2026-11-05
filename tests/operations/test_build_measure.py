from __future__ import annotations

import json
import importlib.machinery
import importlib.util
import itertools
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/build-measure"
SPEC = importlib.util.spec_from_loader("build_measure", importlib.machinery.SourceFileLoader("build_measure", str(SCRIPT)))
MEASURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MEASURE)


@unittest.skipIf(sys.platform == "win32", "measurement hosts are Linux/macOS")
class BuildMeasureTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="build-measure-")
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name).resolve()
        self.source = self.base / "source"
        self.source.mkdir()
        self.web = self.source / "crates/plurxd/src/http/web.rs"
        self.web.parent.mkdir(parents=True)
        self.original = b'fn content(path: &str) -> &str {\n    if path.ends_with(".css") {\n        "text/css; charset=utf-8"\n    } else { "js" }\n}\n'
        self.web.write_bytes(self.original)
        self.css = self.source / "crates/plurxd/src/web/app.css"
        self.css.parent.mkdir(parents=True)
        self.css.write_text("body { color: white; }\n")
        self.core = self.source / "crates/plurx-core/src/lib.rs"
        self.core.parent.mkdir(parents=True)
        self.core.write_text("pub const ORIGINAL: u64 = 1;\n")
        self.fake = self.base / "fake_cargo.py"
        self.fake.write_text('''import json,os,pathlib,signal,sys,time
root=pathlib.Path.cwd()
pending=root/'.build-measure-pending.json'
content=(root/'crates/plurxd/src/http/web.rs').read_text()
mode=sys.argv[1]
if mode=='orphan-exit' and pending.exists():
 import subprocess
 observer="import pathlib,time;time.sleep(.5);p=pathlib.Path("+repr(sys.argv[2]+'.seen')+");p.write_text(pathlib.Path('crates/plurxd/src/http/web.rs').read_text())"
 subprocess.Popen([sys.executable,'-c',observer])
 raise SystemExit(0)
if mode=='wait' and pending.exists():
 def stopped(_signum,_frame):
  pathlib.Path(sys.argv[2]+'.seen').write_text((root/'crates/plurxd/src/http/web.rs').read_text())
  raise SystemExit(0)
 signal.signal(signal.SIGTERM,stopped)
 pathlib.Path(sys.argv[2]).write_text(str(os.getpid()))
 time.sleep(120)
if mode=='fail' and 'build-measure-2' in content:
 print(json.dumps({'reason':'build-finished','success':False}))
 raise SystemExit(7)
counter=pathlib.Path(sys.argv[2]+'.count')
count=int(counter.read_text())+1 if counter.exists() else 1
counter.write_text(str(count))
inputs={name:(root/path).read_text() for name,path in {
 'leaf':'crates/plurxd/src/http/web.rs',
 'asset':'crates/plurxd/src/web/app.css',
 'api':'crates/plurx-core/src/lib.rs'}.items()}
tracked=pathlib.Path(sys.argv[2]+'.inputs.json')
previous=json.loads(tracked.read_text()) if tracked.exists() else None
restoration=not pending.exists() and previous is not None and previous!=inputs
if mode!='missing-noop' or count==1:
 for package,kind in [('fixture','bin'),('core-fixture','lib')]:
  relevant=['api'] if package=='core-fixture' else ['leaf','asset','api']
  if mode in ('asset-none','dirty-asset-restoration') and package=='fixture': relevant=['leaf','api']
  fresh=previous is not None and all(previous[name]==inputs[name] for name in relevant)
  if pending.exists() and (mode in ('stale-probe','unrelated-probe') or (mode=='stale-dependent' and package=='fixture')): fresh=True
  if mode=='dirty-noop' and not pending.exists(): fresh=False
  if restoration and (mode=='stale-restoration' or (mode=='stale-api-restoration' and package=='core-fixture')): fresh=True
  if restoration and mode=='dirty-asset-restoration': fresh=False
  print(json.dumps({'reason':'compiler-artifact','package_id':package,'target':{'name':package,'kind':[kind]},'fresh':fresh}))
tracked.write_text(json.dumps(inputs))
if mode=='unrelated-probe' and pending.exists():
 print(json.dumps({'reason':'compiler-artifact','package_id':'other','target':{'name':'other','kind':['lib']},'fresh':False}))
if mode=='tamper' and 'build-measure-1' in content:
 (root/'crates/plurxd/src/http/web.rs').write_text('unrelated edit during sample one')
print(json.dumps({'reason':'build-finished','success':True}))
''')
        self.recipe = self.base / "recipe.json"
        self.write_recipe("normal")
        self.arguments = [sys.executable, str(SCRIPT), "--source", str(self.source),
                          "--target", str(self.base / "target"),
                          "--recipe", str(self.recipe), "--lease", str(self.base / "slot.lock"),
                          "--revision", "a" * 40, "--tree", "b" * 40, "--label", "fixture"]
        initialized = self.run_tool("--initialize")
        self.assertEqual(initialized.returncode, 0, initialized.stderr)

    def write_recipe(self, mode):
        daemon = {"package_id": "fixture", "target_name": "fixture", "target_kind": "bin"}
        core = {"package_id": "core-fixture", "target_name": "core-fixture", "target_kind": "lib"}
        self.recipe.write_text(json.dumps({
            "command": [sys.executable, str(self.fake), mode, str(self.base / "ready")],
            "capabilities": {"fixture": True}, "cache_state": {
                "target": "fixture", "compiler_result_cache": "none"},
            "environment": {"CARGO_BUILD_JOBS": "1"},
            "expected_artifacts": {"warmup": [daemon], "asset": [daemon],
                                   "leaf": [daemon], "api": [core, daemon]}}))

    def run_tool(self, *extra, output="result"):
        return subprocess.run([*self.arguments, "--output", str(self.base / output), *extra],
                              capture_output=True, text=True, timeout=30)

    def test_used_function_and_asset_probes_restore_bytes_and_keep_all_samples(self):
        css = self.css.read_bytes()
        result = self.run_tool("--cases", "noop,asset,leaf,api")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertTrue(report["complete"])
        for case in ("noop", "asset", "leaf", "api"):
            self.assertEqual(len([r for r in report["samples"] if r["case"] == case]), 3)
        self.assertEqual(report["inventory_sha256"], report["restored_inventory_sha256"])
        self.assertEqual(self.web.read_bytes(), self.original)
        self.assertEqual(self.css.read_bytes(), css)
        self.assertFalse((self.source / ".build-measure-pending.json").exists())

    def test_failed_probe_is_retained_and_original_source_is_restored(self):
        self.write_recipe("fail")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual(report["samples"][-1]["exit_code"], 7)
        self.assertEqual(report["samples"][-1]["sample"], 2)
        self.assertNotIn("summary", report)
        self.assertEqual(self.web.read_bytes(), self.original)
        self.assertIn("unqualified", (self.base / "result/probe-journal.json").read_text())

    def test_claimed_noop_with_compilation_fails(self):
        self.write_recipe("dirty-noop")
        result = self.run_tool("--cases", "noop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no-op started compilation", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertNotIn("summary", report)

    def test_changed_baseline_refuses_without_overwriting_it(self):
        self.web.write_bytes(b"another owner's edit")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.web.read_bytes(), b"another owner's edit")
        self.assertFalse((self.base / "result").exists())

    def test_changed_source_without_compilation_cannot_earn_timing_credit(self):
        self.write_recipe("stale-probe")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not compile", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertNotIn("summary", report)
        self.assertEqual(self.web.read_bytes(), self.original)

    def test_unrelated_rebuild_cannot_qualify_changed_source(self):
        self.write_recipe("unrelated-probe")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not compile", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual(report["samples"][-1]["rebuilt"][0]["package_id"], "other")
        self.assertEqual(self.web.read_bytes(), self.original)

    def test_noop_must_observe_selected_artifact(self):
        self.write_recipe("missing-noop")
        result = self.run_tool("--cases", "noop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("was not observed", result.stderr)
        self.assertFalse(json.loads((self.base / "result/results.json").read_text())["complete"])

    def test_stale_restoration_cannot_qualify_restored_artifacts(self):
        self.write_recipe("stale-restoration")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not compile", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual(report["samples"][-1]["case"], "restore-leaf")
        self.assertEqual(self.web.read_bytes(), self.original)

    def test_api_restoration_requires_owner_as_well_as_dependent_rebuild(self):
        self.write_recipe("stale-api-restoration")
        result = self.run_tool("--cases", "api")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("core-fixture", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual(report["samples"][-1]["case"], "restore-api")

    def test_development_asset_restoration_also_requires_no_rust_compilation(self):
        self.write_recipe("asset-none")
        recipe = json.loads(self.recipe.read_text())
        recipe["rust_activity"] = {"asset": "none"}
        self.recipe.write_text(json.dumps(recipe))
        result = self.run_tool("--cases", "asset")
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertTrue(report["complete"])
        self.assertEqual(report["samples"][-1]["rebuilt"], [])
        recipe["command"][2] = "dirty-asset-restoration"
        self.recipe.write_text(json.dumps(recipe))
        result = self.run_tool("--cases", "asset", output="dirty-restoration")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("restoration unexpectedly started compilation", result.stderr)

    def test_metadata_records_and_probes_inherited_explicit_compiler_and_target(self):
        environment = {"RUSTC": "/toolchain/custom-rustc", "CARGO_BUILD_TARGET": "aarch64-unknown-linux-gnu"}
        with mock.patch.object(MEASURE, "version_output", return_value="stub version") as probe:
            host = MEASURE.machine_context(["cargo", "+1.97.1", "build"], self.source,
                                          self.base / "target", environment, {})
        self.assertEqual(host["rustc_command"], [environment["RUSTC"], "-Vv"])
        self.assertEqual(host["inherited_compiler_environment"], environment)
        rustc_probe = probe.call_args_list[1]
        self.assertEqual(rustc_probe.args[0], [environment["RUSTC"], "-Vv"])
        self.assertEqual(rustc_probe.args[2]["RUSTUP_TOOLCHAIN"], "1.97.1")
        with mock.patch.dict(os.environ, environment):
            result = self.run_tool("--cases", "noop")
        self.assertEqual(result.returncode, 0, result.stderr)
        state = json.loads((self.base / "result/results.json").read_text())["compiler_state"]
        self.assertEqual({key: state[key] for key in environment}, environment)

    def test_api_probe_requires_dependent_rebuild(self):
        self.write_recipe("stale-dependent")
        result = self.run_tool("--cases", "api")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not compile", result.stderr)
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual([row["package_id"] for row in report["samples"][-1]["rebuilt"]], ["core-fixture"])
        self.assertEqual(self.core.read_text(), "pub const ORIGINAL: u64 = 1;\n")

    def test_artifact_target_kind_is_part_of_identity(self):
        artifact = {"package_id": "fixture", "target": {"name": "fixture", "kind": ["example"]}, "fresh": False}
        expected = [{"package_id": "fixture", "target_name": "fixture", "target_kind": "bin"}]
        with self.assertRaisesRegex(ValueError, "did not compile"):
            MEASURE.require_artifacts({"artifacts": [artifact], "rebuilt": [artifact]}, expected, rebuilt=True)

    def test_next_sample_preserves_unrelated_probe_edit(self):
        self.write_recipe("tamper")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to overwrite", result.stderr)
        self.assertEqual(self.web.read_text(), "unrelated edit during sample one")
        report = json.loads((self.base / "result/results.json").read_text())
        self.assertFalse(report["complete"])
        self.assertEqual(len([row for row in report["samples"] if row["case"] == "leaf"]), 1)
        self.assertTrue((self.source / MEASURE.PENDING).exists())
        recovery = self.run_tool("--recover", "--cases", "leaf", output="recovery")
        self.assertNotEqual(recovery.returncode, 0)
        self.assertEqual(self.web.read_text(), "unrelated edit during sample one")

    def lifecycle_fault(self, fault):
        self.write_recipe("wait")
        args = MEASURE.argparse.Namespace(
            source=self.source, target=self.base / "target", output=self.base / "result",
            recipe=self.recipe, lease=self.base / "slot.lock", revision="a" * 40,
            tree="b" * 40, label="fixture", cases="leaf", samples=3,
            timeout=20, initialize=False, recover=False)
        real_popen, real_json = MEASURE.subprocess.Popen, MEASURE.atomic_json
        launched = []
        injected = False

        def launch(*arguments, **kwargs):
            child = real_popen(*arguments, **kwargs)
            if (self.source / MEASURE.PENDING).exists():
                launched.append(child)
                deadline = time.monotonic() + 5
                while not (self.base / "ready").exists():
                    if child.poll() is not None or time.monotonic() > deadline:
                        raise AssertionError("probe child did not start")
                    time.sleep(0.01)
                if fault == "signal":
                    os.kill(os.getpid(), signal.SIGTERM)
            return child

        def publish(path, value):
            nonlocal injected
            if fault == "journal" and path == self.source / MEASURE.PENDING and value.get("compiler_state") == "running" and not injected:
                injected = True
                raise OSError("injected child-registration failure")
            return real_json(path, value)

        try:
            with (mock.patch.object(MEASURE.subprocess, "Popen", side_effect=launch),
                  mock.patch.object(MEASURE, "atomic_json", side_effect=publish),
                  mock.patch.object(MEASURE, "machine_context", return_value={} )):
                with self.assertRaises(KeyboardInterrupt if fault == "signal" else OSError):
                    MEASURE.measure(args)
            self.assertEqual(len(launched), 1)
            self.assertIsNotNone(launched[0].poll())
            self.assertIn("build-measure-1", (self.base / "ready.seen").read_text())
            self.assertEqual(self.web.read_bytes(), self.original)
            self.assertFalse((self.source / MEASURE.PENDING).exists())
            self.assertFalse(json.loads((self.base / "result/results.json").read_text())["complete"])
        finally:
            for child in launched:
                if child.poll() is None:
                    MEASURE.stop_group(child)

    def test_signal_immediately_after_spawn_drains_before_restoration(self):
        self.lifecycle_fault("signal")

    def test_failed_child_registration_drains_before_restoration(self):
        self.lifecycle_fault("journal")

    def reaped_group_fixture(self, child, waitpid):
        signals = []
        def signal_group(_pid, signum):
            if signum:
                signals.append(signum)
            # The numeric PGID now names a live, unrelated group.
        with (mock.patch.object(MEASURE.os, "waitpid", side_effect=waitpid),
              mock.patch.object(MEASURE.os, "killpg", side_effect=signal_group),
              mock.patch.object(MEASURE.time, "monotonic", side_effect=itertools.count()),
              mock.patch.object(MEASURE.time, "sleep")):
            with self.assertRaises(MEASURE.UndrainedCompiler):
                MEASURE.stop_group(child)
        return signals

    def test_reaped_or_nonchild_group_never_receives_a_numeric_signal(self):
        for returncode in (0, None):
            with self.subTest(returncode=returncode):
                child = mock.Mock(pid=98765, returncode=returncode)
                def no_child(*_args):
                    raise ChildProcessError("leader already reaped")
                self.assertEqual(self.reaped_group_fixture(child, no_child), [])

    def test_leader_reaped_after_term_never_receives_a_later_kill(self):
        child = mock.Mock(pid=98765, returncode=None)
        child.wait.side_effect = lambda **_kwargs: setattr(child, "returncode", -signal.SIGTERM)
        self.assertEqual(self.reaped_group_fixture(child, lambda *_args: (0, 0)),
                         [signal.SIGTERM])

    def test_exited_unreaped_leader_is_collected_without_signalling(self):
        child = mock.Mock(pid=98765, returncode=None)
        signals = self.reaped_group_fixture(child, lambda *_args: (child.pid, 7 << 8))
        self.assertEqual(signals, [])
        self.assertEqual(child.returncode, 7)

    def test_unsafe_reaping_context_refuses_before_compiler_launch(self):
        for unsafe in ("signal", "thread"):
            with self.subTest(unsafe=unsafe), mock.patch.object(MEASURE.subprocess, "Popen") as spawn:
                patch = (mock.patch.object(MEASURE.signal, "getsignal", return_value=signal.SIG_IGN)
                         if unsafe == "signal" else
                         mock.patch.object(MEASURE.threading, "active_count", return_value=2))
                with patch, self.assertRaises(MEASURE.UndrainedCompiler):
                    MEASURE.run_command([sys.executable, "-V"], dict(os.environ), self.source,
                                        self.base / "unsafe.log", (), 1)
                spawn.assert_not_called()

    def test_real_term_ignoring_child_is_killed_while_still_owned(self):
        ready = self.base / "ignoring-term"
        code = ("import pathlib,signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                f"pathlib.Path({str(ready)!r}).write_text('ready');time.sleep(60)")
        child = subprocess.Popen([sys.executable, "-c", code], start_new_session=True)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists():
                if child.poll() is not None or time.monotonic() >= deadline:
                    self.fail("TERM-ignoring child did not start")
                time.sleep(.01)
            MEASURE.stop_group(child, grace=.05)
            self.assertEqual(child.returncode, -signal.SIGKILL)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)

    def test_exited_leader_with_descendants_retains_the_probe(self):
        self.write_recipe("orphan-exit")
        result = self.run_tool("--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("command exited before its compiler descendants", result.stderr)
        self.assertTrue((self.source / MEASURE.PENDING).exists())
        self.assertIn("build-measure-1", self.web.read_text())
        self.assertIn("build-measure-1", (self.base / "ready.seen").read_text())

    def test_uncertain_launch_refuses_automatic_recovery(self):
        (self.source / MEASURE.PENDING).write_text(json.dumps({"compiler_state": "launching"}))
        result = self.run_tool("--recover", "--cases", "leaf")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ownership is uncertain", result.stderr)
        self.assertEqual(self.web.read_bytes(), self.original)

    def test_checkout_ancestor_refuses_before_writes(self):
        (self.base / ".git").mkdir()
        result = self.run_tool("--cases", "noop")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("never a Git checkout", result.stderr)
        self.assertFalse((self.base / "result").exists())

    def test_nested_target_cannot_exclude_source_inputs_from_inventory(self):
        arguments = list(self.arguments)
        arguments[arguments.index("--target") + 1] = str(self.source / "crates")
        result = subprocess.run([*arguments, "--output", str(self.base / "result"),
                                 "--cases", "leaf"], capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("disjoint", result.stderr)
        self.assertEqual(self.web.read_bytes(), self.original)
        self.assertFalse((self.base / "result").exists())

    def test_competing_slot_owner_refuses_before_starting_build(self):
        import fcntl
        with (self.base / "slot.lock").open("a") as lease:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_tool("--cases", "noop")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "result").exists())

    def test_termination_drains_child_before_source_restoration(self):
        self.write_recipe("wait")
        child = subprocess.Popen([*self.arguments, "--output", str(self.base / "result"),
                                  "--cases", "leaf"], stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        deadline = time.monotonic() + 10
        try:
            while not (self.base / "ready").exists():
                if child.poll() is not None or time.monotonic() > deadline:
                    self.fail("fixture did not enter actual probe child")
                time.sleep(0.01)
            compiler_pid = int((self.base / "ready").read_text())
            child.send_signal(signal.SIGTERM)
            stdout, stderr = child.communicate(timeout=20)
            self.assertNotEqual(child.returncode, 0, stdout + stderr)
            self.assertEqual(self.web.read_bytes(), self.original)
            self.assertFalse((self.source / ".build-measure-pending.json").exists())
            with self.assertRaises(ProcessLookupError):
                os.kill(compiler_pid, 0)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
