"""Accounting regressions only; no Rust builds or native suite execution."""
import copy
import unittest

from validation.rust_parallel_manifest import ManifestError, build_plan, digest, validate_plan, validate_receipts


def inventory():
    def binary(name, cases):
        return dict(executable=f"/work/target/debug/deps/{name}-abcdef", sha256="c" * 64,
                    package_id=f"path+file:///work/{name}#0.1.0",
                    target=dict(name=name, kind=["bin" if name == "plurxd" else "lib"],
                                src_path=f"/work/{name}/src/lib.rs", doctest=True),
                    features=["hiqlite-store"], profile=dict(test=True, opt_level="0", debuginfo=0,
                                                           debug_assertions=True, overflow_checks=True),
                    listing_complete=True, cases=[dict(name=case, ignored=ignored) for case, ignored in cases])
    return dict(discovery_complete=True, errors=[], source=dict(commit="a" * 40, tree="b" * 40, tracked_clean=True),
                binaries=[binary("plurxd", [("same_name", False), ("serial", True)]),
                          binary("plurx_core", [("same_name", False), ("core", False)]),
                          binary("store_contract", [("store", False)]), binary("empty_target", [])])


def receipts(plan):
    result = []
    for shard in plan["shards"]:
        binaries = []
        for binary in shard["binaries"]:
            row = {key: copy.deepcopy(value) for key, value in binary.items() if key != "cases"}
            row.update(outcomes=[dict(name=case["name"], status="ignored" if case["ignored"] else "ok")
                                 for case in binary["cases"]], returncode=0, success=True,
                       cleanup_complete=True, timed_out=False)
            binaries.append(row)
        result.append(dict(plan_sha256=digest(plan), source=copy.deepcopy(plan["source"]),
                           shard_index=shard["index"], binaries=binaries))
    return result


class ParallelManifestCase(unittest.TestCase):
    def setUp(self):
        self.inventory = inventory()
        self.plan = build_plan(self.inventory)
        self.receipts = receipts(self.plan)

    def refuses_inventory(self, change):
        value = copy.deepcopy(self.inventory)
        change(value)
        with self.assertRaises(ManifestError):
            build_plan(value)

    def refuses_receipt(self, change):
        value = copy.deepcopy(self.receipts)
        change(value)
        with self.assertRaises(ManifestError):
            validate_receipts(self.plan, value)

    def test_plan_is_deterministic_and_does_not_mutate_or_alias_input(self):
        before = copy.deepcopy(self.inventory)
        shuffled = copy.deepcopy(before)
        shuffled["binaries"].reverse()
        for row in shuffled["binaries"]:
            row["cases"].reverse()
        alternate = build_plan(shuffled)
        self.assertEqual(self.plan["shards"], alternate["shards"])
        self.assertNotEqual(self.plan["inventory_sha256"], alternate["inventory_sha256"])
        self.assertEqual(self.inventory, before)
        self.plan["shards"][0]["binaries"][0]["cases"][0]["name"] = "changed"
        self.assertEqual(self.inventory, before)

    def test_default_lanes_preserve_empty_binary_and_case_names_in_other_binaries(self):
        self.assertEqual([[row["target"]["name"] for row in shard["binaries"]] for shard in self.plan["shards"]],
                         [["plurxd"], ["plurx_core"], ["empty_target", "store_contract"]])
        self.assertIs(validate_plan(self.plan), self.plan)
        result = validate_receipts(self.plan, list(reversed(self.receipts)))
        self.assertTrue(result["success"])
        self.assertEqual(result["binary_count"], 4)
        self.assertEqual(result["counts"], dict(total=5, passed=4, failed=0, ignored=1))

    def test_digest_is_key_order_independent_but_type_sensitive(self):
        self.assertEqual(digest(dict(a=1, b=True)), digest(dict(b=True, a=1)))
        self.assertNotEqual(digest(dict(a=1)), digest(dict(a=True)))
        for value in [float("nan"), float("inf"), object(), {1: "non-JSON key"}, ("tuple",)]:
            with self.subTest(value=value), self.assertRaises(ManifestError):
                digest(value)

    def test_nondefault_worker_count_is_deterministic_and_requires_empty_lane_receipt(self):
        plan = build_plan(self.inventory, workers=5, test_threads=1)
        self.assertEqual(plan, build_plan(self.inventory, workers=5, test_threads=1))
        self.assertTrue(validate_receipts(plan, receipts(plan))["success"])
        with self.assertRaises(ManifestError):
            validate_receipts(plan, receipts(plan)[:-1])

    def test_incomplete_inventory_or_listing_is_rejected(self):
        for change in [lambda d: d.update(discovery_complete=False), lambda d: d.update(discovery_complete=1),
                       lambda d: d.update(errors=["compile failed"]),
                       lambda d: d["binaries"][0].update(listing_complete=False),
                       lambda d: d["binaries"][0].update(error="listing failed")]:
            with self.subTest(change=change):
                self.refuses_inventory(change)

    def test_dirty_missing_or_invalid_source_is_rejected(self):
        for field, value in [("tracked_clean", False), ("tracked_clean", 1), ("commit", "a" * 39),
                             ("tree", "B" * 40), ("tree", None)]:
            with self.subTest(field=field, value=value):
                self.refuses_inventory(lambda d: d["source"].update({field: value}))
        self.refuses_inventory(lambda d: d.pop("source"))

    def test_empty_universe_and_invalid_worker_configuration_are_rejected(self):
        self.refuses_inventory(lambda d: d.update(binaries=[]))
        self.refuses_inventory(lambda d: [row.update(cases=[]) for row in d["binaries"]])
        for kwargs in [dict(workers=0), dict(workers=True), dict(workers=1.0),
                       dict(test_threads=0), dict(test_threads=True)]:
            with self.subTest(kwargs=kwargs), self.assertRaises(ManifestError):
                build_plan(self.inventory, **kwargs)

    def test_duplicate_paths_basenames_and_logical_binary_are_rejected(self):
        self.refuses_inventory(lambda d: d["binaries"].append(copy.deepcopy(d["binaries"][0])))
        self.refuses_inventory(lambda d: d["binaries"][1].update(executable="/elsewhere/plurxd-abcdef"))
        def same_logical(d):
            row = copy.deepcopy(d["binaries"][0])
            row["executable"] = "/work/target/alternate-name"
            d["binaries"].append(row)
        self.refuses_inventory(same_logical)

    def test_noncanonical_or_unsafe_paths_are_rejected(self):
        for path in ["relative/bin", "/work/../bin", "/work/./bin", "/work//bin", "/work/bin/", "//work/bin", "/work/bi\x00n"]:
            with self.subTest(path=path):
                self.refuses_inventory(lambda d: d["binaries"][0].update(executable=path))

    def test_malformed_binary_metadata_and_cases_are_rejected(self):
        for field, value in [("sha256", "f" * 63), ("package_id", ""), ("target", []),
                             ("features", "hiqlite-store"), ("features", ["same", "same"]),
                             ("profile", []), ("profile", dict(test=1)), ("cases", {}),
                             ("cases", [dict(name="case", ignored=0)]),
                             ("cases", [dict(name="case", ignored=False)] * 2)]:
            with self.subTest(field=field, value=value):
                self.refuses_inventory(lambda d: d["binaries"][0].update({field: value}))

    def test_mutated_or_reassigned_plan_is_rejected(self):
        for change in [lambda p: p.update(schema=True), lambda p: p.update(inventory_sha256="bad"),
                       lambda p: p["shards"][0].update(index=1),
                       lambda p: p["shards"][0]["binaries"].append(p["shards"][1]["binaries"].pop())]:
            value = copy.deepcopy(self.plan)
            change(value)
            with self.subTest(change=change), self.assertRaises(ManifestError):
                validate_plan(value)

    def test_missing_or_malformed_plan_binary_is_a_typed_refusal(self):
        for change in [lambda p: p["shards"][0]["binaries"][0].pop("cases"),
                       lambda p: p["shards"][0].update(binaries=[None]),
                       lambda p: p["shards"][0]["binaries"][0]["target"].update(doctest=1),
                       lambda p: p["shards"][0]["binaries"][0]["profile"].update(debuginfo=False)]:
            value = copy.deepcopy(self.plan)
            change(value)
            with self.subTest(change=change), self.assertRaises(ManifestError):
                validate_plan(value)

    def test_missing_extra_or_duplicate_shard_receipt_is_rejected(self):
        for values in [self.receipts[:-1], self.receipts + [self.receipts[0]],
                       [self.receipts[0], self.receipts[0], self.receipts[2]]]:
            with self.subTest(values=values), self.assertRaises(ManifestError):
                validate_receipts(self.plan, values)
        self.refuses_receipt(lambda r: r[0].update(shard_index=True))

    def test_source_or_plan_drift_is_rejected(self):
        self.refuses_receipt(lambda r: r[0].update(plan_sha256="d" * 64))
        self.refuses_receipt(lambda r: r[0]["source"].update(tree="d" * 40))
        self.refuses_receipt(lambda r: r[0]["source"].update(tracked_clean=1))

    def test_binary_drift_missing_duplicate_or_wrong_lane_is_rejected(self):
        for field, value in [("sha256", "d" * 64), ("features", []), ("package_id", "other"),
                             ("profile", dict(test=True)), ("target", dict(name="other", kind=["bin"]))]:
            with self.subTest(field=field):
                self.refuses_receipt(lambda r: r[0]["binaries"][0].update({field: value}))
        self.refuses_receipt(lambda r: r[0].update(binaries=[]))
        self.refuses_receipt(lambda r: r[0]["binaries"].append(copy.deepcopy(r[0]["binaries"][0])))
        self.refuses_receipt(lambda r: r[0].update(binaries=copy.deepcopy(r[1]["binaries"])))

    def test_binding_comparison_does_not_coerce_boolean_and_integer_metadata(self):
        self.refuses_receipt(lambda r: r[0]["binaries"][0]["target"].update(doctest=1))
        self.refuses_receipt(lambda r: r[0]["binaries"][0]["profile"].update(debuginfo=False))

    def test_missing_duplicate_unexpected_or_zero_match_outcome_is_rejected(self):
        for outcomes in [[], [dict(name="same_name", status="ok")] * 2,
                         [dict(name="unknown", status="ok"), dict(name="serial", status="ignored")]]:
            with self.subTest(outcomes=outcomes):
                self.refuses_receipt(lambda r: r[0]["binaries"][0].update(outcomes=outcomes))

    def test_same_name_in_other_binary_cannot_supply_missing_case(self):
        self.refuses_receipt(lambda r: r[1]["binaries"][0].update(outcomes=[dict(name="same_name", status="ok")] * 2))

    def test_failed_skipped_or_changed_ignore_status_is_rejected(self):
        for case, status in [("same_name", "FAILED"), ("same_name", "ignored"),
                             ("same_name", "skipped"), ("serial", "ok")]:
            def change(rows):
                for outcome in rows[0]["binaries"][0]["outcomes"]:
                    if outcome["name"] == case:
                        outcome["status"] = status
            with self.subTest(case=case, status=status):
                self.refuses_receipt(change)

    def test_nonzero_exit_timeout_or_unqualified_cleanup_never_becomes_success(self):
        for field, value in [("returncode", 101), ("returncode", False), ("success", False),
                             ("success", 1), ("cleanup_complete", False), ("timed_out", True)]:
            with self.subTest(field=field, value=value):
                self.refuses_receipt(lambda r: r[0]["binaries"][0].update({field: value}))

    def test_retries_truncation_or_explicit_errors_are_rejected(self):
        for field, value in [("attempt", 2), ("attempts", 2), ("retries", 1), ("retry_count", 1),
                             ("truncated", True), ("skipped", True), ("errors", ["incomplete"]),
                             ("blocked_cells", ["unrun"])]:
            with self.subTest(field=field, value=value):
                self.refuses_receipt(lambda r: r[0].update({field: value}))
                self.refuses_receipt(lambda r: r[0]["binaries"][0].update({field: value}))

    def test_optional_outer_and_parser_failure_flags_are_not_discarded(self):
        for field, value in [("success", False), ("cleanup_complete", False), ("timed_out", True),
                             ("returncode", 101), ("qualification_complete", False),
                             ("native_passed", False), ("leaked_process_group", True),
                             ("cleanup_observation_unknown", True)]:
            with self.subTest(field=field):
                self.refuses_receipt(lambda r: r[0].update({field: value}))
                self.refuses_receipt(lambda r: r[0]["binaries"][0].update({field: value}))


if __name__ == "__main__":
    unittest.main()
