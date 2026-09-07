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
    {
        "cluster-store",
        "cluster-store-legacy",
        "cluster-store-backstop",
        "cluster-topology",
        "cluster-transport-recovery",
    }
)
RESULTS = frozenset({"success", "failure", "cancelled"})
FIRST_WORKFLOW_RUN_ATTEMPT = "1"


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
    evidence_name: str | None = None,
    evidence_digest: str | None = None,
    evidence_bytes: int | None = None,
    evidence_build_sha: str | None = None,
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

    evidence_values = (
        evidence_name,
        evidence_digest,
        evidence_bytes,
        evidence_build_sha,
    )
    has_evidence = any(value is not None for value in evidence_values)
    if has_evidence and not all(value is not None for value in evidence_values):
        raise LaneReceiptError("evidence metadata must be supplied together")
    if (
        lane == "cluster-transport-recovery"
        and result == "success"
        and environment["GITHUB_RUN_ATTEMPT"] != FIRST_WORKFLOW_RUN_ATTEMPT
    ):
        raise LaneReceiptError(
            "successful cluster transport recovery must come from workflow "
            f"run attempt {FIRST_WORKFLOW_RUN_ATTEMPT}; found "
            f"{environment['GITHUB_RUN_ATTEMPT']!r}"
        )
    if (
        lane == "cluster-transport-recovery"
        and result == "success"
        and not has_evidence
    ):
        raise LaneReceiptError(
            "successful cluster transport recovery requires retained evidence"
        )

    evidence: dict[str, object] | None = None
    if has_evidence:
        assert evidence_name is not None
        assert evidence_digest is not None
        assert evidence_bytes is not None
        assert evidence_build_sha is not None
        if not evidence_name or Path(evidence_name).name != evidence_name:
            raise LaneReceiptError("evidence name must be one basename")
        if len(evidence_digest) != 64 or any(
            character not in "0123456789abcdef" for character in evidence_digest
        ):
            raise LaneReceiptError("evidence digest must be a lowercase SHA-256")
        if evidence_bytes < 0:
            raise LaneReceiptError("evidence byte count cannot be negative")
        if evidence_build_sha != tested_sha:
            raise LaneReceiptError(
                f"evidence build sha {evidence_build_sha} does not match "
                f"tested sha {tested_sha}"
            )
        evidence = {
            "name": evidence_name,
            "sha256": evidence_digest,
            "bytes": evidence_bytes,
            "build_sha": evidence_build_sha,
        }

    receipt = {
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
    if evidence is not None:
        receipt["evidence"] = evidence
    return receipt


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


def read_evidence(
    path: Path, validator: Path | None = None
) -> tuple[str, int, str]:
    """Validate, hash, and inspect one immutable read of retained evidence."""

    try:
        contents = path.read_bytes()
        evidence = json.loads(contents)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise LaneReceiptError(f"evidence is not valid JSON: {path}") from exc
    if not isinstance(evidence, dict):
        raise LaneReceiptError(f"evidence must be a JSON object: {path}")
    if validator is not None:
        if not validator.is_file():
            raise LaneReceiptError(f"evidence validator does not exist: {validator}")
        try:
            subprocess.run(
                [str(validator), "validate-transport-recovery-stdin"],
                input=contents,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except subprocess.CalledProcessError as exc:
            detail = exc.stderr.decode("utf-8", errors="replace").strip()
            raise LaneReceiptError(
                "transport recovery evidence failed closed-schema and semantic "
                f"validation: {detail or path}"
            ) from exc
    build_sha = evidence.get("build_sha")
    if not isinstance(build_sha, str) or not build_sha:
        raise LaneReceiptError(f"evidence build_sha is missing: {path}")
    return hashlib.sha256(contents).hexdigest(), len(contents), build_sha


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.ci_lane_receipt",
        description="Write exact-tree evidence for one CI lane.",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--evidence-validator", type=Path)
    parser.add_argument("--lane", required=True)
    parser.add_argument("--result", required=True)
    parser.add_argument("--command", action="append", dest="commands", default=[])
    parser.add_argument("--repository", default=Path.cwd(), type=Path)
    args = parser.parse_args(argv)

    try:
        if not args.log.is_file():
            raise LaneReceiptError(f"lane log does not exist: {args.log}")
        evidence_path = None
        if args.result == "success" and args.evidence and args.evidence.is_file():
            evidence_path = args.evidence
        if args.result == "success" and args.evidence and evidence_path is None:
            raise LaneReceiptError(f"lane evidence does not exist: {args.evidence}")
        if (
            args.result == "success"
            and args.lane == "cluster-transport-recovery"
            and args.evidence_validator is None
        ):
            raise LaneReceiptError(
                "successful cluster transport recovery requires its Rust evidence validator"
            )
        evidence_metadata = (
            read_evidence(evidence_path, args.evidence_validator)
            if evidence_path
            else None
        )
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
            evidence_path.name if evidence_path else None,
            evidence_metadata[0] if evidence_metadata else None,
            evidence_metadata[1] if evidence_metadata else None,
            evidence_metadata[2] if evidence_metadata else None,
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
