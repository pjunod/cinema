"""Fail-closed ordinary-cell accounting for cargo-nextest 0.9.140.

Use list JSON and stable JUnit, never parse native child stdout.
The 0.9.140 plus-JSON native probe is malformed around ignored tests and is
intentionally unsupported; its original refusal is retained in probe evidence.
The pinned upstream serializers are nextest-metadata/src/test_list.rs and
nextest-runner/src/reporter/structured/libtest.rs at cargo-nextest-0.9.140.
The controller owns source/configuration authenticity, process containment,
discovery exit codes and any archive relocation. This module rehashes the
listed payload and requires its exact compiler-inventory identity. It never
qualifies doctests, ignored serial bodies, other features or platform lanes.
"""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import re
import tomllib
import uuid
import xml.etree.ElementTree as ET

PINNED_NEXTEST = "0.9.140"
REPORT_FORMAT = "nextest-junit"
HEX = re.compile(r"[0-9a-f]{64}")


class NextestError(ValueError):
    """Discovery, binding or native results cannot qualify this cell."""


def require(condition, message):
    if not condition:
        raise NextestError(message)


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "Duplicate JSON key: " + key)
        result[key] = value
    return result


def _json(value):
    if isinstance(value, (str, bytes)):
        try:
            value = json.loads(value, object_pairs_hook=_pairs,
                               parse_constant=lambda value: (_ for _ in ()).throw(
                                   NextestError("Nonfinite JSON value: " + value)))
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise NextestError("Invalid JSON: " + str(error)) from error
    require(isinstance(value, dict), "Expected a JSON object")
    return value


def document_sha256(value):
    """Canonical input binding; retain raw artifact hashes separately too."""
    return hashlib.sha256(json.dumps(_json(value), sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def _hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _integer(value):
    return type(value) is int and value >= 0


def _duration(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def _kind(target):
    kinds = target.get("kind")
    require(isinstance(kinds, list) and len(kinds) == 1, "Ambiguous compiler target kind")
    require(kinds[0] in ("lib", "proc-macro", "bin", "test", "example"),
            "Unsupported compiler target kind")
    return kinds[0]


def validate_list(inventory, listing):
    """Bind the entire nextest universe to completed native discovery.

    Reject all active filters, including implicit default filters. A sharding
    controller must establish an explicit exact-union layer rather than use
    filtering to redefine this universe. Ignored cases remain unexecuted.
    Empty listed targets need no run event; an all-empty/ignored run refuses.
    """
    inventory, listing = _json(inventory), _json(listing)
    require(inventory.get("discovery_complete") is True and inventory.get("errors", []) == [],
            "Native discovery is incomplete or erroneous")
    binaries = inventory.get("binaries")
    require(isinstance(binaries, list) and binaries, "Native binary universe is empty")
    suites = listing.get("rust-suites")
    require(isinstance(suites, dict), "Missing nextest test suites")
    by_path, result, prefixes = {}, {}, set()
    for binary in binaries:
        path = binary.get("executable")
        require(isinstance(path, str) and Path(path).is_absolute(), "Binary path must be absolute")
        require(path not in by_path, "Duplicate compiler executable")
        require(binary.get("listing_complete") is True, "Native binary discovery is incomplete")
        require(HEX.fullmatch(str(binary.get("sha256", ""))), "Missing native binary SHA256")
        try:
            require(not Path(path).is_symlink() and _hash(Path(path)) == binary["sha256"],
                    "Compiled binary payload changed: " + path)
        except OSError as error:
            raise NextestError("Cannot verify compiled binary: " + str(error)) from error
        require(isinstance(binary.get("package_id"), str) and binary["package_id"],
                "Missing compiler package identity")
        target = binary.get("target", {})
        require(isinstance(target.get("name"), str) and target["name"], "Missing compiler target name")
        _kind(target)
        require(isinstance(binary.get("features"), list) and
                len(binary["features"]) == len(set(binary["features"])), "Missing/duplicate compiler features")
        require(isinstance(binary.get("profile"), dict) and binary["profile"].get("test") is True,
                "Missing compiler test profile")
        cases = binary.get("cases")
        require(isinstance(cases, list), "Missing native cases")
        names = {}
        for case in cases:
            name = case.get("name")
            require(isinstance(name, str) and name and name not in names, "Duplicate/empty native identity")
            require(type(case.get("ignored")) is bool, "Missing native ignore disposition")
            names[name] = case["ignored"]
        by_path[path] = (binary, names)
    require(len(suites) == len(by_path), "Missing/extra nextest binary")
    seen_paths = set()
    for identity, suite in suites.items():
        require(isinstance(suite, dict) and suite.get("binary-id") == identity, "Wrong nextest binary ID")
        path = suite.get("binary-path")
        require(path in by_path and path not in seen_paths, "Wrong/duplicate nextest binary path")
        seen_paths.add(path)
        binary, expected = by_path[path]
        target, package = binary["target"], suite.get("package-name")
        kind = _kind(target)
        require(isinstance(package, str) and re.fullmatch(r"[A-Za-z0-9_-]+", package),
                "Invalid nextest package name")
        expected_id = (package if kind in ("lib", "proc-macro") else
                       package + "::" + ("" if kind == "test" else kind + "/") + target["name"])
        require(identity == expected_id, "Noncanonical nextest binary ID")
        require(suite.get("package-id") == binary["package_id"] and
                suite.get("binary-name") == target["name"] and suite.get("kind") == kind,
                "Nextest package/target/kind differs from compiler inventory")
        require(suite.get("status") == "listed" and suite.get("build-platform") in ("host", "target"),
                "Skipped/unknown nextest binary discovery")
        cases = suite.get("testcases")
        require(isinstance(cases, dict) and set(cases) == set(expected), "Native/nextest exact case set differs")
        for name, case in cases.items():
            require(case.get("kind") == "test" and case.get("ignored") is expected[name],
                    "Nextest case kind/ignore disposition differs")
            match = case.get("filter-match")
            require(match == {"status": "matches"} or
                    expected[name] and match == {"status": "mismatch", "reason": "ignored"},
                    "Unexpected nextest filtering: " + name)
        prefix = package + "::" + target["name"] + "$"
        require(prefix not in prefixes, "Ambiguous nextest event prefix")
        prefixes.add(prefix)
        result[identity] = dict(binary_id=identity, executable=path, sha256=binary["sha256"],
                                package=package, target=target["name"], kind=kind,
                                prefix=prefix, cases=expected)
    count = sum(len(row["cases"]) for row in result.values())
    require(_integer(listing.get("test-count")) and listing["test-count"] == count,
            "Nextest declared inventory count differs")
    require(any(not ignored for row in result.values() for ignored in row["cases"].values()),
            "Zero active native bodies")
    return dict(schema=1, nextest_version=PINNED_NEXTEST, binaries=result, expected_cases=count,
                active_cases=sum(not ignored for row in result.values() for ignored in row["cases"].values()),
                inventory_sha256=document_sha256(inventory), listing_sha256=document_sha256(listing))


def _configuration(process):
    text = process.get("configuration_text")
    require(isinstance(text, str), "Missing captured nextest configuration")
    require(hashlib.sha256(text.encode()).hexdigest() == process.get("configuration_sha256"),
            "Nextest configuration digest differs")
    try:
        profiles = tomllib.loads(text).get("profile", {})
    except tomllib.TOMLDecodeError as error:
        raise NextestError("Invalid nextest configuration: " + str(error)) from error
    selected = profiles.get(process.get("profile"), {})
    require(type(selected.get("retries")) is int and selected["retries"] == 0 and
            selected.get("fail-fast") is False, "Require explicit zero retries and no fail-fast")
    require(isinstance(selected.get("leak-timeout"), dict) and
            selected["leak-timeout"].get("result") == "fail", "Leaks must fail the selected profile")
    for profile in profiles.values():
        for settings in [profile, *profile.get("overrides", [])]:
            require(type(settings.get("retries", 0)) is int and settings.get("retries", 0) == 0,
                    "Retry override is forbidden")
            require(settings.get("default-filter", "all()") == "all()", "Default filtering is forbidden")
            for option in ("leak-timeout", "slow-timeout"):
                value = settings.get(option)
                if isinstance(value, dict):
                    require(value.get("result", "fail") == "fail", "Pass-on-leak/timeout is forbidden")
                elif option == "leak-timeout" and value is not None:
                    raise NextestError("Unqualified scalar leak override")
    junit = selected.get("junit", {})
    path = junit.get("path")
    require(isinstance(path, str) and path and Path(path).name == path and path not in (".", ".."),
            "Require a captured basename-only JUnit report path")
    return junit


def _xml_count(element, name, expected):
    value = element.get(name)
    require(isinstance(value, str) and re.fullmatch(r"[0-9]+", value) and int(value) == expected,
            "JUnit " + name + " count differs")


def _xml_duration(element):
    try:
        value = float(element.get("time", ""))
    except ValueError as error:
        raise NextestError("Missing/invalid JUnit duration") from error
    require(_duration(value), "Missing/invalid JUnit duration")


def validate_run(inventory, listing, junit_text, process_result):
    """Require exact active JUnit identities and fresh-run binding, or raise.

    The controller must capture the actual effective configuration, original
    process result and separate raw stderr, unlink the old report before run,
    and bind source/discovery execution. Report UUID must match the fresh
    runner stderr; a stale successful report cannot satisfy a later command.
    Ignored identities come exclusively from strict full discovery and are
    never treated as executed/passed. There is no plus-JSON fallback.
    """
    bound = validate_list(inventory, listing)
    process = _json(process_result)
    require(process.get("nextest_version") == PINNED_NEXTEST and
            process.get("report_format") == REPORT_FORMAT,
            "Unpinned nextest or unqualified result format")
    require(not any(key in process for key in ("events_sha256", "message_format", "message_format_version")),
            "Unqualified JSON result metadata is unsupported")
    for key in ("inventory_sha256", "listing_sha256"):
        require(process.get(key) == bound[key], "Execution " + key + " differs")
    require(type(process.get("returncode")) is int and process["returncode"] == 0 and
            process.get("success") is True and process.get("timed_out") is False and
            process.get("cleanup_complete") is True and not process.get("cleanup_error"),
            "Native exit/timeout/owned cleanup did not qualify")
    require(process.get("run_ignored") == "default", "Ignored serial execution is a separate obligation")
    junit_config = _configuration(process)
    stderr = process.get("runner_stderr")
    require(isinstance(stderr, str) and hashlib.sha256(stderr.encode()).hexdigest() ==
            process.get("runner_stderr_sha256"), "Runner stderr binding differs")
    stderr = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", stderr)
    run_ids = re.findall(r"(?m)^ *Nextest run ID ([0-9a-f-]+) with nextest profile: " +
                         re.escape(process["profile"]) + r"$", stderr)
    require(len(run_ids) == 1, "Missing/ambiguous fresh nextest run ID")
    try:
        require(str(uuid.UUID(run_ids[0])) == run_ids[0], "Malformed nextest run UUID")
    except ValueError as error:
        raise NextestError("Malformed nextest run UUID") from error
    require(isinstance(junit_text, str) and hashlib.sha256(junit_text.encode()).hexdigest() ==
            process.get("junit_sha256"), "JUnit artifact digest differs")
    require("<!DOCTYPE" not in junit_text.upper() and "<!ENTITY" not in junit_text.upper(),
            "DTD/entities are forbidden in JUnit")
    try:
        root = ET.fromstring(junit_text)
    except ET.ParseError as error:
        raise NextestError("Invalid JUnit XML: " + str(error)) from error
    require(root.tag == "testsuites" and root.get("uuid") == run_ids[0] and
            root.get("name") == junit_config.get("report-name", "nextest-run"),
            "JUnit report is stale or belongs to another runner")
    require(not set(root.attrib) - {"name", "tests", "failures", "errors", "uuid", "timestamp", "time"},
            "Unknown JUnit report attributes")
    _xml_count(root, "tests", bound["active_cases"])
    _xml_count(root, "failures", 0)
    _xml_count(root, "errors", 0)
    _xml_duration(root)
    suites = bound["binaries"]
    expected_suites = {key for key, value in suites.items() if any(not ignored for ignored in value["cases"].values())}
    observed, seen_suites = [], set()
    for element in root:
        require(element.tag == "testsuite", "Unknown JUnit report child")
        identity = element.get("name")
        require(identity in expected_suites and identity not in seen_suites, "Wrong/duplicate JUnit binary")
        seen_suites.add(identity)
        expected = {name for name, ignored in suites[identity]["cases"].items() if not ignored}
        require(not set(element.attrib) - {"name", "tests", "disabled", "errors", "failures", "timestamp", "time"},
                "Unknown JUnit suite attributes")
        _xml_count(element, "tests", len(expected))
        for key in ("disabled", "errors", "failures"):
            _xml_count(element, key, 0)
        names = set()
        for case in element:
            require(case.tag == "testcase" and case.get("classname") == identity,
                    "Wrong/unknown JUnit case owner")
            name = case.get("name")
            require(name in expected and name not in names, "Missing/unknown/retried/duplicate JUnit identity")
            require(not set(case.attrib) - {"name", "classname", "timestamp", "time"},
                    "Unknown JUnit case attributes")
            _xml_duration(case)
            names.add(name)
            output_kinds = set()
            for child in case:
                require(child.tag in ("system-out", "system-err") and child.tag not in output_kinds,
                        "Failed/skipped/retried/leaked/unknown JUnit outcome")
                require(not child.attrib and len(child) == 0, "Structured child output is not opaque")
                output_kinds.add(child.tag)
            observed.append(dict(binary_id=identity, name=name, status="ok"))
        require(names == expected, "Missing exact active JUnit outcomes")
    require(seen_suites == expected_suites, "Missing native binary completion")
    return dict(schema=1, qualification_complete=True, native_passed=True,
                nextest_version=PINNED_NEXTEST, report_format=REPORT_FORMAT, nextest_run_id=run_ids[0],
                active_passed=bound["active_cases"], ignored_unexecuted=bound["expected_cases"] - bound["active_cases"],
                outcomes=sorted(observed, key=lambda row: (row["binary_id"], row["name"])),
                ignored=[dict(binary_id=identity, name=name, status="ignored-unexecuted")
                         for identity, suite in sorted(suites.items())
                         for name, ignored in sorted(suite["cases"].items()) if ignored],
                empty_binaries=[identity for identity, row in suites.items() if not row["cases"]],
                scope=dict(ordinary_native_only=True, doctests="separate-required-cell",
                           ignored_serial_bodies="separate-required-exact-cells",
                           other_features_and_platforms="not-qualified"))
