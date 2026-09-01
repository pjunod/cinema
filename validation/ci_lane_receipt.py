"""Write exact-tree evidence for one independently scheduled CI lane."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Mapping, Sequence


class LaneReceiptError(ValueError):
    """The lane result cannot be represented as trustworthy evidence."""


LANES = frozenset(
    {"cluster-store", "cluster-store-legacy", "cluster-store-backstop", "cluster-topology"}
)
RESULTS = frozenset({"success", "failure", "cancelled"})


def build_receipt(
    environment: Mapping[str, str],
    lane: str,
    result: str,
    commands: Sequence[str],
    log_name: str,
    log_digest: str,
    log_bytes: int,
    tested_sha: str,
    tested_tree: str,
) -> dict[str, object]:
    """Bind one selected lane's result and log to the checked-out tree."""

    required_metadata = (
        "GITHUB_REPOSITORY",
        "GITHUB_SHA",
        "GITHUB_WORKFLOW_REF",
        "GITHUB_RUN_ID",
        "GITHUB_RUN_ATTEMPT",
        "GITHUB_JOB",
    )
    missing_metadata = [
        name for name in required_metadata if not environment.get(name, "")
    ]
    if missing_metadata:
        raise LaneReceiptError(
            "lane metadata is missing: " + ", ".join(missing_metadata)
        )
    if lane not in LANES:
        raise LaneReceiptError(f"unsupported lane: {lane!r}")
    if result not in RESULTS:
        raise LaneReceiptError(f"unsupported lane result: {result!r}")
    if not commands or any(not command.strip() for command in commands):
        raise LaneReceiptError("at least one non-empty command is required")
    if not log_name or Path(log_name).name != log_name:
        raise LaneReceiptError("log name must be one basename")
    if len(log_digest) != 64 or any(
        character not in "0123456789abcdef" for character in log_digest
    ):
        raise LaneReceiptError("log digest must be a lowercase SHA-256")
    if log_bytes < 0:
        raise LaneReceiptError("log byte count cannot be negative")
    if environment["GITHUB_SHA"] != tested_sha:
        raise LaneReceiptError(
            f"checked-out sha {tested_sha} does not match "
            f"GITHUB_SHA {environment['GITHUB_SHA']}"
        )

    return {
        "schema": 1,
        "kind": "ci-lane",
        "lane": lane,
        "repository": environment["GITHUB_REPOSITORY"],
        "workflow_ref": environment["GITHUB_WORKFLOW_REF"],
        "run_id": environment["GITHUB_RUN_ID"],
        "run_attempt": environment["GITHUB_RUN_ATTEMPT"],
        "job": environment["GITHUB_JOB"],
        "tested_sha": tested_sha,
        "tested_tree": tested_tree,
        "result": result,
        "commands": list(commands),
        "log": {
            "name": log_name,
            "sha256": log_digest,
            "bytes": log_bytes,
        },
    }


def git_object(repository: Path, revision: str) -> str:
    """Resolve one Git object without accepting ambiguous output."""

    completed = subprocess.run(
        ["git", "rev-parse", "--verify", revision],
        cwd=repository,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return completed.stdout.strip()


def file_sha256(path: Path) -> str:
    """Hash a retained lane log without loading it all into memory."""

    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.ci_lane_receipt",
        description="Write exact-tree evidence for one CI lane.",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--lane", required=True)
    parser.add_argument("--result", required=True)
    parser.add_argument("--command", action="append", dest="commands", default=[])
    parser.add_argument("--repository", default=Path.cwd(), type=Path)
    args = parser.parse_args(argv)

    try:
        if not args.log.is_file():
            raise LaneReceiptError(f"lane log does not exist: {args.log}")
        receipt = build_receipt(
            os.environ,
            args.lane,
            args.result,
            args.commands,
            args.log.name,
            file_sha256(args.log),
            args.log.stat().st_size,
            git_object(args.repository, "HEAD^{commit}"),
            git_object(args.repository, "HEAD^{tree}"),
        )
    except (LaneReceiptError, OSError, subprocess.CalledProcessError) as exc:
        print(f"CI lane receipt failed: {exc}", file=sys.stderr)
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        f"recorded {receipt['lane']} {receipt['result']} for "
        f"tree {receipt['tested_tree']}",
        file=sys.stdout,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
