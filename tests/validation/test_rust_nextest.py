"""Pinned native-shaped list/JUnit controls; no Cargo invocation."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

from validation.rust_nextest import NextestError, document_sha256, validate_list, validate_run


CONFIG = '''[profile.m3]
retries = 0
fail-fast = false
test-threads = 4
leak-timeout = { period = "500ms", result = "fail" }
[profile.m3.junit]
path = "m3-junit.xml"
'''


class NextestCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.inventory = dict(discovery_complete=True, errors=[], binaries=[],
                              source=dict(commit="a" * 40, tree="b" * 40))
        self.listing = {"rust-build-meta": {"target-directory": str(self.root)},
                        "test-count": 0, "rust-suites": {}}
        self.add_binary("alpha", "alpha", "lib", {"shared::case": False, "manual_child": True})
        self.add_binary("alpha", "integration", "test", {"shared::case": False})
        self.add_binary("empty", "empty", "lib", {})
        self.xml = self.report_xml()
        self.process = dict(returncode=0, success=True, timed_out=False, cleanup_complete=True,
                            nextest_version="0.9.140", report_format="nextest-junit",
                            runner_stderr=" Nextest run ID 00000000-0000-0000-0000-000000000001 with nextest profile: m3\n",
                            run_ignored="default", profile="m3",
                            configuration_text=CONFIG)

    def add_binary(self, package, target, kind, cases):
        identity = package if kind == "lib" else package + "::" + target
        path = self.root / (target + "-aabbccdd")
        path.write_bytes((identity + " compiled payload").encode())
        package_id = f"path+file:///source/{package}#0.1.0"
        self.inventory["binaries"].append(dict(
            executable=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
            package_id=package_id, target=dict(name=target, kind=[kind], src_path="/source/src.rs"),
            features=["fixtures"], profile=dict(test=True, opt_level="0"), listing_complete=True,
            cases=[dict(name=name, ignored=ignored) for name, ignored in cases.items()]))
        self.listing["test-count"] += len(cases)
        self.listing["rust-suites"][identity] = {
            "package-name": package, "binary-id": identity, "binary-name": target,
            "package-id": package_id, "kind": kind, "binary-path": str(path),
            "build-platform": "target", "cwd": "/source", "status": "listed",
            "testcases": {name: {"kind": "test", "ignored": ignored,
                                "filter-match": {"status": "mismatch", "reason": "ignored"}
                                if ignored else {"status": "matches"}}
                          for name, ignored in cases.items()}}

    def report_xml(self):
        root = ET.Element("testsuites", name="nextest-run", tests="2", failures="0", errors="0",
                          uuid="00000000-0000-0000-0000-000000000001", time="0.02")
        for identity, suite in self.listing["rust-suites"].items():
            active = [name for name, case in suite["testcases"].items() if not case["ignored"]]
            if not active:
                continue
            node = ET.SubElement(root, "testsuite", name=identity, tests=str(len(active)),
                                 disabled="0", failures="0", errors="0")
            for name in active:
                case = ET.SubElement(node, "testcase", name=name, classname=identity, time="0.01")
                ET.SubElement(case, "system-out").text = "running 1 test\ntest " + name + " ... ok\n"
                ET.SubElement(case, "system-err")
        return root

    def run_report(self, xml=None, process=None):
        xml = self.xml if xml is None else xml
        process = copy.deepcopy(self.process if process is None else process)
        text = ET.tostring(xml, encoding="unicode") if isinstance(xml, ET.Element) else xml
        process.setdefault("inventory_sha256", document_sha256(self.inventory))
        process.setdefault("listing_sha256", document_sha256(self.listing))
        process.setdefault("junit_sha256", hashlib.sha256(text.encode()).hexdigest())
        process.setdefault("runner_stderr_sha256", hashlib.sha256(process["runner_stderr"].encode()).hexdigest())
        process.setdefault("configuration_sha256", hashlib.sha256(process["configuration_text"].encode()).hexdigest())
        return validate_run(self.inventory, self.listing, text, process)

    def test_exact_binary_qualified_outcomes_and_separate_obligations(self):
        report = self.run_report()
        self.assertTrue(report["native_passed"])
        self.assertEqual(report["active_passed"], 2)
        self.assertEqual(report["ignored_unexecuted"], 1)
        self.assertEqual(report["empty_binaries"], ["empty"])
        self.assertEqual(report["scope"]["doctests"], "separate-required-cell")
        self.assertEqual(len(report["outcomes"]), 2)
        self.assertEqual(report["ignored"][0]["name"], "manual_child")

    def test_real_binary_hash_binds_feature_profile_payload(self):
        Path(self.inventory["binaries"][0]["executable"]).write_bytes(b"different-feature artifact")
        with self.assertRaisesRegex(NextestError, "payload changed"):
            validate_list(self.inventory, self.listing)

    def test_wrong_binary_path_package_kind_or_target_refuses(self):
        for field, value in [("binary-path", "/wrong/alpha"), ("package-id", "other#0.1.0"),
                             ("kind", "test"), ("binary-name", "wrong"), ("binary-id", "wrong")]:
            saved = copy.deepcopy(self.listing)
            with self.subTest(field=field):
                self.listing["rust-suites"]["alpha"][field] = value
                with self.assertRaises(NextestError):
                    validate_list(self.inventory, self.listing)
                self.listing = saved

    def test_missing_added_duplicate_binary_and_case_refuse(self):
        changes = [lambda suites: suites.pop("empty"),
                   lambda suites: suites.update(imposter=copy.deepcopy(suites["alpha"])),
                   lambda suites: suites["alpha"]["testcases"].pop("shared::case"),
                   lambda suites: suites["alpha"]["testcases"].update(unexpected={"ignored": False})]
        for change in changes:
            saved = copy.deepcopy(self.listing)
            change(self.listing["rust-suites"])
            with self.assertRaises(NextestError):
                validate_list(self.inventory, self.listing)
            self.listing = saved

    def test_default_partition_and_expression_filters_are_not_discovery(self):
        for reason in ("partition", "default-filter", "expression", "string"):
            self.listing["rust-suites"]["alpha"]["testcases"]["shared::case"]["filter-match"] = {
                "status": "mismatch", "reason": reason}
            with self.assertRaisesRegex(NextestError, "filtering"):
                validate_list(self.inventory, self.listing)

    def test_changed_ignore_skipped_binary_and_wrong_count_refuse(self):
        for change in [lambda: self.listing["rust-suites"]["alpha"]["testcases"]["manual_child"].update(ignored=False),
                       lambda: self.listing["rust-suites"]["empty"].update(status="skipped"),
                       lambda: self.listing.update({"test-count": 0})]:
            saved = copy.deepcopy(self.listing)
            change()
            with self.assertRaises(NextestError):
                validate_list(self.inventory, self.listing)
            self.listing = saved

    def test_error_incomplete_and_all_empty_or_ignored_discovery_refuse(self):
        for field, value in [("discovery_complete", False), ("errors", ["listing failed"])]:
            old = self.inventory.get(field)
            self.inventory[field] = value
            with self.assertRaises(NextestError):
                validate_list(self.inventory, self.listing)
            self.inventory[field] = old
        self.inventory["binaries"] = [self.inventory["binaries"][-1]]
        self.listing["rust-suites"] = {"empty": self.listing["rust-suites"]["empty"]}
        self.listing["test-count"] = 0
        with self.assertRaisesRegex(NextestError, "Zero active"):
            validate_list(self.inventory, self.listing)

    def test_missing_duplicate_unknown_retried_and_wrong_binary_results_refuse(self):
        for mutation in ("missing", "duplicate", "unknown", "retry", "wrong-owner"):
            xml = copy.deepcopy(self.xml)
            case = xml[0][0]
            if mutation == "missing":
                xml[0].remove(case)
            elif mutation == "duplicate":
                xml[0].append(copy.deepcopy(case))
            elif mutation == "unknown":
                case.set("name", "unexpected")
            elif mutation == "retry":
                case.set("name", "shared::case#2")
            else:
                case.set("classname", "another-binary")
            with self.assertRaises(NextestError):
                self.run_report(xml)

    def test_failed_leaky_timed_out_unknown_or_skipped_active_results_refuse(self):
        for tag in ("failure", "error", "skipped", "flakyFailure", "rerunFailure", "leak", "unknown"):
            xml = copy.deepcopy(self.xml)
            ET.SubElement(xml[0][0], tag)
            with self.assertRaises(NextestError):
                self.run_report(xml)

    def test_ignored_body_cannot_be_substituted_for_missing_active_body(self):
        xml = copy.deepcopy(self.xml)
        xml[0][0].set("name", "manual_child")
        with self.assertRaises(NextestError):
            self.run_report(xml)

    def test_child_output_is_opaque_and_cannot_supply_a_missing_parent_result(self):
        xml = copy.deepcopy(self.xml)
        xml[0][0][0].text = '<testcase name="fake-child" classname="wrong"><failure/></testcase>'
        self.assertTrue(self.run_report(xml)["native_passed"])
        xml[0].remove(xml[0][0])
        with self.assertRaises(NextestError):
            self.run_report(xml)
        xml = copy.deepcopy(self.xml)
        ET.SubElement(xml[0][0][0], "testcase")
        with self.assertRaises(NextestError):
            self.run_report(xml)

    def test_missing_duplicate_wrong_suite_and_footer_counts_refuse(self):
        for mutation in ("missing", "duplicate", "count", "wrong-binary", "disabled", "zero"):
            xml = copy.deepcopy(self.xml)
            if mutation == "missing":
                xml.remove(xml[1])
            elif mutation == "duplicate":
                xml.append(copy.deepcopy(xml[0]))
            elif mutation == "count":
                xml[0].set("tests", "0")
            elif mutation == "wrong-binary":
                xml[0].set("name", "unknown")
            elif mutation == "disabled":
                xml[0].set("disabled", "1")
            else:
                xml.clear()
            with self.assertRaises(NextestError):
                self.run_report(xml)

    def test_stale_report_different_run_or_profile_refuses_even_after_rehash(self):
        xml = copy.deepcopy(self.xml)
        xml.set("uuid", "00000000-0000-0000-0000-000000000002")
        with self.assertRaisesRegex(NextestError, "stale"):
            self.run_report(xml)
        for stderr in ("", self.process["runner_stderr"] * 2,
                       self.process["runner_stderr"].replace("m3", "other-profile")):
            with self.assertRaises(NextestError):
                self.run_report(process=dict(self.process, runner_stderr=stderr))

    def test_broken_plus_json_has_no_qualified_fallback(self):
        with self.assertRaisesRegex(NextestError, "XML"):
            self.run_report('{"type":"suite","event":"ok","passed":2}')
        with self.assertRaises(NextestError):
            self.run_report(process=dict(self.process, message_format="libtest-json-plus"))

    def test_feature_profile_context_cannot_change_after_execution_binding(self):
        digest = document_sha256(self.inventory)
        self.inventory["binaries"][0]["features"] = ["different-feature"]
        with self.assertRaisesRegex(NextestError, "inventory_sha256"):
            self.run_report(process=dict(self.process, inventory_sha256=digest))

    def test_nonzero_timeout_cleanup_unknown_version_and_ignored_execution_refuse(self):
        for field, value in [("returncode", 1), ("returncode", False), ("success", False),
                             ("timed_out", True), ("cleanup_complete", False), ("cleanup_error", "unobservable"),
                             ("nextest_version", "0.9.141"), ("report_format", "libtest-json-plus"),
                             ("run_ignored", "all")]:
            process = dict(self.process, **{field: value})
            with self.assertRaises(NextestError):
                self.run_report(process=process)

    def test_input_configuration_and_event_binding_drift_refuse(self):
        for key in ("inventory_sha256", "listing_sha256", "junit_sha256", "runner_stderr_sha256", "configuration_sha256"):
            with self.assertRaises(NextestError):
                self.run_report(process=dict(self.process, **{key: "0" * 64}))

    def test_retry_leak_pass_timeout_pass_and_default_filter_configs_refuse(self):
        configurations = [CONFIG.replace("retries = 0", "retries = 1"),
                          CONFIG.replace('result = "fail"', 'result = "pass"'),
                          CONFIG.replace("[profile.m3.junit]", 'slow-timeout = { period="60s", terminate-after=1, result="pass" }\n[profile.m3.junit]'),
                          CONFIG.replace("[profile.m3.junit]", 'default-filter = "test(only_one)"\n[profile.m3.junit]'),
                          CONFIG + '[[profile.m3.overrides]]\nfilter="all()"\nretries=2\n']
        for config in configurations:
            with self.assertRaises(NextestError):
                self.run_report(process=dict(self.process, configuration_text=config))

    def test_duplicate_json_keys_nonfinite_and_nonobject_payloads_refuse(self):
        for text in ('{"rust-suites":{},"rust-suites":{}}', '[]', '{"test-count":NaN}'):
            with self.assertRaises(NextestError):
                validate_list(self.inventory, text)


if __name__ == "__main__":
    unittest.main()
