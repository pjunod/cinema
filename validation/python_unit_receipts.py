"""Once-per-PR Python unit evidence; compilation remains current-source evidence."""

from __future__ import annotations

import argparse
import ast
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import urllib.parse
import urllib.request
import zipfile

VERSION = 1
MAX_BYTES = 4 * 1024 * 1024
MAX_TESTS = 20000
MAX_PAGES = 20
SUITES = {"validation": "tests/validation", "operations": "tests/operations"}
JOB_PREFIX = "Python unit receipts"


class ReceiptError(RuntimeError):
    """Evidence is unavailable, ambiguous, or invalid: do not execute units."""


def require(condition, message):
    if not condition:
        raise ReceiptError(message)


def positive(value):
    require(type(value) is int and value > 0, "Invalid numeric evidence identity")
    return value


def sha(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value),
            "Invalid source commit")
    return value


def bounded_json(data):
    require(len(data) <= MAX_BYTES, "Receipt JSON exceeds limit")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "Duplicate JSON field")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def atomic_json(path, value):
    data = json.dumps(value, sort_keys=True).encode()
    require(len(data) <= MAX_BYTES, "Receipt exceeds limit")
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".receipt-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ReceiptError("Receipt API redirect refused")


class API:
    def __init__(self, root, repository, token):
        parsed = urllib.parse.urlsplit(root)
        require(parsed.scheme in ("http", "https") and parsed.hostname
                and not parsed.username and not parsed.password
                and not parsed.query and not parsed.fragment,
                "Invalid CI API origin")
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository),
                "Invalid repository slug")
        require(token, "CI artifact authentication unavailable")
        self.root = root.rstrip("/") + "/repos/" + repository
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect())
        self.requests = 0

    def bytes(self, path, query=None):
        self.requests += 1
        require(self.requests <= 200, "Receipt request budget exhausted; inspect retained evidence")
        require(path.startswith("/") and ".." not in path, "Unsafe API path")
        url = self.root + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        request = urllib.request.Request(url, headers={
            "Authorization": "token " + self.token,
            "Accept": "application/json",
        })
        with self.opener.open(request, timeout=15) as response:
            data = response.read(MAX_BYTES + 1)
        require(len(data) <= MAX_BYTES, "API response exceeds limit")
        return data

    def get(self, path, query=None):
        return bounded_json(self.bytes(path, query))

    def pages(self, path, query=None, field=None):
        items = []
        for page in range(1, MAX_PAGES + 1):
            payload = self.get(path, {**(query or {}), "page": page, "limit": 50})
            batch = payload[field] if field else payload
            require(isinstance(batch, list) and len(batch) <= 50,
                    "Invalid API pagination")
            items.extend(batch)
            if len(batch) < 50:
                return items
        raise ReceiptError("Evidence pagination limit reached; preserve journals and inspect history")


def artifact_json(data):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        require(len(members) == 1 and members[0].filename == "receipt.json"
                and members[0].file_size <= MAX_BYTES
                and not members[0].is_dir(), "Unsafe receipt archive")
        return bounded_json(archive.read(members[0]))


def identity(api, branch, commit):
    repo = api.get("")
    repo_id = positive(repo["id"])
    require(branch and not branch.startswith("/") and "\n" not in branch,
            "Invalid dispatched branch")
    candidates = []
    for pull in api.pages("/pulls", {"state": "open"}):
        head, base = pull["head"], pull["base"]
        if (head["ref"] == branch and head["repo"]["id"] == repo_id
                and head["sha"] == commit and base["repo"]["id"] == repo_id
                and base["ref"].startswith("effort/")):
            candidates.append(pull)
    require(len(candidates) == 1,
            "Dispatch must match exactly one open same-repository PR into effort (branch/head/base)")
    pull = candidates[0]
    return {"repository": repo_id, "pr": positive(pull["number"]),
            "branch": branch, "base": pull["base"]["ref"]}


def key(scope):
    return f"python-units-v1-r{scope['repository']}-pr{scope['pr']}"


def job_name(scope):
    return f"{JOB_PREFIX} · {scope['repository']} · PR {scope['pr']}"


def validate_journal(journal, scope, run, commit, completed=True):
    require(journal.get("version") == VERSION and journal.get("scope") == scope,
            "Receipt repository/PR/branch/base mismatch")
    require(journal.get("run") == run and journal.get("commit") == commit,
            "Receipt artifact/run/source mismatch")
    require(not completed or journal.get("complete") is True,
            f"Incomplete receipt attempt {run}; preserve artifact and recover individual evidence")
    passes = journal.get("passes")
    require(isinstance(passes, dict) and len(passes) <= MAX_TESTS,
            "Invalid receipt success map")
    for test_key, source in passes.items():
        require(isinstance(test_key, str) and len(test_key) <= 1024
                and test_key.split(":", 1)[0] in SUITES,
                "Invalid suite/test identity")
        require(isinstance(source, dict) and set(source) in
                ({"commit", "run"}, {"commit", "run", "provenance"}),
                "Invalid success attribution")
        if source["commit"] is not None:
            sha(source["commit"])
        else:
            require(isinstance(source.get("provenance"), str)
                    and source["provenance"].startswith("attested-local:"),
                    "Unknown worktree source attribution")
        if source["run"] is not None:
            positive(source["run"])
        else:
            require(source.get("provenance") == "approved-local-linux-pr663-correction"
                    or source.get("provenance", "").startswith("attested-local:"),
                    "Unknown non-CI source attribution")
    return passes


def restore(api, scope, run):
    legacy = {**legacy_pr663(api, scope), **local_receipts(api, scope)}
    artifacts = api.pages("/actions/artifacts", {"name": key(scope)})
    indexed = {}
    for artifact in artifacts:
        require(artifact["name"] == key(scope), "Artifact name filter mismatch")
        aid, rid = positive(artifact["id"]), positive(artifact["run_id"])
        require(not artifact["expired"] and artifact["size_in_bytes"] <= MAX_BYTES,
                f"Expired/oversize receipt artifact {aid}; recover evidence before retry")
        require(rid not in indexed, "Multiple final journals for one run: ambiguous attempt")
        indexed[rid] = artifact
    passes = dict(legacy)
    runs = api.pages("/actions/runs", {"workflow_id": "effort-ci.yml",
                                     "ref": "refs/heads/" + scope["branch"]},
                     "workflow_runs")
    trusted_sources = set()
    journals = []
    for prior in runs:
        rid = positive(prior["id"])
        if rid == run:
            require(rid not in indexed, "This run already published units; dispatch a fresh run")
            continue
        jobs = api.pages(f"/actions/runs/{rid}/jobs")
        matching = [job for job in jobs if job["name"] == job_name(scope)]
        if not matching:
            continue  # Legacy policy has no individually attributable journal.
        require(len(matching) == 1 and matching[0]["repo_id"] == scope["repository"],
                "Ambiguous receipt job identity")
        require(matching[0]["attempt"] == 1,
                f"Run {rid} was re-run; ambiguous receipt attempt, dispatch fresh runs only")
        if matching[0]["status"] == "skipped":
            continue
        require(rid in indexed, f"Missing final receipt for run {rid}; do not rerun possibly passed tests")
        markers = api.pages("/actions/artifacts", {"name": key(scope) + f"-start-{rid}"})
        require(len(markers) == 1 and markers[0]["run_id"] == rid
                and not markers[0]["expired"], f"Missing/expired start marker for run {rid}")
        marker = artifact_json(api.bytes(f"/actions/artifacts/{positive(markers[0]['id'])}/zip"))
        validate_journal(marker, scope, rid, sha(prior["commit_sha"]), completed=False)
        require(marker["complete"] is False, "Start marker is not an initial attempt")
        artifact = indexed.pop(rid)
        journal = artifact_json(api.bytes(f"/actions/artifacts/{artifact['id']}/zip"))
        validate_journal(journal, scope, rid, sha(prior["commit_sha"]))
        trusted_sources.add((rid, prior["commit_sha"]))
        journals.append(journal)
    require(not indexed, "Receipt artifacts lack corresponding trusted workflow/job metadata")
    for journal in journals:
        for test_key, source in journal["passes"].items():
            require((source["run"], source["commit"]) in trusted_sources
                    or legacy.get(test_key) == source,
                    "Success source has no trusted completed journal")
            passes.setdefault(test_key, source)
    return passes


def local_receipts(api, scope):
    """Only exact receipt hashes attested by authenticated repository writers."""
    directory = Path("validation/python-unit-local")
    if not directory.exists():
        return {}
    require(not directory.is_symlink(), "Local receipt directory is unsafe")
    files = sorted(directory.iterdir())
    require(len(files) <= 64, "Too many local receipt documents")
    documents = {}
    for path in files:
        require(path.is_file() and not path.is_symlink()
                and re.fullmatch(r"[0-9a-f]{64}\.json", path.name),
                "Unsafe local receipt filename")
        raw = path.read_bytes()
        digest = hashlib.sha256(raw).hexdigest()
        require(path.stem == digest, "Local receipt filename/hash mismatch")
        proof = bounded_json(raw)
        if proof.get("repository") == scope["repository"] and proof.get("branch") == scope["branch"]:
            documents[digest] = proof
    if not documents:
        return {}
    comments = api.pages(f"/issues/{scope['pr']}/comments")
    passes = {}
    for comment in comments:
        claims = [line.removeprefix("Python-Unit-Receipt: ")
                  for line in comment["body"].splitlines()
                  if line.startswith("Python-Unit-Receipt: ")]
        if not claims:
            continue
        require(len(claims) == 1, "Ambiguous duplicate receipt attestations")
        claim = bounded_json(claims[0].encode())
        require(set(claim) == {"repository", "pr", "suite", "sha256"},
                "Unknown receipt attestation fields")
        if claim["sha256"] not in documents:
            continue
        require(claim["repository"] == scope["repository"] and claim["pr"] == scope["pr"]
                and claim["suite"] in SUITES, "Attestation repository/PR/suite mismatch")
        author = comment["user"]
        login = author["login"]
        require(re.fullmatch(r"[A-Za-z0-9_.-]+", login), "Unsafe attestor identity")
        permission = api.get("/collaborators/" + login + "/permission")
        require(permission["user"]["id"] == positive(author["id"])
                and permission["permission"] in ("owner", "admin", "write"),
                "Receipt attestor is not an authenticated repository writer")
        proof = documents[claim["sha256"]]
        require(proof["version"] == 1 and proof["suite"] == claim["suite"]
                and proof["result"] == {"failed": 0, "errors": 0, "skipped": 0,
                                       "expected_failures": 0, "unexpected_successes": 0,
                                       "count": len(proof["test_ids"])}
                and 0 < len(proof["test_ids"]) <= MAX_TESTS
                and len(proof["test_ids"]) == len(set(proof["test_ids"]))
                and isinstance(proof["command"], str) and proof["command"]
                and isinstance(proof["output"], str) and proof["output"]
                and re.fullmatch(r"[0-9a-f]{64}", proof["test_file_sha256"]),
                "Local receipt does not prove exclusively passing tests")
        require(re.fullmatch(SUITES[proof["suite"]] + r"/test_[a-z0-9_]+\.py", proof["test_file"]),
                "Unsafe local test source path")
        for test_id in proof["test_ids"]:
            require(isinstance(test_id, str) and len(test_id) <= 1024
                    and test_id.startswith(Path(proof["test_file"]).stem + "."),
                    "Local receipt ID does not belong to named suite/module")
            passes[proof["suite"] + ":" + test_id] = {
                "commit": sha(proof["source_commit"]) if proof.get("source_commit") else None, "run": None,
                "provenance": f"attested-local:{claim['sha256']}:{proof['test_file_sha256']}:comment-{positive(comment['id'])}"}
    # A present but unattested proof must not quietly rerun its successful IDs.
    claimed = {test_id for proof in documents.values()
               for test_id in (proof["suite"] + ":" + item for item in proof["test_ids"])}
    require(claimed.issubset(passes), "Local pass receipts await authenticated PR hash attestation")
    return passes


def git_source(api, commit, path):
    require(re.fullmatch(r"tests/(operations|validation)/test_[a-z0-9_]+\.py", path)
            or path in ("Makefile", ".github/workflows/effort-ci.yml", "scripts/package-windows"),
            "Unsafe legacy source path")
    return api.bytes("/raw/" + path, {"ref": sha(commit)})


def legacy_pr663(api, scope):
    """One reviewed reconstruction, not a generic pass-ID import or skip switch."""
    if scope != {"repository": 1, "pr": 663,
                 "branch": "codex/p02-packed-debug-artifacts",
                 "base": "effort/architecture-review-2026-09-20"}:
        return {}
    root = Path(__file__).resolve().parent
    proof = bounded_json((root / "python-unit-legacy-pr663.json").read_bytes())
    commit = sha(proof["source_sha"])
    prior = api.get("/actions/runs/3703")
    require(prior["commit_sha"] == commit and prior["workflow_id"] == "effort-ci.yml"
            and prior["prettyref"] == scope["branch"], "Legacy run source/branch mismatch")
    jobs = api.pages("/actions/runs/3703/jobs")
    require(any(job["id"] == 39297 and job["run_id"] == 3703
                and job["repo_id"] == 1 and job["status"] == "failure"
                and job["attempt"] == 1 for job in jobs), "Legacy job identity mismatch")
    log = api.bytes("/actions/jobs/39297/logs")
    require(hashlib.sha256(log).hexdigest() == proof["log_sha256"],
            "Legacy original job log hash mismatch/unavailable")
    text = log.decode()
    require(text.count("Ran 603 tests in 83.021s") == 1
            and text.count("FAILED (errors=1)") == 1
            and proof["failure"] in text and text.count(commit) >= 2,
            "Legacy exhaustive result/checkout evidence mismatch")
    ids = []
    for path, digest in proof["test_file_sha256"].items():
        source = git_source(api, commit, path)
        require(hashlib.sha256(source).hexdigest() == digest, "Legacy test source hash mismatch")
        tree = ast.parse(source)
        require(not any(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                    and node.name == "load_tests"
                        for node in tree.body), "Legacy custom discovery refusal")
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            methods = [m for m in node.body if isinstance(m, ast.FunctionDef)
                       and m.name.startswith("test")]
            if not methods:
                continue
            require(len(node.bases) == 1 and isinstance(node.bases[0], ast.Attribute)
                    and isinstance(node.bases[0].value, ast.Name)
                    and node.bases[0].value.id == "unittest"
                    and node.bases[0].attr == "TestCase"
                    and not any(isinstance(m, ast.FunctionDef)
                                and m.name in ("__init__", "run") for m in node.body),
                    "Legacy test class changes discovery semantics")
            ids.extend(Path(path).stem + "." + node.name + "." + m.name for m in methods)
    require(len(ids) == 603 and len(set(ids)) == 603
            and sorted(ids) == proof["all_ids"], "Legacy exact source inventory mismatch")
    require(b"python3 -m unittest discover -s tests/operations -p 'test_*.py'"
            in git_source(api, commit, "Makefile")
            and b"run: make operations-check" in git_source(api, commit, ".github/workflows/effort-ci.yml"),
            "Legacy command was not exhaustive unittest discovery")
    correction = (root / "python-unit-legacy-pr663-correction.txt").read_bytes()
    require(hashlib.sha256(correction).hexdigest() == proof["correction_receipt_sha256"],
            "Approved local correction receipt mismatch")
    require(hashlib.sha256(git_source(api, proof["correction_commit"],
                                     "tests/operations/test_release_pdb.py")).hexdigest()
            == "f856b72abc429cc9ba24d76eef6a409ddc1a59e270d4994113b549ac88c3565c",
            "Corrected Linux test source mismatch")
    successes = {"operations:" + test_id: {
        "commit": commit, "run": 3703, "provenance": "aggregate-reconstruction-603-minus-sole-error"}
        for test_id in ids if test_id != proof["failure"]}
    validation = bounded_json((root / "python-unit-legacy-pr663-validation.json").read_bytes())
    require(validation["source_sha"] == commit and validation["log_sha256"] == proof["log_sha256"]
            and text.count("Ran 252 tests in 23.016s") == 1
            and "2026-10-01T02:29:29.5929720Z OK" in text,
            "Legacy validation summary/source mismatch")
    validation_ids = []
    for path, digest in validation["test_file_sha256"].items():
        source = git_source(api, commit, path)
        require(hashlib.sha256(source).hexdigest() == digest,
                "Legacy validation module hash mismatch")
        tree = ast.parse(source)
        require(not any(isinstance(node, ast.FunctionDef) and node.name == "load_tests"
                        for node in tree.body), "Legacy validation custom discovery refusal")
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            require(not any(isinstance(method, ast.FunctionDef)
                            and method.name in ("__init__", "run") for method in node.body),
                    "Legacy validation constructor/run override")
            methods = [method for method in node.body if isinstance(method, ast.FunctionDef)
                       and method.name.startswith("test")]
            if not methods:
                continue
            class_id = Path(path).stem + "." + node.name
            require([ast.unparse(base) for base in node.bases]
                    == validation["test_class_bases"][class_id],
                    "Legacy validation inheritance differs from audited inventory")
            validation_ids.extend(class_id + "." + method.name for method in methods)
    require(len(validation_ids) == 252 and len(set(validation_ids)) == 252
            and sorted(validation_ids) == validation["all_ids"],
            "Legacy validation exact inventory mismatch")
    successes.update({"validation:" + test_id: {
        "commit": commit, "run": 3703, "provenance": "aggregate-reconstruction-252-exhaustive-OK"}
        for test_id in validation_ids})
    return successes


def flatten(suite):
    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from flatten(test)
        else:
            yield test


def discover(suite_name):
    loader = unittest.TestLoader()
    tests = list(flatten(loader.discover(SUITES[suite_name], pattern="test_*.py")))
    require(not loader.errors, "Unit discovery failed; no cached pass may hide import errors")
    require(0 < len(tests) <= MAX_TESTS, "Unit discovery produced zero or too many tests")
    ids = [test.id() for test in tests]
    require(len(ids) == len(set(ids)), "Duplicate discovered test IDs")
    return tests


class RecordingResult(unittest.TextTestResult):
    def __init__(self, *args, journal, path, suite, **kwargs):
        super().__init__(*args, **kwargs)
        self.journal, self.path, self.suite = journal, path, suite

    def addSuccess(self, test):
        super().addSuccess(test)
        self.journal["passes"][self.suite + ":" + test.id()] = {
            "commit": self.journal["commit"], "run": self.journal["run"]}
        atomic_json(self.path, self.journal)


def execute(journal, path, suites=None):
    failed = False
    for suite_name in SUITES:
        tests = suites[suite_name] if suites is not None else discover(suite_name)
        require(tests, "Empty unit suite")
        ids = [test.id() for test in tests]
        require(len(ids) == len(set(ids)), "Duplicate discovered test IDs")
        pending = [test for test in tests if suite_name + ":" + test.id() not in journal["passes"]]
        print(f"{suite_name}: discovered={len(tests)}, historical-passes={len(tests)-len(pending)}, pending={len(pending)}")
        if pending:
            runner = unittest.TextTestRunner(verbosity=2, resultclass=lambda *a, **kw:
                RecordingResult(*a, **kw, journal=journal, path=path, suite=suite_name))
            result = runner.run(unittest.TestSuite(pending))
            failed |= not result.wasSuccessful() or bool(result.skipped) or bool(result.expectedFailures)
        require(all(suite_name + ":" + test_id in journal["passes"] for test_id in ids)
                or failed, "Unknown unit outcome")
    journal["complete"] = True
    atomic_json(path, journal)
    return 1 if failed else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("identity", "prepare", "run"))
    args = parser.parse_args(argv)
    # Fixed workspace-relative path, never a caller-selected cache destination.
    path = Path(".python-unit-receipts/receipt.json")
    require(not path.parent.is_symlink(), "Receipt directory must not be a symlink")
    api_origin = urllib.parse.urlsplit(os.environ["GITHUB_API_URL"])
    server_origin = urllib.parse.urlsplit(os.environ["GITHUB_SERVER_URL"])
    require((api_origin.scheme, api_origin.netloc) == (server_origin.scheme, server_origin.netloc),
            "CI API origin differs from configured server; refuse credential forwarding")
    api = API(os.environ["GITHUB_API_URL"], os.environ["GITHUB_REPOSITORY"],
              os.environ.get("GITHUB_TOKEN"))
    commit = sha(os.environ["GITHUB_SHA"])
    require(subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip() == commit,
            "Checkout is not dispatched source")
    run = positive(int(os.environ["GITHUB_RUN_ID"]))
    require(os.environ.get("GITHUB_RUN_ATTEMPT", "1") == "1",
            "Use fresh workflow dispatch, never re-run jobs")
    scope = identity(api, os.environ["GITHUB_REF_NAME"], commit)
    if args.command == "identity":
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"receipt_key={key(scope)}\nreceipt_job={job_name(scope)}\n")
        return 0
    if args.command == "prepare":
        journal = {"version": VERSION, "scope": scope, "run": run,
                   "commit": commit, "complete": False, "passes": restore(api, scope, run)}
        atomic_json(path, journal)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"receipt_key={key(scope)}\nreceipt_job={job_name(scope)}\n")
        return 0
    journal = bounded_json(path.read_bytes())
    validate_journal(journal, scope, run, commit, completed=False)
    require(journal["complete"] is False, "Attempt already completed")
    return execute(journal, path)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        # Never include request headers, token values, or response bodies.
        detail = str(error) if isinstance(error, ReceiptError) else "Evidence unavailable; preserve run/job identity"
        print(f"Python receipt refusal: {type(error).__name__}: {detail}", file=sys.stderr)
        sys.exit(1)
