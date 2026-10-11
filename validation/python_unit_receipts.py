"""Once-per-PR Python unit evidence; compilation remains current-source evidence.

Earlier passes of the same PR are ADOPTED only when they authenticate. History
never refuses the lane: evidence that does not authenticate is reported and
its tests simply run again. Only the current run's own integrity refuses.
"""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import unittest
import urllib.parse
import urllib.request
import urllib.error
import zipfile

VERSION = 1
MAX_BYTES = 4 * 1024 * 1024
MAX_TESTS = 20000
MAX_PAGES = 20
SUITES = {"validation": "tests/validation", "operations": "tests/operations"}
JOB_PREFIX = "Python unit receipts"
MAX_TEST_SOURCE = 1024 * 1024
MAX_SOURCE_CACHE_BYTES = 32 * 1024 * 1024
MAX_SOURCE_CACHE_FILES = 512
MAX_SOURCE_AST_NODES = 100000
MAX_SOURCE_CACHE_AST_NODES = 1000000


class ReceiptError(RuntimeError):
    """Evidence is unavailable, ambiguous, or invalid.

    For a historical record this means "not adopted": its tests run again.
    Raised from the current run's own checks it refuses the lane.
    """


class IntegrityError(ReceiptError):
    """The current run itself is unsafe; never downgraded to "not adopted"."""


class ReceiptHTTPError(ReceiptError):
    def __init__(self, status, path):
        self.status, self.path = status, path
        super().__init__(f"HTTP {status} at {path}; evidence unavailable")


def require(condition, message):
    if not condition:
        raise ReceiptError(message)


# What a failed historical authentication can raise. Each one only withholds
# adoption of that record; IntegrityError is re-raised before this is caught.
NOT_ADOPTED = (ReceiptError, LookupError, TypeError, ValueError, AttributeError,
               zipfile.BadZipFile)


def not_adopted(subject, error):
    print(f"{subject} not adopted — {error}; its tests run again")


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
        self.repository = repository
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect())
        self.requests = 0

    def bytes(self, path, query=None):
        self.requests += 1
        require(self.requests <= 200, "Receipt request budget exhausted")
        require((path == "" or path.startswith("/")) and ".." not in path
                and len(path) <= 1024 and re.fullmatch(r"[A-Za-z0-9_./-]*", path),
                "Unsafe API path")
        url = self.root + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        request = urllib.request.Request(url, headers={
            "Authorization": "token " + self.token,
            "Accept": "application/json",
        })
        try:
            with self.opener.open(request, timeout=15) as response:
                data = response.read(MAX_BYTES + 1)
        except urllib.error.HTTPError as error:
            # Never include URL query, response body, headers, reason or token.
            raise ReceiptHTTPError(error.code, "/repos/" + self.repository + path) from None
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
        raise ReceiptError("Evidence pagination limit reached")


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
    # Forgejo evaluates job names before needs outputs are available.
    return JOB_PREFIX


def verify_attestor(api, scope, author):
    login = author["login"]
    require(re.fullmatch(r"[A-Za-z0-9_.-]+", login), "Unsafe attestor identity")
    path = "/collaborators/" + login + "/permission"
    try:
        permission = api.get(path)
    except ReceiptHTTPError as error:
        if error.status != 403:
            raise
        print(f"Receipt permission diagnostic: {error}; checking explicit enrollment", file=sys.stderr)
        enrolled = Path("validation/python-unit-attestors.json")
        require(enrolled.is_file() and not enrolled.is_symlink(), "Attestor enrollment unavailable")
        document = bounded_json(enrolled.read_bytes())
        require(set(document) == {"version", "verified_on", "verification", "attestors"}
                and document["version"] == 1 and document["verified_on"] == "2026-10-01"
                and isinstance(document["attestors"], list) and len(document["attestors"]) <= 16,
                "Invalid attestor enrollment")
        matches = [row for row in document["attestors"] if row == {
            "repository": scope["repository"], "user": positive(author["id"]),
            "login": login, "permission": "owner"}]
        require(len(matches) == 1, "Denied permission lookup has no independently verified owner enrollment")
        return
    require(permission["user"]["id"] == positive(author["id"])
            and permission["permission"] in ("owner", "admin", "write"),
            "Receipt attestor is not an authenticated repository writer")


def validate_journal(journal, scope, run, commit, completed=True):
    require(journal.get("version") == VERSION and journal.get("scope") == scope,
            "Receipt repository/PR/branch/base mismatch")
    require(journal.get("run") == run and journal.get("commit") == commit,
            "Receipt artifact/run/source mismatch")
    require(not completed or journal.get("complete") is True,
            f"Incomplete receipt attempt {run}")
    fixture_errors = journal.get("fixture_errors")
    require(isinstance(fixture_errors, list) and len(fixture_errors) <= MAX_TESTS
            and all(isinstance(item, str) and len(item) <= 4096 for item in fixture_errors),
            "Invalid/missing unit fixture-error evidence")
    require(not fixture_errors,
            f"Unresolved fixture errors in run {run}: {'; '.join(fixture_errors[:3])}")
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
            require(source.get("provenance", "").startswith("attested-local:"),
                    "Unknown non-CI source attribution")
    return passes


def test_source_path(test_key):
    match = re.fullmatch(
        r"(validation|operations):(test_[A-Za-z0-9_]+)\."
        r"([A-Za-z_][A-Za-z0-9_]*)\.(test[A-Za-z0-9_]*)", test_key)
    require(match is not None, "Unsupported unit source identity")
    suite, module, class_name, method = match.groups()
    return SUITES[suite] + "/" + module + ".py", class_name, method


def read_git_test_source(commit, path):
    """Read immutable local Git blobs, never fetch, execute or import old tests."""
    sha(commit)
    require(re.fullmatch(r"tests/(operations|validation)/test_[A-Za-z0-9_]+\.py", path),
            "Unsafe unit source path")
    object_name = commit + ":" + path
    try:
        size = subprocess.check_output(
            ["git", "cat-file", "-s", object_name], stderr=subprocess.DEVNULL,
            timeout=5, text=True).strip()
        require(size.isascii() and size.isdigit() and 0 < int(size) <= MAX_TEST_SOURCE,
                "Unit source exceeds byte bound")
        raw = subprocess.check_output(
            ["git", "cat-file", "blob", object_name], stderr=subprocess.DEVNULL,
            timeout=5)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        raise ReceiptError(f"Unit source {object_name} unavailable in this checkout") from None
    require(len(raw) == int(size), "Immutable unit source size mismatch")
    return raw


def read_worktree_test_source(path):
    require(re.fullmatch(r"tests/(operations|validation)/test_[A-Za-z0-9_]+\.py", path),
            "Unsafe current unit source path")
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            metadata = os.fstat(stream.fileno())
            require(stat.S_ISREG(metadata.st_mode), "Current unit source is not a regular file")
            require(0 < metadata.st_size <= MAX_TEST_SOURCE, "Current unit source exceeds byte bound")
            raw = stream.read(MAX_TEST_SOURCE + 1)
    except OSError:
        raise ReceiptError("Current unit source unavailable") from None
    require(0 < len(raw) <= MAX_TEST_SOURCE, "Current unit source exceeds byte bound")
    return raw


class SourceApplicability:
    """Method/local-fixture applicability, not arbitrary production-input proof."""

    def __init__(self, current_commit, source_reader=read_git_test_source,
                 worktree_reader=read_worktree_test_source):
        self.current_commit = sha(current_commit)
        self.source_reader = source_reader
        self.worktree_reader = worktree_reader
        self.files = {}
        self.fingerprints = {}
        self.byte_count = 0
        self.node_count = 0
        self.changed = set()
        self.refusals = {}
        self.unavailable = {}

    def source(self, commit, path):
        key = (sha(commit), path)
        if key in self.files:
            return self.files[key]
        if key in self.unavailable:
            raise ReceiptError(self.unavailable[key])
        require(len(self.files) < MAX_SOURCE_CACHE_FILES,
                "Unit applicability source-cache file bound exhausted")
        try:
            raw = self.source_reader(commit, path)
        except ReceiptError as error:
            # A blob absent from this checkout (e.g. a force-pushed-away
            # commit) stays absent: ask git once per file, not once per test.
            self.unavailable[key] = str(error)
            raise
        require(isinstance(raw, bytes) and 0 < len(raw) <= MAX_TEST_SOURCE,
                "Unit applicability source exceeds byte bound")
        if commit == self.current_commit and self.worktree_reader(path) != raw:
            # The current run's own checkout, not history: always refuse.
            raise IntegrityError("Current unit worktree differs from dispatched Git source")
        require(self.byte_count + len(raw) <= MAX_SOURCE_CACHE_BYTES,
                "Unit applicability source-cache byte bound exhausted")
        try:
            tree = ast.parse(raw, filename=path)
        except (SyntaxError, ValueError, RecursionError):
            raise ReceiptError("Unit source cannot be normalized") from None
        nodes = sum(1 for _ in ast.walk(tree))
        require(nodes <= MAX_SOURCE_AST_NODES,
                "Unit applicability AST node bound exhausted")
        require(self.node_count + nodes <= MAX_SOURCE_CACHE_AST_NODES,
                "Unit applicability aggregate AST bound exhausted")
        require(not any(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                        and node.name in ("load_tests", "__getattr__") for node in tree.body),
                "Unsupported dynamic unit discovery")
        classes = [node for node in tree.body if isinstance(node, ast.ClassDef)]
        require(len({node.name for node in classes}) == len(classes),
                "Ambiguous unit class definition")
        # Bind helpers, setup/teardown, module fixtures, imports, class bases
        # and decorators. Omit sibling bodies, not definition-time effects.
        context = copy.deepcopy(tree)
        for node in context.body:
            if isinstance(node, ast.ClassDef):
                bound = []
                for item in node.body:
                    if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)) \
                            and item.name.startswith("test"):
                        metadata = (item.decorator_list or item.args.defaults
                                    or any(value is not None for value in item.args.kw_defaults)
                                    or item.returns is not None or getattr(item, "type_params", [])
                                    or any(isinstance(arg, ast.arg) and arg.annotation is not None
                                           for arg in ast.walk(item.args)))
                        if not metadata:
                            continue
                        # Keep ordering/signature and annotation/decorator/default
                        # expressions; they can mutate fixtures or be introspected.
                        item.body = [ast.Pass()]
                    bound.append(item)
                node.body = bound
                if not node.body:
                    node.body = [ast.Pass()]
        context_hash = hashlib.sha256(
            ast.dump(context, include_attributes=False).encode()).hexdigest()
        unittest_names, case_names = {"unittest"}, set()
        for node in tree.body:
            if isinstance(node, ast.Import):
                unittest_names.update(alias.asname or alias.name for alias in node.names
                                      if alias.name == "unittest")
            if isinstance(node, ast.ImportFrom) and node.module == "unittest":
                case_names.update(alias.asname or alias.name for alias in node.names
                                  if alias.name in ("TestCase", "IsolatedAsyncioTestCase"))
        value = (hashlib.sha256(raw).hexdigest(), {node.name: node for node in classes},
                 context_hash, unittest_names, case_names,
                 {node.name for node in ast.walk(tree) if isinstance(node, ast.ClassDef)},
                 {alias.asname or alias.name for node in tree.body
                  if isinstance(node, (ast.Import, ast.ImportFrom)) for alias in node.names})
        self.files[key] = value
        self.byte_count += len(raw)
        self.node_count += nodes
        return value

    def fingerprint(self, commit, test_key):
        key = (commit, test_key)
        if key in self.fingerprints:
            return self.fingerprints[key]
        path, class_name, method = test_source_path(test_key)
        _, classes, context, unittest_names, case_names, all_classes, imported = self.source(commit, path)
        if class_name not in classes:
            require(class_name not in all_classes,
                    "Unsupported nested/dynamic unit class")
            require(class_name not in imported,
                    "Unsupported imported unit class")
            return None  # Removed IDs remain in historical artifacts, not current pending work.
        methods = []

        def visit(name, seen):
            require(name not in seen and len(seen) < 16, "Unsupported unit inheritance graph")
            node = classes[name]
            require(not node.keywords and not any(
                isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                and item.name in ("run", "id", "__init__", "__getattribute__", "__getattr__")
                for item in node.body), "Unsupported custom unit class semantics")
            direct = [item for item in node.body
                      if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                      and item.name == method]
            require(len(direct) <= 1, "Ambiguous unit method source")
            if not direct:
                require(not any(isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                                and item.name == method for item in ast.walk(node)),
                        "Unsupported conditional unit method")
                require(not any(isinstance(item, ast.Name) and isinstance(item.ctx, ast.Store)
                                and item.id == method for item in ast.walk(node)),
                        "Unsupported assigned unit method")
            local_bases = []
            for base in node.bases:
                standard = (isinstance(base, ast.Attribute) and isinstance(base.value, ast.Name)
                            and base.value.id in unittest_names
                            and base.attr in ("TestCase", "IsolatedAsyncioTestCase"))
                standard |= isinstance(base, ast.Name) and base.id in case_names | {"object"}
                if isinstance(base, ast.Name) and base.id in classes:
                    local_bases.append(base.id)
                else:
                    require(standard, "Unsupported external/dynamic unit base")
            inherited = []
            for base in local_bases:
                inherited.extend(visit(base, seen | {name}))
            return direct or inherited

        methods = visit(class_name, set())
        if not methods:
            return None
        require(len(methods) == 1, "Ambiguous inherited unit method")
        normalized = ast.dump(methods[0], include_attributes=False)
        result = hashlib.sha256((context + "\n" + normalized).encode()).hexdigest()
        require(len(self.fingerprints) < MAX_TESTS * 2, "Unit fingerprint bound exhausted")
        self.fingerprints[key] = result
        return result

    def __call__(self, test_key, source):
        try:
            return self.matches(test_key, source)
        except IntegrityError:
            raise
        except ReceiptError as error:
            # Unprovable applicability is "not adopted": the method executes
            # unless a later authenticated candidate proves it applicable.
            self.refusals[test_key] = str(error)
            return False

    def finish(self, passes):
        """Report IDs no candidate proved applicable; they are not adopted."""
        unresolved = sorted(set(self.refusals) - set(passes))
        reasons = {}
        for test_key in unresolved:
            reasons.setdefault(self.refusals[test_key], []).append(test_key)
        for reason, keys in sorted(reasons.items()):
            print(f"Unit applicability unavailable for {len(keys)} test(s) — {reason}; "
                  "not adopted, they run again if still discovered:")
            for test_key in keys:
                print("  " + test_key)
        return unresolved

    def matches(self, test_key, source):
        current = self.fingerprint(self.current_commit, test_key)
        if current is None:
            return False
        if source["commit"] is None:
            provenance = source.get("provenance", "")
            match = re.fullmatch(r"attested-local:[0-9a-f]{64}:([0-9a-f]{64}):comment-[1-9][0-9]*",
                                 provenance)
            require(match is not None, "Null unit source lacks a bound local file witness")
            path, _, _ = test_source_path(test_key)
            require(self.source(self.current_commit, path)[0] == match.group(1),
                    "Null-source unit file changed since its local receipt")
            return True
        if source.get("provenance", "").startswith("attested-local:"):
            match = re.fullmatch(r"attested-local:[0-9a-f]{64}:([0-9a-f]{64}):comment-[1-9][0-9]*",
                                 source["provenance"])
            require(match is not None, "Local unit source lacks a bound file witness")
            path, _, _ = test_source_path(test_key)
            require(self.source(sha(source["commit"]), path)[0] == match.group(1),
                    "Local unit file witness differs from its recorded commit")
        prior = self.fingerprint(sha(source["commit"]), test_key)
        require(prior is not None, "Historical passing method source missing")
        if current == prior:
            return True
        if test_key not in self.changed:
            print(f"Unit applicability changed: {test_key}; prior={prior}; current={current}")
            self.changed.add(test_key)
        return False


def authenticate_journal(api, scope, artifact, prior):
    """Every proof one earlier final journal needs; raises unless all hold."""
    rid = positive(artifact["run_id"])
    require(artifact["name"] == key(scope), "artifact name filter mismatch")
    require(not artifact["expired"] and artifact["size_in_bytes"] <= MAX_BYTES,
            f"final artifact {positive(artifact['id'])} is expired or oversize")
    # The run must be this branch's effort-ci workflow run, as the forge lists it.
    require(prior is not None, "no effort-ci run of this branch has that id")
    commit = sha(prior["commit_sha"])
    jobs = api.pages(f"/actions/runs/{rid}/jobs")
    matching = [job for job in jobs if job["name"] == job_name(scope)]
    require(len(matching) == 1 and matching[0]["repo_id"] == scope["repository"],
            "ambiguous receipt job identity")
    # A re-run attempt shares the run id; its outcomes are not attributable.
    require(matching[0]["attempt"] == 1, "receipt job was re-run")
    markers = api.pages("/actions/artifacts", {"name": key(scope) + f"-start-{rid}"})
    require(len(markers) == 1 and markers[0]["run_id"] == rid and not markers[0]["expired"],
            "start marker missing, duplicated or expired")
    marker = artifact_json(api.bytes(f"/actions/artifacts/{positive(markers[0]['id'])}/zip"))
    validate_journal(marker, scope, rid, commit, completed=False)
    require(marker["complete"] is False, "start marker is not an initial attempt")
    journal = artifact_json(api.bytes(f"/actions/artifacts/{positive(artifact['id'])}/zip"))
    validate_journal(journal, scope, rid, commit)
    return journal


def restore(api, scope, run, applicability=None):
    """Adopt this PR's authenticated earlier passes; history never refuses.

    Each final journal under the PR's key is authenticated on its own (see
    authenticate_journal) and adopted only if every proof holds. A run with
    no journal contributes nothing, whatever the reason; an inherited record
    is kept only when the run it names was itself adopted. Anything not
    adopted is reported and its tests run again. Only this run's own
    integrity refuses. The CI prepare consumer also filters applicability.
    """
    attributions = set()
    passes = local_receipts(api, scope, applicability, attributions)
    try:
        artifacts = api.pages("/actions/artifacts", {"name": key(scope)})
    except IntegrityError:
        raise
    except NOT_ADOPTED as error:
        not_adopted("Prior final journals", error)
        artifacts = []
    by_run = {}
    for artifact in artifacts:
        try:
            rid = positive(artifact["run_id"])
        except IntegrityError:
            raise
        except NOT_ADOPTED as error:
            not_adopted("Final journal artifact", error)
            continue
        if rid == run:
            raise IntegrityError("This run already published units; dispatch a fresh run")
        by_run.setdefault(rid, []).append(artifact)
    runs = {}
    if by_run:
        try:
            runs = {positive(prior["id"]): prior for prior in api.pages(
                "/actions/runs", {"workflow_id": "effort-ci.yml",
                                  "ref": "refs/heads/" + scope["branch"]}, "workflow_runs")}
        except IntegrityError:
            raise
        except NOT_ADOPTED as error:
            not_adopted("Prior final journals", error)
            by_run = {}
    journals = {}
    # Newest first: if a request budget runs out, the oldest evidence is lost.
    for rid in sorted(by_run, reverse=True):
        try:
            require(len(by_run[rid]) == 1, "multiple final journals for one run")
            journals[rid] = authenticate_journal(api, scope, by_run[rid][0], runs.get(rid))
        except IntegrityError:
            raise
        except NOT_ADOPTED as error:
            not_adopted(f"Run {rid}: journal", error)
    trusted = {(rid, journal["commit"]) for rid, journal in journals.items()}
    for rid, journal in journals.items():
        dropped = 0
        for test_key, source in journal["passes"].items():
            if not ((source["run"], source["commit"]) in trusted
                    or (test_key, json.dumps(source, sort_keys=True)) in attributions):
                dropped += 1
                continue
            if applicability is None or applicability(test_key, source):
                passes.setdefault(test_key, source)
        if dropped:
            not_adopted(f"Run {rid}: {dropped} inherited record(s)",
                        "the run each names was not adopted")
    if applicability is not None:
        applicability.finish(passes)
    return passes


def attested_receipt(api, scope, comment, documents):
    """One comment's authenticated attestation of one local document, or None."""
    claims = [line.removeprefix("Python-Unit-Receipt: ")
              for line in comment["body"].splitlines()
              if line.startswith("Python-Unit-Receipt: ")]
    if not claims:
        return None
    require(len(claims) == 1, "ambiguous duplicate receipt attestations")
    claim = bounded_json(claims[0].encode())
    require(set(claim) == {"repository", "pr", "suite", "sha256"},
            "unknown receipt attestation fields")
    if claim["sha256"] not in documents:
        return None
    require(claim["repository"] == scope["repository"] and claim["pr"] == scope["pr"]
            and claim["suite"] in SUITES, "attestation repository/PR/suite mismatch")
    verify_attestor(api, scope, comment["user"])
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
            "local receipt does not prove exclusively passing tests")
    require(re.fullmatch(SUITES[proof["suite"]] + r"/test_[a-z0-9_]+\.py", proof["test_file"]),
            "unsafe local test source path")
    for test_id in proof["test_ids"]:
        require(isinstance(test_id, str) and len(test_id) <= 1024
                and test_id.startswith(Path(proof["test_file"]).stem + "."),
                "local receipt ID does not belong to named suite/module")
    source_commit = sha(proof["source_commit"]) if proof.get("source_commit") else None
    return claim["sha256"], proof, source_commit, positive(comment["id"])


def local_receipts(api, scope, applicability=None, attributions=None):
    """Only exact receipt hashes attested by authenticated repository writers.

    The directory is current source, so an unsafe entry refuses. A document
    whose attestation does not authenticate, or that nobody attested, is not
    adopted: its tests run again.
    """
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
    try:
        comments = api.pages(f"/issues/{scope['pr']}/comments")
    except IntegrityError:
        raise
    except NOT_ADOPTED as error:
        not_adopted("Local receipts", error)
        return {}
    passes, attested = {}, set()
    for comment in comments:
        label = comment.get("id") if isinstance(comment, dict) else None
        try:
            adopted = attested_receipt(api, scope, comment, documents)
        except IntegrityError:
            raise
        except NOT_ADOPTED as error:
            not_adopted(f"Local receipt attestation comment {label}", error)
            continue
        if adopted is None:
            continue
        digest, proof, source_commit, comment_id = adopted
        attested.add(digest)
        for test_id in proof["test_ids"]:
            test_key = proof["suite"] + ":" + test_id
            source = {"commit": source_commit, "run": None, "provenance":
                      f"attested-local:{digest}:{proof['test_file_sha256']}:comment-{comment_id}"}
            if attributions is not None:
                attributions.add((test_key, json.dumps(source, sort_keys=True)))
            if applicability is None or applicability(test_key, source):
                passes[test_key] = source
    for digest in sorted(set(documents) - attested):
        not_adopted(f"Local receipt {digest}", "no authenticated PR hash attestation")
    return passes


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
    def __init__(self, *args, journal, path, suite, known_ids, **kwargs):
        super().__init__(*args, **kwargs)
        self.journal, self.path, self.suite = journal, path, suite
        self.known_ids = known_ids

    def addSuccess(self, test):
        super().addSuccess(test)
        self.journal["passes"][self.suite + ":" + test.id()] = {
            "commit": self.journal["commit"], "run": self.journal["run"]}
        atomic_json(self.path, self.journal)

    def addError(self, test, error):
        super().addError(test, error)
        self.record_fixture_outcome(test)

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self.record_fixture_outcome(test)

    def record_fixture_outcome(self, test):
        if test.id() not in self.known_ids:
            error_id = self.suite + ":" + test.id()
            if error_id not in self.journal["fixture_errors"]:
                self.journal["fixture_errors"].append(error_id)
            atomic_json(self.path, self.journal)


def execute(journal, path, suites=None):
    if suites is None:
        require(journal.get("applicability_commit") == journal.get("commit"),
                "Production unit execution requires current CLI applicability prepare")
        applicability = SourceApplicability(sha(journal["commit"]))
        applicable = {test_key: source for test_key, source in journal["passes"].items()
                      if applicability(test_key, source)}
        applicability.finish(applicable)
        require(applicable == journal["passes"],
                "Prepared passes lack current applicability; prepare again")
    require(not journal.get("fixture_errors"), "Unresolved fixture error; never rerun successful methods")
    journal.setdefault("fixture_errors", [])
    # Discovery is an all-suite precondition: no first-suite successes before
    # a later import failure. Do not stamp a discovery refusal complete.
    inventories = {name: list(suites[name]) if suites is not None else discover(name) for name in SUITES}
    for tests in inventories.values():
        require(tests and len({test.id() for test in tests}) == len(tests), "Empty or duplicate unit inventory")
    failed = False
    for suite_name in SUITES:
        tests = inventories[suite_name]
        require(tests, "Empty unit suite")
        ids = [test.id() for test in tests]
        require(len(ids) == len(set(ids)), "Duplicate discovered test IDs")
        pending = [test for test in tests if suite_name + ":" + test.id() not in journal["passes"]]
        print(f"{suite_name}: discovered={len(tests)}, historical-passes={len(tests)-len(pending)}, pending={len(pending)}")
        if pending:
            runner = unittest.TextTestRunner(verbosity=2, resultclass=lambda *a, **kw:
                RecordingResult(*a, **kw, journal=journal, path=path, suite=suite_name,
                                known_ids=set(ids)))
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
                   "commit": commit, "complete": False, "fixture_errors": [],
                   "passes": restore(api, scope, run, applicability=SourceApplicability(commit)),
                   "applicability_commit": commit}
        atomic_json(path, journal)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"receipt_key={key(scope)}\nreceipt_job={job_name(scope)}\n")
        return 0
    journal = bounded_json(path.read_bytes())
    validate_journal(journal, scope, run, commit, completed=False)
    require(journal["complete"] is False, "Attempt already completed")
    return execute(journal, path)


if __name__ == "__main__":
    # Anything importing this module by name must share the CLI's exception
    # and API identities, or an `except ReceiptError` would miss the CLI's own.
    sys.modules["validation.python_unit_receipts"] = sys.modules[__name__]
    try:
        sys.exit(main())
    except Exception as error:
        # Never include request headers, token values, or response bodies.
        detail = str(error) if isinstance(error, ReceiptError) else "Evidence unavailable"
        print(f"Python receipt refusal: {type(error).__name__}: {detail}", file=sys.stderr)
        sys.exit(1)
