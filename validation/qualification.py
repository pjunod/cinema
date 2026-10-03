"""Write an exact-tree receipt for a completed effort qualification run."""

from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path
import subprocess
import sys
from typing import Mapping
import urllib.parse
import urllib.request


class QualificationError(ValueError):
    """The workflow metadata cannot prove a complete qualification."""


REQUIRED_JOBS = frozenset(
    {
        "scope",
        "mobile_version",
        "preflight",
        "rust",
        "windows_compile",
        "cluster_store",
        "cluster_topology",
        "cluster_transport_recovery",
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
FIRST_WORKFLOW_RUN_ATTEMPT = "1"


def promotion_ref(ref: str) -> bool:
    """Only the established promotion branch families can bind qualification."""
    return bool(re.fullmatch(r"[A-Za-z0-9._/-]+", ref)) and (
        ref.startswith("effort/") and len(ref) > len("effort/")
        or ref.startswith("integration/")
        and ref.endswith("-into-main")
        and len(ref) > len("integration/-into-main")
    )


def manual_binding(
    environment: Mapping[str, str],
    pull_request: Mapping[str, object],
    head_tip: str,
    base_tip: str,
    tested_sha: str,
    base_is_ancestor: bool,
) -> dict[str, object]:
    """Validate authenticated API facts; inputs alone never establish PR context."""
    repository = environment.get("GITHUB_REPOSITORY", "")
    number = environment.get("PLURX_PROMOTION_PR", "")
    expected_head = environment.get("PLURX_PROMOTION_HEAD_SHA", "")
    expected_base = environment.get("PLURX_PROMOTION_BASE_SHA", "")
    if environment.get("GITHUB_EVENT_NAME") != "workflow_dispatch":
        raise QualificationError("manual qualification requires workflow_dispatch")
    if environment.get("GITHUB_RUN_ATTEMPT") != "1":
        raise QualificationError("manual qualification requires the first run attempt")
    if not number.isdecimal() or int(number) <= 0:
        raise QualificationError("promotion PR must be a positive integer")
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (expected_head, expected_base)):
        raise QualificationError("promotion head and base must be immutable commit SHAs")
    try:
        head = pull_request["head"]
        base = pull_request["base"]
        assert isinstance(head, dict) and isinstance(base, dict)
        assert pull_request["number"] == int(number)
        assert pull_request["state"] == "open" and pull_request.get("merged") is False
        assert head["repo"]["full_name"] == repository
        assert base["repo"]["full_name"] == repository
        assert promotion_ref(head["ref"]) and base["ref"] == "main"
        assert environment.get("GITHUB_REF") == "refs/heads/" + head["ref"]
        assert head["sha"] == head_tip == expected_head == tested_sha == environment.get("GITHUB_SHA")
        assert base["sha"] == base_tip == expected_base
        assert base_is_ancestor
    except (KeyError, TypeError, AssertionError) as exc:
        raise QualificationError("promotion PR identity, route, immutable tips or ancestry mismatch") from exc
    return dict(schema=1, repository=repository, pull_request=int(number),
                head_ref=head["ref"], base_ref="main", head_sha=head_tip, base_sha=base_tip)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        raise QualificationError("qualification API redirect refused")


def api_document(environment: Mapping[str, str], path: str) -> dict[str, object]:
    """Use the runner's repository token only against its configured API origin."""
    root = environment.get("GITHUB_API_URL", "").rstrip("/")
    server = urllib.parse.urlsplit(environment.get("GITHUB_SERVER_URL", ""))
    parsed = urllib.parse.urlsplit(root)
    repository = environment.get("GITHUB_REPOSITORY", "")
    token = environment.get("GITHUB_TOKEN", "")
    if parsed.scheme not in {"http", "https"} or (parsed.scheme, parsed.netloc) != (server.scheme, server.netloc):
        raise QualificationError("qualification API origin must match the workflow server")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or not token:
        raise QualificationError("authenticated repository API context is missing")
    request = urllib.request.Request(root + "/repos/" + repository + path,
                                     headers={"Authorization": "token " + token})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(request, timeout=15) as response:
        raw = response.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise QualificationError("qualification metadata exceeds the bounded response")
    result = json.loads(raw)
    if not isinstance(result, dict):
        raise QualificationError("qualification API metadata must be an object")
    return result


def resolve_manual_binding(environment: Mapping[str, str], repository: Path) -> dict[str, object]:
    """Read live PR/branch facts and prove current main is already in the head."""
    number = environment.get("PLURX_PROMOTION_PR", "")
    if not number.isdecimal() or int(number) <= 0:
        raise QualificationError("promotion PR must be a positive integer")
    pr = api_document(environment, "/pulls/" + number)
    try:
        head_ref = pr["head"]["ref"]
        if not isinstance(head_ref, str) or not promotion_ref(head_ref):
            raise QualificationError("promotion branch route refused")
        head_tip = api_document(environment, "/branches/" + urllib.parse.quote(head_ref, safe=""))["commit"]["id"]
        base_tip = api_document(environment, "/branches/main")["commit"]["id"]
        ancestry = subprocess.run(["git", "merge-base", "--is-ancestor", base_tip, "HEAD"],
                                  cwd=repository, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
    except (KeyError, TypeError) as exc:
        raise QualificationError("promotion API facts are incomplete") from exc
    return manual_binding(environment, pr, head_tip, base_tip,
                          git_object(repository, "HEAD^{commit}"), ancestry.returncode == 0)


def build_receipt(
    environment: Mapping[str, str],
    results: Mapping[str, str],
    tested_sha: str,
    tested_tree: str,
) -> dict[str, object]:
    """Build a receipt only for a complete effort-to-main success."""

    environment = dict(environment)
    binding = None
    if environment.get("GITHUB_EVENT_NAME") == "workflow_dispatch":
        try:
            binding = json.loads(environment["PLURX_PROMOTION_BINDING"])
            assert binding["schema"] == 1 and binding["repository"] == environment["GITHUB_REPOSITORY"]
            assert binding["head_sha"] == tested_sha == environment["GITHUB_SHA"]
            assert binding["head_sha"] == environment["PLURX_PROMOTION_HEAD_SHA"]
            assert binding["base_sha"] == environment["PLURX_PROMOTION_BASE_SHA"]
            assert str(binding["pull_request"]) == environment["PLURX_PROMOTION_PR"]
        except (KeyError, ValueError, TypeError, AssertionError) as exc:
            raise QualificationError("manual qualification binding is missing or inconsistent") from exc
        environment.update(GITHUB_HEAD_REF=binding["head_ref"], GITHUB_BASE_REF=binding["base_ref"],
                           PLURX_PULL_REQUEST=str(binding["pull_request"]), PLURX_HEAD_SHA=binding["head_sha"],
                           PLURX_BASE_SHA=binding["base_sha"], PLURX_CURRENT_HEAD_SHA=binding["head_sha"],
                           PLURX_CURRENT_BASE_SHA=binding["base_sha"])
    required_metadata = (
        "GITHUB_REPOSITORY",
        "GITHUB_SHA",
        "GITHUB_WORKFLOW_REF",
        "GITHUB_RUN_ID",
        "GITHUB_RUN_ATTEMPT",
        "PLURX_HEAD_SHA",
        "PLURX_BASE_SHA",
        "PLURX_CURRENT_HEAD_SHA",
        "PLURX_CURRENT_BASE_SHA",
    )
    missing_metadata = [
        name for name in required_metadata if not environment.get(name, "")
    ]
    if missing_metadata:
        raise QualificationError(
            "qualification metadata is missing: "
            + ", ".join(missing_metadata)
        )
    if environment["GITHUB_RUN_ATTEMPT"] != FIRST_WORKFLOW_RUN_ATTEMPT:
        raise QualificationError(
            "qualification must come from workflow run attempt "
            f"{FIRST_WORKFLOW_RUN_ATTEMPT}; found "
            f"{environment['GITHUB_RUN_ATTEMPT']!r}"
        )

    for label in ("HEAD", "BASE"):
        event_sha = environment[f"PLURX_{label}_SHA"]
        current_sha = environment[f"PLURX_CURRENT_{label}_SHA"]
        if current_sha != event_sha:
            raise QualificationError(
                f"qualification {label.lower()} moved after workflow start: "
                f"event {event_sha}, current {current_sha}"
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
        "binding_mode": "authenticated-manual-promotion" if binding else "pull-request-event",
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
    parser.add_argument("--output", type=Path)
    parser.add_argument("--bind-manual", action="store_true")
    parser.add_argument("--repository", default=Path.cwd(), type=Path)
    args = parser.parse_args(argv)

    try:
        if args.bind_manual:
            binding = resolve_manual_binding(os.environ, args.repository)
            print("promotion_binding=" + json.dumps(binding, sort_keys=True, separators=(",", ":")))
            print("promotion_pr=" + str(binding["pull_request"]))
            print("promotion_base_sha=" + str(binding["base_sha"]))
            return 0
        if args.output is None:
            raise QualificationError("receipt output is required")
        environment = dict(os.environ)
        if environment.get("GITHUB_EVENT_NAME") == "workflow_dispatch":
            live_binding = resolve_manual_binding(environment, args.repository)
            if json.loads(environment.get("PLURX_PROMOTION_BINDING", "null")) != live_binding:
                raise QualificationError("promotion metadata moved after qualification start")
        raw_results = json.loads(os.environ["PLURX_QUALIFICATION_RESULTS"])
        if not isinstance(raw_results, dict) or not all(
            isinstance(key, str) and isinstance(value, str)
            for key, value in raw_results.items()
        ):
            raise QualificationError(
                "PLURX_QUALIFICATION_RESULTS must be a string-to-string object"
            )
        receipt = build_receipt(
            environment,
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
