"""Reconcile fresh M3 runtime arms; observations never authorize adoption."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import statistics

from validation.rust_parallel import SERIAL, doctest_targets
from validation.rust_parallel_manifest import build_plan, digest, validate_receipts

REQUIRED = {"cold-parallel": "parallel", "legacy": "legacy", **{f"warm-{i:02}": "parallel" for i in range(1, 4)}}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def _pairs(rows):
    result = {}
    for key, value in rows:
        require(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def _number(value, label, *, positive=False):
    require(type(value) in (int, float) and math.isfinite(value)
            and (value > 0 if positive else value >= 0), "invalid " + label)
    return value


def _process(result):
    require(isinstance(result, dict), "missing native process result")
    require(type(result.get("returncode")) is int and result["returncode"] == 0, "original native exit was not zero")
    require(result.get("success") is True and result.get("cleanup_complete") is True
            and result.get("timed_out") is False, "native process/cleanup did not qualify")
    for key in ("canceled", "leaked_process_group", "cleanup_observation_unknown", "cleanup_error", "error"):
        require(not result.get(key), "native process retained " + key)
    _number(result.get("seconds"), "process seconds")


def _suite(suite, cases, *, filtered=0):
    """Re-account parser output; a green aggregate alone is insufficient."""
    require(isinstance(suite, dict) and suite.get("status") == "passed", "native suite failed or missing")
    rows = suite.get("outcomes")
    require(isinstance(rows, list), "missing exact native outcomes")
    names, actual = set(), []
    for row in rows:
        require(isinstance(row, dict) and set(row) == {"name", "status"}, "invalid native outcome")
        name, status = row["name"], row["status"]
        require(isinstance(name, str) and name and name not in names, "duplicate/invalid native case")
        require(status in ("ok", "ignored"), "failed/skipped native case")
        names.add(name)
        actual.append((name, status))
    if cases is not None:
        expected = {(case["name"], "ignored" if case["ignored"] else "ok") for case in cases}
        require(set(actual) == expected, "missing/unexpected native case or changed ignore disposition")
    require(type(suite.get("declared_tests")) is int and suite["declared_tests"] == len(rows), "native header count differs")
    counts = suite.get("result")
    require(isinstance(counts, dict), "native footer missing")
    expected_counts = dict(passed=sum(status == "ok" for _, status in actual), failed=0,
                           ignored=sum(status == "ignored" for _, status in actual), measured=0, filtered_out=filtered)
    for key, value in expected_counts.items():
        require(type(counts.get(key)) is int and counts[key] == value, "native footer count differs: " + key)
    _number(counts.get("reported_s"), "native body seconds")
    return sorted(actual)


def _evidence(evidence, expected, docs=(), *, filters=None):
    require(isinstance(evidence, dict) and evidence.get("qualification_complete") is True
            and evidence.get("native_passed") is True and evidence.get("gaps") == [], "native parser evidence incomplete/failed")
    suites = evidence.get("observed_suites")
    require(isinstance(suites, list), "missing parsed native suites")
    seen, doc_rows = set(), []
    wanted = set(expected) | {"doctest:" + target for target in docs}
    for suite in suites:
        name = suite.get("binary")
        require(isinstance(name, str) and name in wanted and name not in seen, "missing/extra/duplicate native binary or doctest")
        seen.add(name)
        if name.startswith("doctest:"):
            rows = _suite(suite, None)
            require(all(status == "ok" for _, status in rows), "unexpected ignored doctest")
            doc_rows.extend((name.removeprefix("doctest:"), case, status) for case, status in rows)
        else:
            _suite(suite, expected[name], filtered=(filters or {}).get(name, 0))
    require(seen == wanted, "native binary/doctest union incomplete")
    if docs:
        require(doc_rows, "no actual doctest body executed")
    return sorted(doc_rows)


def _separate_docs(record, targets):
    require(isinstance(record, dict), "separate doctest cell missing")
    _process(record.get("process"))
    suites = record.get("suites")
    require(isinstance(suites, list), "separate doctest suites missing")
    normalized = []
    for suite in suites:
        require(isinstance(suite, dict) and isinstance(suite.get("target"), str), "doctest target missing")
        normalized.append(dict(suite, binary="doctest:" + suite["target"]))
    return _evidence(dict(qualification_complete=True, native_passed=True, gaps=[], observed_suites=normalized), {}, targets)


def _serial(report, inventory):
    daemon = [binary for binary in inventory["binaries"] if binary["target"]["name"] == "plurxd"]
    require(len(daemon) == 1, "serial daemon binary ambiguous/missing")
    binary = daemon[0]
    records = report.get("serial")
    require(isinstance(records, list) and len(records) == len(SERIAL), "missing/extra serial bodies")
    identities = []
    for name, record in zip(SERIAL, records):
        require([case for case in binary["cases"] if case["name"] == name and case["ignored"] is True],
                "serial body is not an original ignored case")
        _process(record.get("process"))
        argv = record["process"].get("argv")
        require(isinstance(argv, list) and argv[-5:] == [binary["executable"], name, "--exact", "--ignored", "--test-threads=1"],
                "serial exact/ignored/single-thread selection differs")
        basename = Path(binary["executable"]).name
        _evidence(record.get("evidence"), {basename: [dict(name=name, ignored=False)]},
                  filters={basename: len(binary["cases"]) - 1})
        identities.append((basename, name, "ok"))
    return identities


def compare(root):
    root = Path(root)
    report = dict(schema=1, qualification_complete_for_runtime=False, adoption_ready=False, errors=[], runs={},
                  owed=["source-bound runner-loss and lease-release qualification", "host capacity/resource qualification",
                        "broader feature/platform lanes and promotion contract"],
                  scope="fresh ordinary native, doctest and explicit serial runtime accounting only",
                  limits=["Three warm observations provide a median/range, not a defensible p95.",
                          "Directory labels alone do not establish cold/warm cache state.",
                          "Accounting checks do not authenticate CI origin or authorize result adoption."], artifacts={})

    def raw(relative):
        path = root / relative
        data = path.read_bytes()
        report["artifacts"][str(relative)] = hashlib.sha256(data).hexdigest()
        return data.decode("utf-8")

    def read(relative):
        return json.loads(raw(relative), object_pairs_hook=_pairs,
                          parse_constant=lambda token: (_ for _ in ()).throw(ValueError("nonfinite JSON: " + token)))

    failures = (OSError, ValueError, TypeError, KeyError, AttributeError, IndexError)
    try:
        inventory = read(Path("inventory/inventory.json"))
        metadata = read(Path("workspace-metadata.json"))
        expected_plan = build_plan(inventory, workers=3, test_threads=4)
        targets = doctest_targets(metadata)
        report["source"] = inventory["source"]
        binaries = {Path(binary["executable"]).name: binary for binary in inventory["binaries"]}
        expected = {name: binary["cases"] for name, binary in binaries.items()}
        report["expected"] = dict(binaries=len(binaries), native_cases=sum(len(cases) for cases in expected.values()),
                                  ignored=sum(case["ignored"] for cases in expected.values() for case in cases), doctest_targets=targets)
    except failures as error:
        report["errors"].append(dict(run="inputs", error=str(error)))
        return report

    modes = dict(REQUIRED)
    if (root / "nextest").exists():
        modes["nextest"] = "nextest"
    else:
        report["optional_nextest"] = "not run"
    identities = {}
    for label, mode in modes.items():
        original = None
        try:
            original = read(Path(label) / "results.json")
            require(original.get("mode") == mode, "run mode differs from required arm")
            require(digest(original.get("source")) == digest(inventory["source"]), "run source differs from inventory")
            require(original.get("success") is True and original.get("errors") == [], "original run failed or incomplete")
            require(original.get("safe_to_release") is True, "original cleanup/admission release is unqualified")
            seconds = _number(original.get("seconds"), "run seconds", positive=True)
            if mode == "legacy":
                _process(original.get("process"))
                doc_cases = _evidence(original.get("evidence"), expected, targets)
            elif mode == "parallel":
                plan = read(Path(label) / "plan.json")
                require(digest(plan) == digest(expected_plan), "plan differs from original inventory/deterministic assignment")
                receipts = [read(Path(label) / f"shard-{i}/receipt.json") for i in range(plan["workers"])]
                aggregate = validate_receipts(plan, receipts)
                require(digest(original.get("native")) == digest(aggregate), "reported native aggregate differs from original receipts")
                for receipt in receipts:
                    for binary in receipt["binaries"]:
                        _process(binary.get("process"))
                        name = Path(binary["executable"]).name
                        _evidence(binary.get("evidence"), {name: expected[name]})
                        parsed = binary["evidence"]["observed_suites"][0]["outcomes"]
                        require(digest(sorted(parsed, key=lambda row: row["name"])) ==
                                digest(sorted(binary["outcomes"], key=lambda row: row["name"])), "receipt/parser outcomes differ")
                doc_cases = _separate_docs(original.get("doctests"), targets)
            else:
                from validation.rust_nextest import validate_list, validate_run
                nextest = original.get("nextest")
                require(isinstance(nextest, dict), "nextest runtime evidence missing")
                _process(nextest.get("process"))
                listing = read(Path(label) / "nextest-list.log")
                listed = validate_list(inventory, listing)
                require(digest(original.get("nextest_list")) == digest(listed), "nextest discovery binding differs")
                process = nextest["process"]
                require(process.get("runner_stderr") == raw(Path(label) / "nextest-run.stderr"), "nextest raw stderr differs")
                observed = validate_run(inventory, listing, raw(Path(label) / "nextest-junit.xml"), process)
                require(digest(nextest.get("evidence")) == digest(observed), "nextest evidence differs from original JUnit")
                doc_cases = _separate_docs(original.get("doctests"), targets)
            serial_cases = _serial(original, inventory)
            identities[label] = (doc_cases, serial_cases)
            report["runs"][label] = dict(mode=mode, success=True, seconds=seconds,
                                         doctest_identities=doc_cases, serial_identities=serial_cases)
        except failures as error:
            report["errors"].append(dict(run=label, error=str(error)))
            report["runs"][label] = dict(mode=mode, success=False, reported_success=original.get("success") if isinstance(original, dict) else None,
                                         reported_errors=original.get("errors") if isinstance(original, dict) else None)
    if "legacy" in identities:
        for label, observed in identities.items():
            if observed != identities["legacy"]:
                report["errors"].append(dict(run=label, error="doctest/serial exact identities differ from fresh legacy"))
                report["runs"][label]["success"] = False
    if all(report["runs"].get(label, {}).get("success") is True for label in REQUIRED):
        warm = [report["runs"][f"warm-{i:02}"]["seconds"] for i in range(1, 4)]
        report["timings"] = dict(warm_seconds=warm, warm_median_seconds=statistics.median(warm),
                                 warm_range_seconds=[min(warm), max(warm)],
                                 cold_parallel_seconds=report["runs"]["cold-parallel"]["seconds"],
                                 legacy_seconds=report["runs"]["legacy"]["seconds"])
    report["qualification_complete_for_runtime"] = not report["errors"] and len(identities) == len(modes)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    output = (args.output or args.root / "comparison.json").resolve()
    root = args.root.resolve()
    require(output != root and output not in {root / "inventory/inventory.json", root / "workspace-metadata.json"}
            and not any(output.is_relative_to(root / label) for label in [*REQUIRED, "nextest"]),
            "output must not overwrite original evidence")
    report = compare(root)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n")
    temporary.replace(output)
    return 0 if report["qualification_complete_for_runtime"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
