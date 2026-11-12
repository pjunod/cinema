"""Run the Rust unit tests a pull request can affect, once, across the fleet.

Four steps, one module (docs/ci/RUST-UNIT-FANOUT.md is the operating guide):

* ``select`` gives every unit crate a content key — a SHA-256 over the exact
  inputs its tests can observe (validation/rust-unit-inputs.toml defines
  them) — at the PR head and at the merge-base. A crate is due when the two
  keys differ. A change to a crate re-keys it and every crate that depends on
  it; a workspace-level input re-keys them all; anything else re-keys nothing.
* ``adopt`` looks up authenticated receipts for each due crate's head key.
  A test that already passed under that exact key — in any earlier run, on any
  branch — is not run again. A crate whose whole test listing passed is
  dropped. Anything that cannot be authenticated is simply not adopted.
* ``plan`` / ``shard`` build one cargo-nextest archive of the due crates and
  run it in ``hash:i/N`` partitions on idle runners.
* ``verdict`` / ``baseline`` / ``gate`` prove the partitions covered the plan
  exactly once, publish per-crate receipts, and classify any failure by
  running it on the merge-base: a test that fails there too is a pre-existing
  failure on main, reported and not blocking; anything else blocks.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import hashlib
import io
import json
import math
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

from validation.runner import matches


SCHEMA = 1
KEY_VERSION = "rust-units-key-v1"
CONFIG = Path("validation/rust-unit-inputs.toml")
STATE = Path("target/rust-units")
NEXTEST_VERSION = "0.9.151"
# Partitioning: one shard until a selection is worth splitting, then about this
# many tests per shard, never more shards than the x64 `general` pool holds.
TESTS_PER_SHARD = 700
MAX_SHARDS = 6
# One argument may not exceed Linux's MAX_ARG_STRLEN (128 KiB).
MAX_FILTER_BYTES = 96 * 1024
MAX_RECEIPT_BYTES = 8 * 1024 * 1024
MAX_CANDIDATES = 6
JOB_SELECT = "Rust units: select"
JOB_ARCHIVE = "Rust units: build archive"
JOB_SHARD = "Rust units shard {index}"
JOB_VERDICT = "Rust units: verdict"
JOB_BASELINE = "Rust units: merge-base check of failures"
ARTIFACT_RECEIPT = "rust-units-{crate}-{key}"
ARTIFACT_SHARD = "rust-units-shard-{run}-{index}"
ARTIFACT_ARCHIVE = "rust-units-archive-{run}"
CRATE_NAME = re.compile(r"[a-z0-9][a-z0-9_-]{0,63}")
HEX64 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
# nextest's per-test status lines, as printed with --color never. Newer
# releases may insert a "(done/total)" counter after the duration.
STATUS_LINE = re.compile(
    r"^\s*(PASS|LEAK|FAIL|SIGSEGV|SIGABRT|SIGKILL|SIGBUS|TIMEOUT|LEAK-FAIL|ABORT)"
    r" \[\s*[0-9.]+s\](?: \(\s*[0-9]+/\s*[0-9]+\))? (\S+) (\S.*?)\s*$")


class UnitsError(RuntimeError):
    """The current run cannot proceed; this fails the job."""


class NotAdopted(RuntimeError):
    """A historical receipt does not authenticate; its tests run again."""


def require(condition, message, error=UnitsError):
    if not condition:
        raise error(message)


def emit(name, value):
    """Write a step output (also printed, so a log shows every decision)."""
    text = value if isinstance(value, str) else json.dumps(value, sort_keys=True, separators=(",", ":"))
    require("\n" not in text, f"output {name} spans lines")
    print(f"{name}={text}")
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as stream:
            stream.write(f"{name}={text}\n")


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True, indent=1) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def read_json(path, limit=MAX_RECEIPT_BYTES):
    data = Path(path).read_bytes()
    require(len(data) <= limit, f"{path} exceeds {limit} bytes")
    return json.loads(data)


def digest_lines(lines):
    hasher = hashlib.sha256()
    for line in lines:
        hasher.update(line.encode("utf-8"))
        hasher.update(b"\n")
    return hasher.hexdigest()


# --------------------------------------------------------------------------
# A git tree, read without a checkout


class Tree:
    """Every tracked file of one commit: path -> (mode, blob id).

    Keys are computed from git objects, so the merge-base, the head and a
    historical receipt's commit are all read the same way, without checking
    anything out.
    """

    def __init__(self, revision, repository="."):
        self.repository = str(repository)
        self.commit = self._git("rev-parse", "--verify", f"{revision}^{{commit}}").decode().strip()
        listing = self._git("ls-tree", "-r", "-z", "--full-tree", self.commit)
        self.entries = {}
        for record in listing.split(b"\0"):
            if not record:
                continue
            meta, path = record.split(b"\t", 1)
            mode, kind, oid = meta.decode().split(" ")
            if kind == "blob":
                self.entries[path.decode("utf-8", "surrogateescape")] = (mode, oid)
        self.directories = set()
        for path in self.entries:
            parts = path.split("/")
            for end in range(1, len(parts)):
                self.directories.add("/".join(parts[:end]))
        self._blobs = {}

    def _git(self, *arguments, data=None):
        try:
            return subprocess.run(["git", "-C", self.repository, *arguments], input=data,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  check=True, timeout=120).stdout
        except subprocess.CalledProcessError as error:
            raise UnitsError(f"git {' '.join(arguments[:2])} failed: "
                             f"{error.stderr.decode(errors='replace').strip()[:300]}") from None

    def under(self, directory):
        prefix = directory.rstrip("/") + "/"
        return [path for path in self.entries if path.startswith(prefix)]

    def read(self, paths):
        """Blob contents for many paths in one `git cat-file --batch`."""
        wanted = [path for path in paths if path not in self._blobs]
        if wanted:
            request = "".join(self.entries[path][1] + "\n" for path in wanted).encode()
            output = self._git("cat-file", "--batch", data=request)
            offset = 0
            for path in wanted:
                header_end = output.index(b"\n", offset)
                _oid, _kind, size = output[offset:header_end].split(b" ")
                start = header_end + 1
                self._blobs[path] = output[start:start + int(size)]
                offset = start + int(size) + 1
        return {path: self._blobs[path] for path in paths}

    def text(self, path):
        return self.read([path])[path].decode("utf-8", "replace")


# --------------------------------------------------------------------------
# The workspace graph, from the manifests in the tree


@dataclass
class Crate:
    name: str
    directory: str
    # Workspace or external path dependencies by kind: normal, build, dev.
    deps: dict = field(default_factory=lambda: {"normal": set(), "build": set(), "dev": set()})
    external: set = field(default_factory=set)


def _dependency_tables(manifest):
    for kind, table in (("normal", "dependencies"), ("build", "build-dependencies"),
                        ("dev", "dev-dependencies")):
        yield kind, manifest.get(table, {})
        for target in manifest.get("target", {}).values():
            yield kind, target.get(table, {})


def workspace(tree):
    """Workspace members with their path dependencies, from the tree's manifests.

    Only path dependencies matter: registry crates are pinned by Cargo.lock,
    which is an input of every crate.
    """
    root = tomllib.loads(tree.text("Cargo.toml"))
    section = root.get("workspace")
    require(isinstance(section, dict) and isinstance(section.get("members"), list),
            "Cargo.toml has no [workspace] members")
    member_dirs = []
    for pattern in section["members"]:
        if any(marker in pattern for marker in "*?["):
            member_dirs.extend(sorted(d for d in tree.directories
                                      if PurePosixPath(d).match(pattern)
                                      and f"{d}/Cargo.toml" in tree.entries))
        else:
            member_dirs.append(pattern.rstrip("/"))
    shared = section.get("dependencies", {})
    crates, by_dir = {}, {}
    manifests = {}
    for directory in member_dirs:
        path = f"{directory}/Cargo.toml"
        require(path in tree.entries, f"workspace member {directory} has no Cargo.toml")
        manifest = tomllib.loads(tree.text(path))
        name = manifest.get("package", {}).get("name")
        require(isinstance(name, str) and CRATE_NAME.fullmatch(name), f"{path}: invalid package name")
        require(name not in crates, f"duplicate workspace member {name}")
        crates[name] = Crate(name, directory)
        by_dir[directory] = name
        manifests[name] = manifest
    for name, manifest in manifests.items():
        crate = crates[name]
        for kind, table in _dependency_tables(manifest):
            for key, spec in table.items():
                if not isinstance(spec, dict):
                    continue
                if spec.get("workspace") is True:
                    inherited = shared.get(key, {})
                    relative, base = (inherited.get("path"), "") if isinstance(inherited, dict) else (None, "")
                else:
                    relative, base = spec.get("path"), crate.directory
                if not relative:
                    continue
                target = posixpath.normpath(posixpath.join(base, relative))
                if target in by_dir:
                    if by_dir[target] != name:
                        crate.deps[kind].add(by_dir[target])
                else:
                    crate.external.add(target)
    return crates


def closure(crates, name):
    """C, its normal/build/dev dependencies, and their normal/build closure.

    A dependency's own dev-dependencies are not compiled into C's tests.
    """
    first = crates[name]
    seen = {name}
    stack = sorted(first.deps["normal"] | first.deps["build"] | first.deps["dev"])
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        stack.extend(sorted(crates[current].deps["normal"] | crates[current].deps["build"]))
    return seen


def reverse_dependents(crates, name):
    return {other for other in crates if name in closure(crates, other)}


# --------------------------------------------------------------------------
# Inputs and keys


@dataclass
class Config:
    excluded: tuple
    harness: tuple
    exempt: tuple
    dynamic: tuple
    serial_ignored: dict

    @classmethod
    def load(cls, text):
        data = tomllib.loads(text)
        require(data.get("version") == 1, "rust-unit-inputs.toml must have version = 1")
        dynamic = []
        for index, site in enumerate(data.get("dynamic", [])):
            require(set(site) == {"crate", "file", "function", "reads", "reason"},
                    f"dynamic[{index}] must have exactly crate, file, function, reads, reason")
            require(isinstance(site["reads"], list) and all(isinstance(r, str) for r in site["reads"])
                    and isinstance(site["reason"], str) and site["reason"].strip(),
                    f"dynamic[{index}] needs reads (a list) and a reason")
            dynamic.append(site)
        serial = {}
        for index, entry in enumerate(data.get("serial_ignored", [])):
            require(set(entry) == {"crate", "tests"} and isinstance(entry["tests"], list),
                    f"serial_ignored[{index}] must have exactly crate and tests")
            serial.setdefault(entry["crate"], []).extend(entry["tests"])
        return cls(tuple(data.get("excluded_crates", [])), tuple(data.get("harness", [])),
                   tuple(data.get("exempt", [])), tuple(dynamic), serial)


LITERAL = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
DYNAMIC_ROOT = re.compile(r"\.parent\(\)|\.ancestors\(\)|\.pop\(\)")
FUNCTION = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")


def rust_sources(tree, crate):
    return sorted(p for p in tree.under(crate.directory) if p.endswith(".rs"))


def literal_references(tree, crate):
    """Tracked paths outside the crate named through `../` in a string literal.

    Each literal is resolved against both the file's directory (include_str!)
    and the manifest directory (concat!(env!("CARGO_MANIFEST_DIR"), ..) and
    run-time joins, since tests run with the package as working directory).
    A false match only adds an input; it can never remove one.
    """
    found = set()
    sources = rust_sources(tree, crate)
    for path, data in tree.read(sources).items():
        text = data.decode("utf-8", "replace")
        if "../" not in text:
            continue
        for match in LITERAL.finditer(text):
            value = match.group(1)
            if "../" not in value:
                continue
            for base in (posixpath.dirname(path), crate.directory):
                target = posixpath.normpath(posixpath.join(base, value.lstrip("/")))
                if target.startswith("..") or target == crate.directory \
                        or target.startswith(crate.directory + "/"):
                    continue
                if target in tree.entries or target in tree.directories:
                    found.add(target)
    return found


def dynamic_sites(tree, crate):
    """(file, function) of each run-time workspace walk the scanner cannot follow."""
    sites = set()
    for path, data in tree.read(rust_sources(tree, crate)).items():
        lines = data.decode("utf-8", "replace").split("\n")
        for number, line in enumerate(lines):
            if "CARGO_MANIFEST_DIR" not in line or "include" in line:
                continue
            if not DYNAMIC_ROOT.search("\n".join(lines[number:number + 5])):
                continue
            function = "?"
            for back in range(number, -1, -1):
                found = FUNCTION.search(lines[back])
                if found:
                    function = found.group(1)
                    break
            sites.add((path, function))
    return sites


class Keys:
    """Content keys for every unit crate of one tree."""

    def __init__(self, tree, config):
        self.tree, self.config = tree, config
        self.crates = workspace(tree)
        self.units = sorted(name for name in self.crates if name not in config.excluded)
        shared = []
        for path in self.tree.entries:
            if matches(path, config.harness):
                shared.append(path)
            elif not path.startswith("crates/") and not matches(path, config.exempt):
                shared.append(path)
        self.shared = set(shared)
        self._own = {}

    def own_inputs(self, name):
        """Inputs a crate contributes to every crate whose closure holds it."""
        if name not in self._own:
            crate = self.crates[name]
            paths = set(self.tree.under(crate.directory))
            references = literal_references(self.tree, crate)
            for site in self.config.dynamic:
                if site["crate"] == name:
                    for pattern in site["reads"]:
                        paths.update(p for p in self.tree.entries if matches(p, (pattern,)))
            for directory in crate.external:
                references.add(directory)
            for target in references:
                if target in self.tree.entries:
                    paths.add(target)
                else:
                    paths.update(self.tree.under(target))
            self._own[name] = paths
        return self._own[name]

    def inputs(self, name):
        paths = set(self.shared)
        for member in closure(self.crates, name):
            paths |= self.own_inputs(member)
        return paths

    def key(self, name):
        lines = [KEY_VERSION, f"crate {name}"]
        for path in sorted(self.inputs(name)):
            mode, oid = self.tree.entries[path]
            lines.append(f"{mode} {oid} {path}")
        return digest_lines(lines)

    def all_keys(self):
        return {name: self.key(name) for name in self.units}


def dynamic_site_drift(keys):
    """(undeclared, stale) run-time workspace walks versus the declarations."""
    found = set()
    for name, crate in keys.crates.items():
        if name in keys.config.excluded:
            continue
        found |= {(name, path, function) for path, function in dynamic_sites(keys.tree, crate)}
    declared = {(site["crate"], site["file"], site["function"]) for site in keys.config.dynamic}
    return sorted(found - declared), sorted(declared - found)


def changed_inputs(head, base, name, limit=5):
    """A few input paths that differ, to say why a crate is due."""
    if name not in base.crates:
        return ["(crate is new)"]
    now, before = head.inputs(name), base.inputs(name)
    changed = sorted(p for p in now | before
                     if head.tree.entries.get(p) != base.tree.entries.get(p)
                     or (p in now) != (p in before))
    return changed[:limit] + ([f"... {len(changed) - limit} more"] if len(changed) > limit else [])


def selection(head_rev, base_rev, repository=".", config=None):
    head_tree = Tree(head_rev, repository)
    config = config or Config.load(head_tree.text(str(CONFIG)))
    head = Keys(head_tree, config)
    base_tree = Tree(base_rev, repository)
    try:
        base = Keys(base_tree, config)
        base_error = None
    except (UnitsError, tomllib.TOMLDecodeError, KeyError) as error:
        base, base_error = None, str(error)
    crates = {}
    for name in head.units:
        key = head.key(name)
        base_key = base.key(name) if base and name in base.crates else None
        due = key != base_key
        reason = (changed_inputs(head, base, name) if base and due
                  else [f"merge-base workspace unreadable: {base_error}"] if due else [])
        crates[name] = {"key": key, "base_key": base_key, "due": due, "reason": reason}
    return {"schema": SCHEMA, "head": head_tree.commit, "base": base_tree.commit,
            "crates": crates}


# --------------------------------------------------------------------------
# Receipts: authenticated per-test outcomes under a crate key


def receipt_name(crate, key):
    require(CRATE_NAME.fullmatch(crate) and HEX64.fullmatch(key), "invalid receipt identity")
    return ARTIFACT_RECEIPT.format(crate=crate, key=key)


def validate_receipt(receipt, crate, key):
    require(isinstance(receipt, dict) and receipt.get("schema") == SCHEMA
            and receipt.get("producer") == "validation.rust_units", "not a rust_units receipt", NotAdopted)
    require(receipt.get("crate") == crate and receipt.get("key") == key, "receipt names another crate or key",
            NotAdopted)
    for field_name in ("passed", "failed"):
        values = receipt.get(field_name)
        require(isinstance(values, list) and all(isinstance(v, str) and " " in v for v in values)
                and len(values) == len(set(values)), f"receipt {field_name} is malformed", NotAdopted)
    require(not set(receipt["passed"]) & set(receipt["failed"]), "a test both passed and failed", NotAdopted)
    jobs = receipt.get("evidence_jobs")
    require(isinstance(jobs, list) and jobs and all(isinstance(j, str) for j in jobs),
            "receipt names no evidence jobs", NotAdopted)
    listing = receipt.get("listing")
    require(listing is None or (isinstance(listing, dict) and isinstance(listing.get("count"), int)
                                and HEX64.fullmatch(str(listing.get("sha256")))),
            "receipt listing is malformed", NotAdopted)
    require(COMMIT.fullmatch(str(receipt.get("commit"))) and isinstance(receipt.get("run"), int),
            "receipt has no run identity", NotAdopted)


def logged_outcomes(log):
    """{test id: 'PASS' | 'FAIL'} from nextest status lines in a job log."""
    outcomes = {}
    for line in log.splitlines():
        # Forgejo prefixes each line with an RFC 3339 timestamp.
        line = re.sub(r"^\d{4}-\d\d-\d\dT[0-9:.]+Z ", "", line)
        found = STATUS_LINE.match(line)
        if found:
            status, binary, test = found.groups()
            # LEAK is a pass whose test left handles open (nextest's default).
            outcomes[f"{binary} {test}"] = "PASS" if status in ("PASS", "LEAK") else "FAIL"
    return outcomes


class Forge:
    """The CI API, read with the job token. Only GETs; bounded responses."""

    def __init__(self, root, repository, token, budget=300):
        parsed = urllib.parse.urlsplit(root)
        require(parsed.scheme in ("http", "https") and parsed.hostname and not parsed.username
                and not parsed.query, "invalid CI API origin")
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository), "invalid repository slug")
        require(token, "no CI token: receipts cannot be read")
        self.root = root.rstrip("/") + "/repos/" + repository
        self.token = token
        self.budget = budget

    def bytes(self, path, query=None, limit=MAX_RECEIPT_BYTES):
        require(self.budget > 0, "CI API request budget exhausted", NotAdopted)
        self.budget -= 1
        require(path.startswith("/") and ".." not in path and re.fullmatch(r"[A-Za-z0-9_./-]+", path),
                "unsafe API path")
        url = self.root + path + ("?" + urllib.parse.urlencode(query) if query else "")
        request = urllib.request.Request(url, headers={"Authorization": "token " + self.token})
        opener = urllib.request.build_opener(_NoCredentialRedirect())
        try:
            with opener.open(request, timeout=30) as response:
                data = response.read(limit + 1)
        except urllib.error.HTTPError as error:
            raise NotAdopted(f"HTTP {error.code} at {path}") from None
        except OSError as error:
            raise NotAdopted(f"{type(error).__name__} at {path}") from None
        require(len(data) <= limit, f"response from {path} exceeds its bound", NotAdopted)
        return data

    def get(self, path, query=None):
        return json.loads(self.bytes(path, query))


class _NoCredentialRedirect(urllib.request.HTTPRedirectHandler):
    """Artifact downloads may redirect to signed storage; the token stays home."""

    def redirect_request(self, request, fp, code, msg, headers, newurl):
        parsed = urllib.parse.urlsplit(newurl)
        require(parsed.scheme in ("http", "https") and not parsed.username, "unsafe redirect", NotAdopted)
        return urllib.request.Request(newurl)


def forge_from_environment():
    return Forge(os.environ["GITHUB_API_URL"], os.environ["GITHUB_REPOSITORY"],
                 os.environ.get("GITHUB_TOKEN", ""))


def ensure_commit(commit, repository="."):
    """Make a historical receipt's commit readable, or say it cannot be."""
    def present():
        return subprocess.run(["git", "-C", repository, "cat-file", "-e", commit + "^{commit}"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    if present():
        return True
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        import base64
        auth = base64.b64encode(f"x-access-token:{token}".encode()).decode()
        subprocess.run(["git", "-C", repository, "-c", f"http.extraheader=AUTHORIZATION: basic {auth}",
                        "fetch", "--quiet", "--no-tags", "--depth=1", "origin", commit],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
    return present()


def unzip_receipt(data):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        require(len(members) == 1 and members[0].filename == "receipt.json"
                and members[0].file_size <= MAX_RECEIPT_BYTES, "unexpected receipt archive", NotAdopted)
        return json.loads(archive.read(members[0]))


class Authenticator:
    """Checks one historical receipt against what the forge itself recorded.

    A receipt is the producer's claim. It is adopted only when the forge
    confirms the run belongs to this repository and was for the receipt's
    commit, the publishing job uploaded exactly that artifact, every adopted
    pass appears as a nextest PASS line in a named evidence job's own log,
    and the key — recomputed by *this* code from the receipt's commit —
    equals the key being looked up. Because the key covers this module, the
    workflow and the CI image, a run whose harness differs can never match.
    """

    def __init__(self, forge, current_run, config, repository="."):
        self.forge, self.current_run, self.config = forge, current_run, config
        self.repository = repository
        self._keys = {}
        self._jobs = {}
        self._logs = {}

    def key_at(self, commit, crate):
        if commit not in self._keys:
            require(ensure_commit(commit, self.repository), f"commit {commit[:12]} is not fetchable", NotAdopted)
            tree = Tree(commit, self.repository)
            self._keys[commit] = Keys(tree, self.config)
        keys = self._keys[commit]
        require(crate in keys.units, f"{crate} is not a unit crate at {commit[:12]}", NotAdopted)
        return keys.key(crate)

    def jobs(self, run):
        if run not in self._jobs:
            jobs = self.forge.get(f"/actions/runs/{run}/jobs", {"page": 1, "limit": 50})
            require(isinstance(jobs, list), "job listing is not a list", NotAdopted)
            self._jobs[run] = jobs
        return self._jobs[run]

    def log(self, job_id):
        if job_id not in self._logs:
            self._logs[job_id] = self.forge.bytes(f"/actions/jobs/{job_id}/logs", limit=32 * 1024 * 1024
                                                  ).decode("utf-8", "replace")
        return self._logs[job_id]

    def job(self, run, name, statuses):
        found = [job for job in self.jobs(run) if job.get("name") == name]
        require(len(found) == 1, f"run {run} has {len(found)} jobs named {name!r}", NotAdopted)
        require(found[0].get("status") in statuses, f"{name!r} in run {run} is {found[0].get('status')}",
                NotAdopted)
        return found[0]

    def authenticate(self, artifact, crate, key):
        name = receipt_name(crate, key)
        run = artifact.get("run_id")
        require(artifact.get("name") == name and isinstance(run, int) and 0 < run
                and artifact.get("expired") is False, "artifact identity mismatch", NotAdopted)
        require(run != self.current_run, "the current run is not history", NotAdopted)
        meta = self.forge.get(f"/actions/runs/{run}")
        require(meta.get("id") == run, "run identity mismatch", NotAdopted)
        size = artifact.get("size_in_bytes")
        require(isinstance(size, int) and 0 < size <= MAX_RECEIPT_BYTES, "artifact size out of bounds", NotAdopted)
        receipt = unzip_receipt(self.forge.bytes(f"/actions/artifacts/{artifact['id']}/zip"))
        validate_receipt(receipt, crate, key)
        require(receipt["run"] == run and receipt["commit"] == meta.get("commit_sha"),
                "receipt run/commit disagree with the forge", NotAdopted)
        publisher = self.job(run, receipt.get("publisher"), {"success", "failure"})
        require(self.log(publisher["id"]).count(f"Artifact {name} has been successfully uploaded!") >= 1,
                "the publishing job's log does not show this upload", NotAdopted)
        require(self.key_at(receipt["commit"], crate) == key,
                "key recomputed from the receipt's commit differs", NotAdopted)
        logged = {}
        for job_name in receipt["evidence_jobs"]:
            job = self.job(run, job_name, {"success", "failure"})
            logged.update(logged_outcomes(self.log(job["id"])))
        passed = {test for test in receipt["passed"] if logged.get(test) == "PASS"}
        failed = {test for test in receipt["failed"] if logged.get(test) == "FAIL"}
        return {"run": run, "commit": receipt["commit"], "listing": receipt.get("listing"),
                "passed": passed, "failed": failed, "unproven": len(receipt["passed"]) - len(passed)}


def history(forge, authenticator, crate, key):
    """Union of authenticated outcomes for one crate key across earlier runs."""
    name = receipt_name(crate, key)
    try:
        artifacts = forge.get("/actions/artifacts", {"name": name, "page": 1, "limit": 50})
    except NotAdopted as error:
        print(f"{crate}: receipt lookup unavailable ({error}); its tests run")
        return {"passed": set(), "failed": set(), "listings": [], "runs": []}
    if isinstance(artifacts, dict):
        artifacts = artifacts.get("artifacts", [])
    artifacts = sorted((a for a in artifacts if isinstance(a, dict) and a.get("name") == name),
                       key=lambda a: a.get("run_id", 0), reverse=True)[:MAX_CANDIDATES]
    passed, failed, listings, runs = set(), set(), [], []
    for artifact in artifacts:
        try:
            outcome = authenticator.authenticate(artifact, crate, key)
        except (NotAdopted, UnitsError, KeyError, TypeError, ValueError, zipfile.BadZipFile) as error:
            print(f"{crate}: receipt from run {artifact.get('run_id')} not adopted — "
                  f"{type(error).__name__}: {error}; its tests run")
            continue
        passed |= outcome["passed"]
        failed |= outcome["failed"]
        runs.append(outcome["run"])
        if outcome["listing"]:
            listings.append(outcome["listing"])
        note = f", {outcome['unproven']} claimed passes not in its logs" if outcome["unproven"] else ""
        print(f"{crate}: adopted {len(outcome['passed'])} passes from run {outcome['run']}{note}")
    # A pass recorded anywhere under this key outranks a failure elsewhere:
    # every test only has to pass once on these exact inputs.
    return {"passed": passed, "failed": failed - passed, "listings": listings, "runs": runs}


def fully_passed(listing_options, passed):
    for listing in listing_options:
        if listing["count"] == len(passed) and listing["sha256"] == digest_lines(sorted(passed)):
            return True
    return False


# --------------------------------------------------------------------------
# Plan: which tests each shard runs


def nextest_suites(listing):
    """{binary id: {test name: ignored}} from `cargo nextest list --message-format json`."""
    suites = {}
    for binary_id, suite in listing.get("rust-suites", {}).items():
        cases = suite.get("testcases", {})
        suites[binary_id] = {name: bool(case.get("ignored")) for name, case in cases.items()}
    return suites


def crate_of(binary_id):
    return binary_id.split("::", 1)[0]


def crate_tests(suites, crate, serial):
    """Every test `make unit` runs for a crate: non-ignored ones plus its serial ignored ones."""
    tests, serial_found = [], []
    for binary_id, cases in suites.items():
        if crate_of(binary_id) != crate:
            continue
        for name, ignored in cases.items():
            if not ignored:
                tests.append(f"{binary_id} {name}")
            elif name in serial:
                serial_found.append(f"{binary_id} {name}")
    return sorted(tests), sorted(serial_found)


def filter_expressions(test_ids, suites):
    """nextest filtersets selecting exactly `test_ids`, each argument bounded."""
    by_binary = {}
    for test in test_ids:
        binary_id, name = test.split(" ", 1)
        by_binary.setdefault(binary_id, []).append(name)
    terms = []
    for binary_id in sorted(by_binary):
        names = sorted(by_binary[binary_id])
        runnable = sorted(n for n, ignored in suites.get(binary_id, {}).items() if not ignored)
        if names == runnable:
            terms.append(f"binary_id(={binary_id})")
        else:
            for start in range(0, len(names), 200):
                chunk = " | ".join(f"test(={n})" for n in names[start:start + 200])
                terms.append(f"binary_id(={binary_id}) & ({chunk})")
    expressions, current = [], []
    for term in terms:
        if current and len(" | ".join(current + [term])) > MAX_FILTER_BYTES:
            expressions.append(" | ".join(current))
            current = []
        current.append(term)
    if current:
        expressions.append(" | ".join(current))
    return expressions


def shard_count(remaining):
    return max(1, min(MAX_SHARDS, math.ceil(remaining / TESTS_PER_SHARD)))


def make_plan(selection_doc, pending, listing, config, run):
    suites = nextest_suites(listing)
    crates, partitioned, serial_all = {}, [], []
    for crate, entry in sorted(pending["crates"].items()):
        tests, serial = crate_tests(suites, crate, set(config.serial_ignored.get(crate, ())))
        missing = set(config.serial_ignored.get(crate, ())) - {t.split(" ", 1)[1] for t in serial}
        require(not missing, f"{crate}: declared serial ignored tests not found: {sorted(missing)}")
        everything = tests + serial
        adopted = sorted(set(entry["passed"]) & set(everything))
        remaining = sorted(set(tests) - set(adopted))
        remaining_serial = sorted(set(serial) - set(adopted))
        crates[crate] = {"key": entry["key"],
                         "listing": {"count": len(everything), "sha256": digest_lines(sorted(everything))},
                         "adopted": adopted, "remaining": remaining, "serial": remaining_serial}
        partitioned += remaining
        serial_all += remaining_serial
    count = shard_count(len(partitioned))
    return {"schema": SCHEMA, "run": run, "head": selection_doc["head"], "base": selection_doc["base"],
            "shards": count, "crates": crates,
            "filters": filter_expressions(partitioned, suites) if partitioned else [],
            "serial_filters": filter_expressions(serial_all, {}) if serial_all else []}


# --------------------------------------------------------------------------
# Running a shard


def junit_outcomes(path):
    """{test id: 'PASS' | 'FAIL' | 'SKIP'} from nextest's JUnit report."""
    outcomes = {}
    root = ET.parse(path).getroot()
    for suite in root.iter("testsuite"):
        binary_id = suite.get("name")
        for case in suite.iter("testcase"):
            test = f"{binary_id} {case.get('name')}"
            if case.find("failure") is not None or case.find("error") is not None:
                outcomes[test] = "FAIL"
            elif case.find("skipped") is not None:
                outcomes[test] = "SKIP"
            else:
                outcomes[test] = "PASS"
    return outcomes


EXTRACT = STATE / "extract"


def nextest_command(archive, filters, extra):
    # Extract into the workspace so the JUnit report outlives the run, and
    # remap the workspace so `CARGO_MANIFEST_DIR` and fixtures resolve here.
    command = ["cargo-nextest", "nextest", "run", "--archive-file", str(archive),
               "--workspace-remap", ".", "--extract-to", str(EXTRACT), "--extract-overwrite",
               "--profile", "ci", "--no-fail-fast", "--color", "never", *extra]
    for expression in filters:
        command += ["-E", expression]
    return command


def run_shard(plan_path, archive, index, threads):
    plan = read_json(plan_path)
    count = plan["shards"]
    require(1 <= index <= count, f"shard {index} is outside 1..{count}")
    outcomes = {}
    runs = []
    if plan["filters"]:
        runs.append(("partition", plan["filters"],
                     ["--partition", f"hash:{index}/{count}", "--test-threads", str(threads)]))
    if plan["serial_filters"] and index == 1:
        runs.append(("serial ignored", plan["serial_filters"],
                     ["--run-ignored", "only", "--test-threads", "1"]))
    for label, filters, extra in runs:
        for stale in EXTRACT.rglob("junit.xml") if EXTRACT.exists() else ():
            stale.unlink()
        command = nextest_command(archive, filters, extra)
        print(f"::group::nextest {label}: shard {index}/{count}")
        started = time.monotonic()
        result = subprocess.run(command)
        print(f"::endgroup::\nnextest {label} exited {result.returncode} after "
              f"{time.monotonic() - started:.0f}s")
        # 0: all passed; 100: tests failed; 4: no tests matched this partition.
        require(result.returncode in (0, 4, 100), f"nextest {label} could not run (exit {result.returncode})")
        if result.returncode != 4:
            reports = sorted(EXTRACT.rglob("junit.xml")) if EXTRACT.exists() else []
            require(len(reports) == 1, f"nextest {label} wrote {len(reports)} JUnit reports")
            outcomes.update(junit_outcomes(reports[0]))
    planned = {t for crate in plan["crates"].values() for t in crate["remaining"] + crate["serial"]}
    unexpected = sorted(set(outcomes) - planned)
    require(not unexpected, f"shard ran tests outside the plan: {unexpected[:5]}")
    results = {}
    for crate in plan["crates"]:
        mine = {t: o for t, o in outcomes.items() if crate_of(t.split(" ", 1)[0]) == crate}
        results[crate] = {"passed": sorted(t for t, o in mine.items() if o == "PASS"),
                          "failed": sorted(t for t, o in mine.items() if o == "FAIL"),
                          "skipped": sorted(t for t, o in mine.items() if o == "SKIP")}
    receipt = {"schema": SCHEMA, "producer": "validation.rust_units", "run": plan["run"],
               "index": index, "shards": count, "head": plan["head"],
               "job": JOB_SHARD.format(index=index), "results": results}
    write_json(STATE / "shard" / "receipt.json", receipt)
    failed = sum(len(r["failed"]) for r in results.values())
    passed = sum(len(r["passed"]) for r in results.values())
    print(f"shard {index}/{count}: {passed} passed, {failed} failed "
          "(the verdict job judges failures against the merge-base)")
    return receipt


# --------------------------------------------------------------------------
# Verdict, merge-base check and gate


def collect(plan, receipts):
    """Outcomes of every planned test, and what keeps the run from being complete.

    Complete means one receipt per shard and every planned test exactly once.
    Integrity problems (a receipt from another plan, a test in two shards)
    raise; a missing shard or test is returned so the passes that did run can
    still be published.
    """
    by_index = {}
    for receipt in receipts:
        require(receipt.get("schema") == SCHEMA and receipt.get("run") == plan["run"]
                and receipt.get("shards") == plan["shards"] and receipt.get("head") == plan["head"],
                "a shard receipt belongs to another plan")
        index = receipt.get("index")
        require(index not in by_index, f"two receipts for shard {index}")
        by_index[index] = receipt
    problems = []
    if sorted(by_index) != list(range(1, plan["shards"] + 1)):
        problems.append(f"shard receipts {sorted(by_index)} do not cover 1..{plan['shards']}")
    outcome, seen = {}, {}
    for index, receipt in sorted(by_index.items()):
        for crate, result in receipt["results"].items():
            for status in ("passed", "failed", "skipped"):
                for test in result[status]:
                    require(test not in seen, f"{test} ran in shards {seen.get(test)} and {index}")
                    seen[test] = index
                    outcome[test] = status
    planned = {t for entry in plan["crates"].values() for t in entry["remaining"] + entry["serial"]}
    require(not set(outcome) - planned, "a shard receipt holds tests outside the plan")
    missing = sorted(planned - set(outcome))
    if missing:
        problems.append(f"{len(missing)} planned tests did not run, e.g. {missing[:5]}")
    return outcome, problems


def crate_receipt(plan, crate, outcome, run, publisher, evidence_jobs, key=None, commit=None,
                  listing=True):
    entry = plan["crates"].get(crate, {})
    ran = [t for t in outcome if crate_of(t.split(" ", 1)[0]) == crate]
    return {"schema": SCHEMA, "producer": "validation.rust_units", "crate": crate,
            "key": key or entry["key"], "commit": commit or plan["head"], "run": run,
            "publisher": publisher, "evidence_jobs": evidence_jobs,
            "listing": entry.get("listing") if listing else None,
            "passed": sorted(t for t in ran if outcome[t] == "passed"),
            "failed": sorted(t for t in ran if outcome[t] == "failed")}


def write_crate_receipts(directory, receipts):
    names = []
    for receipt in receipts:
        name = receipt_name(receipt["crate"], receipt["key"])
        write_json(Path(directory) / receipt["crate"] / "receipt.json", receipt)
        names.append(name)
    return names


def summarize(lines):
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    text = "\n".join(lines) + "\n"
    print(text)
    if summary:
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write(text)


# --------------------------------------------------------------------------
# Command line


def command_select(args):
    base = subprocess.run(["git", "merge-base", args.base, args.head], stdout=subprocess.PIPE,
                          text=True, check=True).stdout.strip()
    doc = selection(args.head, base)
    write_json(args.output, doc)
    due = sorted(name for name, entry in doc["crates"].items() if entry["due"])
    lines = [f"Rust units: {len(due)} of {len(doc['crates'])} unit crates have different content "
             f"keys at {doc['head'][:12]} than at merge-base {doc['base'][:12]}."]
    for name in due:
        lines.append(f"- `{name}` key {doc['crates'][name]['key'][:16]} — " +
                     ", ".join(doc["crates"][name]["reason"]))
    summarize(lines)
    emit("due", due)
    emit("any", "true" if due else "false")
    return 0


def command_adopt(args):
    doc = read_json(args.selection)
    config = Config.load(Tree(doc["head"]).text(str(CONFIG)))
    pending = {"schema": SCHEMA, "head": doc["head"], "base": doc["base"], "crates": {}, "adopted": {}}
    forge = forge_from_environment() if os.environ.get("GITHUB_TOKEN") else None
    authenticator = Authenticator(forge, int(os.environ.get("GITHUB_RUN_ID", "0")), config) if forge else None
    for crate, entry in sorted(doc["crates"].items()):
        if not entry["due"]:
            continue
        found = (history(forge, authenticator, crate, entry["key"]) if forge
                 else {"passed": set(), "failed": set(), "listings": [], "runs": []})
        if fully_passed(found["listings"], found["passed"]):
            pending["adopted"][crate] = {"key": entry["key"], "runs": found["runs"],
                                         "passed": len(found["passed"])}
            print(f"{crate}: every test already passed under key {entry['key'][:16]}; not run")
            continue
        pending["crates"][crate] = {"key": entry["key"], "passed": sorted(found["passed"])}
    write_json(args.output, pending)
    emit("build", "true" if pending["crates"] else "false")
    emit("crates", sorted(pending["crates"]))
    return 0


def command_plan(args):
    selection_doc, pending = read_json(args.selection), read_json(args.pending)
    config = Config.load(Path(CONFIG).read_text(encoding="utf-8"))
    listing = read_json(args.listing, limit=256 * 1024 * 1024)
    plan = make_plan(selection_doc, pending, listing, config, int(os.environ.get("GITHUB_RUN_ID", "0")))
    write_json(args.output, plan)
    total = sum(len(c["remaining"]) + len(c["serial"]) for c in plan["crates"].values())
    adopted = sum(len(c["adopted"]) for c in plan["crates"].values())
    summarize([f"Rust units plan: {total} tests to run in {plan['shards']} shard(s); "
               f"{adopted} already passed under the same keys."] +
              [f"- `{name}`: {len(c['remaining']) + len(c['serial'])} to run of {c['listing']['count']}"
               for name, c in sorted(plan["crates"].items())])
    emit("shards", list(range(1, plan["shards"] + 1)) if total else [])
    emit("tests", str(total))
    return 0


def command_package_filter(args):
    """The archive filterset: binaries of the crates the plan may run."""
    pending = read_json(args.pending)
    crates = sorted(pending["crates"])
    require(crates and all(CRATE_NAME.fullmatch(c) for c in crates), "no crates to archive")
    print(" | ".join(f"package(={c})" for c in crates))
    return 0


def command_shard(args):
    run_shard(args.plan, args.archive, args.index, args.threads)
    return 0


def command_verdict(args):
    plan = read_json(args.plan)
    receipts = [read_json(p) for p in sorted(Path(args.receipts).rglob("receipt.json"))]
    outcome, problems = collect(plan, receipts)
    run = plan["run"]
    shards = [JOB_SHARD.format(index=i) for i in range(1, plan["shards"] + 1)]
    out = []
    for crate in sorted(plan["crates"]):
        # This run's own outcomes only. The adopted passes stay provable from
        # their original receipts under the same name; a later lookup unions
        # them, which is what lets a whole crate be skipped once its listing
        # has passed piecewise.
        out.append(crate_receipt(plan, crate, outcome, run, JOB_VERDICT, shards))
    write_crate_receipts(args.output, out)
    failures = {crate: r["failed"] for crate, r in zip(sorted(plan["crates"]), out) if r["failed"]}
    passed = sum(len(r["passed"]) for r in out)
    lines = [f"Rust units verdict: {passed} passed, {sum(map(len, failures.values()))} failed, "
             f"{sum(len(c['adopted']) for c in plan['crates'].values())} adopted."]
    for crate, tests in sorted(failures.items()):
        lines.append(f"- `{crate}` failed: " + ", ".join(f"`{t}`" for t in tests[:20])
                     + (f" … {len(tests) - 20} more" if len(tests) > 20 else ""))
    lines += [f"- incomplete: {problem}" for problem in problems]
    summarize(lines)
    write_json(STATE / "failures.json", {"head": plan["head"], "base": plan["base"], "failures": failures})
    emit("failures", "true" if failures else "false")
    emit("complete", "false" if problems else "true")
    emit("receipts", [receipt_name(r["crate"], r["key"]) for r in out])
    for receipt in out:
        emit(f"key_{receipt['crate'].replace('-', '_')}", receipt["key"])
    return 0


def command_baseline_filter(args):
    """Filtersets for the failing tests, to run at the merge-base."""
    failures = read_json(args.failures)["failures"]
    tests = sorted(t for tests in failures.values() for t in tests)
    require(tests, "no failures to check")
    for expression in filter_expressions(tests, {}):
        print(expression)
    return 0


def command_classify(args):
    """Failures that also fail at the merge-base are pre-existing; others block."""
    failures = read_json(args.failures)
    head_failures = failures["failures"]
    base_outcomes = {}
    if Path(args.junit).exists():
        base_outcomes = junit_outcomes(args.junit)
    pre_existing, new = {}, {}
    for crate, tests in sorted(head_failures.items()):
        for test in tests:
            (pre_existing if base_outcomes.get(test) == "FAIL" else new).setdefault(crate, []).append(test)
    lines = ["Rust units: failures checked at merge-base " + failures["base"][:12] + "."]
    for crate, tests in sorted(pre_existing.items()):
        lines += [f"- pre-existing on main (`{crate}`, not blocking): `{t}`" for t in tests]
    for crate, tests in sorted(new.items()):
        for test in tests:
            state = {"PASS": "passes at the merge-base", "SKIP": "skipped at the merge-base"}.get(
                base_outcomes.get(test), "absent at the merge-base")
            lines.append(f"- NEW failure (`{crate}`, {state}): `{test}`")
    summarize(lines)
    write_json(STATE / "classification.json", {"pre_existing": pre_existing, "new": new})
    return 1 if new else 0


def command_check_graph(args):
    """Fail when this module's manifest reading disagrees with cargo's own graph."""
    metadata = read_json(args.metadata, limit=256 * 1024 * 1024)
    ours = workspace(Tree("HEAD"))
    members = {p["name"]: p for p in metadata["packages"] if p["id"] in set(metadata["workspace_members"])}
    require(set(members) == set(ours), f"workspace members differ: cargo {sorted(members)} vs {sorted(ours)}")
    kinds = {None: "normal", "build": "build", "dev": "dev"}
    for name, package in members.items():
        theirs = {"normal": set(), "build": set(), "dev": set()}
        for dep in package["dependencies"]:
            if dep.get("path") and dep["name"] in members and dep["name"] != name:
                theirs[kinds[dep.get("kind")]].add(dep["name"])
        require(theirs == ours[name].deps, f"{name}: dependency graph differs from cargo metadata: "
                                           f"cargo {theirs} vs ours {ours[name].deps}")
    print(f"Workspace graph agrees with cargo metadata for {len(members)} members.")
    return 0


def command_local(args):
    base = subprocess.run(["git", "merge-base", args.base, "HEAD"], stdout=subprocess.PIPE,
                          text=True, check=True).stdout.strip()
    doc = selection("HEAD", base)
    due = sorted(name for name, entry in doc["crates"].items() if entry["due"])
    for name in due:
        print(f"due: {name} — " + ", ".join(doc["crates"][name]["reason"]))
    if not due:
        print("No unit crate's inputs changed; nothing to run.")
        return 0
    config = Config.load(Path(CONFIG).read_text(encoding="utf-8"))
    expression = " | ".join(f"package(={name})" for name in due)
    commands = [["cargo", "nextest", "run", "--locked", "--workspace", "--exclude",
                 *[x for c in config.excluded for x in (c,)], "--no-fail-fast", "-E", expression]]
    serial = [t for name in due for t in config.serial_ignored.get(name, ())]
    if serial:
        commands.append(commands[0][:-2] + ["--run-ignored", "only", "--test-threads", "1", "-E",
                                            " | ".join(f"test(={t})" for t in serial)])
    for command in commands:
        print("+ " + " ".join(f"'{part}'" if " " in part or "|" in part else part for part in command))
        if args.run:
            result = subprocess.run(command)
            if result.returncode:
                return result.returncode
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(prog="python3 -m validation.rust_units", description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("select", help="content keys at head and merge-base; which crates are due")
    p.add_argument("--base", required=True)
    p.add_argument("--head", default="HEAD")
    p.add_argument("--output", default=str(STATE / "selection.json"))
    p.set_defaults(func=command_select)
    p = sub.add_parser("adopt", help="drop tests already passed under the same key")
    p.add_argument("--selection", default=str(STATE / "selection.json"))
    p.add_argument("--output", default=str(STATE / "pending.json"))
    p.set_defaults(func=command_adopt)
    p = sub.add_parser("archive-filter", help="nextest archive filterset for the pending crates")
    p.add_argument("--pending", default=str(STATE / "pending.json"))
    p.set_defaults(func=command_package_filter)
    p = sub.add_parser("check-graph", help="compare the manifest graph with cargo metadata")
    p.add_argument("--metadata", required=True)
    p.set_defaults(func=command_check_graph)
    p = sub.add_parser("plan", help="split the pending tests into shards")
    p.add_argument("--selection", default=str(STATE / "selection.json"))
    p.add_argument("--pending", default=str(STATE / "pending.json"))
    p.add_argument("--listing", required=True)
    p.add_argument("--output", default=str(STATE / "plan.json"))
    p.set_defaults(func=command_plan)
    p = sub.add_parser("shard", help="run one partition of the plan from the archive")
    p.add_argument("--plan", default=str(STATE / "plan.json"))
    p.add_argument("--archive", required=True)
    p.add_argument("--index", type=int, required=True)
    p.add_argument("--threads", type=int, default=2)
    p.set_defaults(func=command_shard)
    p = sub.add_parser("verdict", help="prove coverage, write per-crate receipts")
    p.add_argument("--plan", default=str(STATE / "plan.json"))
    p.add_argument("--receipts", required=True)
    p.add_argument("--output", default=str(STATE / "receipts"))
    p.set_defaults(func=command_verdict)
    p = sub.add_parser("baseline-filter", help="filtersets for the failures, for the merge-base run")
    p.add_argument("--failures", default=str(STATE / "failures.json"))
    p.set_defaults(func=command_baseline_filter)
    p = sub.add_parser("classify", help="pre-existing versus new failures")
    p.add_argument("--failures", default=str(STATE / "failures.json"))
    p.add_argument("--junit", required=True)
    p.set_defaults(func=command_classify)
    p = sub.add_parser("local", help="print (or --run) the same selection with cargo nextest")
    p.add_argument("--base", default="origin/main")
    p.add_argument("--run", action="store_true")
    p.set_defaults(func=command_local)
    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except UnitsError as error:
        print(f"rust_units: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
