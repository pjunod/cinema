"""Per-ID main-preflight journal: adopt authenticated prior outcomes, run the rest.

prepare iterates this PR's published final journals and authenticates each
one's run, job, start marker, publication log, producer and declared inputs.
Only journals that pass every check contribute outcomes. Anything that does
not authenticate (including a journal whose commit objects are no longer in
this checkout) is not adopted: its tests run again and the lane continues.
Refusals are reserved for the current run's own integrity.
"""

from __future__ import annotations

import hashlib
import fnmatch
import http.client
import json
import os
import platform
from pathlib import Path
import re
import subprocess
import sys
import unittest
import zipfile
from typing import Mapping

from validation.python_unit_receipts import (API, IntegrityError, ReceiptError, SourceApplicability,
    artifact_json, atomic_json, bounded_json, discover, positive, require, test_source_path)


# Every way a prior journal can fail to authenticate. Each means "not adopted"
# and never refuses the lane; programming errors outside this set still raise.
# IntegrityError (the current run is unsafe) is a ReceiptError, so every
# handler re-raises it first.
UNADOPTABLE = (ReceiptError, subprocess.SubprocessError, OSError, http.client.HTTPException,
               KeyError, IndexError, TypeError, ValueError, AttributeError, zipfile.BadZipFile)


def reason(error):
    """A ReceiptError states its own reason; anything else names its type.

    A TypeError or AttributeError may be a bug in this module rather than a
    bad journal, so the printed line always shows which it was.
    """
    return str(error) if isinstance(error, ReceiptError) else f"{type(error).__name__}: {error}"
JOB = "fast policy and contract preflight"


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


def terminal_status(metadata: Mapping[str, object]) -> object:
    """Forgejo has returned both keys; disagreements are ambiguous."""
    values = [metadata[key] for key in ("status", "state") if key in metadata]
    require(values and len(set(values)) == 1, "Ambiguous terminal metadata")
    return values[0]


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


def commit_available(commit):
    """A force push can remove a journal's commit from this checkout.

    Its sources cannot be read back, so nothing it recorded can be bound to
    them; the caller treats False as "not adopted", never as a refusal.
    """
    if not (isinstance(commit, str) and re.fullmatch(r"[0-9a-f]{40}", commit)):
        return False
    try:
        return subprocess.run(["git", "cat-file", "-e", commit + "^{commit}"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              timeout=10).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


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
    require(live["head"]["sha"] == commit
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
            and job["name"] == JOB
            and terminal_status(job) in ("success", "failure", "cancelled"),
            "Journal producer job is not an authenticated terminal preflight")
    require(isinstance(journal["phase_errors"], list) and not journal["phase_errors"],
            "Journal records a fixture/import/discovery failure")
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
    """The journal's producer workflow executed every adoption phase it claims."""
    phased = all("python3 -m validation.main_preflight_adoption " + phase in commands
              for phase in ("prepare", "validation", "operations", "node"))
    split = ("python3 -m validation.main_unit_receipts prepare" in commands
              and "python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations" in commands
              and "python3 -m validation.main_preflight_adoption node" in commands)
    return phased or split


def journal_key(scope):
    return f"main-preflight-v1-r{scope['repository']}-pr{scope['pr']}"


def authenticate_journal(api, scope, artifact, env, candidates, skipped_history):
    """Return (outcomes, skips, dropped) from one prior journal, or raise.

    Own records need the job's one original outcome event and their producer
    commit's declared inputs. Inherited records are kept only when they match
    the record already adopted from an earlier authenticated journal; one
    whose provenance journal was not adopted is dropped (it runs again).
    """
    name = journal_key(scope)
    rid = positive(artifact["run_id"])
    require(artifact["name"] == name and not artifact["expired"], "artifact expired or misnamed")
    prior = api.get(f"/actions/runs/{rid}")
    require(prior["id"] == rid and prior["repository"]["id"] == scope["repository"]
            and prior["workflow_id"] == "main-fast-lane.yml", "Untrusted receipt run")
    bind_event(prior, scope)
    jobs = api.pages(f"/actions/runs/{rid}/jobs")
    preflight = [job for job in jobs if job["name"] == JOB]
    require(len(preflight) == 1, "Ambiguous prior job")
    journal = artifact_json(api.bytes(f"/actions/artifacts/{positive(artifact['id'])}/zip"))
    validate_journal(journal, scope, prior, preflight[0], env)
    commit = journal["commit"]
    # Before any git read: the PR #997 force push left a journal whose commit
    # was gone, and reading its workflow crashed prepare outright.
    require(commit_available(commit), f"commit {commit} is not available in this checkout")
    producer_commands = run_commands(git("show", commit + ":.github/workflows/main-fast-lane.yml"))
    require(adapter_workflow(producer_commands)
            and journal["producer_blob"] == git("rev-parse", commit + ":validation/main_preflight_adoption.py")
            and journal["manifest_blob"] == git("rev-parse", commit + ":" + str(MANIFEST)),
            "Unknown workflow/journal producer")
    log = api.bytes(f"/actions/jobs/{positive(preflight[0]['id'])}/logs").decode()
    require(commit + f":refs/remotes/pull/{scope['pr']}/head" in log,
            "Journal PR source identity unavailable")
    require(log.count(f"Artifact {name} has been successfully uploaded!") == 1,
            "Final journal publication absent or ambiguous")
    starts = api.pages("/actions/artifacts", {"name": name + "-start-" + str(rid)})
    require(len(starts) == 1 and starts[0]["run_id"] == rid
            and starts[0]["name"] == name + "-start-" + str(rid)
            and not starts[0]["expired"], "Prior start journal unavailable")
    start = artifact_json(api.bytes(f"/actions/artifacts/{positive(starts[0]['id'])}/zip"))
    validate_journal(start, scope, prior, preflight[0], env)
    require(all(start[key] == journal[key] for key in JOURNAL_FIELDS - {"outcomes", "skips"}),
            "Start/final journal identity mismatch")
    require(all(journal["outcomes"].get(identity) == record
                for identity, record in start["outcomes"].items()),
            "Final journal dropped/changed an inherited positive")
    require(log.count(f"Artifact {starts[0]['name']} has been successfully uploaded!") == 1,
            "Start journal publication absent or ambiguous")
    original_families = json.loads(git("show", commit + ":" + str(MANIFEST)))["families"]

    def declared(identity, record):
        require(record["environment"] == journal["environment"]
                and record["inputs"] == input_digest(record["commit"], witness(identity, original_families)),
                "Original-source declared input digest mismatch")

    outcomes, skips, dropped = {}, {}, 0
    for identity, record in journal["outcomes"].items():
        require(record["outcome"] == "success", "Only positive outcomes may carry")
        if record["run"] == journal["run"]:
            require_record_origin(record, journal, None)
            require(log.count(outcome_event(identity, record)) == 1,
                    "Own success lacks exactly one original executable outcome event")
            declared(identity, record)
        else:
            try:
                require_record_origin(record, journal, candidates.get(identity))
                declared(identity, record)
            except IntegrityError:
                raise
            except UNADOPTABLE:
                dropped += 1
                continue
        outcomes[identity] = record
    for identity, record in journal["skips"].items():
        if record["run"] == journal["run"]:
            require_record_origin(record, journal, None)
            require(log.count(outcome_event(identity, record)) == 1,
                    "Own skip lacks original executable outcome event")
        else:
            try:
                require_record_origin(record, journal, skipped_history.get(identity))
            except IntegrityError:
                raise
            except UNADOPTABLE:
                continue
        skips[identity] = record
    return outcomes, skips, dropped


def prepare(output_key="receipt_key"):
    """Write this run's start journal; prior history can only add adoptions."""
    api, scope, commit, run_id = api_context()
    require(not JOURNAL.parent.is_symlink(), "Receipt directory symlink")
    document = bounded_json(MANIFEST.read_bytes())
    require(document["version"] == 1, "Unknown input manifest")
    families = document["families"]
    name = journal_key(scope)
    env = environment()
    current_jobs = api.pages(f"/actions/runs/{run_id}/jobs")
    current_job = [job for job in current_jobs if job["name"] == JOB]
    require(len(current_job) == 1 and current_job[0]["attempt"] == 1, "Current job identity unavailable")
    try:
        artifacts = api.pages("/actions/artifacts", {"name": name})
    except IntegrityError:
        raise
    except UNADOPTABLE as error:
        # Without the listing nothing can be adopted; GITHUB_RUN_ATTEMPT
        # (checked in api_context) still guards against a re-run.
        print(f"Prior preflight journals unavailable — {reason(error)}; nothing adopted, every test runs")
        artifacts = []
    if any(artifact.get("run_id") == run_id for artifact in artifacts):
        raise IntegrityError("Current run already has a journal")
    by_run = {}
    for artifact in artifacts:
        by_run.setdefault(artifact.get("run_id"), []).append(artifact)
    candidates, skipped_history = {}, {}
    # Oldest first: an inherited record is checked against what the earlier
    # journal that produced it actually contributed.
    for rid in sorted(by_run, key=lambda value: value if type(value) is int else -1):
        try:
            require(len(by_run[rid]) == 1, "several final journals for one run")
            outcomes, skips, dropped = authenticate_journal(
                api, scope, by_run[rid][0], env, candidates, skipped_history)
        except IntegrityError:
            raise
        except UNADOPTABLE as error:
            print(f"run {rid}: preflight journal not adopted — {reason(error)}; its tests run again")
            continue
        if dropped:
            print(f"run {rid}: {dropped} inherited outcomes not adopted — their originating journal was not adopted")
        candidates.update(outcomes)
        skipped_history.update(skips)
    source = SourceApplicability(commit)
    current_test_paths = set(git("ls-tree", "-r", "--name-only", commit, "tests").splitlines())
    adopted = {}
    for identity, record in candidates.items():
        try:
            if not identity.startswith("node:"):
                if test_source_path(identity)[0] not in current_test_paths:
                    continue  # Authenticated tracked tree proves the entire module was removed.
                if source.fingerprint(commit, identity) is None:
                    continue  # Removed IDs retain history; they are not current pending work.
            row = witness(identity, families)
            same = record["environment"] == env and input_digest(commit, row) == input_digest(record["commit"], row)
            if not identity.startswith("node:"):
                same = source(identity, {"commit": record["commit"], "run": record["run"]}) and same
        except IntegrityError:
            raise  # The current worktree is not the dispatched source.
        except UNADOPTABLE as error:
            # An unknown witness or unreadable source proves nothing either
            # way about the current control; executing it is always safe.
            print(f"Not adopted: {identity} — {reason(error)}; it runs again")
            continue
        if same:
            adopted[identity] = {**record, "inputs": input_digest(record["commit"], row)}
        else:
            print("Changed declared inputs/local fixture; execute current control: " + identity)
    source.finish(adopted)
    print(f"Authenticated candidates={len(candidates)}, adopted={len(adopted)}, invalidated-or-removed={len(candidates)-len(adopted)}")
    journal = {"version": 1, "scope": scope, "run": run_id, "commit": commit,
               "job": current_job[0]["id"], "attempt": 1, "environment": env,
               "producer_blob": git("rev-parse", commit + ":validation/main_preflight_adoption.py"),
               "manifest_blob": git("rev-parse", commit + ":" + str(MANIFEST)),
               "outcomes": adopted, "skips": skipped_history, "phase_errors": []}
    atomic_json(JOURNAL, journal)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(output_key + "=" + name + "\n")
    return adopted


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
