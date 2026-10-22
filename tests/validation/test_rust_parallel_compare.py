"""Final accounting controls; generated receipts never claim native execution."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

from validation.rust_parallel import SERIAL
from validation.rust_parallel_compare import REQUIRED, compare, main
from validation.rust_parallel_manifest import BINDINGS, build_plan, digest, validate_receipts


def inventory():
    result = dict(discovery_complete=True, errors=[], source=dict(commit="a" * 40, tree="b" * 40, tracked_clean=True), binaries=[])
    for name, cases in [("plurxd", [("daemon", False), *[(case, True) for case in SERIAL]]),
                        ("plurx_core", [("shared", False)]), ("store_contract", [("shared", False)]), ("empty", [])]:
        result["binaries"].append(dict(executable="/work/target/" + name + "-abcdef", sha256="c" * 64,
            package_id="package:" + name, target=dict(name=name, kind=["bin" if name == "plurxd" else "lib"]),
            features=["hiqlite-store"], profile=dict(test=True, opt_level="0", debuginfo=0), listing_complete=True,
            cases=sorted([dict(name=case, ignored=ignored) for case, ignored in cases], key=lambda row: row["name"])))
    return result


def process(argv=None):
    return dict(success=True, returncode=0, cleanup_complete=True, timed_out=False, seconds=0.1, argv=argv or [])


def suite(name, cases, filtered=0):
    outcomes = [dict(name=case["name"], status="ignored" if case["ignored"] else "ok") for case in cases]
    return dict(binary=name, status="passed", declared_tests=len(cases), outcomes=outcomes,
                result=dict(passed=sum(not case["ignored"] for case in cases), failed=0,
                            ignored=sum(case["ignored"] for case in cases), measured=0,
                            filtered_out=filtered, reported_s=0.1))


def evidence(suites):
    return dict(qualification_complete=True, native_passed=True, gaps=[], observed_suites=suites)


class ParallelCompareCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.inventory = inventory()
        self.plan = build_plan(self.inventory)
        self.put("inventory/inventory.json", self.inventory)
        self.put("workspace-metadata.json", dict(version=1, workspace_members=["core", "empty"], packages=[
            dict(id="core", name="core", targets=[dict(name="plurx_core", kind=["lib"], doctest=True)]),
            dict(id="empty", name="empty", targets=[dict(name="empty", kind=["lib"], doctest=True)])]))
        self.docs = [suite("doctest:plurx_core", [dict(name="src/lib.rs - example (line 1)", ignored=False)]),
                     suite("doctest:empty", [])]
        self.native = [suite(Path(binary["executable"]).name, binary["cases"]) for binary in self.inventory["binaries"]]
        daemon = self.inventory["binaries"][0]
        self.serial = [dict(process=process([daemon["executable"], name, "--exact", "--ignored", "--test-threads=1"]),
                            evidence=evidence([suite(Path(daemon["executable"]).name, [dict(name=name, ignored=False)],
                                                     filtered=len(daemon["cases"]) - 1)])) for name in SERIAL]
        for label, mode in REQUIRED.items():
            report = dict(mode=mode, source=copy.deepcopy(self.inventory["source"]), success=True, errors=[],
                          safe_to_release=True,
                          seconds={"cold-parallel": 30, "legacy": 50, "warm-01": 20, "warm-02": 18, "warm-03": 22}[label],
                          serial=copy.deepcopy(self.serial))
            if mode == "legacy":
                report.update(process=process(), evidence=evidence(copy.deepcopy(self.native + self.docs)))
            else:
                self.put(label + "/plan.json", self.plan)
                receipts = []
                for shard in self.plan["shards"]:
                    receipt = dict(plan_sha256=digest(self.plan), source=self.plan["source"], shard_index=shard["index"], binaries=[])
                    for binary in shard["binaries"]:
                        parsed = suite(Path(binary["executable"]).name, binary["cases"])
                        receipt["binaries"].append(dict({key: binary[key] for key in BINDINGS},
                            outcomes=parsed["outcomes"], returncode=0, success=True, cleanup_complete=True,
                            timed_out=False, process=process(), evidence=evidence([parsed]), parser_failure=False))
                    receipts.append(receipt)
                    self.put(label + f"/shard-{shard['index']}/receipt.json", receipt)
                report.update(native=validate_receipts(self.plan, receipts), doctests=dict(process=process(), suites=[
                    dict({key: value for key, value in row.items() if key != "binary"}, target=row["binary"].removeprefix("doctest:"))
                    for row in copy.deepcopy(self.docs)]))
            self.put(label + "/results.json", report)

    def put(self, relative, value):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))

    def change(self, relative, mutation):
        value = json.loads((self.root / relative).read_text())
        mutation(value)
        self.put(relative, value)

    def assert_red(self, label):
        result = compare(self.root)
        self.assertFalse(result["qualification_complete_for_runtime"])
        self.assertFalse(result["adoption_ready"])
        self.assertTrue(any(row["run"] == label for row in result["errors"]), result)
        return result

    def test_all_fresh_arms_reconcile_complete_runtime_but_never_adopt(self):
        result = compare(self.root)
        self.assertTrue(result["qualification_complete_for_runtime"], result["errors"])
        self.assertFalse(result["adoption_ready"])
        self.assertEqual(result["expected"]["binaries"], 4)
        self.assertEqual(result["expected"]["native_cases"], 5)
        self.assertEqual(result["expected"]["ignored"], 2)
        self.assertEqual(result["timings"], dict(warm_seconds=[20, 18, 22], warm_median_seconds=20,
            warm_range_seconds=[18, 22], cold_parallel_seconds=30, legacy_seconds=50))
        self.assertNotIn("p95", json.dumps(result["timings"]))
        self.assertEqual(result["optional_nextest"], "not run")
        self.assertTrue(result["owed"])
        self.assertIn("warm-03/shard-2/receipt.json", result["artifacts"])

    def test_missing_required_arm_is_not_replaced_by_other_green_runs(self):
        (self.root / "warm-03/results.json").unlink()
        self.assert_red("warm-03")

    def test_original_failed_arm_and_errors_remain_visible(self):
        self.change("warm-02/results.json", lambda d: d.update(success=False, errors=["assertion failed"]))
        result = self.assert_red("warm-02")
        self.assertFalse(result["runs"]["warm-02"]["reported_success"])
        self.assertEqual(result["runs"]["warm-02"]["reported_errors"], ["assertion failed"])

    def test_wrong_mode_or_source_is_rejected(self):
        self.change("cold-parallel/results.json", lambda d: d.update(mode="legacy"))
        self.assert_red("cold-parallel")
        self.change("legacy/results.json", lambda d: d["source"].update(tree="d" * 40))
        self.assert_red("legacy")

    def test_incomplete_dirty_or_invalid_original_inventory_is_rejected(self):
        self.change("inventory/inventory.json", lambda d: d["source"].update(tracked_clean=False))
        self.assert_red("inputs")

    def test_legacy_green_aggregate_cannot_hide_missing_or_duplicate_empty_binary(self):
        self.change("legacy/results.json", lambda d: d["evidence"]["observed_suites"].pop(3))
        self.assert_red("legacy")

    def test_legacy_case_failure_or_ignore_drift_is_rejected_despite_green_summary(self):
        self.change("legacy/results.json", lambda d: d["evidence"]["observed_suites"][0]["outcomes"][0].update(status="FAILED"))
        self.assert_red("legacy")
        self.change("warm-01/shard-0/receipt.json", lambda d: d["binaries"][0]["outcomes"][1].update(status="ok"))
        self.assert_red("warm-01")

    def test_legacy_native_footer_or_parser_completeness_must_match_exact_outcomes(self):
        self.change("legacy/results.json", lambda d: d["evidence"]["observed_suites"][0]["result"].update(passed=999))
        self.assert_red("legacy")
        self.change("warm-02/shard-1/receipt.json", lambda d: d["binaries"][0]["evidence"].update(qualification_complete=False))
        self.assert_red("warm-02")

    def test_missing_receipt_or_binary_hash_drift_is_rejected(self):
        (self.root / "warm-01/shard-2/receipt.json").unlink()
        self.assert_red("warm-01")
        self.change("warm-02/shard-1/receipt.json", lambda d: d["binaries"][0].update(sha256="d" * 64))
        self.assert_red("warm-02")

    def test_rehashed_self_consistent_truncated_plan_cannot_redefine_original_universe(self):
        label = "warm-01"
        plan = copy.deepcopy(self.plan)
        plan["shards"][2]["binaries"].pop()
        self.put(label + "/plan.json", plan)
        for i in range(3):
            self.change(label + f"/shard-{i}/receipt.json", lambda d: d.update(plan_sha256=digest(plan)))
        self.assert_red(label)

    def test_reported_parallel_counts_cannot_replace_original_receipts(self):
        self.change("warm-01/results.json", lambda d: d["native"]["counts"].update(passed=1000))
        self.assert_red("warm-01")

    def test_safe_top_level_cannot_hide_unsafe_original_process(self):
        self.change("warm-01/shard-1/receipt.json", lambda d: d["binaries"][0]["process"].update(cleanup_complete=False))
        self.assert_red("warm-01")
        self.change("legacy/results.json", lambda d: d["process"].update(returncode=101))
        self.assert_red("legacy")

    def test_missing_false_or_nonboolean_release_qualification_is_rejected(self):
        for label, value in [("warm-01", False), ("warm-02", 1)]:
            self.change(label + "/results.json", lambda d: d.update(safe_to_release=value))
            self.assert_red(label)
        self.change("warm-03/results.json", lambda d: d.pop("safe_to_release"))
        self.assert_red("warm-03")

    def test_retry_or_truncated_shard_never_becomes_complete(self):
        self.change("warm-01/shard-0/receipt.json", lambda d: d.update(retries=1))
        self.assert_red("warm-01")
        self.change("warm-02/shard-0/receipt.json", lambda d: d.update(truncated=True))
        self.assert_red("warm-02")

    def test_missing_or_failed_actual_serial_body_is_rejected(self):
        self.change("legacy/results.json", lambda d: d["serial"].pop())
        self.assert_red("legacy")
        self.change("warm-01/results.json", lambda d: d["serial"][0]["evidence"]["observed_suites"][0]["outcomes"][0].update(status="ignored"))
        self.assert_red("warm-01")

    def test_serial_body_requires_original_case_and_explicit_ignored_selection(self):
        self.change("warm-01/results.json", lambda d: d["serial"][1].update(evidence=copy.deepcopy(d["serial"][0]["evidence"])))
        self.assert_red("warm-01")
        self.change("warm-02/results.json", lambda d: d["serial"][0]["process"].update(argv=[]))
        self.assert_red("warm-02")

    def test_doctest_target_union_and_actual_body_are_required(self):
        self.change("warm-01/results.json", lambda d: d["doctests"]["suites"].pop())
        self.assert_red("warm-01")
        self.change("warm-02/results.json", lambda d: d["doctests"]["suites"][0].update(
            declared_tests=0, outcomes=[], result=dict(passed=0, failed=0, ignored=0, measured=0, filtered_out=0, reported_s=0)))
        self.assert_red("warm-02")

    def test_passing_different_doctest_case_is_not_the_same_runtime_universe(self):
        self.change("warm-03/results.json", lambda d: d["doctests"]["suites"][0]["outcomes"][0].update(name="different actual doc case"))
        result = self.assert_red("warm-03")
        self.assertNotIn("timings", result)

    def test_matching_all_zero_doctests_across_every_arm_cannot_qualify(self):
        def empty(row):
            row.update(declared_tests=0, outcomes=[], result=dict(passed=0, failed=0, ignored=0,
                       measured=0, filtered_out=0, reported_s=0))
        for label, mode in REQUIRED.items():
            def change(report):
                if mode == "legacy":
                    for row in report["evidence"]["observed_suites"]:
                        if row["binary"].startswith("doctest:"):
                            empty(row)
                else:
                    for row in report["doctests"]["suites"]:
                        empty(row)
            self.change(label + "/results.json", change)
        result = compare(self.root)
        self.assertFalse(result["qualification_complete_for_runtime"])
        self.assertEqual({row["run"] for row in result["errors"]}, set(REQUIRED))
        self.assertTrue(all("no actual doctest body" in row["error"] for row in result["errors"]))

    def test_present_failed_nextest_is_retained_and_never_silently_falls_back(self):
        self.put("nextest/results.json", dict(mode="nextest", source=self.inventory["source"], success=False, errors=["nextest exit 101"]))
        result = self.assert_red("nextest")
        self.assertNotIn("optional_nextest", result)
        self.assertEqual(result["runs"]["nextest"]["reported_errors"], ["nextest exit 101"])

    def test_present_incomplete_nextest_directory_cannot_be_treated_as_absent(self):
        (self.root / "nextest").mkdir()
        self.assert_red("nextest")

    def test_nonfinite_duplicate_key_and_boolean_timings_are_rejected(self):
        self.change("warm-01/results.json", lambda d: d.update(seconds=True))
        self.assert_red("warm-01")
        (self.root / "warm-02/results.json").write_text('{"success":true,"success":false}')
        self.assert_red("warm-02")
        (self.root / "warm-03/results.json").write_text('{"seconds":NaN}')
        self.assert_red("warm-03")

    def test_cli_writes_honest_failure_and_never_overwrites_source_evidence(self):
        (self.root / "warm-03/results.json").unlink()
        output = self.root / "final.json"
        self.assertEqual(main([str(self.root), "--output", str(output)]), 1)
        self.assertFalse(json.loads(output.read_text())["qualification_complete_for_runtime"])
        protected = self.root / "legacy/results.json"
        old = protected.read_bytes()
        with self.assertRaises(ValueError):
            main([str(self.root), "--output", str(protected)])
        self.assertEqual(protected.read_bytes(), old)


if __name__ == "__main__":
    unittest.main()
