import json
import tempfile
import unittest
from pathlib import Path
from subprocess import CalledProcessError
from unittest.mock import patch

from validation import workspace_rust_partitions as release


class OfficialWorkspaceRustTests(unittest.TestCase):
    def fixture(self, directory):
        identity = {"run": "123", "sha": "4" * 40}
        inventory = sorted([
            "plurx_core@src/lib.rs",
            "plurxd@src/main.rs",
            "store_contract@tests/store_contract.rs",
            "hiqlite_encoded_bootstrap@tests/hiqlite_encoded_bootstrap.rs",
        ])
        for name in release.PARTITIONS:
            assigned = [target for target in inventory if release.partition(target) == name]
            data = {**identity, "partition": name, "inventory": inventory, "passed": assigned}
            (directory / f"official-rust-{name}.json").write_text(json.dumps(data))
        return identity

    def test_release_checks_track_the_original_workspace_recipe(self):
        makefile = (release.workspace.ROOT / "Makefile").read_text()
        recipe = makefile.split("\nci-rust-gate: ", 1)[1].split("\n\n", 1)[0]
        prerequisites, run, restart = recipe.splitlines()
        self.assertEqual(prerequisites.split("##", 1)[0].split(), list(release.PREREQUISITES))
        self.assertEqual(run.strip().split()[2:], [*release.workspace.WORKSPACE, "--no-fail-fast"])
        self.assertEqual(restart.strip().split()[1], release.RESTART_CHECK)

    def test_rejects_missing_coverage_or_receipts_from_another_run(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            identity = self.fixture(directory)
            path = directory / "official-rust-store.json"
            correct = json.loads(path.read_text())
            with patch.object(release, "identity", return_value=identity):
                release.verify(directory)
                for changes in (
                    {"run": "122"},
                    {"sha": "8" * 40},
                    {"partition": "core"},
                    {"passed": []},
                    {"passed": correct["passed"] + correct["passed"]},
                    {"inventory": correct["inventory"][:-1]},
                ):
                    with self.subTest(changes=changes):
                        path.write_text(json.dumps({**correct, **changes}))
                        with self.assertRaises(RuntimeError):
                            release.verify(directory)
                path.unlink()
                with self.assertRaises(FileNotFoundError):
                    release.verify(directory)

    def test_runner_only_executes_assigned_binary_and_rejects_a_failed_target(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)
            core = output / "core"
            store = output / "store"
            for program, status in ((core, 0), (store, 1)):
                program.write_text(f"#!/bin/sh\nexit {status}\n")
                program.chmod(0o755)
            plan = output / "plan.json"
            targets = {
                str(core.resolve()): "plurx_core@src/lib.rs",
                str(store.resolve()): "store_contract@tests/store_contract.rs",
            }
            with patch.object(release, "OUTPUT", output), patch.object(release, "PLAN", plan), \
                 patch.dict("os.environ", {release.workspace.RUNNER_VARIABLE: "fixture"}):
                plan.write_text(json.dumps({"partition": "core", "targets": targets}))
                release.run_target(str(core))
                release.run_target(str(store))
                self.assertEqual([file.read_text() for file in output.glob("passed-*")],
                                 ["plurx_core@src/lib.rs"])
                plan.write_text(json.dumps({"partition": "store", "targets": targets}))
                with self.assertRaises(CalledProcessError):
                    release.run_target(str(store))


if __name__ == "__main__":
    unittest.main()
