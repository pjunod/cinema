"""Run and validate deterministic shards of the compiled Store test binary."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import Any, Mapping, Sequence


SCHEMA = 1
ALGORITHM = "sha256-mod-v1"
GIT_OBJECT = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")
SHA256 = re.compile(r"[0-9a-f]{64}")
TEST_RESULT = re.compile(r"^test (.+) \.\.\. (ok|FAILED|ignored(?:,.*)?)$")
ELF64 = 2
ELF_LITTLE_ENDIAN = 1
X86_64_MACHINE = 62


class StoreShardError(ValueError):
    """Shard evidence is missing, inconsistent, or untrustworthy."""


def file_sha256(path: Path) -> str:
    """Hash one file without loading the test binary into memory."""

    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_object(repository: Path, revision: str) -> str:
    """Resolve one unambiguous Git object from the exact checkout."""

    completed = subprocess.run(
        ["git", "rev-parse", "--verify", revision],
        cwd=repository,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return completed.stdout.strip()


def require_metadata(environment: Mapping[str, str], tested_sha: str) -> dict[str, str]:
    """Validate the GitHub identity bound into every shard receipt."""

    names = (
        "GITHUB_REPOSITORY",
        "GITHUB_SHA",
        "GITHUB_WORKFLOW_REF",
        "GITHUB_RUN_ID",
        "GITHUB_RUN_ATTEMPT",
        "GITHUB_JOB",
    )
    missing = [name for name in names if not environment.get(name, "")]
    if missing:
        raise StoreShardError("shard metadata is missing: " + ", ".join(missing))
    if environment["GITHUB_SHA"] != tested_sha:
        raise StoreShardError(
            f"checked-out sha {tested_sha} does not match "
            f"GITHUB_SHA {environment['GITHUB_SHA']}"
        )
    return {name.lower(): environment[name] for name in names}


def require_git_object(value: str, field: str) -> str:
    if GIT_OBJECT.fullmatch(value) is None:
        raise StoreShardError(f"{field} must be a Git object id")
    return value


def inspect_binary(
    binary: Path,
    rustc_vv_path: Path,
    *,
    require_x86_64: bool = True,
) -> dict[str, object]:
    """Bind the executable bytes, architecture, and pinned compiler identity."""

    if binary.is_symlink() or not binary.is_file():
        raise StoreShardError(f"Store test binary is missing or unsafe: {binary}")
    if not os.access(binary, os.X_OK):
        raise StoreShardError(f"Store test binary is not executable: {binary}")
    try:
        rustc_vv = rustc_vv_path.read_text(encoding="utf-8")
    except OSError as exc:
        raise StoreShardError(f"cannot read rustc identity: {exc}") from exc
    if "release: 1.97.1\n" not in rustc_vv:
        raise StoreShardError("Store shard binary was not built by pinned Rust 1.97.1")

    machine: int | str = "fixture"
    if require_x86_64:
        try:
            with binary.open("rb") as source:
                header = source.read(20)
        except OSError as exc:
            raise StoreShardError(f"cannot inspect Store test binary: {exc}") from exc
        if (
            len(header) < 20
            or header[:4] != b"\x7fELF"
            or header[4] != ELF64
            or header[5] != ELF_LITTLE_ENDIAN
        ):
            raise StoreShardError(
                "Store shard binary must be a little-endian ELF64 executable"
            )
        machine = int.from_bytes(header[18:20], byteorder="little")
        if machine != X86_64_MACHINE:
            raise StoreShardError(
                f"Store shard binary must target x86_64 machine 62; got {machine}"
            )

    return {
        "name": binary.name,
        "sha256": file_sha256(binary),
        "bytes": binary.stat().st_size,
        "elf_machine": machine,
        "rustc_vv": rustc_vv,
    }


def parse_test_listing(output: str) -> list[str]:
    """Parse libtest's stable terse listing without freezing today's count."""

    names: list[str] = []
    for line in output.splitlines():
        if not line.endswith(": test"):
            continue
        name = line[: -len(": test")]
        if not name:
            raise StoreShardError("Store test inventory contains an empty name")
        names.append(name)
    if not names:
        raise StoreShardError("Store test inventory is empty")
    if len(names) != len(set(names)):
        raise StoreShardError("Store test inventory contains duplicate names")
    return sorted(names)


def discover_tests(binary: Path, *, ignored_only: bool = False) -> list[str]:
    """Ask the compiled binary for its inventory or ignored subset."""

    command = [str(binary), "--list", "--format=terse"]
    if ignored_only:
        command.insert(2, "--ignored")
    completed = subprocess.run(
        command,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if completed.returncode != 0:
        raise StoreShardError(
            f"Store test discovery exited {completed.returncode}: "
            f"{completed.stdout.strip()}"
        )
    if ignored_only and not any(
        line.endswith(": test") for line in completed.stdout.splitlines()
    ):
        return []
    return parse_test_listing(completed.stdout)


def assigned_tests(
    inventory: Sequence[str], shard_count: int, shard_index: int
) -> list[str]:
    """Assign the dynamic inventory with stable SHA-256 modulo partitioning."""

    if shard_count < 2:
        raise StoreShardError("Store sharding requires at least two shards")
    if shard_index < 0 or shard_index >= shard_count:
        raise StoreShardError(
            f"shard index {shard_index} is outside 0..{shard_count - 1}"
        )
    return [
        name
        for name in sorted(inventory)
        if int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "big")
        % shard_count
        == shard_index
    ]


def receipt_base(
    environment: Mapping[str, str],
    *,
    tested_sha: str,
    tested_tree: str,
    binary_record: dict[str, object] | None,
    shard_count: int,
    shard_index: int,
) -> dict[str, object]:
    metadata = require_metadata(environment, tested_sha)
    require_git_object(tested_sha, "tested sha")
    require_git_object(tested_tree, "tested tree")
    return {
        "schema": SCHEMA,
        "kind": "store-shard",
        "repository": metadata["github_repository"],
        "workflow_ref": metadata["github_workflow_ref"],
        "run_id": metadata["github_run_id"],
        "run_attempt": metadata["github_run_attempt"],
        "job": metadata["github_job"],
        "tested_sha": tested_sha,
        "tested_tree": tested_tree,
        "binary": binary_record,
        "inventory": [],
        "ignored": [],
        "assignment": {
            "algorithm": ALGORITHM,
            "shard_count": shard_count,
            "shard_index": shard_index,
            "tests": [],
        },
        "started": [],
        "completed": [],
        "exit_code": None,
        "duration_ms": 0,
        "result": "failure",
        "errors": [],
    }


def write_receipt(path: Path, receipt: Mapping[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def run_shard(
    binary: Path,
    rustc_vv_path: Path,
    receipt_path: Path,
    log_path: Path,
    *,
    environment: Mapping[str, str],
    tested_sha: str,
    tested_tree: str,
    shard_count: int,
    shard_index: int,
    require_x86_64: bool = True,
) -> dict[str, object]:
    """Discover, assign, and run one exact multi-filter libtest invocation."""

    receipt = receipt_base(
        environment,
        tested_sha=tested_sha,
        tested_tree=tested_tree,
        binary_record=None,
        shard_count=shard_count,
        shard_index=shard_index,
    )
    log_path.parent.mkdir(parents=True, exist_ok=True)
    errors: list[str] = []
    started_at = time.monotonic()

    try:
        binary_record = inspect_binary(
            binary, rustc_vv_path, require_x86_64=require_x86_64
        )
        receipt["binary"] = binary_record
        inventory = discover_tests(binary)
        ignored = discover_tests(binary, ignored_only=True)
        if not set(ignored).issubset(inventory):
            raise StoreShardError("ignored Store tests are outside the full inventory")
        assigned = assigned_tests(inventory, shard_count, shard_index)
        if not assigned:
            raise StoreShardError(f"Store shard {shard_index} has no assigned tests")
        receipt["inventory"] = inventory
        receipt["ignored"] = ignored
        assignment = receipt["assignment"]
        assert isinstance(assignment, dict)
        assignment["tests"] = list(assigned)

        command = [
            str(binary),
            "--exact",
            "--test-threads=1",
            "--format=pretty",
            *assigned,
        ]
        receipt["started"] = list(assigned)
        completed_records: list[dict[str, object]] = []
        completed_names: set[str] = set()
        last_completion = started_at
        with log_path.open("w", encoding="utf-8") as log:
            log.write("command: " + json.dumps(command) + "\n")
            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                bufsize=1,
            )
            assert process.stdout is not None
            with process.stdout as output:
                for line in output:
                    log.write(line)
                    log.flush()
                    print(line, end="")
                    match = TEST_RESULT.fullmatch(line.rstrip("\n"))
                    if match is None:
                        continue
                    name, raw_outcome = match.groups()
                    if name not in assigned:
                        errors.append(f"unexpected test result line for {name}")
                        continue
                    if name in completed_names:
                        errors.append(f"duplicate test result line for {name}")
                        continue
                    now = time.monotonic()
                    outcome = {
                        "ok": "passed",
                        "FAILED": "failed",
                    }.get(raw_outcome, "ignored")
                    completed_records.append(
                        {
                            "name": name,
                            "outcome": outcome,
                            "duration_ms": max(
                                0, round((now - last_completion) * 1000)
                            ),
                        }
                    )
                    completed_names.add(name)
                    last_completion = now
            exit_code = process.wait()

        receipt["completed"] = completed_records
        receipt["exit_code"] = exit_code
        if file_sha256(binary) != binary_record["sha256"]:
            errors.append("Store test binary changed during shard execution")
        missing = sorted(set(assigned) - completed_names)
        if missing:
            errors.append("assigned tests did not complete: " + ", ".join(missing))
        ignored_set = set(ignored)
        for record in completed_records:
            name = str(record["name"])
            outcome = record["outcome"]
            expected = "ignored" if name in ignored_set else "passed"
            if outcome != expected:
                errors.append(
                    f"{name} outcome was {outcome}; expected {expected}"
                )
        if exit_code != 0:
            errors.append(f"Store test binary exited {exit_code}")
    except (OSError, StoreShardError) as exc:
        errors.append(str(exc))
        with log_path.open("a", encoding="utf-8") as log:
            log.write(f"store shard failed: {exc}\n")

    receipt["duration_ms"] = max(0, round((time.monotonic() - started_at) * 1000))
    receipt["errors"] = errors
    receipt["result"] = "success" if not errors else "failure"
    write_receipt(receipt_path, receipt)
    return receipt


def record_preexecution_failure(
    receipt_path: Path,
    log_path: Path,
    *,
    environment: Mapping[str, str],
    tested_sha: str,
    tested_tree: str,
    shard_count: int,
    shard_index: int,
    error: str,
) -> dict[str, object]:
    """Retain exact-tree evidence when the binary could not be produced."""

    receipt = receipt_base(
        environment,
        tested_sha=tested_sha,
        tested_tree=tested_tree,
        binary_record=None,
        shard_count=shard_count,
        shard_index=shard_index,
    )
    receipt["errors"] = [error]
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text(error + "\n", encoding="utf-8")
    write_receipt(receipt_path, receipt)
    return receipt


def _require_string_list(value: object, field: str) -> list[str]:
    if not isinstance(value, list) or not all(
        isinstance(item, str) and item for item in value
    ):
        raise StoreShardError(f"{field} must be a list of non-empty strings")
    if value != sorted(set(value)):
        raise StoreShardError(f"{field} must be sorted and unique")
    return value


def validate_receipts(receipts: Sequence[Mapping[str, Any]]) -> dict[str, object]:
    """Prove exact union, disjointness, completion, tree, and binary identity."""

    if len(receipts) < 2:
        raise StoreShardError("at least two Store shard receipts are required")
    first = receipts[0]
    for field, expected in (("schema", SCHEMA), ("kind", "store-shard")):
        if first.get(field) != expected:
            raise StoreShardError(f"receipt 0 has invalid {field}")
    first_assignment = first.get("assignment")
    if not isinstance(first_assignment, dict):
        raise StoreShardError("receipt 0 has no assignment")
    shard_count = first_assignment.get("shard_count")
    if not isinstance(shard_count, int) or shard_count < 2:
        raise StoreShardError("receipt shard count is invalid")
    if len(receipts) != shard_count:
        raise StoreShardError(
            f"expected {shard_count} receipts; found {len(receipts)}"
        )

    identity_fields = (
        "repository",
        "workflow_ref",
        "run_id",
        "run_attempt",
        "job",
        "tested_sha",
        "tested_tree",
        "binary",
        "inventory",
        "ignored",
    )
    inventory = _require_string_list(first.get("inventory"), "inventory")
    ignored = _require_string_list(first.get("ignored"), "ignored")
    if not set(ignored).issubset(inventory):
        raise StoreShardError("ignored tests are outside the discovered inventory")
    binary = first.get("binary")
    if not isinstance(binary, dict):
        raise StoreShardError("receipt has no test-binary identity")
    if not isinstance(first.get("repository"), str) or not first["repository"]:
        raise StoreShardError("receipt repository is invalid")
    require_git_object(str(first.get("tested_sha", "")), "receipt tested sha")
    require_git_object(str(first.get("tested_tree", "")), "receipt tested tree")
    if SHA256.fullmatch(str(binary.get("sha256", ""))) is None:
        raise StoreShardError("receipt test-binary digest is invalid")
    if not isinstance(binary.get("bytes"), int) or binary["bytes"] <= 0:
        raise StoreShardError("receipt test-binary byte count is invalid")
    if binary.get("elf_machine") != X86_64_MACHINE:
        raise StoreShardError("receipt test binary is not x86_64 ELF")
    rustc_vv = binary.get("rustc_vv")
    if not isinstance(rustc_vv, str) or "release: 1.97.1\n" not in rustc_vv:
        raise StoreShardError("receipt compiler identity is not pinned Rust 1.97.1")

    indexes: set[int] = set()
    union: set[str] = set()
    shard_summaries: list[dict[str, object]] = []
    for position, receipt in enumerate(receipts):
        for field, expected in (("schema", SCHEMA), ("kind", "store-shard")):
            if receipt.get(field) != expected:
                raise StoreShardError(f"receipt {position} has invalid {field}")
        for field in identity_fields:
            if receipt.get(field) != first.get(field):
                raise StoreShardError(
                    f"Store shard receipts disagree on {field}"
                )
        assignment = receipt.get("assignment")
        if not isinstance(assignment, dict):
            raise StoreShardError(f"receipt {position} has no assignment")
        if assignment.get("algorithm") != ALGORITHM:
            raise StoreShardError("Store shard assignment algorithm is unsupported")
        if assignment.get("shard_count") != shard_count:
            raise StoreShardError("Store shard receipts disagree on shard count")
        index = assignment.get("shard_index")
        if not isinstance(index, int) or index < 0 or index >= shard_count:
            raise StoreShardError(f"receipt {position} has invalid shard index")
        if index in indexes:
            raise StoreShardError(f"duplicate Store shard index {index}")
        indexes.add(index)
        assigned = _require_string_list(
            assignment.get("tests"), f"shard {index} assignments"
        )
        expected_assignment = assigned_tests(inventory, shard_count, index)
        if assigned != expected_assignment:
            raise StoreShardError(
                f"Store shard {index} assignment does not match {ALGORITHM}"
            )
        overlap = union.intersection(assigned)
        if overlap:
            raise StoreShardError(
                "Store shard assignments overlap: " + ", ".join(sorted(overlap))
            )
        union.update(assigned)

        completed = receipt.get("completed")
        if not isinstance(completed, list):
            raise StoreShardError(f"Store shard {index} completed list is invalid")
        completed_names: list[str] = []
        ignored_set = set(ignored)
        for record in completed:
            if not isinstance(record, dict) or set(record) != {
                "name",
                "outcome",
                "duration_ms",
            }:
                raise StoreShardError(
                    f"Store shard {index} has an invalid completion record"
                )
            name = record.get("name")
            duration = record.get("duration_ms")
            if not isinstance(name, str) or name not in assigned:
                raise StoreShardError(
                    f"Store shard {index} completed an unassigned test"
                )
            if not isinstance(duration, int) or duration < 0:
                raise StoreShardError(
                    f"Store shard {index} has an invalid test duration"
                )
            expected_outcome = "ignored" if name in ignored_set else "passed"
            if record.get("outcome") != expected_outcome:
                raise StoreShardError(
                    f"Store shard {index} did not pass assigned test {name}"
                )
            completed_names.append(name)
        if len(completed_names) != len(set(completed_names)):
            raise StoreShardError(f"Store shard {index} completed a test twice")
        if set(completed_names) != set(assigned):
            raise StoreShardError(
                f"Store shard {index} did not complete every assigned test exactly once"
            )
        if receipt.get("started") != assigned:
            raise StoreShardError(
                f"Store shard {index} did not start its exact assignment"
            )
        if receipt.get("result") != "success" or receipt.get("exit_code") != 0:
            raise StoreShardError(f"Store shard {index} did not succeed")
        if receipt.get("errors") != []:
            raise StoreShardError(f"Store shard {index} retained execution errors")
        duration_ms = receipt.get("duration_ms")
        if not isinstance(duration_ms, int) or duration_ms < 0:
            raise StoreShardError(f"Store shard {index} duration is invalid")
        shard_summaries.append(
            {
                "shard_index": index,
                "assigned": len(assigned),
                "duration_ms": duration_ms,
            }
        )

    if indexes != set(range(shard_count)):
        raise StoreShardError("Store shard indexes are incomplete")
    if union != set(inventory):
        missing = sorted(set(inventory) - union)
        extra = sorted(union - set(inventory))
        raise StoreShardError(
            f"Store shard union mismatch: missing={missing}, extra={extra}"
        )
    return {
        "schema": SCHEMA,
        "kind": "store-shard-aggregate",
        "algorithm": ALGORITHM,
        "repository": first["repository"],
        "tested_sha": first["tested_sha"],
        "tested_tree": first["tested_tree"],
        "binary": binary,
        "shard_count": shard_count,
        "inventory_count": len(inventory),
        "ignored_count": len(ignored),
        "shards": sorted(shard_summaries, key=lambda item: int(item["shard_index"])),
        "result": "success",
    }


def load_receipts(paths: Sequence[Path]) -> list[Mapping[str, Any]]:
    receipts: list[Mapping[str, Any]] = []
    for path in sorted(paths):
        try:
            receipt = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise StoreShardError(f"cannot read Store shard receipt {path}: {exc}") from exc
        if not isinstance(receipt, dict):
            raise StoreShardError(f"Store shard receipt {path} is not an object")
        receipts.append(receipt)
    return receipts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.store_shard",
        description="Run or validate deterministic Store contract shards.",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    run = commands.add_parser("run")
    run.add_argument("--binary", required=True, type=Path)
    run.add_argument("--rustc-vv", required=True, type=Path)
    run.add_argument("--receipt", required=True, type=Path)
    run.add_argument("--log", required=True, type=Path)
    run.add_argument("--shard-count", required=True, type=int)
    run.add_argument("--shard-index", required=True, type=int)
    run.add_argument("--repository", default=Path.cwd(), type=Path)

    failure = commands.add_parser("record-failure")
    failure.add_argument("--receipt", required=True, type=Path)
    failure.add_argument("--log", required=True, type=Path)
    failure.add_argument("--shard-count", required=True, type=int)
    failure.add_argument("--shard-index", required=True, type=int)
    failure.add_argument("--error", required=True)
    failure.add_argument("--repository", default=Path.cwd(), type=Path)

    validate = commands.add_parser("validate")
    validate.add_argument("--receipt", action="append", type=Path, default=[])
    validate.add_argument("--receipts-directory", type=Path)
    validate.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)

    try:
        if args.command in {"run", "record-failure"}:
            tested_sha = git_object(args.repository, "HEAD^{commit}")
            tested_tree = git_object(args.repository, "HEAD^{tree}")
            if args.command == "run":
                receipt = run_shard(
                    args.binary,
                    args.rustc_vv,
                    args.receipt,
                    args.log,
                    environment=os.environ,
                    tested_sha=tested_sha,
                    tested_tree=tested_tree,
                    shard_count=args.shard_count,
                    shard_index=args.shard_index,
                )
                return 0 if receipt["result"] == "success" else 1
            record_preexecution_failure(
                args.receipt,
                args.log,
                environment=os.environ,
                tested_sha=tested_sha,
                tested_tree=tested_tree,
                shard_count=args.shard_count,
                shard_index=args.shard_index,
                error=args.error,
            )
            # This evidence step must complete so the always-running artifact
            # upload can retain the receipt; the final step fails the job.
            return 0

        paths = list(args.receipt)
        if args.receipts_directory is not None:
            paths.extend(args.receipts_directory.glob("*-receipt.json"))
        aggregate = validate_receipts(load_receipts(paths))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(
            json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(
            f"validated {aggregate['inventory_count']} Store tests across "
            f"{aggregate['shard_count']} shards"
        )
        return 0
    except (OSError, subprocess.CalledProcessError, StoreShardError) as exc:
        print(f"Store shard failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
