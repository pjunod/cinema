"""Capture only the pilot's selected sources, then parse their frozen bytes.

Discovery graphs are never returned to callers. A second capture comparison
includes stat change counters, so an observed write-and-restore is a retry.
"""
from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path
import stat
import subprocess
import tomllib
from typing import Any

from . import model
from .model import KnowledgeError, canonical_json, content_digest, digest_bytes, require, safe_relative

VERSION = 2
BASE_PATHS = ("validation/knowledge/pilot.json", "validation/knowledge/records.toml",
              "validation/knowledge/coverage.toml", "validation/knowledge/conflicts.toml",
              "tests/playback/routing-decisions.toml", "validation/points.toml",
              "validation/main-preflight-inputs.json")
MODULES = ("__init__.py", "model.py", "adapters.py", "snapshot.py", "index.py",
           "evidence.py", "queries.py", "capture.py", "lint.py", "cli.py")


class SnapshotChanged(KnowledgeError):
    """The selected source did not remain stable across two attempts."""


def git(root: Path, *args: str) -> bytes:
    try:
        return subprocess.check_output(["git", "-C", str(root), *args],
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"}, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.CalledProcessError) as error:
        raise KnowledgeError("Git identity/history unavailable") from error


def _signature(info: os.stat_result) -> tuple[int, ...]:
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
            info.st_ctime_ns, info.st_mode)


def checked_path(root: Path, relative: str) -> Path:
    """Refuse every symlink component before stat/open can follow it."""
    relative = safe_relative(relative)
    path = root
    for component in relative.split("/"):
        path = path / component
        try:
            require(not stat.S_ISLNK(path.lstat().st_mode), "symlink source/cache refused: " + relative)
        except FileNotFoundError:
            pass
    require(path.resolve().is_relative_to(root), "source/cache escapes repository")
    return path


def _safe_selection(path: str) -> str:
    # One policy owns source privacy for discovery, globs and actual members.
    return model.safe_source_relative(path, pattern=True)


def _document_bytes(root: Path, path: str) -> bytes:
    item = _read(root, path)
    require(item.data is not None, "missing discovery source: " + path)
    return item.data


def _preflight(root: Path) -> None:
    """Reject private literal declarations before the read-only M1 discovery."""
    for path in BASE_PATHS:
        checked_path(root, path)
    try:
        pilot = json.loads(_document_bytes(root, BASE_PATHS[0]))
        authored = tomllib.loads(_document_bytes(root, BASE_PATHS[1]).decode("utf-8"))
    except (ValueError, UnicodeError) as error:
        raise KnowledgeError("invalid discovery metadata") from error
    literals = set()
    for anchor in authored.get("anchors", ()):
        literals.update((anchor["path"], anchor.get("owner_module", anchor["path"])))
    for node in authored.get("records", ()):
        literals.update(node.get("scope", ()))
    routing = pilot["cohorts"]["routing"]
    literals.add(routing["manifest"])
    receipt = pilot["cohorts"]["receipt"]
    literals.update((*receipt["sources"], receipt["policy"], *pilot.get("document_roles", {})))
    for path in literals:
        _safe_selection(path)
        if not any(c in path for c in "*?[]"):
            checked_path(root, path)
    try:
        routes = tomllib.loads(_document_bytes(root, routing["manifest"]).decode("utf-8"))
    except (ValueError, UnicodeError) as error:
        raise KnowledgeError("invalid routing discovery metadata") from error
    for row in routes.get("decisions", ()):
        if row["id"] in routing["ids"]:
            for path in (row["source"], row["test"]):
                _safe_selection(path)
                checked_path(root, path)


def _environment(value):
    """Explicit portable identities only; never serialize credentials or paths."""
    if isinstance(value, dict):
        for key, child in value.items():
            require(isinstance(key, str) and not any(word in key.lower() for word in
                    ("secret", "password", "token", "credential")), "private environment field refused")
            _environment(child)
    elif isinstance(value, (list, tuple)):
        for child in value:
            _environment(child)
    elif isinstance(value, str):
        require(not value.startswith(("/", "~", "file:")) and "\\" not in value,
                "absolute environment path refused")
    else:
        require(value is None or type(value) in {int, float, bool}, "invalid environment identity")
    return value


def _matches(path: str, pattern: str) -> bool:
    # Reuse the canonical descriptor's component-bounded selection semantics.
    from validation.main_preflight_adoption import matches_input
    return matches_input(path, pattern)


def _inventory(root: Path) -> tuple[set[str], set[str]]:
    tracked = {p.decode("utf-8") for p in git(root, "ls-files", "-z", "--cached").split(b"\0") if p}
    other = {p.decode("utf-8") for p in git(root, "ls-files", "-z", "--others", "--exclude-standard").split(b"\0") if p}
    return tracked, other


def selection(graph: model.GraphData, extra_paths=()) -> tuple[str, ...]:
    """Select physical graph sources, authored scopes and actual receipt proof.

    Imported impact patterns and definition-only check witnesses remain logical
    metadata. Their canonical manifest bytes already bind those definitions.
    """
    paths = set(BASE_PATHS)
    nodes = {node.id: node for node in graph.nodes}
    for anchor in graph.anchors:
        paths.update((anchor.path, anchor.owner_module, *anchor.physical_paths))
    for node in graph.nodes:
        if node.kind in model.AUTHORED_KINDS and node.provenance.method == "reviewed":
            paths.update(node.scope)
        if node.kind == "artifact" and node.attributes.get("path"):
            paths.add(node.attributes["path"])
        if not node.provenance.origin.startswith("plurx:"):
            paths.add(node.provenance.origin)
        descriptor = node.attributes.get("descriptor", {})
        if descriptor.get("source_manifest"):
            paths.add(descriptor["source_manifest"])
    for receipt in graph.receipts:
        # The receipt's claimed inputs are relevant even when incomplete. Only
        # canonical linked witnesses can add a proof closure; catalog impact
        # patterns cannot turn into evidence inputs merely by being adjacent.
        descriptors = [receipt.scope_descriptor]
        check = nodes.get(receipt.check_id)
        if check is not None and check.attributes.get("descriptor_completeness") == "canonical":
            descriptors.append(check.attributes.get("descriptor", {}))
        for descriptor in descriptors:
            for field in ("inputs", "pathsets"):
                paths.update(descriptor.get(field, ()))
            if descriptor.get("source_manifest"):
                paths.add(descriptor["source_manifest"])
    patterns = tuple(sorted(_safe_selection(path) for path in paths))
    for path in extra_paths:
        _safe_selection(path)
        require(any(_matches(path, pattern) for pattern in patterns), "extra source outside pilot selection")
    return patterns


def _members(root: Path, patterns: tuple[str, ...]):
    tracked, other = _inventory(root)
    # Most pilot inputs are literal paths. Exact set membership preserves the
    # canonical glob semantics while avoiding a glob call for every file/path.
    literals = {p for p in patterns if not any(c in p for c in "*?[]")}
    globs = tuple(p for p in patterns if p not in literals)
    selected = set(literals)  # Missing literals bind absence as before.
    if globs:
        selected.update(p for p in tracked | other if any(_matches(p, pat) for pat in globs))
    for path in selected:
        _safe_selection(path)
    return tuple(sorted(selected)), tuple(sorted(other & selected))


def _identity(root: Path) -> dict[str, Any]:
    head = git(root, "rev-parse", "HEAD").decode().strip()
    tree = git(root, "rev-parse", "HEAD^{tree}").decode().strip()
    gitdir = git(root, "rev-parse", "--absolute-git-dir").decode().strip()
    index_path = Path(git(root, "rev-parse", "--git-path", "index").decode().strip())
    if not index_path.is_absolute():
        index_path = root / index_path
    index_sig = _signature(index_path.stat()) if index_path.exists() else ()
    branch = git(root, "rev-parse", "--symbolic-full-name", "HEAD").decode().strip()
    return {"head": head, "tree": tree, "branch": branch,
            "worktree_id": content_digest([str(root), gitdir]),
            "index": digest_bytes(git(root, "ls-files", "--stage", "-z")),
            "index_stat": index_sig,
            "repository_dirty": bool(git(root, "status", "--porcelain=v1", "--untracked-files=normal"))}


@dataclass(frozen=True)
class CapturedInput:
    path: str
    mode: int
    data: bytes | None
    digest: str
    stat: tuple[int, ...]

    def metadata(self):
        return {"path": self.path, "mode": self.mode, "digest": self.digest,
                "present": self.data is not None}


def _read(root: Path, relative: str) -> CapturedInput:
    # A reviewed glob never grants permission to read a private matched file.
    relative = model.safe_source_relative(relative)
    path = checked_path(root, relative)
    try:
        before = path.lstat()
    except FileNotFoundError:
        return CapturedInput(relative, 0, None, digest_bytes(b""), ())
    require(stat.S_ISREG(before.st_mode), "nonregular source refused: " + relative)
    require(before.st_size <= model.MAX_BYTES, "source exceeds byte bound")
    # O_NOFOLLOW also closes a last-component symlink replacement race.
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in relative.split("/")[:-1]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        descriptor = os.open(relative.split("/")[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            data = stream.read(model.MAX_BYTES + 1)
            after = os.fstat(stream.fileno())
        final = os.stat(relative.split("/")[-1], dir_fd=directory, follow_symlinks=False)
    finally:
        os.close(directory)
    require(len(data) <= model.MAX_BYTES, "source exceeds byte bound")
    if _signature(before) != _signature(opened) or _signature(before) != _signature(after) or _signature(before) != _signature(final):
        raise SnapshotChanged("snapshot-changed: source modified during capture")
    mode = 0o100755 if before.st_mode & 0o111 else 0o100644
    return CapturedInput(relative, mode, data, digest_bytes(data), _signature(before))


def versions() -> dict[str, str | int]:
    parent = Path(__file__).parent
    return {"schema": model.SCHEMA_VERSION, "snapshot": VERSION,
            **{name: digest_bytes((parent / name).read_bytes()) for name in MODULES},
            **{"validation/" + name: digest_bytes((parent.parent / name).read_bytes())
               for name in ("runner.py", "rust_modules.py", "main_preflight_adoption.py")}}


@dataclass(frozen=True)
class Snapshot:
    key: str
    head: str
    tree: str
    worktree_id: str
    inputs: tuple[CapturedInput, ...]
    patterns: tuple[str, ...]
    untracked: tuple[str, ...]
    identity: dict[str, Any]
    module_versions: dict[str, Any]
    environment: dict[str, Any]
    descriptor_digests: tuple[str, ...]

    def __post_init__(self):
        for name in ("identity", "module_versions", "environment"):
            object.__setattr__(self, name, model._freeze(getattr(self, name)))

    @property
    def repository_dirty(self):
        return self.identity["repository_dirty"]

    def metadata(self) -> dict[str, Any]:
        return {"key": self.key, "head": self.head, "tree": self.tree,
                "worktree_id": self.worktree_id, "environment": json.loads(canonical_json(self.environment)),
                "input_count": len(self.inputs), "repository_dirty": self.repository_dirty,
                "versions": json.loads(canonical_json(self.module_versions)), "descriptor_digests": list(self.descriptor_digests)}

    def recipe(self) -> dict[str, Any]:
        """Complete source-selection recipe, never input bytes or cache keys.

        An index may reuse this recipe only after verifying its local MAC.
        Source byte digests are always checked again against the live worktree.
        """
        return json.loads(canonical_json({"key": self.key, "identity": self.identity,
            "inputs": [item.metadata() for item in self.inputs], "patterns": self.patterns,
            "untracked": self.untracked, "versions": self.module_versions,
            "environment": self.environment, "descriptor_digests": self.descriptor_digests}))

    def materialize(self, dest: Path) -> None:
        dest = Path(dest)
        require(dest.exists() and not dest.is_symlink() and not any(dest.iterdir()), "frozen destination must be empty owned directory")
        dest = dest.resolve()
        for item in self.inputs:
            if item.data is not None:
                path = checked_path(dest, item.path)
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("xb") as stream:
                    stream.write(item.data)
                path.chmod(item.mode & 0o777)

    def verify(self, root: Path) -> bool:
        root = Path(root).resolve()
        try:
            if _identity(root) != self.identity or versions() != self.module_versions:
                return False
            paths, untracked = _members(root, self.patterns)
            if paths != tuple(i.path for i in self.inputs) or untracked != self.untracked:
                return False
            return all(_read(root, item.path) == item for item in self.inputs) and _identity(root) == self.identity
        except (KnowledgeError, OSError):
            return False


def _assemble(identity, inputs, patterns, untracked, module_versions, environment, descriptors):
    payload = {"identity": {k: v for k, v in identity.items() if k != "index_stat"},
               "inputs": [item.metadata() for item in inputs], "patterns": patterns,
               "untracked": untracked, "versions": module_versions,
               "environment": environment, "descriptor_digests": descriptors}
    return Snapshot(content_digest(payload).removeprefix("sha256:"), identity["head"], identity["tree"],
                    identity["worktree_id"], inputs, patterns, untracked, identity,
                    module_versions, environment, descriptors)


def recapture(root: Path, recipe: dict, *, environment=None) -> Snapshot | None:
    """Re-read an authenticated cache selection without reparsing the graph.

    This is an internal cache primitive, not an authority for caller-supplied
    scopes: index.load must authenticate the whole graph and recipe first.
    Changed source/identity means a cold rebuild; an observed read race still
    refuses/retries rather than returning any cached graph.
    """
    required = {"key", "identity", "inputs", "patterns", "untracked", "versions", "environment", "descriptor_digests"}
    require(isinstance(recipe, dict) and set(recipe) == required, "invalid snapshot recipe")
    root = Path(root).resolve()
    before = _identity(root)
    expected = recipe["identity"]
    require(isinstance(expected, dict), "invalid snapshot identity")
    if ({k: v for k, v in before.items() if k != "index_stat"} !=
            {k: v for k, v in expected.items() if k != "index_stat"}):
        return None
    module_versions = versions()
    env = json.loads(canonical_json(_environment(environment or {})))
    if module_versions != recipe["versions"] or env != recipe["environment"]:
        return None
    require(isinstance(recipe["patterns"], list) and len(recipe["patterns"]) <= model.MAX_ROWS,
            "invalid snapshot selection")
    patterns = tuple(_safe_selection(path) for path in recipe["patterns"])
    require(patterns == tuple(sorted(set(patterns))), "noncanonical snapshot selection")
    paths, untracked = _members(root, patterns)
    require(isinstance(recipe["inputs"], list) and len(recipe["inputs"]) <= model.MAX_ROWS, "invalid input recipe")
    if paths != tuple(row.get("path") for row in recipe["inputs"]) or list(untracked) != recipe["untracked"]:
        return None
    inputs = tuple(_read(root, path) for path in paths)
    if [item.metadata() for item in inputs] != recipe["inputs"]:
        return None
    descriptors = tuple(recipe["descriptor_digests"])
    snapshot = _assemble(before, inputs, patterns, untracked, module_versions, env, descriptors)
    if snapshot.key != recipe["key"]:
        return None
    if not snapshot.verify(root):
        raise SnapshotChanged("snapshot-changed: cached selection changed during validation")
    return snapshot


def capture(root: Path, *, extra_paths=(), environment=None) -> Snapshot:
    root = Path(root).resolve()
    for attempt in range(2):
        before = _identity(root)
        try:
            _preflight(root)
            discovery = model.load_graph(root, revision=before["head"])
            patterns = selection(discovery, extra_paths)
            paths, untracked = _members(root, patterns)
            inputs = tuple(_read(root, path) for path in paths)
            module_versions = versions()
            # Environment is explicitly provided identity, never process secrets.
            env = json.loads(canonical_json(_environment(environment or {})))
            descriptors = tuple(sorted({node.attributes["descriptor_digest"] for node in discovery.nodes
                                        if "descriptor_digest" in node.attributes}))
            snapshot = _assemble(before, inputs, patterns, untracked, module_versions, env, descriptors)
            if snapshot.verify(root):
                return snapshot
        except (SnapshotChanged, model.SourceReadChanged):
            pass
        except KnowledgeError:
            if _identity(root) == before:
                raise
        if attempt:
            raise SnapshotChanged("snapshot-changed: source did not remain stable")
    raise SnapshotChanged("snapshot-changed")
