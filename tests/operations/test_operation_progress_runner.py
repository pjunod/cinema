"""Package boundary and receipt contracts for the operation-progress TLC runner."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("check_formal_progress", str(ROOT / "scripts/check-formal"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None and SPEC.loader is not None
formal = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(formal)


class OperationProgressPackageTests(unittest.TestCase):
    def test_cli_package_selection_is_explicit_and_closed(self) -> None:
        for command in ("check-mapping", "smoke", "full"):
            with self.subTest(command=command):
                args = formal.parser().parse_args([command, "--package", "operation-progress"])
                self.assertEqual(args.package, "operation-progress")
        with self.assertRaises(SystemExit):
            formal.parser().parse_args(["smoke", "--package", "../operation-progress"])
        self.assertEqual(formal.parser().parse_args(["smoke"]).package, "durable-jobs")

    def test_registry_fixes_model_configs_smoke_and_deadlock_semantics(self) -> None:
        package = formal.package_spec("operation-progress")
        expected = (
            "progress.cfg", "contention.cfg", "control-lock-cycle.cfg",
            "control-renewal-livelock.cfg", "control-external-stall.cfg",
            "control-lost-settlement.cfg", "witness-success.cfg",
            "witness-reconcile.cfg", "witness-cancel-cleanup.cfg",
        )
        self.assertEqual(package["mapping"], "verification/operation-progress/mapping.json")
        self.assertEqual(package["model_file"], "verification/operation-progress/OperationProgress.tla")
        self.assertEqual(package["config_order"], expected)
        self.assertEqual(formal.select_configs("smoke", None, "operation-progress"), ["progress.cfg", "witness-success.cfg"])
        self.assertEqual(formal.select_configs("full", None, "operation-progress"), list(expected))
        self.assertFalse(package["disable_deadlock_check"])
        self.assertEqual(package["smoke_wall_budget_seconds"], 180)
        self.assertEqual(package["config_timeout_cap_seconds"], 120)
        package["smoke_configs"] = ()
        self.assertEqual(formal.package_spec("operation-progress")["smoke_configs"], ("progress.cfg", "witness-success.cfg"))
        self.assertEqual(formal.config_timeout("full", "contention.cfg", 700, None, "operation-progress"), 120)
        self.assertEqual(formal.config_timeout("smoke", "progress.cfg", 700, 90, "operation-progress"), 90)
        with self.assertRaises(formal.RunnerError):
            formal.select_configs("smoke", "unregistered.cfg", "operation-progress")

    def test_mapping_drift_rejects_unreviewed_model_and_timeout_over_cap(self) -> None:
        package = formal.package_spec("operation-progress")
        configs = {}
        for name in package["config_order"]:
            if name.startswith("control-"):
                verdict, purpose, expected_exit = "violation", "defect_control", 13
            elif name.startswith("witness-"):
                verdict, purpose, expected_exit = "witness", "reachability_witness", 12
            else:
                verdict, purpose, expected_exit = "pass", "safety", 0
            config = {
                "verdict": verdict, "purpose": purpose,
                "expected_error": "Prop" if verdict != "pass" else None,
                "expected_exit": expected_exit,
                "timeout_seconds": 120,
                "expected_error_match": ({
                    "line": "Invariant Prop is violated.",
                    "attribution": "named_property",
                    "allowed_error_lines": ["The behavior up to this point is:"],
                } if verdict != "pass" else None),
                "minimum_trace_states": 2 if verdict != "pass" else None,
                "analysis": {
                    "status": "reviewed", "bounds": {"N": [0, 1]},
                    "properties": ["Prop"], "reductions": [], "assumptions": [],
                },
                "sha256": "a" * 64,
            }
            configs[name] = config
        mapping = {
            "model_file": package["model_file"], "model_sha256": "a" * 64,
            "configs": configs,
            "source_mapping": {"status": "reviewed", "tables": [{
                "table": "reviewed source", "source": "src.rs", "anchor": "source anchor",
                "sha256": "a" * 64,
            }]},
        }

        class Source:
            def __init__(self, text: str = "") -> None:
                self.text = text

            def read_text(self, encoding: str = "utf-8") -> str:
                return self.text

        def fake_repo_file(relative: str, label: str) -> Source:
            return Source("SPECIFICATION Spec\nINVARIANT Prop\n" if relative.endswith(".cfg") else "source anchor")

        with (
            mock.patch.object(formal, "repo_file", side_effect=fake_repo_file),
            mock.patch.object(formal, "sha256", return_value="a" * 64),
        ):
            drift, _ = formal.mapping_inputs(mapping, "operation-progress")
            self.assertEqual(drift, [])
            mapping["configs"]["contention.cfg"]["timeout_seconds"] = 121
            drift, _ = formal.mapping_inputs(mapping, "operation-progress")
            self.assertTrue(any("120-second cap" in item for item in drift))
            mapping["configs"]["contention.cfg"]["timeout_seconds"] = 120
            mapping["model_file"] = "verification/durable-jobs/DurableJobs.tla"
            drift, _ = formal.mapping_inputs(mapping, "operation-progress")
            self.assertTrue(any("reviewed package model" in item for item in drift))

    def test_repository_path_guard_rejects_external_and_parent_paths(self) -> None:
        for candidate in ("/etc/passwd", "verification/operation-progress/../durable-jobs/DurableJobs.tla"):
            with self.subTest(candidate=candidate), mock.patch.object(formal, "ROOT", Path("/repo")):
                with self.assertRaises(formal.RunnerError):
                    formal.repo_file(candidate, "model")

    def test_progress_affected_paths_exclude_unrelated_durable_model(self) -> None:
        mapping = {
            "model_file": "verification/operation-progress/OperationProgress.tla",
            "configs": {name: {} for name in formal.package_spec("operation-progress")["config_order"]},
            "source_mapping": {"tables": [{"source": "crates/plurx-core/src/cluster/operation.rs"}]},
        }
        with (
            mock.patch.object(formal, "repo_file", return_value=Path("/reviewed/mapping.json")),
            mock.patch.object(formal, "read_json", return_value=mapping),
            mock.patch.object(formal, "mapping_inputs", return_value=([], {})),
            mock.patch.object(formal, "changed_paths", return_value=["verification/durable-jobs/DurableJobs.tla"]),
            mock.patch.object(formal, "sha256", return_value="a" * 64),
            contextlib.redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(formal.mapping_check("base", "operation-progress"), 0)
        receipt = json.loads(output.getvalue())
        self.assertEqual(receipt["package"], "operation-progress")
        self.assertFalse(receipt["affected"])

        with (
            mock.patch.object(formal, "repo_file", return_value=Path("/reviewed/mapping.json")),
            mock.patch.object(formal, "read_json", return_value=mapping),
            mock.patch.object(formal, "mapping_inputs", return_value=([], {})),
            mock.patch.object(formal, "changed_paths", return_value=["crates/plurx-core/src/cluster/operation.rs"]),
            mock.patch.object(formal, "sha256", return_value="a" * 64),
            contextlib.redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(formal.mapping_check("base", "operation-progress"), 0)
        self.assertTrue(json.loads(output.getvalue())["affected"])

    def test_run_receipt_is_bound_to_package_and_progress_config(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output_dir = root / "receipts"
            model_path = root / "OperationProgress.tla"
            config_path = root / "progress.cfg"
            model_path.write_text("---- MODULE OperationProgress ----\n", encoding="utf-8")
            config_path.write_text("SPECIFICATION Spec\n", encoding="utf-8")
            mapping = {
                "model_file": "verification/operation-progress/OperationProgress.tla",
                "configs": {
                    "progress.cfg": {
                        "verdict": "pass", "purpose": "safety", "expected_error": None,
                        "expected_exit": 0, "timeout_seconds": 120, "analysis": {"status": "reviewed"},
                    },
                },
            }
            input_hashes = {
                mapping["model_file"]: "b" * 64,
                "verification/operation-progress/progress.cfg": "c" * 64,
            }

            def resolve(relative: str, label: str) -> Path:
                return model_path if relative == mapping["model_file"] else config_path

            args = SimpleNamespace(
                package="operation-progress", mode="smoke", config="progress.cfg",
                output_dir=output_dir, cache_dir=root / "cache",
            )
            with (
                mock.patch.object(formal, "repo_file", side_effect=resolve),
                mock.patch.object(formal, "read_json", return_value=mapping),
                mock.patch.object(formal, "mapping_inputs", return_value=([], input_hashes)),
                mock.patch.object(formal, "durable_output", return_value=output_dir),
                mock.patch.object(formal, "source_git_identity", return_value={"governed_dirty_paths": []}),
                mock.patch.object(formal, "sha256", return_value="d" * 64),
                mock.patch.object(formal, "resolve_toolchain", return_value={
                    "manifest_sha256": "e" * 64,
                    "java": {"path": "/java", "sha256": "f" * 64},
                    "tlc": {"path": "/tlc.jar", "sha256": "1" * 64},
                }),
                mock.patch.object(formal, "execute", return_value={
                    "output": "Model checking completed. No error has been found.\n"
                              "3 states generated, 2 distinct states found, 0 states left on queue.\n"
                              "The depth of the complete state graph search is 2.\n",
                    "returncode": 0, "timed_out": False, "log_problem": None,
                    "elapsed_seconds": 0.01, "peak_rss_bytes": 1024,
                }) as execute_mock,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(formal.run(args), 0)
            receipt_path = next(output_dir.glob("*/receipt.json"))
            receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
            self.assertEqual(receipt["package"], "operation-progress")
            self.assertEqual(receipt["configs"][0]["config"], "progress.cfg")
            self.assertEqual(receipt["configs"][0]["config_sha256"], "c" * 64)
            self.assertNotIn("-deadlock", execute_mock.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
