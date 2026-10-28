"""Fail-closed Kani receipt parsing and mutation tests; no verifier is invoked."""
from __future__ import annotations

import copy
import hashlib
import json
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


def real_positive() -> tuple[str, dict]:
    directory = ROOT / "tests/fixtures/worker-control"
    log_path = directory / "positive-t2-kani-0.68.0.txt"
    provenance = json.loads((directory / "positive-t2-kani-0.68.0.provenance.json").read_text())
    if hashlib.sha256(log_path.read_bytes()).hexdigest() != provenance["fixture_sha256"]:
        raise AssertionError("real pinned fixture digest differs from its provenance")
    return log_path.read_text(), provenance


class WorkerControlPinnedPositiveTests(unittest.TestCase):
    def test_real_pinned_positive_with_all_literal_markers_succeeds(self) -> None:
        output, provenance = real_positive()
        result = runner.verdict(output, 0, provenance["harness"], None,
                                fmt_stdlib_sha256=provenance["reparse_tools"]["fmt_stdlib_sha256"])
        facts = ("harness", "outcome", "exit_code", "property_count", "failures", "status_counts", "specification_assertions")
        self.assertEqual({key: result[key] for key in facts}, {key: provenance["parse_result"][key] for key in facts})
        self.assertEqual(result["status_counts"], {"SUCCESS": 246, "UNREACHABLE": 1, "FAILURE": 0})
        self.assertEqual(result["specification_assertions"], 5)
        self.assertFalse(provenance["acceptance"])
        self.assertEqual(provenance["raw_log_sha256"], "49de1f27032977f3e0bfad1abc38287c9d81057eafe50d4574e1c0e5beabe84e")

    def test_observed_format_guard_requires_exact_tuple_location_and_source_pin(self) -> None:
        output, provenance = real_positive()
        for changed, pin in (
            (output, None),
            (output, "0" * 64),
            (output.replace(runner.FMT_GUARD_NAME, runner.FMT_GUARD_NAME + "_other"), runner.FMT_STDLIB_SHA256),
            (output.replace(runner.FMT_GUARD_DESCRIPTION, "attempt to shift right with overflow"), runner.FMT_STDLIB_SHA256),
            (output.replace(runner.FMT_GUARD_LOCATION, runner.FMT_GUARD_LOCATION.replace(":820:38", ":821:38")), runner.FMT_STDLIB_SHA256),
            (output.replace("(1 unreachable)", "(2 unreachable)"), runner.FMT_STDLIB_SHA256),
            (output.replace("- Status: UNREACHABLE", "- Status: FAILURE"), runner.FMT_STDLIB_SHA256),
            (output.replace("- Status: UNREACHABLE", "- Status: UNDETERMINED"), runner.FMT_STDLIB_SHA256),
        ):
            with self.subTest(pin=pin, changed=changed[-300:]), self.assertRaises(RuntimeError):
                runner.verdict(changed, 0, provenance["harness"], None, fmt_stdlib_sha256=pin)



def real_linux_t6() -> tuple[str, dict]:
    directory = ROOT / "tests/fixtures/worker-control"
    path = directory / "positive-t6-linux-kani-0.68.0.txt"
    provenance = json.loads((directory / "positive-t6-linux-kani-0.68.0.provenance.json").read_text())
    if hashlib.sha256(path.read_bytes()).hexdigest() != provenance["fixture_sha256"]:
        raise AssertionError("real Linux T6 fixture digest differs")
    return path.read_text(), provenance


class WorkerControlLinuxT6Tests(unittest.TestCase):
    def test_real_t6_requires_the_separate_exact_slice_source_attestation(self) -> None:
        raw, provenance = real_linux_t6()
        result = runner.verdict(raw, 0, provenance["harness"], None,
            fmt_stdlib_sha256=provenance["tools"]["fmt_stdlib_sha256"],
            parser_attestation=provenance["parser_stdlib_attestation"])
        self.assertEqual(result["property_count"], 417)
        self.assertEqual(result["status_counts"], {"SUCCESS": 415, "UNREACHABLE": 2, "FAILURE": 0})
        self.assertEqual(result["specification_assertions"], 8)
        for altered, attestation in (
            (raw, None),
            (raw, {"slice_cmp_sha256": "0" * 64}),
            (raw.replace(runner.SLICE_GUARD_NAME, runner.SLICE_GUARD_NAME + "_other"), provenance["parser_stdlib_attestation"]),
            (raw.replace(runner.SLICE_GUARD_DESCRIPTION, "attempt to subtract with overflow"), provenance["parser_stdlib_attestation"]),
            (raw.replace(runner.SLICE_GUARD_LOCATION, runner.SLICE_GUARD_LOCATION.replace(":136:13", ":137:13")), provenance["parser_stdlib_attestation"]),
        ):
            with self.subTest(attestation=attestation), self.assertRaises(RuntimeError):
                runner.verdict(altered, 0, provenance["harness"], None,
                    fmt_stdlib_sha256=provenance["tools"]["fmt_stdlib_sha256"], parser_attestation=attestation)

    def test_theorem_class_and_reachability_policy_remain_strict(self) -> None:
        raw, provenance = real_linux_t6()
        labelled = raw.replace(provenance["harness"] + ".assertion.7", provenance["harness"] + ".unreachable.99")
        vacuous = raw.replace(
            provenance["harness"] + '.assertion.7\n\t - Status: SUCCESS',
            provenance["harness"] + '.assertion.7\n\t - Status: UNREACHABLE').replace('(2 unreachable)', '(3 unreachable)')
        for altered in (labelled, vacuous):
            with self.subTest(altered=altered[-300:]), self.assertRaises(RuntimeError):
                runner.verdict(altered, 0, provenance["harness"], None,
                    fmt_stdlib_sha256=provenance["tools"]["fmt_stdlib_sha256"],
                    parser_attestation=provenance["parser_stdlib_attestation"])

    def test_exact_slice_guard_never_allows_undetermined_or_failure(self) -> None:
        raw, provenance = real_linux_t6()
        prefix = runner.SLICE_GUARD_NAME + '\n\t - Status: UNREACHABLE'
        unknown = raw.replace(prefix, runner.SLICE_GUARD_NAME + '\n\t - Status: UNDETERMINED').replace('(2 unreachable)', '(1 undetermined,1 unreachable)')
        failed = raw.replace(prefix, runner.SLICE_GUARD_NAME + '\n\t - Status: FAILURE').replace('0 of 417 failed (2 unreachable)', '1 of 417 failed (1 unreachable)').replace('VERIFICATION:- SUCCESSFUL', 'VERIFICATION:- FAILED').replace('1 successfully verified harnesses, 0 failures', '0 successfully verified harnesses, 1 failures')
        for altered in (unknown, failed):
            with self.subTest(altered=altered[-300:]), self.assertRaises(RuntimeError):
                runner.verdict(altered, 0, provenance["harness"], None,
                    fmt_stdlib_sha256=provenance["tools"]["fmt_stdlib_sha256"],
                    parser_attestation=provenance["parser_stdlib_attestation"])

    def test_attestation_reads_the_actual_scoped_source_and_rejects_drift(self) -> None:
        env = {"RUSTUP_HOME": "/owned/scoped/rustup", "RUSTUP_TOOLCHAIN": "nightly-2026-08-21-x86_64-unknown-linux-gnu"}
        source = Path(env["RUSTUP_HOME"]) / "toolchains" / env["RUSTUP_TOOLCHAIN"] / "lib/rustlib/src/rust/library/core/src/slice/cmp.rs"
        with mock.patch.object(runner, "digest", return_value=runner.SLICE_STDLIB_SHA256) as hashed:
            self.assertEqual(runner.parser_stdlib_attestation(env)["slice_cmp_sha256"], runner.SLICE_STDLIB_SHA256)
            hashed.assert_called_once_with(source)
        with mock.patch.object(runner, "digest", return_value="wrong"), self.assertRaises(RuntimeError):
            runner.parser_stdlib_attestation(env)
        with mock.patch.object(runner, "digest", side_effect=FileNotFoundError), self.assertRaises(OSError):
            runner.parser_stdlib_attestation(env)

    def test_original_rejected_t6_is_reparsed_without_solver_or_tool_normalization(self) -> None:
        raw, provenance = real_linux_t6()
        base = ROOT / "target/worker-control-runner-fixtures"
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as directory:
            prior, output = Path(directory) / "prior", Path(directory) / "next"
            prior.mkdir(); output.mkdir()
            harness = provenance["harness"]; name = harness.rsplit("::", 1)[1]
            log_path = prior / f"{name}.log"; log_path.write_text(raw)
            source, tools = provenance["source_binding"], provenance["tools"]
            rejected = {"harness": harness, "outcome": "rejected", "error": provenance["original_parser_rejection"]["error"],
                "exit_code": 0, "source": source, "tools": tools, "execution_directory": str(prior),
                "command": ["cargo-kani", "--harness", harness, "--exact", "--output-format", "regular", "--manifest-path", str(prior / "source/Cargo.toml"), "--target-dir", str(prior / "build")],
                "unwinding_checks": True, "default_safety_checks": True, "log_sha256": runner.digest(log_path)}
            runner.write_json(prior / f"{name}.json", rejected)
            with mock.patch.object(runner, "validate_case_sources"), mock.patch.object(runner, "command", side_effect=AssertionError("solver replay forbidden")):
                retained = runner.verify(Path("unused"), {}, output, harness, source, tools,
                    manifest=output / "source/Cargo.toml", prior=prior, parser_attestation=provenance["parser_stdlib_attestation"])
                self.assertEqual(retained["outcome"], "proved")
                self.assertEqual(retained["tools"], tools)
                self.assertEqual(retained["original_parser_rejection"]["error"], rejected["error"])
                self.assertTrue(retained["parser_attestation_bridge"]["solver_tool_identity_unchanged"])
                self.assertNotIn("slice_cmp_sha256", retained["tools"])
                changed_tools = dict(tools, driver_sha256="different-native-binary")
                self.assertIsNone(runner.retain_previous(prior, output, harness, source, changed_tools, None, None,
                    parser_attestation=provenance["parser_stdlib_attestation"]))



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
            output.replace('core::result::unwrap_failed.assertion.1', "core::fmt::Arguments::<'_>::from_str.assertion.1").replace('reachable process-gating event accepted', 'attempt to shift left with overflow'),
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



class WorkerControlRejectedParserReuseTests(unittest.TestCase):
    def test_real_log_is_reused_after_parser_repair_but_bad_attempts_are_not(self) -> None:
        raw, provenance = real_positive()
        harness = provenance["harness"]
        name = harness.rsplit("::", 1)[1]
        base = ROOT / "target/worker-control-runner-fixtures"
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as directory:
            prior, output = Path(directory) / "prior", Path(directory) / "next"
            prior.mkdir(); output.mkdir()
            source = copy.deepcopy(provenance["source_binding"])
            tools = provenance["reparse_tools"]
            def captured_command(args, cwd, env, log_path, **kwargs):
                log_path.write_text(raw)
                return 0, raw
            with mock.patch.object(runner, "validate_case_sources"), mock.patch.object(runner, "command", side_effect=captured_command) as executed:
                with mock.patch.object(runner, "verdict", side_effect=RuntimeError("previous strict parser rejected pinned guard")):
                    with self.assertRaises(RuntimeError):
                        runner.verify(Path("unused"), {}, prior, harness, source, tools,
                                      manifest=prior / "source/Cargo.toml")
                case_path = prior / f"{name}.json"
                rejected = json.loads(case_path.read_text())
                self.assertEqual(rejected["execution_directory"], str(prior))
                self.assertTrue(rejected["unwinding_checks"] and rejected["default_safety_checks"])
                retained = runner.verify(Path("unused"), {}, output, harness, source, tools,
                                         manifest=output / "source/Cargo.toml", prior=prior)
                self.assertEqual(executed.call_count, 1, "compatible raw proof must not execute the solver again")
                self.assertEqual(retained["outcome"], "proved")
                self.assertEqual(retained["original_parser_rejection"]["error"], rejected["error"])
                self.assertNotIn("error", retained)
                self.assertEqual(retained["source"], source)
                self.assertEqual(retained["revalidated_by_parser_sha256"], runner.digest(ROOT / "scripts/check-worker-control"))
                for kind in ("weak command", "timeout", "compiler", "missing harness"):
                    bad = copy.deepcopy(rejected)
                    log_path = prior / f"{name}.log"
                    log_path.write_text(raw)
                    if kind == "weak command": bad["command"].append("--no-unwinding-checks")
                    elif kind == "timeout": bad["exit_code"] = None
                    elif kind == "compiler":
                        bad["exit_code"] = 1
                        log_path.write_text("error: failed to compile proof crate\n")
                    else: log_path.write_text(raw.replace(f"Checking harness {harness}...", "no matching harness"))
                    bad["log_sha256"] = runner.digest(log_path)
                    runner.write_json(case_path, bad)
                    with self.subTest(kind=kind):
                        self.assertIsNone(runner.retain_previous(prior, output, harness, source, tools, None, None))



class ResumeDirectoryCliTests(unittest.TestCase):
    def test_root_empty_and_overlapping_resume_reject_before_tools(self):
        scratch = ROOT / "target/worker-control-resume-path-fixtures"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            base = Path(temporary)
            ancestor = base / "ancestor"
            ancestor.mkdir()
            descendant = base / "output/child"
            descendant.mkdir(parents=True)
            for prior, output in (("", base / "empty-output"),
                                  (str(ROOT), base / "root-output"),
                                  (str(ancestor), ancestor / "output"),
                                  (str(descendant), base / "output")):
                argv = ["check-worker-control", "full", "--cache-dir", str(base / "cache"),
                        "--output-dir", str(output), "--resume-from", prior]
                with self.subTest(prior=prior), mock.patch("sys.argv", argv), \
                        mock.patch.object(runner, "toolchain") as tools, \
                        mock.patch.object(runner, "source_identity") as identity:
                    with self.assertRaisesRegex(RuntimeError, "resume input"):
                        runner.main()
                    tools.assert_not_called()
                    identity.assert_not_called()

    def test_sibling_resume_remains_valid_at_cli_boundary(self):
        scratch = ROOT / "target/worker-control-resume-path-fixtures"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            base = Path(temporary)
            prior = base / "prior"
            prior.mkdir()
            output = base / "output"
            argv = ["check-worker-control", "full", "--cache-dir", str(base / "cache"),
                    "--output-dir", str(output), "--resume-from", str(prior)]
            with mock.patch("sys.argv", argv), \
                    mock.patch.object(runner, "source_identity", side_effect=RuntimeError("fixture reached source")) as identity, \
                    mock.patch.object(runner, "toolchain") as tools:
                self.assertEqual(runner.main(), 1)
                identity.assert_called_once()
                tools.assert_not_called()
                self.assertTrue((output / "receipt.json").is_file())


if __name__ == "__main__":
    unittest.main()
