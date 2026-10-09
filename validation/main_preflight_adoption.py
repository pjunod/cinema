"""Bounded legacy main-preflight evidence; input applicability stays explicit.

The legacy decoder does not execute units or grant a CI status. The CLI must
authenticate run/job/source metadata, provide the immutable discovered ID
order, and prove each adopted ID's test/fixture/production inputs applicable.
Unknown input witnesses refuse adoption; they never authorize replay.
"""

from __future__ import annotations

import hashlib
import ast
import fnmatch
import json
import os
import platform
from pathlib import Path
import re
import subprocess
import sys
import unittest
from typing import Mapping, Sequence

from validation.python_unit_receipts import (API, ReceiptError, SourceApplicability,
    artifact_json, atomic_json, bounded_json, discover, require, test_source_path)


LEGACY = {
    "repository": 1,
    "pr": 845,
    "run": 4275,
    "job": 43709,
    "attempt": 1,
    "workflow": "main-fast-lane.yml",
    "commit": "0c5ab4890316da40d44806eba9125ac2e1fead0e",
    "log_bytes": 208075,
    "log_sha256": "272d7509dc94961593f77ad3961f98ee2be097a167baff07e8e1eb0cd0eadd0b",
}

PREUNIT4294 = {
    "repository": 1, "pr": 845, "run": 4294, "job": 43864, "attempt": 1,
    "commit": "289c9c5f3d6c5bdd48ff937c0c76476c95e9b7b2",
    "base": "854c206a7bb1d8b78c203e9d561991507264ee83",
    "log_bytes": 139814,
    "log_sha256": "2db26ec8a677447202a8fa03e3895db12e840c4c931d186e242b7a6a6083fd95",
    "sources": {
        "validation/__init__.py": "347a3ad6f8a7cdabc285fac0bdd906d7c579115edebde81b08fad15426de5b4e",
        ".github/workflows/main-fast-lane.yml": "04d0aca9a4eaf66020f84905506bf0df92cdc203518004fdb4cfc6d86a14ef68",
        "validation/main_preflight_adoption.py": "8bc1e892c5a22fb744939c6898b505c1ff28410c36dddfd61a226e12e0b44a79",
        "validation/main-preflight-inputs.json": "a9554b88e1f98df3716530705197f716f846869499345bd0fc03b2e3219444ec",
        "validation/python_unit_receipts.py": "8b0f9cfb68465095d80fb493e0e833812c3084284d3391758ff1e86bcc73b258",
    },
    "jobs": (
        (43862, "fast-lane validation scope", 16494, "success"),
        (43863, "fast-lane mobile release version", 0, "skipped"),
        (43864, "fast policy and contract preflight", 16495, "failure"),
        (43865, "fast Rust gate", 16496, "skipped"),
        (43866, "fast Windows compile", 0, "skipped"),
        (43867, "fast web syntax gate", 0, "skipped"),
        (43868, "fast Apple compile", 0, "skipped"),
        (43869, "fast Android compile", 0, "skipped"),
        (43870, "Main promotion gate", 16497, "failure"),
    ),
}

PREUNIT4299 = {
    **PREUNIT4294, "run": 4299, "job": 43909,
    "commit": "ef65129def6f8286976f8ff2a18394c0c0183704",
    "log_bytes": 141038,
    "log_sha256": "758d98b2e40c73432f8ca328fda7c5cf8aa1eedf4cf88f2a0e5e7af47f323e2c",
    "sources": {**PREUNIT4294["sources"],
        "validation/main_preflight_adoption.py": "6639de6317fc07a580a563a2d57a24a0257a9668d03b6a7527122177c974e6cc"},
    "jobs": (
        (43907, "fast-lane validation scope", 16504, "success"),
        (43908, "fast-lane mobile release version", 0, "skipped"),
        (43909, "fast policy and contract preflight", 16506, "failure"),
        (43910, "fast Rust gate", 16513, "skipped"),
        (43911, "fast Windows compile", 0, "skipped"),
        (43912, "fast web syntax gate", 0, "skipped"),
        (43913, "fast Apple compile", 0, "skipped"),
        (43914, "fast Android compile", 0, "skipped"),
        (43915, "Main promotion gate", 16515, "failure"),
    ),
}


def run_commands(workflow, job="preflight"):
    """Literal unconditional run steps; comments/disabled steps are not proof."""
    jobs = list(re.finditer(r"(?m)^  ([A-Za-z0-9_]+):\s*$", workflow))
    matches = [i for i, match in enumerate(jobs) if match.group(1) == job]
    require(len(matches) == 1, "Ambiguous workflow job")
    i = matches[0]
    block = workflow[jobs[i].end():jobs[i + 1].start() if i + 1 < len(jobs) else len(workflow)]
    starts = list(re.finditer(r"(?m)^      - ", block))
    commands = []
    for i, start in enumerate(starts):
        step = block[start.start():starts[i + 1].start() if i + 1 < len(starts) else len(block)]
        if re.search(r"(?m)^(?:        |      - )if:", step):
            continue
        lines = step.splitlines()
        for n, line in enumerate(lines):
            match = re.match(r"^(?:        |      - )run:\s*(.*)$", line)
            if not match:
                continue
            value = match.group(1)
            if value in ("|", "|-", ">", ">-"):
                for body in lines[n + 1:]:
                    if body.strip() and len(body) - len(body.lstrip()) <= 8:
                        break
                    if body.strip() and not body.lstrip().startswith("#"):
                        commands.append(body.strip())
            elif value and not value.startswith("#"):
                commands.append(value.strip("\"'"))
    return commands


def bind_event(run, scope, ready=True):
    require(run.get("event") == "pull_request" and isinstance(run.get("event_payload"), str),
            "Prior run lacks authenticated PR event payload")
    event = bounded_json(run["event_payload"].encode())
    pull = event["pull_request"]
    require(event["number"] == pull["number"] == scope["pr"]
            and event["repository"]["id"] == run["repository"]["id"] == scope["repository"]
            and event["repository"]["full_name"] == run["repository"]["full_name"]
            and pull["head"]["repo"]["id"] == pull["base"]["repo"]["id"] == scope["repository"]
            and pull["base"]["ref"] == "main"
            and pull["state"] == "open" and pull["head"]["sha"] == run["commit_sha"]
            and run["workflow_id"] == "main-fast-lane.yml"
            and event["action"] in ("opened", "synchronize", "synchronized", "reopened", "ready_for_review", "converted_to_draft"),
            "Prior event/source/PR/base/readiness binding mismatch")
    if ready:
        require(pull["draft"] is False and event["action"] != "converted_to_draft",
                "Prior attempted preflight was not a ready PR event")
    return event


def prior_query(scope):
    return {"workflow_id": "main-fast-lane.yml", "ref": f"refs/pull/{scope['pr']}/head"}


def terminal_status(metadata: Mapping[str, object]) -> object:
    """Forgejo has returned both keys; disagreements are ambiguous."""
    values = [metadata[key] for key in ("status", "state") if key in metadata]
    require(values and len(set(values)) == 1, "Ambiguous terminal metadata")
    return values[0]


def authenticate_legacy(run: Mapping[str, object], job: Mapping[str, object],
                        log: bytes) -> list[str]:
    """Bind retained raw bytes to the exact prior same-PR successful job."""
    event = bind_event(run, {"repository": 1, "pr": 845})
    require(event["action"] == "reopened"
            and event["pull_request"]["base"]["sha"] == "9023815cb997394de34c10c3dc574f169b6ff011",
            "Legacy original base/action mismatch")
    require(run["id"] == LEGACY["run"]
            and run["repository"]["id"] == LEGACY["repository"]
            and run["workflow_id"] == LEGACY["workflow"]
            and run["commit_sha"] == LEGACY["commit"],
            "Legacy run/source identity mismatch")
    # Overall run cancellation after preflight success does not invalidate
    # the already terminal preflight job. Never infer its status from run.
    require(job["id"] == LEGACY["job"]
            and job["run_id"] == LEGACY["run"]
            and job["repo_id"] == LEGACY["repository"]
            and job["attempt"] == LEGACY["attempt"]
            and job["name"] == "fast policy and contract preflight"
            and terminal_status(job) == "success",
            "Legacy preflight job identity/outcome mismatch")
    require(len(log) == LEGACY["log_bytes"]
            and hashlib.sha256(log).hexdigest() == LEGACY["log_sha256"],
            "Legacy raw log differs from retained authenticated witness")
    lines = [re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line)
             for line in log.decode("utf-8").splitlines()]
    require(any(LEGACY["commit"] + ":refs/remotes/pull/845/head" in line
                for line in lines), "Legacy checkout does not name this PR")
    require(any("/usr/lib/python3.12/ast.py:52:" in line for line in lines)
            and "node: v22.23.2" in lines
            and any("node/22.23.2/x64" in line for line in lines),
            "Legacy Python major/minor/Linux-x64/Node provenance unavailable")
    return lines


def decode_progress(lines: Sequence[str], inventories: Mapping[str, Sequence[str]]) -> dict[str, str]:
    """Decode original verbosity-one events against immutable ID order.

    A successful summary alone is insufficient. Exactly one progress event
    per original discovered ID, exact summary count and exact skipped count
    are required. Skip positions are decoded, never guessed from decorators.
    """
    require(set(inventories) == {"validation", "operations"}, "Incomplete inventory")
    cursor = 0
    outcomes = {}
    for suite, count, skips in (("validation", 293, 0), ("operations", 735, 2)):
        ids = inventories[suite]
        require(len(ids) == count and len(set(ids)) == count,
                "Immutable source inventory count/identity mismatch")
        summaries = [index for index in range(cursor, len(lines))
                     if re.fullmatch(rf"Ran {count} tests in [0-9.]+s", lines[index])]
        require(len(summaries) == 1, "Ambiguous legacy suite summary")
        end = summaries[0]
        # Ordinary checkout/tool messages such as "state..." and "Found..."
        # are not outcomes. In this successful immutable legacy log, progress
        # starts with a dot or an all-progress skip line; dots may precede
        # test subprocess output on the same line. Exact event counts below
        # refuse any unrecognized/interleaved event shape.
        events = "".join(match.group(0) for line in lines[cursor:end]
                         if (line.startswith(".") or re.fullmatch(r"s[.s]*", line)
                             or line.startswith("s."))
                         and (match := re.match(r"^[.s]+", line)))
        require(len(events) == count and set(events) <= {".", "s"}
                and events.count("s") == skips, "Legacy individual outcomes incomplete")
        following = [line for line in lines[end + 1:end + 5] if line]
        expected = "OK" if not skips else f"OK (skipped={skips})"
        require(following and following[0] == expected, "Legacy summary did not succeed")
        for identity, outcome in zip(ids, events):
            outcomes[suite + ":" + identity] = "success" if outcome == "." else "skipped"
        cursor = end + 1
    return outcomes


def select_adoption(outcomes: Mapping[str, str], applicability: Mapping[str, bool | None]) -> dict[str, str]:
    """Unknown applicability blocks before any previously passed ID replays."""
    unknown = [identity for identity in outcomes if applicability.get(identity) is None]
    require(not unknown, "Legacy input applicability unavailable; preserve evidence, do not replay: "
            + ", ".join(sorted(unknown)[:3]))
    return {identity: outcome for identity, outcome in outcomes.items()
            if applicability[identity] is True}


MANIFEST = Path("validation/main-preflight-inputs.json")
JOURNAL = Path(".main-preflight-receipts/receipt.json")
NODE = (
    "tests/playback/player-input-contract.test.js",
    "tests/playback/web-policy.test.js",
    "tests/playback/web-control.test.js",
    "tests/web/seek-telemetry.test.js",
    "tests/web/player-dom.test.js",
    "tests/web/read-after.test.js",
    "tests/web/live-tv.test.js",
)
_TREE_CACHE = {}
_DIGEST_CACHE = {}
_HISTORY_CACHE = {}
_ENVIRONMENT = None


def git(*arguments):
    return subprocess.check_output(["git", *arguments], text=True, timeout=10).strip()


def inventory(commit):
    """Reconstruct standard TestCase and local fixture-base discovery.

    Refuse dynamic/imported bases and custom execution semantics. Local
    inheritance is resolved from immutable AST, without importing tests.
    SourceApplicability independently checks each method/local fixture.
    """
    result = {}
    for suite in ("validation", "operations"):
        ids = []
        paths = git("ls-tree", "-r", "--name-only", commit, "tests/" + suite).splitlines()
        for path in sorted(p for p in paths if re.fullmatch(
                rf"tests/{suite}/test_[A-Za-z0-9_]+\.py", p)):
            tree = ast.parse(git("show", commit + ":" + path))
            require(not any(isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
                            and n.name == "load_tests" for n in tree.body),
                    "Legacy inventory has dynamic discovery")
            classes = {n.name: n for n in tree.body if isinstance(n, ast.ClassDef)}
            def methods_of(name, seen):
                require(name not in seen and len(seen) < 16,
                        "Unsupported legacy local inheritance graph")
                node = classes[name]
                require(not node.keywords,
                        "Unsupported legacy inventory class " + name)
                require(not any(isinstance(n, ast.FunctionDef) and n.name in
                                ("id", "run", "__init__", "__getattribute__", "__getattr__") for n in node.body),
                        "Legacy class overrides loader identity/execution")
                inherited, is_case = set(), False
                for base in node.bases:
                    if isinstance(base, ast.Name) and base.id in classes:
                        names, local_case = methods_of(base.id, seen | {name})
                        inherited.update(names)
                        is_case |= local_case
                    elif isinstance(base, ast.Name) and base.id == "object":
                        continue
                    else:
                        require(isinstance(base, ast.Attribute) and isinstance(base.value, ast.Name)
                                and base.value.id == "unittest" and base.attr == "TestCase",
                                "Unsupported imported/dynamic legacy inventory base " + name)
                        is_case = True
                declared = [n.name for n in node.body if isinstance(n, ast.FunctionDef)
                            and n.name.startswith("test")]
                require(len(declared) == len(set(declared)), "Duplicate legacy test declaration")
                return inherited | set(declared), is_case
            for name, node in sorted(classes.items()):
                # Helper classes outside the standard unittest hierarchy have
                # no methods and are not loader candidates.
                if not any(isinstance(n, ast.FunctionDef) and n.name.startswith("test")
                           for n in node.body) and not any(isinstance(b, ast.Name) and b.id in classes
                                                         for b in node.bases):
                    continue
                methods, is_case = methods_of(name, set())
                if not is_case or not methods:
                    continue
                ids.extend(Path(path).stem + "." + name + "." + method for method in sorted(methods))
        result[suite] = ids
    return result


def witness(identity, families):
    matches = [row for row in families
               if (identity.startswith(row["id_prefix"]) if row["id_prefix"].endswith(".")
                   else identity == row["id_prefix"])]
    require(matches, "Missing reviewed input witness: " + identity)
    size = max(len(row["id_prefix"]) for row in matches)
    exact = [row for row in matches if len(row["id_prefix"]) == size]
    require(len(exact) == 1, "Ambiguous input witness: " + identity)
    row = exact[0]
    require(row["reason"] and row["inputs"], "Empty input witness: " + identity)
    if identity.startswith("node:"):
        script = identity.removeprefix("node:")
        return {**row, "inputs": [*row["inputs"], script] if script not in row["inputs"] else row["inputs"]}
    return row


def matches_input(path, pattern):
    """Path globs: ordinary wildcards stay within one path component."""
    components, glob = path.split("/"), pattern.split("/")
    require(glob.count("**") <= 8 and len(pattern) <= 1024,
            "Input glob expansion bound exhausted")
    memo = {}
    def match(i, j):
        if (i, j) in memo:
            return memo[i, j]
        if i == len(glob):
            result = j == len(components)
        elif glob[i] == "**":
            result = match(i + 1, j) or (j < len(components) and match(i, j + 1))
        else:
            result = (j < len(components) and fnmatch.fnmatchcase(components[j], glob[i])
                      and match(i + 1, j + 1))
        memo[i, j] = result
        return result
    return match(0, 0)


def input_digest(commit, row):
    key = (commit, json.dumps(row, sort_keys=True))
    if key in _DIGEST_CACHE:
        return _DIGEST_CACHE[key]
    if commit not in _TREE_CACHE:
        require(len(_TREE_CACHE) < 64, "Input tree-cache bound exhausted")
        files = {}
        for entry in git("ls-tree", "-r", "-z", commit).split("\0"):
            if entry:
                header, path = entry.split("\t", 1)
                files[path] = tuple(header.split())
        _TREE_CACHE[commit] = files
    files = _TREE_CACHE[commit]
    paths = list(files)
    patterns = row["inputs"]
    require(isinstance(patterns, list) and patterns
            and all(isinstance(p, str) and p and not p.startswith("/")
                and ".." not in p.split("/") for p in patterns), "Unsafe input path")
    selected = sorted(p for p in paths if any(matches_input(p, pattern) for pattern in patterns))
    payload = [(path, files[path]) for path in selected]
    pathsets = row.get("pathsets", [])
    require(isinstance(pathsets, list)
            and all(isinstance(p, str) and p and not p.startswith("/")
                and ".." not in p.split("/") for p in pathsets), "Unsafe path-set pattern")
    membership = sorted(path for path in paths if any(matches_input(path, pattern)
                        for pattern in pathsets))
    # The pattern list and membership bind absent/deleted/new matching files.
    if row.get("history"):
        if commit not in _HISTORY_CACHE:
            _HISTORY_CACHE[commit] = (git("log", "--format=%H:%ct:%s", commit),
                                    git("rev-parse", "--is-shallow-repository"))
        history, shallow = _HISTORY_CACHE[commit]
        payload.extend((("HEAD-history", history), ("shallow", shallow)))
    result = hashlib.sha256(json.dumps([patterns, payload, pathsets, membership], sort_keys=True).encode()).hexdigest()
    _DIGEST_CACHE[key] = result
    return result


def environment():
    global _ENVIRONMENT
    if _ENVIRONMENT is None:
        _ENVIRONMENT = {"platform": sys.platform, "python": list(sys.version_info[:2]),
                        "machine": platform.machine(),
                        "node": subprocess.check_output(["node", "--version"], text=True).strip()}
    return _ENVIRONMENT


def api_context():
    event = bounded_json(Path(os.environ["GITHUB_EVENT_PATH"]).read_bytes())
    pull = event["pull_request"]
    repo = event["repository"]
    require(os.environ.get("GITHUB_EVENT_NAME") == "pull_request"
            and pull["head"]["repo"]["id"] == pull["base"]["repo"]["id"] == repo["id"]
            and event["number"] == pull["number"] and pull["draft"] is False
            and pull["state"] == "open" and pull["base"]["ref"] == "main"
            and repo["full_name"] == os.environ["GITHUB_REPOSITORY"], "Not a ready same-repository main PR")
    commit = git("rev-parse", "HEAD")
    require(commit == pull["head"]["sha"] == os.environ["GITHUB_SHA"], "Checkout/source mismatch")
    require(not git("diff", "--name-only", "HEAD"), "Current tracked source differs from prepared commit")
    require(os.environ.get("GITHUB_RUN_ATTEMPT", "1") == "1", "Use a new attempt, not rerun")
    root = os.environ["GITHUB_API_URL"]
    require(root.startswith(os.environ["GITHUB_SERVER_URL"].rstrip("/") + "/"),
            "CI API origin mismatch")
    api = API(root, os.environ["GITHUB_REPOSITORY"], os.environ.get("GITHUB_TOKEN"))
    scope = {"repository": repo["id"], "pr": pull["number"]}
    run_id = int(os.environ["GITHUB_RUN_ID"])
    current = api.get(f"/actions/runs/{run_id}")
    require(current["id"] == run_id and current["commit_sha"] == commit,
            "Current API run ID/source mismatch")
    require(bind_event(current, scope) == event, "Current API/event payload mismatch")
    live = api.get(f"/pulls/{pull['number']}")
    require(live["head"]["sha"] == commit and live["base"]["sha"] == pull["base"]["sha"]
            and live["draft"] is False and live["state"] == "open"
            and live["head"]["repo"]["id"] == live["base"]["repo"]["id"] == repo["id"]
            and live["base"]["ref"] == "main", "Live PR moved/closed/drafted; do not execute stale units")
    return api, scope, commit, run_id


JOURNAL_FIELDS = {"version", "scope", "run", "commit", "job", "attempt", "environment",
                  "producer_blob", "manifest_blob", "outcomes", "skips", "phase_errors"}
RECORD_FIELDS = {"commit", "run", "job", "attempt", "outcome", "environment", "inputs"}


def validate_journal(journal, scope, prior, job, env):
    require(set(journal) == JOURNAL_FIELDS and journal["version"] == 1
            and journal["scope"] == scope and journal["run"] == prior["id"]
            and journal["commit"] == prior["commit_sha"]
            and journal["job"] == job["id"] and journal["attempt"] == job["attempt"] == 1
            and journal["environment"] == env,
            "Journal schema/source/job/attempt/environment mismatch")
    require(job["run_id"] == prior["id"] and job["repo_id"] == scope["repository"]
            and job["name"] == "fast policy and contract preflight"
            and terminal_status(job) in ("success", "failure", "cancelled"),
            "Journal producer job is not an authenticated terminal preflight")
    require(isinstance(journal["phase_errors"], list) and not journal["phase_errors"],
            "Prior fixture/import/discovery failure needs evidence recovery; do not replay successes")
    require(isinstance(journal["outcomes"], dict) and isinstance(journal["skips"], dict),
            "Journal outcome map malformed")
    for outcome, records in (("success", journal["outcomes"]), ("skipped", journal["skips"])):
        for identity, record in records.items():
            require(isinstance(identity, str) and identity.startswith(("validation:", "operations:", "node:"))
                    and set(record) == RECORD_FIELDS and record["outcome"] == outcome
                    and record["attempt"] == 1 and record["environment"] == env
                    and isinstance(record["run"], int) and record["run"] > 0
                    and isinstance(record["job"], int) and record["job"] > 0
                    and re.fullmatch(r"[0-9a-f]{40}", record["commit"])
                    and re.fullmatch(r"[0-9a-f]{64}", record["inputs"]),
                    "Journal individual record schema mismatch")


def require_missing_journal_safe(prior, jobs, scope):
    """Only authenticated all-skipped runs prove no unit execution."""
    bind_event(prior, scope, ready=False)
    require(jobs and all(job["run_id"] == prior["id"]
                        and job["repo_id"] == scope["repository"]
                        and job["attempt"] == 1 for job in jobs),
            "Missing-journal job inventory/source unavailable")
    if all(terminal_status(job) == "skipped" for job in jobs):
        return
    bind_event(prior, scope)
    preflight = [job for job in jobs if job["name"] == "fast policy and contract preflight"]
    require(len(preflight) == 1 and terminal_status(preflight[0]) == "skipped",
            f"Missing final journal for prior attempt {prior['id']}; preserve possibly passed IDs")



def recover_missing_final_before_node(api, scope, prior, jobs, workflow):
    """Account a terminal Python-phase interruption, granting no Node outcomes.

    The successful Python-final upload is a mandatory predecessor of Node.
    Its absence is useful only with the immutable step order, both published
    start markers, a completed framed Python journal, and the terminal log.
    Generic Python continuation separately authenticates its positive records.
    """
    from validation import main_unit_receipts as main
    rid, commit = prior['id'], prior['commit_sha']
    if jobs and all(terminal_status(job) == 'skipped' for job in jobs):
        return False  # The existing skipped-history validator authenticates it.
    matches = [job for job in jobs if job['name'] == main.JOB]
    require(len(matches) == 1, "Ambiguous interrupted preflight")
    job = matches[0]
    if terminal_status(job) not in ('failure', 'cancelled') or job.get('task_id') == 0:
        return False
    event = bind_event(prior, scope)
    require(job['run_id'] == rid and job['repo_id'] == scope['repository']
            and job['attempt'] == 1 and type(job.get('task_id')) is int
            and job['task_id'] > 0, "Interrupted preflight identity/attempt mismatch")
    local_workflow = main.source(commit, '.github/workflows/main-fast-lane.yml')
    require(workflow == local_workflow, "Interrupted workflow/source mismatch")
    blocks = list(re.finditer(r'(?m)^  ([A-Za-z0-9_]+):\s*$', workflow.decode()))
    selected = [i for i, block in enumerate(blocks) if block.group(1) == 'preflight']
    require(len(selected) == 1, "Ambiguous interrupted workflow job")
    i = selected[0]
    block = workflow.decode()[blocks[i].end():blocks[i + 1].start() if i + 1 < len(blocks) else len(workflow.decode())]
    steps = re.split(r'(?m)(?=^      - )', block)[1:]
    executor = 'run: python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations'
    positions = [i for i, step in enumerate(steps) if executor in step]
    require(len(positions) == 1 and positions[0] + 2 < len(steps), "Missing Python/Node upload barrier")
    upload, node = steps[positions[0] + 1:positions[0] + 3]
    # Admit the canonical step shapes, not substring matches on YAML keys.
    # Alternative/duplicate control keys or merges must never bypass the upload.
    expected_upload = """      - name: Preserve main Python success journal even on unit failure
        if: always() && steps.receipts.outcome == 'success'
        uses: https://data.forgejo.org/forgejo/upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e # v4
        with:
          name: ${{ steps.receipts.outputs.receipt_key }}
          path: .main-python-unit-receipts/receipt.json
          if-no-files-found: error
          retention-days: 90"""
    expected_node = """      - name: Check the shared player input contract
        env:
          GITHUB_TOKEN: ${{ github.token }}
        run: python3 -m validation.main_preflight_adoption node"""
    require(upload.rstrip() == expected_upload and node.rstrip() == expected_node,
            "Node is not behind the mandatory successful Python upload")
    raw = api.bytes(f"/actions/jobs/{job['id']}/logs")
    lines = main.log_lines(raw)
    snapshots = main.read_snapshots(lines)
    require(set(snapshots) == {'start', 'final'}, "Interrupted Python journal is incomplete")
    python_scope = dict(scope, branch=event['pull_request']['head']['ref'],
                        base='main', workflow=main.WORKFLOW)
    for phase in ('start', 'final'):
        main.receipts.validate_journal(snapshots[phase], python_scope, rid, commit, completed=phase == 'final')
    require(snapshots['start']['complete'] is False
            and all(snapshots['final']['passes'].get(test) == value
                    for test, value in snapshots['start']['passes'].items()), "Interrupted journal inheritance mismatch")
    python_key = main.key(python_scope)
    adoption_key = f"main-preflight-v1-r{scope['repository']}-pr{scope['pr']}"
    for name in (python_key, adoption_key):
        require(not any(a['run_id'] == rid for a in api.pages('/actions/artifacts', {'name': name})),
                "Interrupted preflight has a final artifact")
        markers = api.pages('/actions/artifacts', {'name': name + f'-start-{rid}'})
        require(len(markers) == 1 and markers[0]['run_id'] == rid and not markers[0]['expired']
                and markers[0]['name'] == name + f'-start-{rid}',
                "Interrupted preflight start marker unavailable")
        start = artifact_json(api.bytes(f"/actions/artifacts/{markers[0]['id']}/zip"))
        if name == python_key:
            require(start == snapshots['start'], "Python artifact/log start mismatch")
        else:
            validate_journal(start, scope, prior, job, environment())
            require(start['producer_blob'] == git('rev-parse', commit + ':validation/main_preflight_adoption.py')
                    and start['manifest_blob'] == git('rev-parse', commit + ':' + str(MANIFEST)),
                    "Interrupted adoption start producer mismatch")
        require(lines.count(f"Artifact {name}-start-{rid} has been successfully uploaded!") == 1,
                "Interrupted preflight prepare/start publication unavailable")
        require(not any(f'Artifact {name} has been successfully uploaded!' in line for line in lines),
                "Final upload completed; Node execution needs its own evidence")
    end = lines.index('MAIN-UNIT-END final')
    require(sum(commit in line for line in lines[:end]) >= 2
            and any('triggered by event: pull_request' in line for line in lines[:end])
            and any('context deadline exceeded' in line for line in lines[end + 1:])
            and not any(line.startswith('Main preflight outcome ') for line in lines)
            and not any('MAIN-UNIT-' in line for line in lines[end + 1:]),
            "Interrupted terminal log does not prove the Python-phase timeout")
    return True

def recover_preunit4294(api, scope, prior, jobs):
    """One authenticated early refusal; import neither outcomes nor a journal."""
    return recover_preunit(api, scope, prior, jobs, PREUNIT4294,
                          "Prior event/source/PR/base/readiness binding mismatch")


def recover_preunit4299(api, scope, prior, jobs):
    """Exact environment refusal, before any Python/Node unit or journal."""
    return recover_preunit(api, scope, prior, jobs, PREUNIT4299,
                          "Legacy Linux/Python/Node environment applicability unavailable")


def recover_preunit_upload4324(api, scope, prior, jobs):
    """Share exact zero-execution proof; import no Node/Python outcomes."""
    from validation import main_unit_receipts as main
    if prior['id'] != main.PREUNIT_UPLOAD4324['run']:
        return False
    matches = [job for job in jobs if job['name'] == main.JOB]
    require(len(matches) == 1, 'Upload4324 preflight job unavailable')
    return main.recover_preunit_upload4324(api, scope, prior, matches[0]) is not None


def recover_preunit4431(api, scope, prior, jobs):
    """Share both-attempt zero-execution proof without constructing outcomes."""
    from validation import main_unit_receipts as main
    return main.recover_preunit4431(api, scope, prior, jobs)


def recover_preunit(api, scope, prior, jobs, proof, reason):
    if scope != {"repository": proof["repository"], "pr": proof["pr"]} or prior["id"] != proof["run"]:
        return False
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual["id"] == prior["id"] == proof["run"]
            and actual["commit_sha"] == prior["commit_sha"] == proof["commit"]
            and terminal_status(actual) == "failure",
            "Prepare4294 exact terminal run/source mismatch")
    event = bind_event(actual, scope)
    require(event == bind_event(prior, scope) and event["action"] == "synchronized"
            and event["pull_request"]["base"]["sha"] == proof["base"],
            "Prepare4294 original event/base mismatch")
    require(len(jobs) == len(proof["jobs"])
            and all(job["run_id"] == proof["run"] and job["repo_id"] == proof["repository"]
                    and job["attempt"] == proof["attempt"] for job in jobs)
            and tuple(sorted((job["id"], job["name"], job["task_id"], terminal_status(job))
                             for job in jobs)) == proof["jobs"],
            "Prepare4294 complete terminal job/attempt inventory mismatch")
    for path, expected in proof["sources"].items():
        original = api.bytes("/raw/" + path, {"ref": proof["commit"]})
        require(hashlib.sha256(original).hexdigest() == expected,
                "Prepare4294 immutable producer/workflow/source mismatch")
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(len(raw) == proof["log_bytes"] and hashlib.sha256(raw).hexdigest() == proof["log_sha256"],
            "Prepare4294 original raw log mismatch")
    lines = [re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line)
             for line in raw.decode("utf-8").splitlines()]
    refusal = "Main preflight receipt refusal: " + reason
    start = "skipping post step for 'Publish preflight attempt-start journal'; main step was skipped"
    final = "skipping post step for 'Preserve per-ID preflight journal even on failure'; main step was skipped"
    log = "\n".join(lines)
    require(raw.endswith(b"\n") and proof["commit"] + ":refs/remotes/pull/845/head" in log
            and lines.count(refusal) == lines.count(start) == lines.count(final) == 1
            and lines.index(refusal) < min(lines.index(start), lines.index(final))
            and lines[-1] == "Job 'fast policy and contract preflight' failed"
            and not any(marker in log for marker in (
                "Main preflight outcome ", "Authenticated candidates=", "discovered=", "pending=",
                "has been successfully uploaded!", "... ok", "... FAIL", "... ERROR"))
            and not any(re.fullmatch(r"Ran \d+ tests in .*", line) for line in lines),
            "Prepare4294 contradicts reviewed zero-unit phase evidence")
    name = "main-preflight-v1-r1-pr845"
    require(api.get(f"/actions/runs/{proof['run']}/artifacts") == []
            and not any(artifact["run_id"] == proof["run"] for artifact in
                        api.pages("/actions/artifacts", {"name": name}))
            and not api.pages("/actions/artifacts", {"name": name + f"-start-{proof['run']}"}),
            "Prepare4294 unexpectedly has start/final/run artifacts")
    print(f"Recovered exact prepare refusal run{proof['run']}/job{proof['job']}: zero units, no outcomes or journal imported")
    return True


def require_record_origin(record, journal, original):
    own = all(record[key] == journal[key] for key in ("run", "commit", "job", "attempt", "environment"))
    immutable = RECORD_FIELDS - {"inputs"}
    require(own or (isinstance(original, dict)
                    and all(record[key] == original[key] for key in immutable)),
            "Inherited outcome lacks authenticated original provenance")


def outcome_event(identity, record):
    return "Main preflight outcome " + json.dumps({"id": identity, "record": record}, sort_keys=True)


def record_phase_error(journal, phase, message):
    journal["phase_errors"].append({"phase": phase, "error": message})
    atomic_json(JOURNAL, journal)


def adapter_workflow(commands):
    old = all("python3 -m validation.main_preflight_adoption " + phase in commands
              for phase in ("prepare", "validation", "operations", "node"))
    bridge = ("python3 -m validation.main_unit_receipts prepare" in commands
              and "python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations" in commands
              and "python3 -m validation.main_preflight_adoption node" in commands)
    return old or bridge


def prepare(output_key="receipt_key"):
    api, scope, commit, run_id = api_context()
    require(not JOURNAL.parent.is_symlink(), "Receipt directory symlink")
    document = bounded_json(MANIFEST.read_bytes())
    require(document["version"] == 1, "Unknown input manifest")
    families = document["families"]
    name = f"main-preflight-v1-r{scope['repository']}-pr{scope['pr']}"
    candidates = {}
    authenticated_runs = set()
    skipped_history = {}
    env = environment()
    if scope == {"repository": 1, "pr": 845}:
        run = api.get("/actions/runs/4275")
        jobs = api.pages("/actions/runs/4275/jobs")
        matching = [job for job in jobs if job["id"] == 43709]
        require(len(matching) == 1, "Legacy preflight job unavailable")
        raw = api.bytes("/actions/jobs/43709/logs")
        lines = authenticate_legacy(run, matching[0], raw)
        authenticated_runs.add(LEGACY["run"])
        outcomes = decode_progress(lines, inventory(LEGACY["commit"]))
        require(env == {"platform": "linux", "machine": "x86_64", "python": [3, 12], "node": "v22.23.2"},
                "Legacy Linux/Python/Node environment applicability unavailable")
        for identity, outcome in outcomes.items():
            record = {"commit": LEGACY["commit"], "run": 4275, "job": 43709, "attempt": 1,
                      "outcome": outcome, "environment": env,
                      "inputs": input_digest(LEGACY["commit"], witness(identity, families))}
            if outcome == "skipped":
                skipped_history[identity] = record
            else:
                candidates[identity] = record
        # Successful raw job and immutable workflow prove every listed command
        # completed. Scripts stay distinct identities, not invented test IDs.
        legacy_commands = run_commands(git("show", LEGACY["commit"] + ":.github/workflows/main-fast-lane.yml"))
        for script in NODE:
            command = "node --test " + script if script.endswith("seek-telemetry.test.js") else "node " + script
            require(command in legacy_commands,
                    "Legacy Node command absent")
            candidates["node:" + script] = {"commit": LEGACY["commit"], "run": 4275,
                "job": 43709, "attempt": 1, "outcome": "success", "environment": env,
                "inputs": input_digest(LEGACY["commit"], witness("node:" + script, families))}
    artifacts = api.pages("/actions/artifacts", {"name": name})
    indexed_runs = {artifact["run_id"] for artifact in artifacts}
    prior_runs = api.pages("/actions/runs", prior_query(scope), "workflow_runs")
    for prior in prior_runs:
        if prior["id"] == run_id or prior["id"] in indexed_runs:
            continue
        workflow = api.bytes("/raw/.github/workflows/main-fast-lane.yml", {"ref": prior["commit_sha"]})
        if adapter_workflow(run_commands(workflow.decode())):
            jobs = api.pages(f"/actions/runs/{prior['id']}/jobs")
            if (recover_preunit4294(api, scope, prior, jobs)
                    or recover_preunit4299(api, scope, prior, jobs)
                    or recover_preunit_upload4324(api, scope, prior, jobs)
                    or recover_preunit4431(api, scope, prior, jobs)):
                authenticated_runs.add(prior["id"])
                continue
            from validation.main_source_skew4513 import recover as recover_source_skew
            if recover_source_skew(api, scope, prior, jobs):
                authenticated_runs.add(prior['id'])
                continue
            from validation.main_prepare_refusal_recovery import recover
            if recover(api, scope, prior, jobs):
                authenticated_runs.add(prior['id'])
                continue
            if recover_missing_final_before_node(api, scope, prior, jobs, workflow):
                authenticated_runs.add(prior["id"])
                continue
            require_missing_journal_safe(prior, jobs, scope)
            authenticated_runs.add(prior["id"])
    require(len({a["run_id"] for a in artifacts}) == len(artifacts), "Ambiguous duplicate journals")
    for artifact in sorted(artifacts, key=lambda a: a["run_id"]):
        require(artifact["name"] == name and not artifact["expired"], "Unavailable prior journal")
        require(artifact["run_id"] != run_id, "Current run already has a journal")
        prior = api.get(f"/actions/runs/{artifact['run_id']}")
        require(prior["repository"]["id"] == scope["repository"]
                and prior["workflow_id"] == "main-fast-lane.yml", "Untrusted receipt run")
        bind_event(prior, scope)
        journal = artifact_json(api.bytes(f"/actions/artifacts/{artifact['id']}/zip"))
        producer_commands = run_commands(git("show", journal["commit"] + ":.github/workflows/main-fast-lane.yml"))
        require(adapter_workflow(producer_commands)
                and journal["producer_blob"] == git("rev-parse", journal["commit"] + ":validation/main_preflight_adoption.py")
                and journal["manifest_blob"] == git("rev-parse", journal["commit"] + ":" + str(MANIFEST)),
                "Unknown workflow/journal producer")
        jobs = api.pages(f"/actions/runs/{artifact['run_id']}/jobs")
        preflight = [job for job in jobs if job["name"] == "fast policy and contract preflight"]
        require(len(preflight) == 1, "Ambiguous prior job")
        validate_journal(journal, scope, prior, preflight[0], env)
        log = api.bytes(f"/actions/jobs/{preflight[0]['id']}/logs").decode()
        require(journal["commit"] + f":refs/remotes/pull/{scope['pr']}/head" in log,
                "Journal PR source identity unavailable")
        publication = f"Artifact {name} has been successfully uploaded!"
        require(log.count(publication) == 1, "Final journal publication absent or ambiguous")
        starts = api.pages("/actions/artifacts", {"name": name + "-start-" + str(prior["id"])})
        require(len(starts) == 1 and starts[0]["run_id"] == prior["id"]
                and starts[0]["name"] == name + "-start-" + str(prior["id"])
                and not starts[0]["expired"], "Prior start journal unavailable")
        start = artifact_json(api.bytes(f"/actions/artifacts/{starts[0]['id']}/zip"))
        validate_journal(start, scope, prior, preflight[0], env)
        require(all(start[key] == journal[key] for key in JOURNAL_FIELDS - {"outcomes", "skips"}),
                "Start/final journal identity mismatch")
        require(all(journal["outcomes"].get(identity) == record
                    for identity, record in start["outcomes"].items()),
                "Final journal dropped/changed an inherited positive")
        require(log.count(f"Artifact {starts[0]['name']} has been successfully uploaded!") == 1,
                "Start journal publication absent or ambiguous")
        original_families = json.loads(git("show", journal["commit"] + ":" + str(MANIFEST)))["families"]
        for identity, record in journal["outcomes"].items():
            require(record["outcome"] == "success", "Only positive outcomes may carry")
            require_record_origin(record, journal, candidates.get(identity))
            if record["run"] == journal["run"]:
                require(log.count(outcome_event(identity, record)) == 1,
                        "Own success lacks exactly one original executable outcome event")
            require(record["environment"] == journal["environment"]
                    and record["inputs"] == input_digest(record["commit"], witness(identity, original_families)),
                    "Original-source declared input digest mismatch")
            candidates[identity] = record
        for identity, record in journal["skips"].items():
            require_record_origin(record, journal, skipped_history.get(identity))
            if record["run"] == journal["run"]:
                require(log.count(outcome_event(identity, record)) == 1,
                        "Own skip lacks original executable outcome event")
        skipped_history.update(journal["skips"])
        authenticated_runs.add(prior["id"])
    source = SourceApplicability(commit)
    current_test_paths = set(git("ls-tree", "-r", "--name-only", commit, "tests").splitlines())
    adopted = {}
    missing = []
    for identity, record in candidates.items():
        if not identity.startswith("node:") and test_source_path(identity)[0] not in current_test_paths:
            continue  # Authenticated tracked tree proves the entire module was removed.
        if not identity.startswith("node:") and source.fingerprint(commit, identity) is None:
            continue  # Removed IDs retain history; they are not current pending work.
        try:
            row = witness(identity, families)
        except ReceiptError:
            missing.append(identity)
            continue
        same = record["environment"] == env and input_digest(commit, row) == input_digest(record["commit"], row)
        if not identity.startswith("node:"):
            same = source(identity, {"commit": record["commit"], "run": record["run"]}) and same
        if same:
            adopted[identity] = {**record, "inputs": input_digest(record["commit"], row)}
        else:
            print("Changed declared inputs/local fixture; execute current control: " + identity)
    require(not missing, "Missing reviewed input witnesses; do not replay positives: " + ", ".join(missing))
    source.finish(adopted)
    print(f"Authenticated candidates={len(candidates)}, adopted={len(adopted)}, invalidated-or-removed={len(candidates)-len(adopted)}")
    current_jobs = api.pages(f"/actions/runs/{run_id}/jobs")
    current_job = [job for job in current_jobs if job["name"] == "fast policy and contract preflight"]
    require(len(current_job) == 1 and current_job[0]["attempt"] == 1, "Current job identity unavailable")
    journal = {"version": 1, "scope": scope, "run": run_id, "commit": commit,
               "job": current_job[0]["id"], "attempt": 1, "environment": env,
               "producer_blob": git("rev-parse", commit + ":validation/main_preflight_adoption.py"),
               "manifest_blob": git("rev-parse", commit + ":" + str(MANIFEST)),
               "outcomes": adopted, "skips": skipped_history, "phase_errors": []}
    atomic_json(JOURNAL, journal)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(output_key + "=" + name + "\n")
    return candidates, authenticated_runs


def execute_phase(phase):
    _api, scope, commit, run = api_context()
    journal = bounded_json(JOURNAL.read_bytes())
    require(not git("diff", "--name-only", "HEAD"), "Current tracked source differs from prepared commit")
    require(set(journal) == JOURNAL_FIELDS and journal["version"] == 1
            and journal["scope"] == scope and journal["commit"] == commit and journal["run"] == run
            and journal["attempt"] == 1 and journal["environment"] == environment()
            and journal["producer_blob"] == git("rev-parse", commit + ":validation/main_preflight_adoption.py")
            and journal["manifest_blob"] == git("rev-parse", commit + ":" + str(MANIFEST)),
            "Prepared journal identity mismatch")
    require(not journal["phase_errors"], "Unresolved fixture/import/discovery error; preserve positives")
    families = bounded_json(MANIFEST.read_bytes())["families"]
    def record(identity, outcome):
        row = witness(identity, families)
        journal["outcomes"][identity] = {"commit": commit, "run": run, "outcome": outcome,
            "job": journal["job"], "attempt": 1, "environment": environment(), "inputs": input_digest(commit, row)}
        if outcome == "skipped":
            journal["skips"][identity] = journal["outcomes"].pop(identity)
        else:
            journal["skips"].pop(identity, None)
        atomic_json(JOURNAL, journal)
        print(outcome_event(identity, journal["skips" if outcome == "skipped" else "outcomes"][identity]),
              flush=True)
    if phase == "node":
        for script in NODE:
            identity = "node:" + script
            witness(identity, families)
            if identity in journal["outcomes"]:
                print("Adopted " + identity)
                continue
            command = ["node", "--test", script] if script.endswith("seek-telemetry.test.js") else ["node", script]
            in_progress = {"phase": "node", "error": "script in progress; preserve partial TAP log: " + script}
            journal["phase_errors"].append(in_progress)
            atomic_json(JOURNAL, journal)
            try:
                subprocess.run(command, check=True)
            except Exception:
                record_phase_error(journal, "node", "script failed/aborted; preserve partial TAP log: " + script)
                raise
            journal["phase_errors"].remove(in_progress)
            record(identity, "success")
        return 0
    in_progress = {"phase": phase, "error": "Python discovery/fixture/runner in progress; preserve original log"}
    journal["phase_errors"].append(in_progress)
    atomic_json(JOURNAL, journal)
    try:
        tests = discover(phase)
    except Exception:
        record_phase_error(journal, phase, "discovery/import failed")
        raise
    # Resolve every input before the first new control may execute.
    for test in tests:
        witness(phase + ":" + test.id(), families)
    pending = [test for test in tests if phase + ":" + test.id() not in journal["outcomes"]]
    class Result(unittest.TextTestResult):
        def addError(self, test, error):
            super().addError(test, error)
            if not isinstance(test, unittest.TestCase) or test.__class__.__name__ == "_FailedTest":
                record_phase_error(journal, phase, "fixture/import/discovery: " + test.id())
        def addSuccess(self, test):
            super().addSuccess(test)
            record(phase + ":" + test.id(), "success")
        def addSkip(self, test, reason):
            super().addSkip(test, reason)
            record(phase + ":" + test.id(), "skipped")
    print(f"{phase}: discovered={len(tests)}, adopted={len(tests)-len(pending)}, pending={len(pending)}")
    try:
        result = unittest.TextTestRunner(verbosity=2, resultclass=Result).run(unittest.TestSuite(pending))
    except Exception:
        record_phase_error(journal, phase, "unit runner aborted")
        raise
    journal["phase_errors"].remove(in_progress)
    atomic_json(JOURNAL, journal)
    return 0 if result.wasSuccessful() and not result.expectedFailures else 1


if __name__ == "__main__":
    try:
        require(len(sys.argv) == 2 and sys.argv[1] in ("prepare", "validation", "operations", "node"),
                "Unknown receipt phase")
        if sys.argv[1] == "prepare":
            prepare()
        else:
            sys.exit(execute_phase(sys.argv[1]))
    except Exception as error:
        message = str(error) if isinstance(error, ReceiptError) else "Evidence unavailable; preserve job/source"
        print("Main preflight receipt refusal: " + message, file=sys.stderr)
        sys.exit(1)
