"""Once-per-PR Python unit evidence; compilation remains current-source evidence."""

from __future__ import annotations

import argparse
import ast
import base64
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
    """Evidence is unavailable, ambiguous, or invalid: do not execute units."""


class ReceiptHTTPError(ReceiptError):
    def __init__(self, status, path):
        self.status, self.path = status, path
        super().__init__(f"HTTP {status} at {path}; evidence unavailable")


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
        self.repository = repository
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect())
        self.requests = 0

    def bytes(self, path, query=None):
        self.requests += 1
        require(self.requests <= 200, "Receipt request budget exhausted; inspect retained evidence")
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


def pre_unit_recovery(api, scope, prior, jobs):
    """One source/run/job/log-bound failed prepare; never a generic skip list."""
    proof_path = Path("validation/python-unit-preunit-failure3705.json")
    require(proof_path.is_file() and not proof_path.is_symlink(), "Pre-unit recovery proof unavailable")
    proof = bounded_json(proof_path.read_bytes())
    require(set(proof) == {"version", "scope", "run", "job", "commit", "log_sha256",
                           "source_hashes", "provenance"} and proof["version"] == 1
            and proof["run"] == 3705 and proof["job"] == 39306
            and proof["scope"] == {"repository": 1, "pr": 668,
                                    "branch": "codex/python-unit-pr-receipts",
                                    "base": "effort/architecture-review-2026-09-20"}
            and proof["commit"] == "a3b795a37db1c796353a622028b0cf4d383f3c26"
            and proof["log_sha256"] == "61eda0ef7be00f3fc9ae12c07f5302a75b2cbaa5d7de7b66bb48841a01c158fe",
            "Unknown or corrupt bounded pre-unit recovery record")
    if proof["scope"] != scope or proof["run"] != prior["id"]:
        return False
    require(prior["commit_sha"] == proof["commit"], "Pre-unit recovery source mismatch")
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual["repository"]["id"] == scope["repository"]
            and actual["commit_sha"] == proof["commit"]
            and actual["prettyref"] == scope["branch"]
            and actual["workflow_id"] == "effort-ci.yml" and actual["status"] == "failure",
            "Pre-unit recovery run metadata mismatch")
    matching = [job for job in jobs if job["id"] == proof["job"]]
    require(len(matching) == 1 and matching[0]["run_id"] == proof["run"]
            and matching[0]["repo_id"] == scope["repository"] and matching[0]["attempt"] == 1
            and matching[0]["name"] == "" and matching[0]["status"] == "failure",
            "Pre-unit recovery job metadata mismatch")
    for path, expected in proof["source_hashes"].items():
        require(path in (".github/workflows/effort-ci.yml", "validation/python_unit_receipts.py"),
                "Unknown pre-unit recovery source")
        require(hashlib.sha256(api.bytes("/raw/" + path, {"ref": proof["commit"]})).hexdigest() == expected,
                "Pre-unit recovery exact-source hash mismatch")
    require(set(proof["source_hashes"]) == {
        ".github/workflows/effort-ci.yml", "validation/python_unit_receipts.py"}, "Incomplete source-order proof")
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(hashlib.sha256(raw).hexdigest() == proof["log_sha256"], "Pre-unit recovery log mismatch")
    log = raw.decode("utf-8")
    require(proof["commit"] in log
            and "Python receipt refusal: HTTPError: Evidence unavailable" in log
            and "skipping post step for 'Publish Python attempt-start marker'; main step was skipped" in log
            and "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped" in log
            and not any(marker in log for marker in ("discovered=", "historical-passes=", "pending=", "Ran ")),
            "Pre-unit recovery contradicts zero-unit source-order evidence")
    require(not api.pages("/actions/artifacts", {"name": key(scope) + f"-start-{proof['run']}"}),
            "Pre-unit recovery unexpectedly has a started attempt")
    print(f"Recovered failed prepare run {proof['run']}/job {proof['job']}: zero units, no successes imported")
    return True


def prepare_refusal_case():
    """One reviewed actual failure; callers cannot enroll arbitrary attempts."""
    path = Path('validation/python-unit-preunit-failure3747.json')
    require(path.is_file() and not path.is_symlink(), 'Prepare refusal proof unavailable')
    with path.open('rb') as source:
        raw = source.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES, 'Prepare refusal descriptor exceeds byte cap')
    require(hashlib.sha256(raw).hexdigest() ==
            'd19d54eb94dadfef94ab0af765885131465386b1258e047fde5bee876836fbad',
            'Unknown or corrupt prepare refusal proof')
    return bounded_json(raw)


def zero_unit_prepare_recovery(api, scope, prior, jobs):
    """Authenticate a missing-journal prepare refusal; import no successes."""
    proof = prepare_refusal_case()
    require(set(proof) == {'version', 'scope', 'run', 'job', 'commit',
                           'log_sha256', 'source_hashes', 'provenance'}
            and proof['version'] == 1, 'Invalid prepare refusal descriptor')
    if proof['scope'] != scope or proof['run'] != prior['id']:
        return False
    require(prior['commit_sha'] == proof['commit'], 'Prepare refusal prior source mismatch')
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual['id'] == proof['run']
            and actual['repository']['id'] == scope['repository']
            and actual['commit_sha'] == proof['commit']
            and actual['prettyref'] == scope['branch']
            and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'failure',
            'Prepare refusal run metadata mismatch')
    matching = [job for job in jobs if job['name'] == job_name(scope)]
    require(len(matching) == 1 and matching[0]['id'] == proof['job']
            and matching[0]['run_id'] == proof['run']
            and matching[0]['repo_id'] == scope['repository']
            and matching[0]['attempt'] == 1 and matching[0]['status'] == 'failure',
            'Prepare refusal job metadata mismatch')
    require(set(proof['source_hashes']) == {
        '.github/workflows/effort-ci.yml', 'validation/python_unit_receipts.py'},
        'Incomplete prepare source-order proof')
    for path, expected in proof['source_hashes'].items():
        require(hashlib.sha256(api.bytes('/raw/' + path, {'ref': proof['commit']})).hexdigest()
                == expected, 'Prepare refusal exact-source hash mismatch')
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(hashlib.sha256(raw).hexdigest() == proof['log_sha256'],
            'Prepare refusal log mismatch')
    log = raw.decode('utf-8')
    refusal = 'Python receipt refusal: ReceiptError: Local pass receipts await authenticated PR hash attestation'
    start = "skipping post step for 'Publish Python attempt-start marker'; main step was skipped"
    final = "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped"
    require(proof['commit'] in log and log.count(refusal) == 1
            and log.count(start) == 1 and log.count(final) == 1
            and log.index(refusal) < min(log.index(start), log.index(final))
            and not any(marker in log for marker in (
                'discovered=', 'historical-passes=', 'pending=', 'Ran ',
                'Unit discovery failed', 'fixture_errors', '... ok', '... FAIL', '... ERROR')),
            'Prepare refusal contradicts zero-unit phase evidence')
    require(not any(artifact['run_id'] == proof['run']
                    for artifact in api.pages('/actions/artifacts', {'name': key(scope)}))
            and not api.pages('/actions/artifacts', {'name': key(scope) + f"-start-{proof['run']}"})
            and api.get(f"/actions/runs/{proof['run']}/artifacts") == [],
            'Prepare refusal unexpectedly has start/final/run artifacts')
    print(f"Recovered failed prepare run {proof['run']}/job {proof['job']}: zero units, no successes imported")
    return True


def lost_journal_case():
    """One immutable archival witness, not caller-selected enrollment."""
    path = Path('validation/python-unit-lost-journals742.json')
    require(path.is_file() and not path.is_symlink(), 'Lost journal witness unavailable')
    with path.open('rb') as stream:
        raw = stream.read(MAX_BYTES + 1)
    digest = hashlib.sha256(raw).hexdigest()
    require(len(raw) <= MAX_BYTES and digest ==
            '31ef59fb7a45a480df9129499d34772fcfe071c03a284a2967caa4e8bd96ed31',
            'Unknown or corrupt lost journal witness')
    return bounded_json(raw), digest


def authenticate_lost_journal(api, scope, digest):
    expected = {'repository': scope['repository'], 'pr': scope['pr'], 'sha256': digest}
    matches = []
    for comment in api.pages(f"/issues/{scope['pr']}/comments"):
        for line in comment['body'].splitlines():
            if line.startswith('Python-Journal-Recovery: '):
                claim = bounded_json(line.removeprefix('Python-Journal-Recovery: ').encode())
                if claim == expected:
                    verify_attestor(api, scope, comment['user'])
                    matches.append(positive(comment['id']))
    require(matches, 'Lost journal awaits authenticated PR hash attestation')


def recovery_metadata(api, scope, prior, jobs, proof, status):
    require(prior['commit_sha'] == proof['commit'], 'Lost journal prior source mismatch')
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual['id'] == proof['run'] and actual['repository']['id'] == scope['repository']
            and actual['commit_sha'] == proof['commit'] and actual['prettyref'] == scope['branch']
            and actual['workflow_id'] == 'effort-ci.yml' and actual['status'] == 'failure',
            'Lost journal run metadata mismatch')
    matching = [job for job in jobs if job['name'] == job_name(scope)]
    require(len(matching) == 1 and matching[0]['id'] == proof['job']
            and matching[0]['run_id'] == proof['run'] and matching[0]['repo_id'] == scope['repository']
            and matching[0]['attempt'] == 1 and matching[0]['status'] == status,
            'Lost journal Python job metadata mismatch')
    require(set(proof['source_hashes']) == {
        '.github/workflows/effort-ci.yml', 'validation/python_unit_receipts.py'},
        'Lost journal source-order proof incomplete')
    for path, expected in proof['source_hashes'].items():
        require(hashlib.sha256(api.bytes('/raw/' + path, {'ref': proof['commit']})).hexdigest()
                == expected, 'Lost journal original source mismatch')
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(hashlib.sha256(raw).hexdigest() == proof['log_sha256'], 'Lost journal original log mismatch')
    require(proof['commit'] in raw.decode(), 'Lost journal checkout source absent')
    return [re.sub(r'^\d{4}-\d\d-\d\dT[0-9:.]+Z ', '', line)
            for line in raw.decode().splitlines()]


def recovery_absence(api, scope, run):
    require(not any(artifact['run_id'] == run for artifact in
                    api.pages('/actions/artifacts', {'name': key(scope)}))
            and not api.pages('/actions/artifacts', {'name': key(scope) + f'-start-{run}'})
            and api.get(f'/actions/runs/{run}/artifacts') == [],
            'Lost journal unexpectedly has live start/final/run artifacts')


def recover_lost_pr742(api, scope, prior, jobs):
    """Return (handled, journal); a zero-unit refusal never imports a journal."""
    # Avoid imposing this exceptional record or authority on unrelated PRs.
    if scope != {'repository': 1, 'pr': 742, 'branch': 'opus/client-evidence',
                 'base': 'effort/architecture-review-2026-09-20'} or prior['id'] not in (3915, 3930):
        return False, None
    proof, digest = lost_journal_case()
    require(proof['version'] == 1 and proof['scope'] == scope
            and proof['journal']['run'] == 3915 and proof['journal']['job'] == 40909
            and proof['journal']['commit'] == '5fa478987bf464ef6ab9b50bf49dea842acef018'
            and proof['refusal']['run'] == 3930 and proof['refusal']['job'] == 41018
            and proof['refusal']['commit'] == '2a299d44b38e67c029b66087f69edc4d5315222e',
            'Lost journal exact-case identity mismatch')
    authenticate_lost_journal(api, scope, digest)
    zero = prior['id'] == proof['refusal']['run']
    case = proof['refusal'] if zero else proof['journal']
    lines = recovery_metadata(api, scope, prior, jobs, case, 'failure' if zero else 'success')
    recovery_absence(api, scope, case['run'])
    if zero:
        refusal = 'Python receipt refusal: ReceiptError: Missing final receipt for run 3915; do not rerun possibly passed tests'
        start = "skipping post step for 'Publish Python attempt-start marker'; main step was skipped"
        final = "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped"
        require(lines.count(refusal) == lines.count(start) == lines.count(final) == 1
                and lines.index(refusal) < min(lines.index(start), lines.index(final))
                and not any(marker in '\n'.join(lines) for marker in (
                    'discovered=', 'historical-passes=', 'pending=', 'Ran ', 'Unit discovery failed',
                    'fixture_errors', '... ok', '... FAIL', '... ERROR')),
                'Lost journal refusal contradicts zero-unit phase evidence')
        print(f"Recovered failed prepare run {case['run']}/job {case['job']}: zero units, no successes imported")
        return True, None
    archives, uploads = {}, {}
    for label in ('start', 'final'):
        artifact = case['artifacts'][label]
        require(isinstance(artifact['zip_base64'], str) and len(artifact['zip_base64']) <= MAX_BYTES,
                'Lost journal encoded archive exceeds bound')
        try:
            raw = base64.b64decode(artifact['zip_base64'], validate=True)
        except ValueError:
            raise ReceiptError('Lost journal archive encoding invalid') from None
        require(len(raw) == artifact['size'] <= MAX_BYTES
                and hashlib.sha256(raw).hexdigest() == artifact['sha256'],
                'Lost journal archive witness mismatch')
        upload = (f"Artifact {key(scope)}" + (f"-start-{case['run']}" if label == 'start' else '')
                  + f" has been successfully uploaded! Final size is {artifact['size']} bytes. Artifact ID is {artifact['id']}")
        require(lines.count(upload) == 1, 'Lost journal upload metadata mismatch')
        uploads[label] = lines.index(upload)
        archives[label] = artifact_json(raw)
    start, journal = archives['start'], archives['final']
    validate_journal(start, scope, case['run'], case['commit'], completed=False)
    validate_journal(journal, scope, case['run'], case['commit'])
    require(start['complete'] is False and len(start['passes']) == case['baseline_count']
            and all(source == {'run': case['baseline_run'], 'commit': case['baseline_commit']}
                    and journal['passes'].get(test_key) == source
                    for test_key, source in start['passes'].items()),
            'Lost journal inherited baseline changed')
    fresh = {test_key: source for test_key, source in journal['passes'].items()
             if test_key not in start['passes']}
    require(all(source == {'run': case['run'], 'commit': case['commit']} for source in fresh.values())
            and {suite: sum(test_key.startswith(suite + ':') for test_key in fresh)
                 for suite in SUITES} == case['fresh_counts'], 'Lost journal fresh attribution mismatch')
    headers = [(i, re.match(r'^test\w+ \((test_[A-Za-z0-9_.]+)\)', line))
               for i, line in enumerate(lines)]
    headers = [(i, match.group(1)) for i, match in headers if match]
    require(len(headers) == len(fresh) and len({name for _, name in headers}) == len(fresh),
            'Lost journal individual events ambiguous')
    require(headers and uploads['start'] < headers[0][0]
            and headers[-1][0] < uploads['final'], 'Lost journal publication ordering mismatch')
    seen = set()
    for position, (i, name) in enumerate(headers):
        end = headers[position + 1][0] if position + 1 < len(headers) else len(lines)
        outcomes = [line for line in lines[i:end] if line == 'ok' or line.endswith(' ... ok')]
        candidates = [test_key for test_key in fresh if test_key.endswith(':' + name)]
        require(len(outcomes) == len(candidates) == 1, 'Lost journal lacks one positive event per fresh ID')
        seen.add(candidates[0])
    require(seen == set(fresh) and lines.count('OK') == 2
            and sum(bool(re.fullmatch(r'Ran 2 tests in [0-9.]+s', line)) for line in lines) == 1
            and sum(bool(re.fullmatch(r'Ran 7 tests in [0-9.]+s', line)) for line in lines) == 1
            and lines.count('validation: discovered=283, historical-passes=281, pending=2') == 1
            and lines.count('operations: discovered=652, historical-passes=645, pending=7') == 1
            and lines.index('validation: discovered=283, historical-passes=281, pending=2') < uploads['final']
            and lines.index('operations: discovered=652, historical-passes=645, pending=7') < uploads['final'],
            'Lost journal completed phase/count evidence mismatch')
    print(f"Recovered original {len(journal['passes'])} passes from run {case['run']}/job {case['job']}; no units replayed")
    return True, journal


def inherited_failure_case():
    """Two reviewed PR742 attempts; never a general incomplete waiver."""
    path = Path(__file__).resolve().parent / 'python-unit-inherited-failure742.json'
    require(path.is_file() and not path.is_symlink(), 'Inherited failure witness unavailable')
    with path.open('rb') as source:
        raw = source.read(MAX_BYTES + 1)
    digest = hashlib.sha256(raw).hexdigest()
    require(len(raw) <= MAX_BYTES and digest ==
            'c94d6df515346d3ce24a99959b112ad5585f9294dfc966937f6c22b1ce351e55',
            'Unknown or corrupt inherited failure witness')
    return bounded_json(raw), digest


def recover_inherited_pr742(api, scope, prior, jobs, marker=None, marker_raw=None,
                           artifact=None, final_raw=None, journal=None):
    """Preserve an unchanged inherited map, or prove an empty prepare refusal."""
    if scope != {'repository': 1, 'pr': 742, 'branch': 'opus/client-evidence',
                 'base': 'effort/architecture-review-2026-09-20'} or prior['id'] not in (3994, 4002):
        return False
    proof, digest = inherited_failure_case()
    require(proof['version'] == 1 and proof['scope'] == scope
            and (proof['inherited']['run'], proof['inherited']['job'], proof['inherited']['commit']) ==
            (3994, 41507, 'b18c01904af3f33193be3ee15db88bac69c519c7')
            and (proof['refusal']['run'], proof['refusal']['job'], proof['refusal']['commit']) ==
            (4002, 41568, '4a087b75be2ad886f664c72596c36f8cc0311914'),
            'Inherited failure exact-case identity mismatch')
    authenticate_lost_journal(api, scope, digest)
    zero = prior['id'] == 4002
    case = proof['refusal'] if zero else proof['inherited']
    lines = recovery_metadata(api, scope, prior, jobs, case, 'failure')
    log = '\n'.join(lines)
    require(not any(token in log for token in (
        'discovered=', 'historical-passes=', 'pending=', 'Ran ', 'Unit discovery failed',
        'fixture_errors', '... ok', '... FAIL', '... ERROR')),
        'Inherited failure contradicts zero-unit phase evidence')
    if zero:
        require(all(value is None for value in (marker, marker_raw, artifact, final_raw, journal)),
                'Prepare refusal unexpectedly has journal inputs')
        refusal = ('Python receipt refusal: ReceiptError: Incomplete receipt attempt 3994; '
                   'preserve artifact and recover individual evidence')
        start = "skipping post step for 'Publish Python attempt-start marker'; main step was skipped"
        final = "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped"
        require(lines.count(refusal) == lines.count(start) == lines.count(final) == 1
                and lines.index(refusal) < min(lines.index(start), lines.index(final)),
                'Inherited prepare refusal phase evidence mismatch')
        recovery_absence(api, scope, case['run'])
        print('Recovered failed prepare run 4002/job 41568: zero units, no successes imported')
        return True
    require(all(value is not None for value in (marker, marker_raw, artifact, final_raw, journal)),
            'Inherited failure requires both live start and final witnesses')
    uploads = []
    for label, item, raw, name in (
            ('start', marker, marker_raw, key(scope) + '-start-3994'),
            ('final', artifact, final_raw, key(scope))):
        expected = case['artifacts'][label]
        require(item['id'] == expected['id'] and item['run_id'] == case['run']
                and item['name'] == name and not item['expired']
                and item['size_in_bytes'] == len(raw) == expected['size'] <= MAX_BYTES
                and hashlib.sha256(raw).hexdigest() == expected['sha256'],
                'Inherited failure artifact mismatch')
        upload = (f"Artifact {name} has been successfully uploaded! Final size is "
                  f"{expected['size']} bytes. Artifact ID is {expected['id']}")
        require(lines.count(upload) == 1, 'Inherited failure upload evidence mismatch')
        uploads.append(lines.index(upload))
    start = artifact_json(marker_raw)
    validate_journal(start, scope, case['run'], case['commit'], completed=False)
    validate_journal(journal, scope, case['run'], case['commit'], completed=False)
    require(start == journal and start['complete'] is False
            and len(start['passes']) == case['count']
            and start.get('applicability_commit') == case['commit']
            and all(source in case['sources'] and source['run'] != case['run']
                    and source['commit'] != case['commit'] for source in start['passes'].values()),
            'Inherited failure changed its initial map or attribution')
    history = 'historical regression coverage is incomplete:'
    failure = 'make: *** [Makefile:140: history-check] Error 1'
    require(lines.count(history) == lines.count(failure) == 1
            and uploads[0] < lines.index(history) < lines.index(failure) < uploads[1]
            and lines.count("Job 'Python unit receipts' failed") == 1,
            'Inherited failure history-before-units ordering mismatch')
    print('Preserved unchanged inherited map from failed run 3994/job 41507; zero new passes')
    return True


def validate_journal(journal, scope, run, commit, completed=True):
    require(journal.get("version") == VERSION and journal.get("scope") == scope,
            "Receipt repository/PR/branch/base mismatch")
    require(journal.get("run") == run and journal.get("commit") == commit,
            "Receipt artifact/run/source mismatch")
    require(not completed or journal.get("complete") is True,
            f"Incomplete receipt attempt {run}; preserve artifact and recover individual evidence")
    fixture_errors = journal.get("fixture_errors")
    require(isinstance(fixture_errors, list) and len(fixture_errors) <= MAX_TESTS
            and all(isinstance(item, str) and len(item) <= 4096 for item in fixture_errors),
            "Invalid/missing unit fixture-error evidence")
    require(not fixture_errors,
            f"Unresolved fixture errors in run {run}: {'; '.join(fixture_errors[:3])}; "
            "preserve successes and recover failed-fixture-only evidence")
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


def discovery_recovery_case():
    """One reviewed historical case, not a caller-provided incomplete waiver."""
    path = Path(__file__).resolve().parent / "python-unit-discovery-failure3719.json"
    require(path.is_file() and not path.is_symlink(), "Discovery recovery record unavailable")
    raw = path.read_bytes()
    require(hashlib.sha256(raw).hexdigest() == "8f41ac8eb0fed5881df00ab0a2f4189db9af0a7057e3d3dbec4056138b2d1836",
            "Discovery recovery descriptor differs from approved exact record")
    proof = bounded_json(raw)
    require(proof["version"] == 1 and proof["run"] == 3719 and proof["job"] == 39416
            and proof["scope"] == {"repository": 1, "pr": 674,
                "branch": "codex/k06-owned-measurement-launcher",
                "base": "effort/architecture-review-2026-09-20"}
            and proof["commit"] == "cd2e2f1467ab0e77e8e9e09c90b75580d6411294"
            and proof["log_sha256"] == "1c7b7b0fd2738988a5e1aef91ebc7ad0c446e7d3882264b6b429fa665c2d0f40"
            and proof["count"] == 277 and proof["local_count"] == 4,
            "Unknown discovery recovery case")
    return proof


def recover_discovery_passes(api, scope, prior, job, marker, marker_raw,
                             artifact, final_raw, journal, legacy):
    proof = discovery_recovery_case()
    if scope != proof["scope"] or prior["id"] != proof["run"]:
        return False
    require(prior["commit_sha"] == proof["commit"], "Discovery recovery source mismatch")
    actual = api.get(f"/actions/runs/{proof['run']}")
    require(actual["repository"]["id"] == scope["repository"]
            and actual["commit_sha"] == proof["commit"]
            and actual["prettyref"] == scope["branch"]
            and actual["workflow_id"] == "effort-ci.yml" and actual["status"] == "failure",
            "Discovery recovery run metadata mismatch")
    require(job["id"] == proof["job"] and job["run_id"] == proof["run"]
            and job["repo_id"] == scope["repository"] and job["attempt"] == 1
            and job["name"] == job_name(scope) and job["status"] == "failure",
            "Discovery recovery job metadata mismatch")
    for item, raw, expected, name in (
            (marker, marker_raw, proof["start"], key(scope) + f"-start-{proof['run']}"),
            (artifact, final_raw, proof["final"], key(scope))):
        require(item["id"] == expected["id"] and item["run_id"] == proof["run"]
                and item["name"] == name and not item["expired"]
                and item["size_in_bytes"] == len(raw) == expected["bytes"]
                and hashlib.sha256(raw).hexdigest() == expected["sha256"],
                "Discovery recovery artifact mismatch")
    start = artifact_json(marker_raw)
    validate_journal(start, scope, proof["run"], proof["commit"], completed=False)
    validate_journal(journal, scope, proof["run"], proof["commit"], completed=False)
    require(start["complete"] is False and journal["complete"] is False,
            "Discovery recovery must retain incomplete historical attempt")
    baseline = start["passes"]
    require(len(baseline) == proof["local_count"]
            and all(k.startswith("operations:") and legacy.get(k) == source
                    and source["commit"] is None and source["run"] is None
                    and source.get("provenance", "").startswith("attested-local:")
                    and journal["passes"].get(k) == source for k, source in baseline.items()),
            "Discovery recovery local baseline lacks current authenticated attribution")
    fresh = {k: v for k, v in journal["passes"].items() if k not in baseline}
    require(len(fresh) == proof["count"] and all(k.startswith("validation:")
            and v == {"commit": proof["commit"], "run": proof["run"]}
            for k, v in fresh.items()), "Discovery recovery positive pass provenance mismatch")
    require(set(proof["source_hashes"]) == {
        ".github/workflows/effort-ci.yml", "validation/python_unit_receipts.py"},
        "Discovery recovery source-order proof incomplete")
    for path, digest in proof["source_hashes"].items():
        require(hashlib.sha256(api.bytes("/raw/" + path, {"ref": proof["commit"]})).hexdigest() == digest,
                "Discovery recovery original source mismatch")
    raw = api.bytes(f"/actions/jobs/{proof['job']}/logs")
    require(hashlib.sha256(raw).hexdigest() == proof["log_sha256"], "Discovery recovery log mismatch")
    lines = [re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line) for line in raw.decode().splitlines()]
    require(proof["commit"] in raw.decode() and lines.count(proof["summary"]) == 1
            and lines.count("OK") == 1 and lines.count(
                "Python receipt refusal: ReceiptError: Unit discovery failed; no cached pass may hide import errors") == 1
            and lines.count(f"validation: discovered={proof['count']}, historical-passes=0, pending={proof['count']}") == 1
            and not any(line.startswith("operations: discovered=") for line in lines),
            "Discovery recovery phase evidence mismatch")
    summary = lines.index(proof["summary"])
    okay = lines.index("OK")
    refusal = next(i for i, line in enumerate(lines) if line.startswith("Python receipt refusal:"))
    require(summary < okay < refusal, "Discovery failure preceded validation completion")
    headers = [(i, re.match(r"^test\w+ \((test_[A-Za-z0-9_.]+)\)", line))
               for i, line in enumerate(lines)]
    headers = [(i, match.group(1)) for i, match in headers if match]
    require(len(headers) == proof["count"] and len({identity for _, identity in headers}) == proof["count"]
            and all(i < summary for i, _ in headers), "Discovery recovery individual IDs ambiguous")
    for position, (i, identity) in enumerate(headers):
        end = headers[position + 1][0] if position + 1 < len(headers) else summary
        outcomes = [line for line in lines[i:end] if line == "ok" or line.endswith(" ... ok")]
        require(len(outcomes) == 1 and "validation:" + identity in fresh,
                "Discovery recovery lacks one positive event per journal ID")
    print(f"Recovered {proof['count']} individual validation passes from incomplete run {proof['run']}; operations not executed")
    return True


def test_source_path(test_key):
    match = re.fullmatch(
        r"(validation|operations):(test_[A-Za-z0-9_]+)\."
        r"([A-Za-z_][A-Za-z0-9_]*)\.(test[A-Za-z0-9_]*)", test_key)
    require(match is not None, "Unsupported unit source identity; preserve evidence")
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
        raise ReceiptError("Historical unit source unavailable; retain receipts and recover exact source") from None
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
        raise ReceiptError("Current unit source unavailable; preserve evidence") from None
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

    def source(self, commit, path):
        key = (sha(commit), path)
        if key in self.files:
            return self.files[key]
        require(len(self.files) < MAX_SOURCE_CACHE_FILES,
                "Unit applicability source-cache file bound exhausted")
        raw = self.source_reader(commit, path)
        require(isinstance(raw, bytes) and 0 < len(raw) <= MAX_TEST_SOURCE,
                "Unit applicability source exceeds byte bound")
        if commit == self.current_commit:
            require(self.worktree_reader(path) == raw,
                    "Current unit worktree differs from dispatched Git source")
        require(self.byte_count + len(raw) <= MAX_SOURCE_CACHE_BYTES,
                "Unit applicability source-cache byte bound exhausted")
        try:
            tree = ast.parse(raw, filename=path)
        except (SyntaxError, ValueError, RecursionError):
            raise ReceiptError("Unit source cannot be normalized; preserve evidence") from None
        nodes = sum(1 for _ in ast.walk(tree))
        require(nodes <= MAX_SOURCE_AST_NODES,
                "Unit applicability AST node bound exhausted")
        require(self.node_count + nodes <= MAX_SOURCE_CACHE_AST_NODES,
                "Unit applicability aggregate AST bound exhausted")
        require(not any(isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                        and node.name in ("load_tests", "__getattr__") for node in tree.body),
                "Unsupported dynamic unit discovery; preserve evidence")
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
                    "Unsupported nested/dynamic unit class; preserve evidence")
            require(class_name not in imported,
                    "Unsupported imported unit class; preserve evidence")
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
                        "Unsupported conditional unit method; preserve evidence")
                require(not any(isinstance(item, ast.Name) and isinstance(item.ctx, ast.Store)
                                and item.id == method for item in ast.walk(node)),
                        "Unsupported assigned unit method; preserve evidence")
            local_bases = []
            for base in node.bases:
                standard = (isinstance(base, ast.Attribute) and isinstance(base.value, ast.Name)
                            and base.value.id in unittest_names
                            and base.attr in ("TestCase", "IsolatedAsyncioTestCase"))
                standard |= isinstance(base, ast.Name) and base.id in case_names | {"object"}
                if isinstance(base, ast.Name) and base.id in classes:
                    local_bases.append(base.id)
                else:
                    require(standard, "Unsupported external/dynamic unit base; preserve evidence")
            inherited = []
            for base in local_bases:
                inherited.extend(visit(base, seen | {name}))
            return direct or inherited

        methods = visit(class_name, set())
        if not methods:
            return None
        require(len(methods) == 1, "Ambiguous inherited unit method; preserve evidence")
        normalized = ast.dump(methods[0], include_attributes=False)
        result = hashlib.sha256((context + "\n" + normalized).encode()).hexdigest()
        require(len(self.fingerprints) < MAX_TESTS * 2, "Unit fingerprint bound exhausted")
        self.fingerprints[key] = result
        return result

    def __call__(self, test_key, source):
        try:
            return self.matches(test_key, source)
        except ReceiptError as error:
            # A later authenticated candidate may prove this current method.
            # Otherwise finish refuses BEFORE any method can execute.
            self.refusals[test_key] = str(error)
            return False

    def finish(self, passes):
        unresolved = sorted(set(self.refusals) - set(passes))
        if unresolved:
            test_key = unresolved[0]
            raise ReceiptError(f"Unit applicability unavailable: {test_key}: "
                               f"{self.refusals[test_key]}; retain historical evidence")

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
                    "Null-source unit file changed; recover original bytes, never guess or replay")
            return True
        if source.get("provenance", "").startswith("attested-local:"):
            match = re.fullmatch(r"attested-local:[0-9a-f]{64}:([0-9a-f]{64}):comment-[1-9][0-9]*",
                                 source["provenance"])
            require(match is not None, "Local unit source lacks a bound file witness")
            path, _, _ = test_source_path(test_key)
            require(self.source(sha(source["commit"]), path)[0] == match.group(1),
                    "Local unit file witness differs from its recorded commit")
        prior = self.fingerprint(sha(source["commit"]), test_key)
        require(prior is not None, "Historical passing method source missing; preserve evidence")
        if current == prior:
            return True
        if test_key not in self.changed:
            print(f"Unit applicability changed: {test_key}; prior={prior}; current={current}")
            self.changed.add(test_key)
        return False


def restore(api, scope, run, applicability=None):
    """Authenticate history; the CI prepare consumer also filters applicability."""
    local_attributions = set()
    legacy = {**legacy_pr663(api, scope),
              **local_receipts(api, scope, applicability, local_attributions)}
    artifacts = api.pages("/actions/artifacts", {"name": key(scope)})
    indexed = {}
    for artifact in artifacts:
        require(artifact["name"] == key(scope), "Artifact name filter mismatch")
        aid, rid = positive(artifact["id"]), positive(artifact["run_id"])
        require(not artifact["expired"] and artifact["size_in_bytes"] <= MAX_BYTES,
                f"Expired/oversize receipt artifact {aid}; recover evidence before retry")
        require(rid not in indexed, "Multiple final journals for one run: ambiguous attempt")
        indexed[rid] = artifact
    passes = {test_key: source for test_key, source in legacy.items()
              if applicability is None or applicability(test_key, source)}
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
            workflow = api.bytes("/raw/.github/workflows/effort-ci.yml", {"ref": sha(prior["commit_sha"])})
            if b"python3 -m validation.python_unit_receipts prepare" in workflow:
                require(pre_unit_recovery(api, scope, prior, jobs),
                        f"Unknown receipt-policy attempt in run {rid}; recover exact job evidence")
            continue  # Legacy policy has no individually attributable journal.
        require(len(matching) == 1 and matching[0]["repo_id"] == scope["repository"],
                "Ambiguous receipt job identity")
        require(matching[0]["attempt"] == 1,
                f"Run {rid} was re-run; ambiguous receipt attempt, dispatch fresh runs only")
        from validation.python_unit_empty_recovery import recover_empty_pr920
        if recover_empty_pr920(api, scope, prior, jobs):
            indexed.pop(rid, None)
            continue  # Exact empty failures remain incomplete; no outcomes are imported.
        if matching[0]["status"] == "skipped":
            continue
        if rid not in indexed:
            if recover_inherited_pr742(api, scope, prior, jobs):
                continue
            from validation.python_unit_interrupted_recovery import recover_interrupted_pr767
            handled, recovered_journal = recover_interrupted_pr767(api, scope, prior, jobs, legacy, run)
            if handled:
                if recovered_journal is not None:
                    trusted_sources.add((rid, prior["commit_sha"]))
                    journals.append(recovered_journal)
                continue
            handled, recovered_journal = recover_lost_pr742(api, scope, prior, jobs)
            if handled:
                if recovered_journal is not None:
                    trusted_sources.add((rid, prior['commit_sha']))
                    journals.append(recovered_journal)
                continue
            if zero_unit_prepare_recovery(api, scope, prior, jobs):
                continue
        require(rid in indexed, f"Missing final receipt for run {rid}; do not rerun possibly passed tests")
        markers = api.pages("/actions/artifacts", {"name": key(scope) + f"-start-{rid}"})
        require(len(markers) == 1 and markers[0]["run_id"] == rid
                and not markers[0]["expired"], f"Missing/expired start marker for run {rid}")
        marker_raw = api.bytes(f"/actions/artifacts/{positive(markers[0]['id'])}/zip")
        marker = artifact_json(marker_raw)
        validate_journal(marker, scope, rid, sha(prior["commit_sha"]), completed=False)
        require(marker["complete"] is False, "Start marker is not an initial attempt")
        artifact = indexed.pop(rid)
        final_raw = api.bytes(f"/actions/artifacts/{artifact['id']}/zip")
        journal = artifact_json(final_raw)
        inherited = journal.get('complete') is False and recover_inherited_pr742(
            api, scope, prior, jobs, markers[0], marker_raw, artifact, final_raw, journal)
        from validation.python_unit_inherited_recovery920 import recover_inherited_pr920
        inherited = inherited or recover_inherited_pr920(
            api, scope, prior, jobs, markers[0], marker_raw, artifact, final_raw, journal)
        recovered = inherited or journal.get("complete") is False and recover_discovery_passes(
            api, scope, prior, matching[0], markers[0], marker_raw, artifact, final_raw, journal, legacy)
        validate_journal(journal, scope, rid, sha(prior["commit_sha"]), completed=not recovered)
        if not inherited:
            trusted_sources.add((rid, prior["commit_sha"]))
        journals.append(journal)
    require(not indexed, "Receipt artifacts lack corresponding trusted workflow/job metadata")
    for journal in journals:
        for test_key, source in journal["passes"].items():
            require((source["run"], source["commit"]) in trusted_sources
                    or legacy.get(test_key) == source
                    or (test_key, json.dumps(source, sort_keys=True)) in local_attributions,
                    "Success source has no trusted completed journal")
            if applicability is None or applicability(test_key, source):
                passes.setdefault(test_key, source)
    if applicability is not None:
        applicability.finish(passes)
    return passes


def local_receipts(api, scope, applicability=None, attributions=None):
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
    attested = set()
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
        verify_attestor(api, scope, author)
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
            test_key = proof["suite"] + ":" + test_id
            source = {
                "commit": sha(proof["source_commit"]) if proof.get("source_commit") else None, "run": None,
                "provenance": f"attested-local:{claim['sha256']}:{proof['test_file_sha256']}:comment-{positive(comment['id'])}"}
            attested.add(test_key)
            if attributions is not None:
                attributions.add((test_key, json.dumps(source, sort_keys=True)))
            if applicability is None or applicability(test_key, source):
                passes[test_key] = source
    # A present but unattested proof must not quietly rerun its successful IDs.
    claimed = {test_id for proof in documents.values()
               for test_id in (proof["suite"] + ":" + item for item in proof["test_ids"])}
    require(claimed.issubset(attested), "Local pass receipts await authenticated PR hash attestation")
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
                "Prepared passes lack current applicability; retain evidence and prepare again")
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
    # Recovery consumers must share the CLI's exception and API identities.
    sys.modules["validation.python_unit_receipts"] = sys.modules[__name__]
    try:
        sys.exit(main())
    except Exception as error:
        # Never include request headers, token values, or response bodies.
        detail = str(error) if isinstance(error, ReceiptError) else "Evidence unavailable; preserve run/job identity"
        print(f"Python receipt refusal: {type(error).__name__}: {detail}", file=sys.stderr)
        sys.exit(1)
