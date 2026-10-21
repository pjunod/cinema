"""Fail-closed parsing tests for synthetic TLC output; no Java/TLC process runs."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import copy
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("check_formal", str(ROOT / "scripts/check-formal"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None and SPEC.loader is not None
formal = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(formal)


PASS = {
    "verdict": "pass",
    "purpose": "safety",
    "expected_error": None,
}
VIOLATION = {
    "verdict": "violation",
    "purpose": "defect_control",
    "expected_error": "S2_PublicationAuthority",
    "expected_exit": 23,
    "expected_error_match": {
        "line": "Invariant S2_PublicationAuthority is violated.",
        "attribution": "named_property",
        "allowed_error_lines": ["The behavior up to this point is:"],
    },
    "minimum_trace_states": 2,
    "analysis": {"properties": ["S2_PublicationAuthority"]},
}
WITNESS = {
    "verdict": "witness",
    "purpose": "reachability_witness",
    "expected_error": "Witness_ThreeSamples",
    "expected_exit": 37,
    "expected_error_match": {
        "line": "Invariant Witness_ThreeSamples is violated.",
        "attribution": "named_property",
        "allowed_error_lines": ["The behavior up to this point is:"],
    },
    "minimum_trace_states": 2,
    "analysis": {"properties": ["Witness_ThreeSamples"]},
}


def complete_output() -> str:
    return """Model checking completed. No error has been found.
3 states generated, 2 distinct states found, 0 states left on queue.
The depth of the complete state graph search is 2.
"""


def violation_output(name: str = "S2_PublicationAuthority") -> str:
    return f"""Error: Invariant {name} is violated.
Error: The behavior up to this point is:
State 1: <Initial predicate>
State 2: <Next predicate>
2 states generated, 2 distinct states found, 0 states left on queue.
The depth of the complete state graph search is 1.
"""


def temporal_violation_output() -> str:
    states = "".join(
        f"State {number}: {'Stuttering' if number == 8 else '<Next predicate>'}\n"
        for number in range(1, 9)
    )
    return (
        "Error: Temporal properties were violated.\n"
        "Error: The following behavior constitutes a counter-example:\n"
        + states
        + "135 states generated, 78 distinct states found, 0 states left on queue.\n"
        "Finished checking temporal properties in 00s at (synthetic).\n"
        "Finished in 00s at (synthetic).\n"
    )


class TLCOutputClassificationTests(unittest.TestCase):
    def test_positive_completion_requires_clean_exit_summary_and_counts(self) -> None:
        result = formal.classify(complete_output(), 0, PASS, False, None)
        self.assertTrue(result["complete"])
        self.assertEqual(result["actual_verdict"], "pass")
        self.assertEqual(result["state_counts"]["distinct"], 2)
        self.assertEqual(result["depth"], 2)

    def test_positive_rejects_nonzero_exit_even_with_success_text(self) -> None:
        result = formal.classify(complete_output(), 1, PASS, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("exit 0", result["reason"])

    def test_positive_rejects_missing_completion_marker(self) -> None:
        output = complete_output().replace("Model checking completed. No error has been found.\n", "")
        result = formal.classify(output, 0, PASS, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("completion marker", result["reason"])

    def test_positive_rejects_nonempty_state_queue(self) -> None:
        output = complete_output().replace("0 states left on queue", "1 states left on queue")
        result = formal.classify(output, 0, PASS, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("states still queued", result["reason"])

    def test_positive_rejects_error_even_if_success_marker_is_present(self) -> None:
        output = complete_output() + "Error: unrelated failure\n"
        result = formal.classify(output, 0, PASS, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("unexpected TLC error", result["reason"])

    def test_violation_requires_expected_exit_named_error_and_trace(self) -> None:
        result = formal.classify(violation_output(), 23, VIOLATION, False, None)
        self.assertTrue(result["complete"])
        self.assertEqual(result["actual_verdict"], "violation")
        self.assertEqual(result["state_counts"]["generated"], 2)
        self.assertFalse(result["exploration_complete"])
        self.assertTrue(result["counterexample_found"])
        self.assertEqual(result["completion_kind"], "expected_defect_counterexample_detected")

    def test_reachability_witness_is_not_reported_as_a_defect_or_exhaustive_pass(self) -> None:
        result = formal.classify(violation_output("Witness_ThreeSamples"), 37, WITNESS, False, None)
        self.assertTrue(result["complete"])
        self.assertEqual(result["actual_verdict"], "witness")
        self.assertEqual(result["completion_kind"], "normal_reachability_witness_detected")
        self.assertFalse(result["counterexample_found"])
        self.assertFalse(result["exploration_complete"])
        self.assertEqual(result["trace_states"], 2)

    def test_reachability_witness_requires_two_trace_states(self) -> None:
        output = violation_output("Witness_ThreeSamples").replace("State 2: <Next predicate>\n", "")
        result = formal.classify(output, 37, WITNESS, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("requires 2", result["reason"])


class RegistryProfileTests(unittest.TestCase):
    def test_registry_profiles_are_explicit_and_bounded(self) -> None:
        self.assertEqual(len(formal.CONFIG_ORDER), 16)
        self.assertEqual(len(formal.SMOKE_CONFIGS), 9)
        self.assertEqual(formal.select_configs("smoke", None), list(formal.SMOKE_CONFIGS))
        self.assertEqual(formal.select_configs("full", None), list(formal.CONFIG_ORDER))
        self.assertIn("lifecycle.cfg", formal.select_configs("full", None))
        self.assertNotIn("lifecycle.cfg", formal.select_configs("smoke", None))
        self.assertEqual(formal.select_configs("smoke", "control-bind-before-work.cfg"), ["control-bind-before-work.cfg"])
        self.assertEqual(formal.config_timeout("smoke", "smoke.cfg", 60, 170), 60)
        self.assertEqual(formal.config_timeout("smoke", "smoke.cfg", 12, None), 12)
        self.assertEqual(formal.config_timeout("smoke", "smoke.cfg", 12, 8), 8)
        self.assertEqual(formal.config_timeout("smoke", "recovery.cfg", 900, 170), 30)
        self.assertEqual(formal.config_timeout("smoke", "witness-displacement.cfg", 30, 10), 10)
        with self.assertRaises(formal.RunnerError):
            formal.select_configs("smoke", "witness-time-advance.cfg")

    def test_abandoned_feasibility_attempt_stays_non_qualifying(self) -> None:
        attempt = {
            "config_file": "baseline-abandoned10.cfg",
            "config_sha256": "a" * 64,
            "model_sha256": "b" * 64,
            "status": "incomplete",
            "qualifying": False,
            "wall_seconds": 647.03,
            "metrics": {
                "generated": 97_640_975,
                "distinct": 12_616_232,
                "queued": 7_137_143,
                "depth": 9,
                "child_maxrss_bytes": 4_514_365_440,
            },
        }
        self.assertEqual(formal.validate_feasibility_attempts([attempt]), [])
        attempt["qualifying"] = True
        self.assertTrue(formal.validate_feasibility_attempts([attempt]))


class ConfigContractParsingTests(unittest.TestCase):
    def test_bounds_accept_homogeneous_string_and_numeric_sets(self) -> None:
        self.assertTrue(formal.valid_bound(["hiqlite", "sqlite"]))
        self.assertTrue(formal.valid_bound([0, 1, 2, 3]))
        self.assertFalse(formal.valid_bound([1, -1]))
        self.assertFalse(formal.valid_bound(["hiqlite", 1]))
        self.assertFalse(formal.valid_bound([["nested"]]))

    def test_multiline_properties_stop_before_config_reductions(self) -> None:
        text = """SPECIFICATION Spec
INVARIANTS
  S0_TypeAndIdentity
  S1_ExclusiveCurrentAttempt
PROPERTIES
  S5_CancellationPrecedence
CONSTRAINT PublicationWitnessBound
SYMMETRY Symm
VIEW ModelView
"""
        properties, reductions = formal.parse_config_contract(text)
        self.assertEqual(
            properties,
            {"S0_TypeAndIdentity", "S1_ExclusiveCurrentAttempt", "S5_CancellationPrecedence"},
        )
        self.assertEqual(
            reductions,
            ["CONSTRAINT PublicationWitnessBound", "SYMMETRY Symm", "VIEW ModelView"],
        )

    def test_indented_known_directive_is_not_mistaken_for_a_property(self) -> None:
        properties, reductions = formal.parse_config_contract(
            "INVARIANTS\n  S0_TypeAndIdentity\n  VIEW ModelView\n"
        )
        self.assertEqual(properties, {"S0_TypeAndIdentity"})
        self.assertEqual(reductions, ["VIEW ModelView"])

    def test_malformed_property_block_is_rejected_instead_of_consuming_directives(self) -> None:
        with self.assertRaises(formal.RunnerError):
            formal.parse_config_contract("INVARIANTS\n  SYMMETRY Symm extra\n")
        with self.assertRaises(formal.RunnerError):
            formal.parse_config_contract("UNKNOWN_DIRECTIVE Anything\n")
        self.assertEqual(formal.select_configs("full", "baseline.cfg"), ["baseline.cfg"])

    def test_violation_rejects_wrong_exit(self) -> None:
        result = formal.classify(violation_output(), 1, VIOLATION, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("exit 23", result["reason"])

    def test_violation_rejects_wrong_named_error(self) -> None:
        result = formal.classify(violation_output("DifferentInvariant"), 23, VIOLATION, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("exact TLC error line", result["reason"])

    def test_violation_rejects_unreviewed_additional_error(self) -> None:
        output = violation_output() + "Error: unrelated property failed\n"
        result = formal.classify(output, 23, VIOLATION, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("additional error", result["reason"])

    def test_generic_temporal_error_requires_single_configured_property(self) -> None:
        expected = {
            "verdict": "violation",
            "expected_error": "L1_FreshClaimEventually",
            "expected_exit": 37,
            "expected_error_match": {
                "line": "Temporal properties were violated.",
                "attribution": "sole_property",
                "allowed_error_lines": ["The following behavior constitutes a counter-example:"],
            },
            "minimum_trace_states": 2,
            "analysis": {"properties": ["L1_FreshClaimEventually"]},
        }
        result = formal.classify(temporal_violation_output(), 37, expected, False, None)
        self.assertTrue(result["complete"])
        self.assertIsNone(result["depth"])
        self.assertEqual(result["completion_kind"], "temporal_counterexample_detected")
        expected["analysis"]["properties"].append("S2_PublicationAuthority")
        result = formal.classify(temporal_violation_output(), 37, expected, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("exactly property", result["reason"])

    def test_temporal_counterexample_without_completion_marker_is_incomplete(self) -> None:
        expected = {
            "verdict": "violation",
            "expected_error": "L1_FreshClaimEventually",
            "expected_exit": 37,
            "expected_error_match": {
                "line": "Temporal properties were violated.",
                "attribution": "sole_property",
                "allowed_error_lines": ["The following behavior constitutes a counter-example:"],
            },
            "minimum_trace_states": 2,
            "analysis": {"properties": ["L1_FreshClaimEventually"]},
        }
        output = temporal_violation_output().replace("Finished checking temporal properties in 00s at (synthetic).\n", "")
        result = formal.classify(output, 37, expected, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("completion marker", result["reason"])

    def test_temporal_counterexample_requires_stuttering_or_back_edge(self) -> None:
        expected = {
            "verdict": "violation",
            "expected_error": "L1_FreshClaimEventually",
            "expected_exit": 37,
            "expected_error_match": {
                "line": "Temporal properties were violated.",
                "attribution": "sole_property",
                "allowed_error_lines": ["The following behavior constitutes a counter-example:"],
            },
            "minimum_trace_states": 2,
            "analysis": {"properties": ["L1_FreshClaimEventually"]},
        }
        output = temporal_violation_output().replace("State 8: Stuttering\n", "State 8: <Next predicate>\n")
        result = formal.classify(output, 37, expected, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("stuttering or back-edge", result["reason"])

    def test_temporal_back_edge_accepts_reviewed_annotation(self) -> None:
        expected = {
            "verdict": "violation",
            "expected_error": "L1_FreshClaimEventually",
            "expected_exit": 37,
            "expected_error_match": {
                "line": "Temporal properties were violated.",
                "attribution": "sole_property",
                "allowed_error_lines": ["The following behavior constitutes a counter-example:"],
            },
            "minimum_trace_states": 2,
            "analysis": {"properties": ["L1_FreshClaimEventually"]},
        }
        output = temporal_violation_output().replace(
            "State 8: Stuttering\n", "State 8: <Tick>\n"
        ).replace(
            "Finished checking temporal properties in 00s at (synthetic).\n",
            "Back to state 19: <Tick line 331, col 3 to line 332, col 53 of module OperationProgress>\n"
            "Finished checking temporal properties in 00s at (synthetic).\n",
        )
        result = formal.classify(output, 37, expected, False, None)
        self.assertTrue(result["complete"])
        self.assertEqual(result["completion_kind"], "temporal_counterexample_detected")
        bare_back_edge = output.replace(
            "Back to state 19: <Tick line 331, col 3 to line 332, col 53 of module OperationProgress>",
            "Back to state 19",
        )
        self.assertTrue(
            formal.classify(bare_back_edge, 37, expected, False, None)["complete"]
        )

        malformed_terminators = (
            "Back to state 19: <>\n",
            "Back to state 19: < line 331, col 3 to line 332, col 53 "
            "of module OperationProgress>\n",
            "Back to state 19: <Tick line 331, col 3 to line 332, col 53 "
            "of module OperationProgress\n",
            "Back to state 19: <Tick line 331, col 3 to line 332, col 53 "
            "of module OperationProgress> trailing\n",
        )
        for terminator in malformed_terminators:
            with self.subTest(terminator=terminator):
                malformed = output.replace(
                    "Back to state 19: <Tick line 331, col 3 to line 332, col 53 "
                    "of module OperationProgress>\n",
                    terminator,
                )
                result = formal.classify(malformed, 37, expected, False, None)
                self.assertFalse(result["complete"])
                self.assertIn("stuttering or back-edge", result["reason"])

    def test_violation_rejects_missing_counterexample(self) -> None:
        output = violation_output().replace("State 1: <Initial predicate>\n", "")
        result = formal.classify(output, 23, VIOLATION, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("requires 2", result["reason"])

    def test_violation_rejects_parse_failure_even_if_expected_name_appears(self) -> None:
        output = "Parsing Error: S2_PublicationAuthority\n" + violation_output()
        result = formal.classify(output, 23, VIOLATION, False, None)
        self.assertFalse(result["complete"])
        self.assertIn("fatal error", result["reason"])

    def test_timeout_and_oversized_log_are_incomplete(self) -> None:
        timeout = formal.classify(complete_output(), 0, PASS, True, None)
        oversized = formal.classify(complete_output(), 0, PASS, False, "log too large")
        self.assertFalse(timeout["complete"])
        self.assertIn("timed out", timeout["reason"])
        self.assertFalse(oversized["complete"])
        self.assertIn("log too large", oversized["reason"])


class MappingFailClosedTests(unittest.TestCase):
    def test_unreviewed_fable_mapping_cannot_be_accepted(self) -> None:
        mapping = copy.deepcopy(formal.read_json(formal.MAPPING_PATH, "test mapping"))
        mapping["model_file"] = "verification/durable-jobs/__runner_test_missing__.tla"
        mapping["model_sha256"] = None
        mapping["source_mapping"] = {"status": "pending_fable_authority", "tables": []}
        drift, _ = formal.mapping_inputs(mapping)
        self.assertTrue(any("pending review" in issue for issue in drift))
        self.assertTrue(any("missing or unsafe model" in issue for issue in drift))

    def test_source_identity_records_commit_tree_and_untracked_governed_inputs(self) -> None:
        def git_output(*args: str, nul: bool = False) -> list[str]:
            if args == ("rev-parse", "--verify", "HEAD"):
                return ["a" * 40]
            if args == ("rev-parse", "--verify", "HEAD^{tree}"):
                return ["b" * 40]
            if args == ("diff", "--name-only", "-z", "HEAD", "--"):
                return ["verification/durable-jobs/DurableJobs.tla"]
            if args == ("ls-files", "--others", "--exclude-standard", "-z"):
                return ["verification/durable-jobs/smoke.cfg"]
            self.fail(f"unexpected git identity request: {args}")

        mapping = {
            "model_file": "verification/durable-jobs/DurableJobs.tla",
            "configs": {"smoke.cfg": {}},
            "source_mapping": {"tables": []},
        }
        with mock.patch.object(formal, "_git_output", side_effect=git_output):
            result = formal.source_git_identity(mapping)
        self.assertEqual(result["commit"], "a" * 40)
        self.assertEqual(result["tree"], "b" * 40)
        self.assertTrue(result["dirty"])
        self.assertIn("verification/durable-jobs/smoke.cfg", result["untracked_paths"])
        self.assertIn("verification/durable-jobs/smoke.cfg", result["governed_dirty_paths"])


class TLCMetadataCleanupTests(unittest.TestCase):
    def _config_dir(self) -> Path:
        scratch_root = ROOT / "target/formal-tests"
        scratch_root.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch_root)
        self.addCleanup(temporary.cleanup)
        out_dir = Path(temporary.name) / "config"
        out_dir.mkdir()
        return out_dir

    def test_cleanup_removes_only_metadir_and_preserves_receipt_log_sibling(self) -> None:
        out_dir = self._config_dir()
        metadir = out_dir / "metadir"
        metadir.mkdir()
        (metadir / "states.chkpt").write_text("generated state data")
        log = out_dir / "tlc.log"
        log.write_text("counterexample trace")

        result = formal.cleanup_metadir(out_dir, metadir)

        self.assertTrue(result["removed"])
        self.assertFalse(metadir.exists())
        self.assertEqual(log.read_text(), "counterexample trace")

    def test_cleanup_refuses_symlinked_metadata_directory(self) -> None:
        out_dir = self._config_dir()
        outside = out_dir.parent / "preserve"
        outside.mkdir()
        marker = outside / "keep.txt"
        marker.write_text("keep")
        metadir = out_dir / "metadir"
        metadir.symlink_to(outside, target_is_directory=True)

        with self.assertRaises(formal.RunnerError):
            formal.cleanup_metadir(out_dir, metadir)
        self.assertTrue(marker.exists())


if __name__ == "__main__":
    unittest.main()
