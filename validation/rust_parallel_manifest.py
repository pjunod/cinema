"""Binary-level Rust execution plans and exact native receipt accounting.

Receipts prove accounting against the supplied plan, not authenticated CI origin.
The orchestrator must independently bind that plan to the frozen inventory,
compiler environment, executable bytes and actual native parser observations.
"""
from __future__ import annotations

import copy
import hashlib
import json
import math
from pathlib import PurePosixPath
import re

SCHEMA = 1
BINDINGS = ("executable", "sha256", "package_id", "target", "features", "profile")
BINARY_FIELDS = {*BINDINGS, "cases"}


class ManifestError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ManifestError(message)


def digest(value):
    """SHA256 of sorted, compact, ASCII JSON; nonfinite/non-JSON data refuse."""
    def json_value(item):
        require(item is None or type(item) in (str, int, bool, float, list, dict),
                "Not a JSON value")
        if isinstance(item, dict):
            require(all(isinstance(key, str) for key in item), "Non-string JSON key")
            for child in item.values():
                json_value(child)
        elif isinstance(item, list):
            for child in item:
                json_value(child)
        elif type(item) is float:
            require(math.isfinite(item), "Nonfinite JSON number")

    json_value(value)
    try:
        encoded = json.dumps(value, sort_keys=True, separators=(",", ":"),
                             ensure_ascii=True, allow_nan=False).encode("ascii")
    except (TypeError, ValueError, OverflowError) as error:
        raise ManifestError(f"Not canonical JSON: {error}") from error
    return hashlib.sha256(encoded).hexdigest()


def _text(value, label):
    require(isinstance(value, str) and value and value.strip() == value
            and not any(ord(char) < 32 or ord(char) == 127 for char in value),
            f"Invalid {label}")


def _hash(value, length, label):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{" + str(length) + "}", value),
            f"Invalid {label}")


def _positive(value, label):
    require(type(value) is int and value > 0, f"Invalid {label}")


def _source(source):
    require(isinstance(source, dict) and set(source) == {"commit", "tree", "tracked_clean"},
            "Source must contain commit/tree/tracked_clean")
    _hash(source["commit"], 40, "source commit")
    _hash(source["tree"], 40, "source tree")
    require(source["tracked_clean"] is True, "Source is not tracked-clean")


def _strings(values, label, *, nonempty=False):
    require(isinstance(values, list) and (values or not nonempty), f"Invalid {label}")
    for value in values:
        _text(value, label)
    require(len(set(values)) == len(values), f"Duplicate {label}")


def _json_types(value):
    require(value is None or type(value) in (str, int, bool, list, dict), "Invalid metadata JSON type")
    if isinstance(value, dict):
        require(all(isinstance(key, str) for key in value), "Non-string metadata key")
        for item in value.values():
            _json_types(item)
    elif isinstance(value, list):
        for item in value:
            _json_types(item)


def _binary(record):
    require(isinstance(record, dict) and set(record) == BINARY_FIELDS, "Invalid binary record fields")
    path = record["executable"]
    _text(path, "executable")
    require(path.startswith("/") and not path.startswith("//") and "\\" not in path
            and path == str(PurePosixPath(path)) and ".." not in path.split("/")
            and "." not in path.split("/") and PurePosixPath(path).name,
            "Executable must be a canonical absolute POSIX path")
    _hash(record["sha256"], 64, "binary hash")
    _text(record["package_id"], "package identity")
    target = record["target"]
    require(isinstance(target, dict), "Invalid target mapping")
    _text(target.get("name"), "target name")
    _strings(target.get("kind"), "target kinds", nonempty=True)
    _json_types(target)
    for key in ("src_path", "edition"):
        if key in target:
            _text(target[key], "target " + key)
    for key in ("doctest", "test", "doc"):
        if key in target:
            require(type(target[key]) is bool, "Invalid target " + key)
    for key in ("crate_types", "required_features"):
        if key in target:
            _strings(target[key], "target " + key)
    _strings(record["features"], "features")
    require(record["features"] == sorted(record["features"]), "Features must be sorted")
    profile = record["profile"]
    require(isinstance(profile, dict) and profile, "Invalid profile mapping")
    _json_types(profile)
    require(profile.get("test") is True, "Binary profile is not a test profile")
    if "opt_level" in profile:
        _text(profile["opt_level"], "profile opt_level")
    if "debuginfo" in profile:
        require(profile["debuginfo"] is None or type(profile["debuginfo"]) is int
                and profile["debuginfo"] >= 0, "Invalid profile debuginfo")
    for key in ("debug_assertions", "overflow_checks"):
        if key in profile:
            require(type(profile[key]) is bool, f"Invalid profile {key}")
    cases = record["cases"]
    require(isinstance(cases, list), "Invalid case list")
    names = []
    for case in cases:
        require(isinstance(case, dict) and set(case) == {"name", "ignored"}, "Invalid case record")
        _text(case["name"], "case identity")
        require(type(case["ignored"]) is bool, "Case ignored disposition must be boolean")
        names.append(case["name"])
    require(len(set(names)) == len(names) and names == sorted(names), "Duplicate or unsorted cases")


def _universe(binaries):
    require(binaries, "Empty native universe")
    for binary in binaries:
        _binary(binary)
    require(any(binary["cases"] for binary in binaries), "Empty native universe")
    paths, basenames, identities = set(), set(), set()
    for binary in binaries:
        path, basename = binary["executable"], PurePosixPath(binary["executable"]).name
        identity = digest({key: binary[key] for key in ("package_id", "target", "features", "profile")})
        require(path not in paths and basename not in basenames, "Duplicate/colliding executable path or basename")
        require(identity not in identities, "Duplicate logical binary identity")
        paths.add(path)
        basenames.add(basename)
        identities.add(identity)


def validate_plan(plan):
    """Validate a complete deterministic plan; return it without mutation."""
    require(isinstance(plan, dict) and set(plan) == {
        "schema", "inventory_sha256", "source", "workers", "test_threads", "shards"}, "Invalid plan fields")
    require(type(plan["schema"]) is int and plan["schema"] == SCHEMA, "Unsupported plan schema")
    _hash(plan["inventory_sha256"], 64, "inventory hash")
    _source(plan["source"])
    _positive(plan["workers"], "workers")
    _positive(plan["test_threads"], "test threads")
    shards = plan["shards"]
    require(isinstance(shards, list) and len(shards) == plan["workers"], "Missing/extra shards")
    binaries = []
    for index, shard in enumerate(shards):
        require(isinstance(shard, dict) and set(shard) == {"index", "binaries"}, "Invalid shard record")
        require(type(shard["index"]) is int and shard["index"] == index, "Reordered/duplicate shard index")
        require(isinstance(shard["binaries"], list), "Invalid shard binaries")
        binaries.extend(shard["binaries"])
    _universe(binaries)
    expected = _assign(binaries, plan["workers"])
    require(digest(shards) == digest(expected), "Plan differs from deterministic binary assignment")
    return plan


def _assign(binaries, workers):
    shards = [dict(index=index, binaries=[]) for index in range(workers)]
    loads = [0] * workers
    for binary in sorted(binaries, key=lambda row: (-len(row["cases"]), row["executable"])):
        if workers == 3:
            index = {"plurxd": 0, "plurx_core": 1}.get(binary["target"]["name"], 2)
        else:
            index = min(range(workers), key=lambda position: (loads[position], position))
        shards[index]["binaries"].append(binary)
        loads[index] += len(binary["cases"])
    for shard in shards:
        shard["binaries"].sort(key=lambda row: row["executable"])
    return shards


def build_plan(inventory, *, workers=3, test_threads=4):
    require(isinstance(inventory, dict) and inventory.get("discovery_complete") is True,
            "Native inventory discovery incomplete")
    require(inventory.get("errors", []) == [], "Native inventory retains errors")
    _source(inventory.get("source"))
    _positive(workers, "workers")
    _positive(test_threads, "test threads")
    require(isinstance(inventory.get("binaries"), list), "Invalid binary inventory")
    binaries = []
    for original in inventory["binaries"]:
        require(isinstance(original, dict) and original.get("listing_complete") is True,
                "Native binary listing incomplete")
        require(not original.get("error"), "Native binary retains discovery error")
        require(BINARY_FIELDS <= original.keys(), "Native binary identity incomplete")
        binary = copy.deepcopy({key: original[key] for key in BINARY_FIELDS})
        require(isinstance(binary["cases"], list), "Invalid case list")
        for case in binary["cases"]:
            require(isinstance(case, dict) and isinstance(case.get("name"), str), "Invalid case identity")
        binary["cases"].sort(key=lambda row: row["name"])
        binaries.append(binary)
    _universe(binaries)
    plan = dict(schema=SCHEMA, inventory_sha256=digest(inventory), source=copy.deepcopy(inventory["source"]),
                workers=workers, test_threads=test_threads, shards=_assign(binaries, workers))
    return validate_plan(plan)


def _no_retry_or_gap(record):
    for key, expected in (("attempt", 1), ("attempts", 1), ("retries", 0), ("retry_count", 0)):
        if key in record:
            require(type(record[key]) is int and record[key] == expected, "Retry/attempt count is not the original execution")
    for key in ("truncated", "skipped", "timed_out", "leaked_process_group", "cleanup_observation_unknown"):
        if key in record:
            require(record[key] is False, "Skipped/truncated execution")
    for key in ("success", "cleanup_complete", "qualification_complete", "native_passed"):
        if key in record:
            require(record[key] is True, "Unqualified execution/evidence")
    if "returncode" in record:
        require(type(record["returncode"]) is int and record["returncode"] == 0, "Original execution failed")
    for key in ("errors", "blocked_cells"):
        if key in record:
            require(record[key] == [], "Execution retains errors or blocked work")


def validate_receipts(plan, receipts):
    """Require every planned binary/case exactly once with its original outcome."""
    validate_plan(plan)
    require(isinstance(receipts, list) and len(receipts) == plan["workers"], "Missing/extra shard receipts")
    plan_hash = digest(plan)
    seen, total, passed, ignored, binary_count = set(), 0, 0, 0, 0
    for receipt in receipts:
        require(isinstance(receipt, dict), "Invalid shard receipt")
        require(receipt.get("plan_sha256") == plan_hash, "Receipt plan hash differs")
        require(digest(receipt.get("source")) == digest(plan["source"]), "Receipt source differs")
        index = receipt.get("shard_index")
        require(type(index) is int and 0 <= index < plan["workers"] and index not in seen,
                "Invalid/duplicate shard receipt index")
        seen.add(index)
        _no_retry_or_gap(receipt)
        records = receipt.get("binaries")
        require(isinstance(records, list), "Invalid receipt binaries")
        expected = {binary["executable"]: binary for binary in plan["shards"][index]["binaries"]}
        require(len(records) == len(expected), "Missing/extra native binary receipt")
        observed = set()
        for record in records:
            require(isinstance(record, dict), "Invalid native binary receipt")
            executable = record.get("executable")
            require(isinstance(executable, str) and executable in expected and executable not in observed,
                    "Unexpected/duplicate native binary receipt")
            observed.add(executable)
            binary = expected[executable]
            require(all(key in record for key in BINDINGS), "Native binary binding missing")
            require(digest({key: record[key] for key in BINDINGS}) == digest({key: binary[key] for key in BINDINGS}),
                    "Native binary source/features/profile/hash binding differs")
            _no_retry_or_gap(record)
            require(type(record.get("returncode")) is int and record["returncode"] == 0, "Native exit is not zero")
            require(record.get("success") is True and record.get("cleanup_complete") is True
                    and record.get("timed_out") is False, "Native execution or cleanup unqualified")
            outcomes = record.get("outcomes")
            require(isinstance(outcomes, list) and len(outcomes) == len(binary["cases"]), "Missing/extra native case outcomes")
            cases = {case["name"]: case for case in binary["cases"]}
            names = set()
            for outcome in outcomes:
                require(isinstance(outcome, dict) and set(outcome) == {"name", "status"}, "Invalid native outcome")
                name = outcome["name"]
                require(isinstance(name, str) and name in cases and name not in names, "Unexpected/duplicate native case outcome")
                names.add(name)
                expected_status = "ignored" if cases[name]["ignored"] else "ok"
                require(outcome["status"] == expected_status, "Failed/skipped case or ignored disposition changed")
                total += 1
                ignored += expected_status == "ignored"
                passed += expected_status == "ok"
            binary_count += 1
    return dict(schema=SCHEMA, success=True, plan_sha256=plan_hash, source=copy.deepcopy(plan["source"]),
                workers=plan["workers"], test_threads=plan["test_threads"], binary_count=binary_count,
                counts=dict(total=total, passed=passed, failed=0, ignored=ignored))
