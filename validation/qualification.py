"""Write an exact-tree receipt for a completed effort qualification run."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Mapping


class QualificationError(ValueError):
    """The workflow metadata cannot prove a complete qualification."""


REQUIRED_JOBS = frozenset(
    {
        "scope",
        "mobile_version",
        "preflight",
        "rust",
        "cluster_store",
        "cluster_topology",
        "cluster_wal",
        "cluster_daemon",
        "web_layout",
        "vod_web",
        "android_jvm",
        "apple",
        "android_device",
        "package_smoke",
    }
)


def build_receipt(
    environment: Mapping[str, str],
    results: Mapping[str, str],
    tested_sha: str,
    tested_tree: str,
) -> dict[str, object]:
    """Build a receipt only for a complete effort-to-main success."""

    required_metadata = (
        "GITHUB_REPOSITORY",
        "GITHUB_SHA",
        "GITHUB_WORKFLOW_REF",
        "GITHUB_RUN_ID",
        "GITHUB_RUN_ATTEMPT",
        "PLURX_HEAD_SHA",
        "PLURX_BASE_SHA",
    )
    missing_metadata = [
        name for name in required_metadata if not environment.get(name, "")
    ]
    if missing_metadata:
        raise QualificationError(
            "qualification metadata is missing: "
            + ", ".join(missing_metadata)
        )

    head_ref = environment.get("GITHUB_HEAD_REF", "")
    base_ref = environment.get("GITHUB_BASE_REF", "")
    integration_prefix = "integration/"
    integration_suffix = "-into-main"
    is_effort_head = head_ref.startswith("effort/")
    is_integration_head = (
        head_ref.startswith(integration_prefix)
        and head_ref.endswith(integration_suffix)
        and len(head_ref) > len(integration_prefix) + len(integration_suffix)
    )
    if not (is_effort_head or is_integration_head):
        raise QualificationError(
            "qualification head must match effort/** or "
            f"integration/*-into-main; found {head_ref!r}"
        )
    if base_ref != "main":
        raise QualificationError(
            f"qualification base must be main; found {base_ref!r}"
        )
    missing = REQUIRED_JOBS - results.keys()
    if missing:
        raise QualificationError(
            "qualification results are missing required jobs: "
            + ", ".join(sorted(missing))
        )
    incomplete = {
        name: result for name, result in results.items() if result != "success"
    }
    if incomplete:
        detail = ", ".join(
            f"{name}={result}" for name, result in sorted(incomplete.items())
        )
        raise QualificationError(
            f"qualification requires every full-fan-out job to pass: {detail}"
        )

    event_sha = environment["GITHUB_SHA"]
    if event_sha != tested_sha:
        raise QualificationError(
            f"checked-out sha {tested_sha} does not match GITHUB_SHA {event_sha}"
        )

    pull_request = environment.get("PLURX_PULL_REQUEST", "")
    try:
        pull_request_number = int(pull_request)
    except ValueError as exc:
        raise QualificationError(
            f"pull request number must be an integer; found {pull_request!r}"
        ) from exc

    return {
        "schema": 1,
        "kind": "effort-qualification",
        "repository": environment["GITHUB_REPOSITORY"],
        "pull_request": pull_request_number,
        "head_ref": head_ref,
        "base_ref": base_ref,
        "head_sha": environment["PLURX_HEAD_SHA"],
        "base_sha": environment["PLURX_BASE_SHA"],
        "tested_sha": tested_sha,
        "tested_tree": tested_tree,
        "workflow_ref": environment["GITHUB_WORKFLOW_REF"],
        "run_id": environment["GITHUB_RUN_ID"],
        "run_attempt": environment["GITHUB_RUN_ATTEMPT"],
        "jobs": dict(sorted(results.items())),
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m validation.qualification",
        description="Write the exact-tree evidence for an effort qualification.",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--repository", default=Path.cwd(), type=Path)
    args = parser.parse_args(argv)

    try:
        raw_results = json.loads(os.environ["PLURX_QUALIFICATION_RESULTS"])
        if not isinstance(raw_results, dict) or not all(
            isinstance(key, str) and isinstance(value, str)
            for key, value in raw_results.items()
        ):
            raise QualificationError(
                "PLURX_QUALIFICATION_RESULTS must be a string-to-string object"
            )
        receipt = build_receipt(
            os.environ,
            raw_results,
            git_object(args.repository, "HEAD^{commit}"),
            git_object(args.repository, "HEAD^{tree}"),
        )
    except (KeyError, json.JSONDecodeError, QualificationError) as exc:
        print(f"qualification receipt failed: {exc}", file=sys.stderr)
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        f"qualified {receipt['head_ref']} tree {receipt['tested_tree']}",
        file=sys.stdout,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
