"""Fail-closed Kani receipt parsing and mutation tests; no verifier is invoked."""
from __future__ import annotations

import copy
import importlib.machinery
import importlib.util
import unittest
import tempfile
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("check_worker_control", str(ROOT / "scripts/check-worker-control"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(runner)
HARNESS = runner.HARNESSES[2]
ASSERTION = "T3 queue retired before admission dropped last"


def inventory() -> dict:
    return {
        "kani-version": "0.68.0", "file-version": "0.1",
        "standard-harnesses": {"src/proofs.rs": list(runner.HARNESSES)},
        "contract-harnesses": {}, "contracts": [],
        "totals": {"standard-harnesses": 6, "contract-harnesses": 0, "functions-under-contract": 0},
    }


def log(*, failed: bool = False, property_class: str = "assertion", description: str = ASSERTION,
        status: str | None = None, harness: str = HARNESS) -> str:
    actual_status = status or ("FAILURE" if failed else "SUCCESS")
    count = int(actual_status != "SUCCESS")
    return (
        f"Checking harness {harness}...\n"
        f"Check 1: {harness}.{property_class}.1\n"
        f"\t - Status: {actual_status}\n"
        f'\t - Description: "{description}"\n'
        "\t - Location: src/proofs.rs:1:1\n"
        f"SUMMARY:\n ** {count} of 1 failed\n"
        f"VERIFICATION:- {'FAILED' if failed else 'SUCCESSFUL'}\n"
        f"Complete - {int(not failed)} successfully verified harnesses, {int(failed)} failures, 1 total.\n"
    )


class WorkerControlInventoryTests(unittest.TestCase):
    def test_exact_six_names_and_counts_are_required(self) -> None:
        self.assertEqual(runner.parse_inventory(inventory()), list(runner.HARNESSES))
        for change in ("missing", "duplicate", "extra", "count", "contract", "version", "schema"):
            with self.subTest(change=change):
                data = inventory()
                names = data["standard-harnesses"]["src/proofs.rs"]
                if change == "missing": names.pop()
                elif change == "duplicate": names[-1] = names[0]
                elif change == "extra": names.append("another_module::unreviewed_harness")
                elif change == "count": data["totals"]["standard-harnesses"] = 5
                elif change == "contract": data["contracts"] = ["stubbed_step"]
                elif change == "version": data["kani-version"] = "different"
                else: data["unknown"] = True
                with self.assertRaises(RuntimeError):
                    runner.parse_inventory(data)


class WorkerControlVerdictTests(unittest.TestCase):
    def test_positive_requires_complete_checked_properties(self) -> None:
        self.assertEqual(runner.verdict(log(), 0, HARNESS, None)["outcome"], "proved")
        corruptions = (
            (log(), 1),
            (log().replace("Check 1:", "Check 2:"), 0),
            (log().replace("Description:", "corrupted:"), 0),
            (log().replace("0 of 1", "0 of 9"), 0),
            (log().replace("Complete -", "Incomplete -"), 0),
            (log(harness="other::harness"), 0),
            (log() + log(), 0),
        )
        for output, code in corruptions:
            with self.subTest(output=output):
                with self.assertRaises(RuntimeError):
                    runner.verdict(output, code, HARNESS, None)
        for status in ("SKIPPED", "UNKNOWN", "UNREACHABLE", "UNDETERMINED"):
            with self.subTest(status=status), self.assertRaises(RuntimeError):
                runner.verdict(log(status=status), 0, HARNESS, None)

    def test_nonmarker_panic_unreachable_is_explicitly_counted(self) -> None:
        extra = ('Check 2: core::result::unwrap_failed.assertion.1\n'
                 '\t - Status: UNREACHABLE\n'
                 '\t - Description: "reachable process-gating event accepted"\n')
        output = log().replace('SUMMARY:', extra + 'SUMMARY:').replace('0 of 1 failed', '0 of 2 failed (1 unreachable)')
        result = runner.verdict(output, 0, HARNESS, None)
        self.assertEqual(result["status_counts"], {"SUCCESS": 1, "UNREACHABLE": 1, "FAILURE": 0})
        for altered in (
            output.replace('core::result::unwrap_failed.assertion.1', 'slice.array_bounds.1'),
            output.replace('reachable process-gating event accepted', 'T3 unreachable specification'),
            output.replace('(1 unreachable)', ''),
            output.replace('(1 unreachable)', '(2 unreachable)'),
            output.replace('(1 unreachable)', '(1 unreachable,1 unreachable)'),
            output.replace('(1 unreachable)', '(1 ignored)'),
        ):
            with self.subTest(altered=altered), self.assertRaises(RuntimeError):
                runner.verdict(altered, 0, HARNESS, None)

    def test_vacuous_or_absent_specification_assertions_reject(self) -> None:
        unreachable = log(status="UNREACHABLE").replace('1 of 1 failed', '0 of 1 failed (1 unreachable)')
        for output in (unreachable, log(description="compiler-generated bounds check")):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                runner.verdict(output, 0, HARNESS, None)

    def test_parenthetical_undetermined_counts_never_create_a_pass(self) -> None:
        checks = [("guard.assertion.1", "UNDETERMINED", "guard")]
        output = log(status="UNDETERMINED").replace('1 of 1 failed', '0 of 1 failed (1 undetermined)')
        runner.check_summary(output, checks)
        with self.assertRaises(RuntimeError):
            runner.verdict(output, 0, HARNESS, None)

    def test_control_requires_named_semantic_assertion_counterexample(self) -> None:
        result = runner.verdict(log(failed=True), 1, HARNESS, ASSERTION, [ASSERTION])
        self.assertEqual(result["outcome"], "semantic_counterexample")
        for output, code in (
            ("error: failed to compile crate", 1),
            (log(failed=True, property_class="unwind"), 1),
            (log(failed=True, property_class="overflow"), 1),
            (log(failed=True, description="unrelated assertion"), 1),
            (log(failed=True, harness="missing::target"), 1),
            (log(failed=True), -9),
            (log(failed=True), 0),
            (log(), 0),
        ):
            with self.subTest(output=output, code=code), self.assertRaises(RuntimeError):
                runner.verdict(output, code, HARNESS, ASSERTION, [ASSERTION])

    def test_optional_downstream_assertion_does_not_replace_required_one(self) -> None:
        with self.assertRaises(RuntimeError):
            runner.verdict(log(failed=True, description="T3 admission is last effect"), 1,
                           HARNESS, ASSERTION, [ASSERTION, "T3 admission is last effect"])


class WorkerControlMutationTests(unittest.TestCase):
    def test_exactly_one_changed_anchor_is_mandatory(self) -> None:
        control = {"name": "fixture", "before": "saturating_add", "after": "wrapping_add"}
        self.assertEqual(runner.mutate_source("now.saturating_add(5_000)", control), "now.wrapping_add(5_000)")
        for source in ("no anchor", "saturating_add saturating_add"):
            with self.subTest(source=source), self.assertRaises(RuntimeError):
                runner.mutate_source(source, control)
        unchanged = copy.deepcopy(control)
        unchanged["after"] = unchanged["before"]
        with self.assertRaises(RuntimeError):
            runner.mutate_source("saturating_add", unchanged)


class WorkerControlResumeTests(unittest.TestCase):
    def test_success_is_reparsed_and_preserves_original_candidate(self) -> None:
        base = ROOT / "target/worker-control-runner-fixtures"
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as directory:
            prior, output = Path(directory) / "prior", Path(directory) / "next"
            prior.mkdir(); output.mkdir()
            name = HARNESS.rsplit("::", 1)[1]
            log_path = prior / f"{name}.log"
            log_path.write_text(log())
            original = {"head": "original", "files": {path: "same-input" for path in runner.PROOF_INPUTS}, "staging": {"manifest_sha256": "same-envelope"}}
            current = copy.deepcopy(original)
            current["head"] = "runner-only-new-commit"
            tools = {"driver_sha256": "native-tool"}
            result = runner.verdict(log(), 0, HARNESS, None)
            result.update({"source": original, "tools": tools,
                           "command": ["cargo-kani", "--harness", HARNESS, "--exact", "--output-format", "regular", "--manifest-path", str(prior / "source/Cargo.toml"), "--target-dir", str(prior / "build")],
                           "execution_directory": str(prior), "unwinding_checks": True,
                           "default_safety_checks": True, "log_sha256": runner.digest(log_path)})
            runner.write_json(prior / f"{name}.json", result)
            with mock.patch.object(runner, "validate_case_sources"):
                retained = runner.retain_previous(prior, output, HARNESS, current, tools, None, None)
                self.assertIsNotNone(retained)
                self.assertEqual(retained["source"]["head"], "original")
                self.assertEqual(retained["source_bridge"]["current_candidate"]["head"], "runner-only-new-commit")
                changed = copy.deepcopy(current)
                changed["files"][runner.CORE] = "changed-core"
                self.assertIsNone(runner.retain_previous(prior, output, HARNESS, changed, tools, None, None))
                self.assertIsNone(runner.retain_previous(prior, output, HARNESS, current, {"driver_sha256": "other-tool"}, None, None))
                log_path.write_text(log() + "corruption")
                self.assertIsNone(runner.retain_previous(prior, output, HARNESS, current, tools, None, None))



if __name__ == "__main__":
    unittest.main()
