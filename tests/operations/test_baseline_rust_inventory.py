"""No real Rust compilation/test execution: exercise artifact and listing boundaries."""
from __future__ import annotations

import json
import sys
import shutil
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from validation import baseline_rust_inventory as inventory


class BaselineRustInventoryCase(unittest.TestCase):
    def artifacts(self, root, *, ignored="ignored_case", list_exit=0, invalid=False):
        executable = root / "fixture-binary"
        executable.write_text("#!/bin/sh\n"
            'case "$*" in\n'
            '  "--list --format=terse") printf "ordinary: test\\nignored_case: test\\n";;\n'
            f'  "--list --format=terse --ignored") printf "{ignored}: test\\n";;\n'
            f'  *) touch "{root / "BODY_EXECUTED"}"; exit 99;;\n'
            f'esac\nexit {list_exit}\n')
        executable.chmod(0o755)
        artifact = dict(reason="compiler-artifact", package_id="fixture 0.1.0", executable=str(executable),
                        target=dict(name="fixture", kind=["lib"], doctest=True),
                        features=["hiqlite-store"], profile=dict(test=True, opt_level="0"))
        path = root / "artifacts.jsonl"
        path.write_text("not-json\n" if invalid else json.dumps(artifact) + "\n")
        return path, executable

    def discover(self, root, artifact, status=0, rust="1.97.1"):
        def compiler(command, **kwargs):
            if command[-1] == "--version":
                return f"rustc {rust} (fixture)\n"
            return f"rustc {rust}\nhost: x86_64-unknown-linux-gnu\n"
        with mock.patch.object(inventory, "git_identity", return_value=dict(commit="a" * 40, tree="b" * 40, tracked_clean=True)), \
             mock.patch.object(inventory.subprocess, "check_output", side_effect=compiler):
            return inventory.discover(root / "result", ["cargo", "test", "--workspace", "--locked", "--exclude", "plurx-cluster-check"],
                                      artifacts=artifact, compiler_exit_code=status)

    def test_exact_binary_inventory_marks_ignored_without_executing_bodies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, executable = self.artifacts(root)
            result = self.discover(root, artifact)
            self.assertTrue(result["discovery_complete"], result["errors"])
            self.assertEqual(result["execution_status"], "not_executed")
            self.assertEqual(result["expected_cases"], 2)
            self.assertEqual(result["ignored_cases"], 1)
            binary = result["binaries"][0]
            self.assertEqual(binary["sha256"], inventory.sha256(executable))
            self.assertEqual(binary["features"], ["hiqlite-store"])
            self.assertEqual(binary["cases"], [dict(name="ignored_case", ignored=True), dict(name="ordinary", ignored=False)])
            self.assertEqual(result["source"]["tree"], "b" * 40)
            self.assertEqual(result["doctests"]["status"], "not_discovered_or_executed")
            self.assertEqual(len(result["doctests"]["targets"]), 1)
            self.assertFalse((root / "BODY_EXECUTED").exists())
            self.assertEqual(json.loads((root / "result/inventory.json").read_text()), result)

    def test_failed_compile_preserves_partial_inventory_without_claiming_completeness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, _ = self.artifacts(root)
            result = self.discover(root, artifact, status=101)
            self.assertFalse(result["discovery_complete"])
            self.assertEqual(result["compile"]["exit_code"], 101)
            self.assertEqual(result["expected_cases"], 2)
            self.assertEqual(result["execution_status"], "not_executed")

    def test_failed_listing_and_non_subset_ignored_identities_are_incomplete(self):
        for options in (dict(list_exit=6), dict(ignored="undiscovered"), dict(invalid=True)):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                artifact, _ = self.artifacts(root, **options)
                result = self.discover(root, artifact)
                self.assertFalse(result["discovery_complete"])
                self.assertTrue(result["errors"])
                self.assertTrue((root / "result/inventory.json").is_file())

    def test_wrong_compiler_refuses_listing_and_retains_incomplete_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, _ = self.artifacts(root)
            result = self.discover(root, artifact, rust="1.96.0")
            self.assertFalse(result["discovery_complete"])
            self.assertIn("expected Rust 1.97.1", result["errors"][0])
            self.assertEqual(result["binaries"], [])

    def test_compile_command_cannot_accidentally_execute_test_bodies(self):
        command = ["cargo", "test", "--workspace", "--locked", "--exclude", "plurx-cluster-check", "--features", "one,two"]
        self.assertEqual(inventory.compile_command(command), [*command, "--no-run", "--message-format=json"])
        for bad in (["cargo", "run"], ["cargo", "test", "--", "test_name"],
                    ["cargo", "test", "--doc"], ["cargo", "test", "--message-format=human"]):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                inventory.compile_command(bad)

    def test_unknown_or_duplicate_listing_output_never_silently_disappears(self):
        for text in ("case: test\ncase: test\n", "starting test bodies\n", "case: benchmark\n",
                     "case: test\n2 tests, 0 benchmarks\n", "0 tests, 0 benchmarks\ncase: test\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                inventory.parse_listing(text)

    def test_capture_keeps_json_and_diagnostics_separate_and_owns_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stdout, stderr = root / "out", root / "err"
            result = inventory.run_capture([sys.executable, "-c",
                'import sys; print("{}", flush=True); print("diagnostic", file=sys.stderr, flush=True)'],
                stdout, stderr, 5)
            self.assertTrue(result["success"], result)
            self.assertEqual(json.loads(stdout.read_text()), {})
            self.assertEqual(stderr.read_text().strip(), "diagnostic")
            result = inventory.run_capture([sys.executable, "-c", "import time; time.sleep(60)"],
                                           stdout, stderr, 0.1)
            self.assertTrue(result["timed_out"])
            self.assertFalse(result["success"])
            self.assertTrue(result["cleanup_complete"], result)
            self.assertTrue(result["leader_reaped_after_cleanup"])
            self.assertIsNotNone(result["exit_code"])

    def test_compile_cleanup_failure_retains_progress_and_blocks_all_listing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, _ = self.artifacts(root)
            def capture(command, stdout, stderr, timeout):
                progress = json.loads((root / "result/inventory.json").read_text())
                self.assertEqual(progress["stage"], "compiling")
                self.assertFalse(progress["discovery_complete"])
                self.assertEqual(progress["source"]["commit"], "a" * 40)
                shutil.copyfile(artifact, stdout)
                Path(stderr).write_text("cleanup refused")
                return dict(exit_code=0, success=False, cleanup_complete=False,
                            seconds=0.1, cleanup_error="group unqualified")
            with mock.patch.object(inventory, "git_identity", return_value=dict(commit="a" * 40, tree="b" * 40, tracked_clean=True)), \
                 mock.patch.object(inventory.subprocess, "check_output", side_effect=lambda args, **kw:
                    "rustc 1.97.1\nhost: x86_64-unknown-linux-gnu\n"), \
                 mock.patch.object(inventory, "run_capture", side_effect=capture) as launch:
                result = inventory.discover(root / "result", ["cargo", "test", "--workspace"])
            self.assertEqual(launch.call_count, 1)
            self.assertFalse(result["discovery_complete"])
            self.assertFalse(result["binaries"][0]["listing_complete"])
            self.assertIn("blocked", result["binaries"][0]["error"])
            self.assertEqual(result["stage"], "finished")

    def test_listing_progress_is_persisted_before_each_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, _ = self.artifacts(root)
            original = inventory.run_capture
            stages = []
            def capture(*arguments):
                progress = json.loads((root / "result/inventory.json").read_text())
                stages.append(progress["stage"])
                self.assertFalse(progress["discovery_complete"])
                self.assertIn("sha256", progress["binaries"][0])
                return original(*arguments)
            with mock.patch.object(inventory, "run_capture", side_effect=capture):
                result = self.discover(root, artifact)
            self.assertTrue(result["discovery_complete"], result["errors"])
            self.assertEqual(stages, ["listing:fixture:all", "listing:fixture:ignored"])

    def test_listing_cleanup_failure_blocks_remaining_binary_processes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, executable = self.artifacts(root)
            second = root / "second"
            shutil.copyfile(executable, second)
            second.chmod(0o755)
            row = json.loads(artifact.read_text())
            row["executable"] = str(second)
            row["target"]["name"] = "second"
            with artifact.open("a") as stream:
                stream.write(json.dumps(row) + "\n")
            def capture(command, stdout, stderr, timeout):
                Path(stdout).write_text("ordinary: test\n")
                Path(stderr).write_text("")
                return dict(exit_code=0, success=False, cleanup_complete=False, seconds=0.1)
            with mock.patch.object(inventory, "run_capture", side_effect=capture) as launch:
                result = self.discover(root, artifact)
            self.assertEqual(launch.call_count, 1)
            self.assertFalse(result["discovery_complete"])
            self.assertEqual(len(result["binaries"]), 2)
            self.assertIn("blocked", result["binaries"][1]["error"])
            self.assertTrue((root / "result/inventory.json").is_file())

    def test_nonfinite_timeouts_are_rejected_before_launch(self):
        for timeout in (0, -1, float("inf"), float("nan"), True):
            with self.subTest(timeout=timeout), tempfile.TemporaryDirectory() as directory:
                with mock.patch.object(inventory, "run_capture") as launch:
                    result = inventory.discover(Path(directory), ["cargo", "test"], compile_timeout=timeout)
                self.assertFalse(result["discovery_complete"])
                self.assertIn("positive and finite", result["errors"][0])
                launch.assert_not_called()

    def test_ingested_artifacts_require_actual_compiler_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact, _ = self.artifacts(root)
            result = self.discover(root, artifact, status=None)
            self.assertFalse(result["discovery_complete"])
            self.assertIn("--compiler-exit-code", result["errors"][0])


if __name__ == "__main__":
    unittest.main()
