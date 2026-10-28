"""Backend-neutral, source-bound engineering knowledge; no execution authority.

All prose is inert data. Reviewed attribution is preserved, never authenticated
by this module. The index is disposable; the authored manifests remain canonical.
"""
from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, fields, is_dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import stat
from types import MappingProxyType
import tomllib
from typing import Any

SCHEMA_VERSION = 1
AUTHORED_KINDS = frozenset({"contract", "obligation", "decision", "finding", "check", "receipt"})
KINDS = AUTHORED_KINDS | {"artifact", "section", "symbol", "route", "point", "source-snapshot"}
RELATIONS = frozenset({"implements", "specified-by", "constrained-by", "delivered-by", "assessed-by", "creates", "checked-by", "executed-as", "supersedes", "resolved-by", "depends-on", "mentions"})
_ID = re.compile(r"plurx:([a-z-]+):([A-Za-z0-9][A-Za-z0-9._/-]*)\Z")
_SHA = re.compile(r"sha256:[0-9a-f]{64}\Z")
_REVISION = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
MAX_BYTES = 8 * 1024 * 1024
MAX_ROWS = 4096


class KnowledgeError(ValueError):
    """Invalid schema, unsafe source reference, or unresolved identity."""


def require(condition: Any, message: str) -> None:
    if not condition:
        raise KnowledgeError(message)


def text(value: Any, label: str, *, empty: bool = False) -> str:
    require(isinstance(value, str) and (empty or bool(value.strip())), f"{label}: expected text")
    require(not any(ord(c) < 32 and c not in "\n\t" for c in value), f"{label}: control character")
    return value


def strings(value: Any, label: str) -> tuple[str, ...]:
    require(isinstance(value, (list, tuple)), f"{label}: expected text array")
    return tuple(text(item, label) for item in value)


def exact(raw: Any, required: set[str], optional: set[str], label: str) -> Mapping:
    require(isinstance(raw, Mapping), f"{label}: expected table")
    require(required <= raw.keys(), f"{label}: missing fields {sorted(required - raw.keys())}")
    require(not (raw.keys() - required - optional), f"{label}: unknown fields {sorted(raw.keys() - required - optional)}")
    return raw


def identifier(value: Any, kind: str | None = None) -> str:
    value = text(value, "id")
    match = _ID.fullmatch(value)
    require(match and match[1] in KINDS | {"anchor", "coverage", "conflict"}, f"invalid knowledge id: {value}")
    require(".." not in match[2].split("/"), f"unsafe knowledge id: {value}")
    require(kind is None or match[1] == kind, f"id kind mismatch: {value}")
    return value


def revision(value: Any) -> str:
    require(isinstance(value, str) and _REVISION.fullmatch(value), "revision must be a full Git object id")
    return value


def digest_bytes(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def digest(value: Any) -> str:
    require(isinstance(value, str) and _SHA.fullmatch(value), "digest must be sha256: followed by 64 lowercase hex digits")
    return value


def safe_relative(value: Any, *, pattern: bool = False) -> str:
    value = text(value, "repository path")
    require(not value.startswith(("/", "~")) and "\\" not in value and ":" not in value and "\n" not in value and "\t" not in value, "unsafe repository path")
    require(all(p not in {"", ".", ".."} for p in value.split("/")), "unsafe repository path segment")
    require(pattern or not any(c in value for c in "*?[]"), "unexpected glob in literal path")
    require(len(value) <= 1024, "repository path exceeds bound")
    return value


_PRIVATE_OR_GENERATED_PARTS = frozenset({
    ".git", ".ssh", ".aws", ".codex", ".hermes", "target", "node_modules",
    "__pycache__", "evaluation-private",
})
_CREDENTIAL_NAMES = frozenset({
    "forgejo_token", ".ssh-deploy-key", "auth.json", "credentials", "credentials.json",
    "id_rsa", "id_ed25519", ".netrc", ".npmrc", ".pypirc",
})


def _source_access_policy(relative: str) -> None:
    """Defense in depth, not secret detection or a substitute for an allowlist.

    Check lexically before filesystem access. A tracked synthetic credential
    name is allowed only beneath a named fixture directory; private/generated
    roots, dotenv files and local fleet configuration remain excluded there.
    """
    parts = tuple(part.casefold() for part in PurePosixPath(relative).parts)
    require(not (_PRIVATE_OR_GENERATED_PARTS & set(parts)), "private/generated source path refused")
    name = parts[-1]
    require(name != ".env" and not name.startswith(".env."), "dotenv source path refused")
    require(not name.endswith(".fleet.json") or name.endswith(".fleet.example.json"), "local fleet source path refused")
    fixture = any(part in {"fixture", "fixtures"} for part in parts[:-1])
    require(name not in _CREDENTIAL_NAMES or fixture, "credential source path refused outside fixtures")


def safe_source_relative(value: Any, *, pattern: bool = False) -> str:
    """Shared lexical source policy for discovery, capture and guarded reads.

    Glob patterns are checked lexically; every expanded path still needs this
    guard plus the caller's positive scope and untracked/cache restrictions.
    """
    relative = safe_relative(value, pattern=pattern)
    _source_access_policy(relative)
    return relative


def source_path(root: Path, relative: str, *, exists: bool = True) -> Path:
    relative = safe_source_relative(relative)
    root = root.resolve()
    path = root / relative
    resolved = path.resolve()
    require(resolved.is_relative_to(root), f"symlink escapes repository: {relative}")
    _source_access_policy(resolved.relative_to(root).as_posix())
    require(not exists or path.is_file(), f"missing source: {relative}")
    return path


@dataclass(frozen=True)
class SourceFile:
    data: bytes | None
    mode: int
    signature: tuple[int, ...]


class SourceReadChanged(KnowledgeError):
    """A descriptor-relative source changed while its bytes were read."""


def _source_signature(info: os.stat_result) -> tuple[int, ...]:
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
            info.st_ctime_ns, info.st_mode)


def read_source_file(root: Path, relative: str, *, allow_missing: bool = False) -> SourceFile:
    """Read beneath one directory descriptor; never follow raced symlinks.

    Shared discovery/capture primitive. Scope selection remains the caller's
    responsibility; this additionally enforces the common lexical privacy rule.
    """
    relative = safe_source_relative(relative)
    directory = None
    started = False
    try:
        directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        components = relative.split("/")
        for component in components[:-1]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        before = os.stat(components[-1], dir_fd=directory, follow_symlinks=False)
        started = True
        require(stat.S_ISREG(before.st_mode), "symlink/nonregular source refused: " + relative)
        require(before.st_size <= MAX_BYTES, "source exceeds byte bound: " + relative)
        descriptor = os.open(components[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            require(stat.S_ISREG(opened.st_mode), "nonregular opened source refused: " + relative)
            raw = stream.read(MAX_BYTES + 1)
            after = os.fstat(stream.fileno())
        final = os.stat(components[-1], dir_fd=directory, follow_symlinks=False)
        require(len(raw) <= MAX_BYTES, "source exceeds byte bound: " + relative)
        signature = _source_signature(before)
        if any(_source_signature(info) != signature for info in (opened, after, final)):
            raise SourceReadChanged("source changed during descriptor read: " + relative)
        return SourceFile(raw, 0o100755 if before.st_mode & 0o111 else 0o100644, signature)
    except FileNotFoundError as error:
        if started:
            raise SourceReadChanged("source disappeared during descriptor read: " + relative) from error
        if allow_missing:
            return SourceFile(None, 0, ())
        raise KnowledgeError("missing source: " + relative) from error
    except OSError as error:
        raise KnowledgeError("descriptor-relative source read refused: " + relative) from error
    finally:
        if directory is not None:
            os.close(directory)


def read_source(root: Path, relative: str) -> bytes:
    result = read_source_file(root, relative)
    require(result.data is not None, "missing source: " + relative)
    return result.data


def _freeze(value: Any) -> Any:
    if isinstance(value, Mapping):
        require(all(isinstance(k, str) for k in value), "JSON object keys must be text")
        return MappingProxyType({k: _freeze(v) for k, v in value.items()})
    if isinstance(value, (list, tuple)):
        return tuple(_freeze(v) for v in value)
    require(value is None or isinstance(value, (str, bool, int, float)), "non-JSON value")
    if isinstance(value, float):
        require(value == value and abs(value) != float("inf"), "non-finite JSON number")
    return value


def _plain(value: Any) -> Any:
    if is_dataclass(value):
        return {f.name: _plain(getattr(value, f.name)) for f in fields(value)}
    if isinstance(value, Mapping):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (tuple, list)):
        return [_plain(v) for v in value]
    return value


def canonical_json(value: Any) -> str:
    return json.dumps(_plain(value), sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)


def content_digest(value: Any) -> str:
    return digest_bytes(canonical_json(value).encode())


@dataclass(frozen=True)
class Provenance:
    method: str
    author: str
    reviewer: str
    origin: str
    source_digest: str
    revision: str

    def __post_init__(self):
        require(self.method in {"parsed", "imported", "reviewed", "candidate"}, "invalid acquisition method")
        text(self.author, "author", empty=True)
        text(self.reviewer, "reviewer", empty=True)
        text(self.origin, "provenance origin")
        if self.origin.startswith("plurx:"):
            identifier(self.origin)
        else:
            safe_relative(self.origin)
        digest(self.source_digest)
        revision(self.revision)
        if self.method == "reviewed":
            require(bool(self.author.strip()) and bool(self.reviewer.strip()), "reviewed provenance requires actual attribution")

    @classmethod
    def from_dict(cls, raw):
        exact(raw, {f.name for f in fields(cls)}, set(), "provenance")
        return cls(**raw)


@dataclass(frozen=True)
class Node:
    id: str
    kind: str
    title: str
    scope: tuple[str, ...]
    anchors: tuple[str, ...]
    attributes: Mapping[str, Any]
    provenance: Provenance

    def __post_init__(self):
        identifier(self.id, self.kind)
        require(self.kind in KINDS, "unknown node kind")
        text(self.title, "title")
        object.__setattr__(self, "scope", tuple(safe_relative(p, pattern=True) for p in strings(self.scope, "source scope")))
        object.__setattr__(self, "anchors", tuple(identifier(a) for a in strings(self.anchors, "node anchors")))
        require(isinstance(self.attributes, Mapping), "node attributes must be a table")
        object.__setattr__(self, "attributes", _freeze(self.attributes))

    @classmethod
    def from_dict(cls, raw):
        exact(raw, {f.name for f in fields(cls)}, set(), "node")
        return cls(**{**raw, "provenance": Provenance.from_dict(raw["provenance"])})


@dataclass(frozen=True)
class Edge:
    source: str
    relation: str
    target: str
    provenance: Provenance

    def __post_init__(self):
        identifier(self.source)
        identifier(self.target)
        require(self.relation in RELATIONS, "unknown relation")
        require(self.provenance.method != "candidate", "candidate edges cannot enter accepted graph")

    @classmethod
    def from_dict(cls, raw):
        exact(raw, {f.name for f in fields(cls)}, set(), "edge")
        return cls(**{**raw, "provenance": Provenance.from_dict(raw["provenance"])})


@dataclass(frozen=True)
class Anchor:
    id: str
    path: str
    kind: str
    selector: str | tuple[str, ...]
    digest: str
    current_digest: str
    owner_module: str
    physical_paths: tuple[str, ...]
    integrity: str
    line: int
    end_line: int
    excerpt: str = ""

    def __post_init__(self):
        identifier(self.id, self.kind)
        text(self.excerpt, "anchor excerpt", empty=True)
        require(len(self.excerpt) <= 4096, "anchor excerpt exceeds bound")
        safe_relative(self.path)
        safe_relative(self.owner_module)
        require(self.kind in {"section", "symbol"}, "invalid anchor kind")
        require(self.integrity in {"valid", "changed"}, "missing/ambiguous anchors must refuse")
        digest(self.digest)
        digest(self.current_digest)
        object.__setattr__(self, "physical_paths", tuple(safe_relative(p) for p in strings(self.physical_paths, "physical paths")))
        if self.kind == "section":
            object.__setattr__(self, "selector", strings(self.selector, "heading path"))
            require(bool(self.selector), "empty heading path")
        else:
            text(self.selector, "symbol selector")
        require(type(self.line) is int and 0 < self.line <= self.end_line, "invalid display lines")


@dataclass(frozen=True)
class Claim:
    subject: str
    dimension: str
    value: str
    provenance: Provenance

    def __post_init__(self):
        identifier(self.subject)
        require(self.dimension in {"authority", "applicability", "lifecycle"}, "unknown claim dimension")
        text(self.value, "claim value")

    @classmethod
    def from_dict(cls, raw):
        exact(raw, {f.name for f in fields(cls)}, set(), "claim")
        return cls(**{**raw, "provenance": Provenance.from_dict(raw["provenance"])})


@dataclass(frozen=True)
class Coverage:
    id: str
    scope: tuple[str, ...]
    level: str
    owner: str
    reviewed_revision: str
    provenance: Provenance
    cohort: str = ""
    boundaries: tuple[str, ...] = ()

    def __post_init__(self):
        identifier(self.id, "coverage")
        object.__setattr__(self, "scope", tuple(safe_relative(p, pattern=True) for p in strings(self.scope, "source scope")))
        require(bool(self.scope), "coverage needs bounded scope")
        require(self.level in {"none", "structural", "curated"}, "invalid coverage level")
        text(self.owner, "coverage owner")
        revision(self.reviewed_revision)
        text(self.cohort, "coverage cohort", empty=True)
        object.__setattr__(self, "boundaries", strings(self.boundaries, "coverage boundaries"))

    @classmethod
    def from_dict(cls, raw):
        exact(raw, {"id", "scope", "level", "owner", "reviewed_revision", "provenance"}, {"cohort", "boundaries"}, "coverage")
        return cls(**{**raw, "provenance": Provenance.from_dict(raw["provenance"])})


@dataclass(frozen=True)
class Conflict:
    id: str
    subjects: tuple[str, ...]
    dimension: str
    owner: str
    reason: str
    revisit_condition: str
    disposition: str
    resolution: str = ""
    precedence: tuple[str, ...] = ()

    def __post_init__(self):
        identifier(self.id, "conflict")
        object.__setattr__(self, "subjects", tuple(identifier(s) for s in self.subjects))
        object.__setattr__(self, "precedence", tuple(identifier(s) for s in self.precedence))
        require(bool(self.subjects), "conflict needs subjects")
        for name in ("dimension", "owner", "reason", "revisit_condition"):
            text(getattr(self, name), name)
        require(self.disposition in {"unresolved", "resolved"}, "invalid conflict disposition")
        if self.disposition == "resolved":
            text(self.resolution, "conflict resolution")
            require(bool(self.precedence), "resolved conflict needs explicit precedence")
        else:
            require(not self.precedence, "unresolved conflict cannot select precedence")


@dataclass(frozen=True)
class Receipt:
    id: str
    execution_id: str
    check_id: str
    source_revision: str
    source_tree: str
    environment: Mapping[str, Any]
    result: str
    scope_descriptor: Mapping[str, Any]
    descriptor_digest: str
    origin: str
    artifact_ref: str = ""
    artifact_digest: str = ""
    source_compatibility: str = "unknown"
    descriptor_completeness: str = "unknown"
    validator_identity: str = ""
    validator_scope: Mapping[str, Any] | None = None
    validator_verdict: str = ""

    def __post_init__(self):
        identifier(self.id, "receipt")
        identifier(self.check_id, "check")
        text(self.execution_id, "execution identity")
        revision(self.source_revision)
        revision(self.source_tree)
        require(self.result in {"pass", "fail", "skip", "not-run"}, "invalid execution result")
        require(self.origin in {"reported", "unavailable"}, "only existing validators may authenticate evidence; no bridge available")
        require(self.source_compatibility == "unknown", "M1 receipt import cannot assert source compatibility")
        require(self.descriptor_completeness in {"unknown", "impact-only", "canonical"}, "invalid descriptor completeness")
        require(not self.validator_identity and not self.validator_verdict and self.validator_scope is None, "raw validator assertions forbidden")
        digest(self.descriptor_digest)
        require(isinstance(self.environment, Mapping) and isinstance(self.scope_descriptor, Mapping), "receipt environment/descriptor must be objects")
        object.__setattr__(self, "environment", _freeze(self.environment))
        object.__setattr__(self, "scope_descriptor", _freeze(self.scope_descriptor))
        require(content_digest(self.scope_descriptor) == self.descriptor_digest, "receipt descriptor digest mismatch")
        for key in ("inputs", "pathsets", "paths"):
            if key in self.scope_descriptor:
                for path in strings(self.scope_descriptor[key], key):
                    safe_relative(path, pattern=True)
        require(bool(self.artifact_ref) == bool(self.artifact_digest), "artifact reference/digest must be paired")
        if self.artifact_ref:
            if self.artifact_ref.startswith("artifact:sha256:"):
                digest(self.artifact_ref.removeprefix("artifact:"))
                require(self.artifact_ref.removeprefix("artifact:") == self.artifact_digest, "artifact content-address digest mismatch")
            else:
                safe_relative(self.artifact_ref)
            digest(self.artifact_digest)
        require(self.origin == "unavailable" or bool(self.artifact_ref), "reported execution needs artifact provenance")


@dataclass(frozen=True)
class GraphData:
    nodes: tuple[Node, ...] = ()
    edges: tuple[Edge, ...] = ()
    claims: tuple[Claim, ...] = ()
    anchors: tuple[Anchor, ...] = ()
    coverage: tuple[Coverage, ...] = ()
    receipts: tuple[Receipt, ...] = ()
    conflicts: tuple[Conflict, ...] = ()

    def __post_init__(self):
        for field in fields(self):
            object.__setattr__(self, field.name, tuple(getattr(self, field.name)))

    def to_dict(self) -> dict[str, Any]:
        return {"schema_version": SCHEMA_VERSION, "repository": "plurx", **_plain(self)}

    def to_json(self) -> str:
        return canonical_json(self.to_dict())

    @classmethod
    def from_dict(cls, raw):
        names = {f.name for f in fields(cls)}
        exact(raw, names | {"schema_version", "repository"}, set(), "serialized graph")
        require(raw["schema_version"] == 1 and raw["repository"] == "plurx", "unknown graph schema/repository")
        constructors = {"nodes": Node.from_dict, "edges": Edge.from_dict, "claims": Claim.from_dict,
                        "anchors": lambda v: Anchor(**exact(v, {f.name for f in fields(Anchor)}, set(), "anchor")),
                        "coverage": Coverage.from_dict,
                        "receipts": lambda v: Receipt(**exact(v, {f.name for f in fields(Receipt)}, set(), "receipt")),
                        "conflicts": lambda v: Conflict(**exact(v, {f.name for f in fields(Conflict)}, set(), "conflict"))}
        data = cls(**{name: tuple(constructors[name](v) for v in _rows(raw[name], name)) for name in names})
        validate_graph(data)
        return data


def _rows(value: Any, label: str) -> list:
    require(isinstance(value, list) and len(value) <= MAX_ROWS, f"{label}: expected bounded table array")
    return value


def _sections(raw: bytes):
    """Yield exact heading paths, byte slices, and display ranges; ignore fences."""
    lines = raw.decode("utf-8").splitlines(keepends=True)
    headings = []
    stack: list[tuple[int, str]] = []
    fence = ""
    for index, line in enumerate(lines):
        fence_match = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if fence_match:
            mark = fence_match[1]
            if not fence:
                fence = mark
            elif mark[0] == fence[0] and len(mark) >= len(fence):
                fence = ""
            continue
        match = re.match(r"^ {0,3}(#{1,6})[ \t]+(.+?)[ \t]*(?:\r?\n)?$", line)
        if fence or not match:
            continue
        level = len(match[1])
        heading = re.sub(r"[ \t]+#+[ \t]*$", "", match[2]).strip()
        while stack and stack[-1][0] >= level:
            stack.pop()
        stack.append((level, heading))
        headings.append((index, level, tuple(item[1] for item in stack)))
    for position, (start, level, path) in enumerate(headings):
        stop = next((item[0] for item in headings[position + 1:] if item[1] <= level), len(lines))
        yield path, "".join(lines[start:stop]).encode("utf-8"), start + 1, stop


def normalize_child_source(raw: bytes, owner: str) -> str:
    """Canonical inventory normalization over already safely acquired bytes."""
    from validation.rust_modules import CHILD_HEADER, _RESTATED, _RELOCATED
    source = raw.decode("utf-8")
    if source.startswith(CHILD_HEADER):
        source = source[len(CHILD_HEADER):]
    source = source.replace("pub(super) ", "")
    for spelling in _RESTATED.get(owner, ()):
        source = source.replace(spelling, "pub(super) ")
    if owner in _RELOCATED:
        source = source.replace(*_RELOCATED[owner])
    return source


def _logical_module_source(root: Path, owner: str, owner_text: str) -> tuple[str, tuple[str, ...]]:
    """Expand an inventory module using its logical path, not process REPO_ROOT.

    Equivalent to the canonical module_source composition. The upstream child
    normalizer receives the explicit owner key so frozen corpora and worktrees
    retain HLS visibility/path rewrites without changing upstream globals.
    """
    from validation.rust_modules import _product_blocks
    blocks = list(_product_blocks(owner_text))
    relatives = [owner]
    child_paths = {}
    # Validate the complete input set before any upstream child reader runs.
    for _, children in blocks:
        for child in children:
            relative = (PurePosixPath(owner).parent / safe_relative(child)).as_posix()
            child_paths[child] = read_source(root, relative)
            relatives.append(relative)
    pieces = []
    last = 0
    for block, children in blocks:
        bodies = [normalize_child_source(child_paths[child], owner) for child in children]
        if len(bodies) == 1:
            expanded = bodies[0]
        else:
            header = bodies[0].split("\n", 1)[0] + "\n"
            inner = [body[len(header):-len("}\n")] for body in bodies]
            expanded = header + "\n".join(inner) + "}\n"
        pieces.extend((owner_text[last:block.start()], expanded))
        last = block.end()
    pieces.append(owner_text[last:])
    return "".join(pieces), tuple(dict.fromkeys(relatives))


def resolve_anchor(root: Path, raw: Mapping[str, Any]) -> Anchor:
    exact(raw, {"id", "path", "kind", "selector", "digest"}, {"owner_module", "excerpt"}, "anchor declaration")
    identifier(raw["id"])
    kind = raw["kind"]
    require(kind in {"section", "symbol"}, "unknown anchor kind")
    path = safe_relative(raw["path"])
    expected = digest(raw["digest"])
    owner = safe_relative(raw.get("owner_module", path))
    excerpt = text(raw.get("excerpt", ""), "anchor excerpt", empty=True)
    require(len(excerpt) <= 4096, "anchor excerpt exceeds bound")
    if kind == "section":
        selector = strings(raw["selector"], "heading path")
        require(bool(selector), "empty heading path")
        matches = [entry for entry in _sections(read_source(root, path)) if entry[0] == selector]
        require(len(matches) == 1, f"{'missing' if not matches else 'ambiguous'} section anchor: {raw['id']}")
        _, passage, line, end_line = matches[0]
        physical = (path,)
        current = digest_bytes(passage)
        require(not excerpt or excerpt in passage.decode("utf-8"), "anchor excerpt absent from current section")
    else:
        selector = text(raw["selector"], "symbol selector")
        require("\n" not in selector, "symbol selector must be one line")
        # Existing inventories normalize split children. Track every expansion
        # input and locate the physical match independently of display offsets.
        owner_text = read_source(root, owner).decode("utf-8")
        if owner.endswith(".rs"):
            expanded, relatives = _logical_module_source(root, owner, owner_text)
        else:
            expanded, relatives = owner_text, (owner,)
        require(expanded.count(selector) == 1, f"{'missing' if selector not in expanded else 'ambiguous'} symbol anchor: {raw['id']}")
        candidates = []
        for relative in relatives:
            original = read_source(root, relative).decode("utf-8")
            normalized = normalize_child_source(original.encode("utf-8"), owner) if relative != owner else original
            if selector in normalized:
                candidates.append((relative, original, normalized))
        require(len(candidates) == 1, f"ambiguous physical symbol anchor: {raw['id']}")
        physical_path, original, normalized = candidates[0]
        require(not excerpt or excerpt in original, "anchor excerpt absent from physical source")
        # The digest binds actual bytes of all owning/expanded sources, not the
        # synthetic expansion. Changed plumbing is still invalidation input.
        physical = tuple(sorted(set(relatives)))
        current = content_digest({p: digest_bytes(read_source(root, p)) for p in physical})
        line = normalized[:normalized.index(selector)].count("\n") + 1
        if physical_path != owner and original.startswith("use super::*;\n\n"):
            line += 2
        end_line = line
        path = physical_path
    return Anchor(raw["id"], path, kind, selector, expected, current, owner, physical,
                  "valid" if current == expected else "changed", line, end_line, excerpt)


_ATTRIBUTE_FIELDS = {
    "contract": ({"behavior", "authoritative_section", "check_ids", "status"}, set()),
    "obligation": ({"action", "authority", "disposition", "responsible_role"}, {"resolution", "remaining_evidence", "waiver_reason", "waiver_owner", "waiver_scope"}),
    "decision": ({"rationale", "status", "source_section"}, set()),
    "finding": ({"review", "finding_id", "severity", "disposition", "resolution", "remaining_evidence"}, set()),
    "check": ({"check_kind", "governed_ids"}, {"reference", "selector", "descriptor", "descriptor_digest"}),
    "receipt": ({"execution_id", "check_id", "source_revision", "source_tree", "environment", "result", "scope_descriptor", "descriptor_digest", "origin"}, {"artifact_ref", "artifact_digest"}),
}


def _authored(node: Node) -> None:
    required, optional = _ATTRIBUTE_FIELDS[node.kind]
    attrs = exact(node.attributes, required, optional, node.id)
    require(bool(node.scope), f"authored record needs source scope: {node.id}")
    for key, value in attrs.items():
        if key not in {"check_ids", "governed_ids", "descriptor", "environment", "scope_descriptor", "waiver_scope"}:
            text(value, f"{node.id}.{key}", empty=key in {"resolution", "remaining_evidence"})
    if node.kind == "contract":
        require(attrs["status"] in {"active", "superseded", "withdrawn"}, "invalid contract lifecycle")
        identifier(attrs["authoritative_section"], "section")
        for check in strings(attrs["check_ids"], "contract checks"):
            identifier(check, "check")
    elif node.kind == "obligation":
        require(attrs["disposition"] in {"open", "implemented-awaiting-evidence", "satisfied", "waived", "superseded"}, "invalid obligation disposition")
        identifier(attrs["authority"])
        if attrs["disposition"] == "waived":
            for field in ("waiver_reason", "waiver_owner"):
                text(attrs.get(field), field)
            waiver_scope = strings(attrs.get("waiver_scope"), "waiver scope")
            require(bool(waiver_scope), "waiver needs bounded scope")
            for path in waiver_scope:
                safe_relative(path, pattern=True)
        if attrs["disposition"] == "satisfied":
            text(attrs.get("resolution"), "satisfaction source claim")
            require("remaining_evidence" in attrs, "satisfaction must state remaining evidence explicitly")
    elif node.kind == "decision":
        require(attrs["status"] in {"proposed", "accepted", "superseded", "withdrawn"}, "invalid decision status")
        identifier(attrs["source_section"], "section")
    elif node.kind == "finding":
        require(attrs["severity"] in {"critical", "high", "medium", "low", "info"}, "invalid severity")
        require(attrs["disposition"] in {"unresolved", "accepted", "rejected", "resolved"}, "invalid finding disposition")
        if attrs["disposition"] in {"resolved", "rejected"}:
            text(attrs["resolution"], "finding resolution")
        elif not attrs["resolution"]:
            text(attrs["remaining_evidence"], "unresolved finding remaining evidence")
    elif node.kind == "check":
        require(attrs["check_kind"] in {"behavioral", "compile", "static", "lint", "operations", "qualification"}, "invalid check kind")
        require(bool(attrs.get("reference") or attrs.get("selector")), "check needs existing reference or exact selector")
        for governed in strings(attrs["governed_ids"], "governed ids"):
            identifier(governed)
        if "descriptor" in attrs:
            require(isinstance(attrs["descriptor"], Mapping), "descriptor must be a table")
            require(content_digest(attrs["descriptor"]) == attrs.get("descriptor_digest"), "check descriptor digest mismatch")
        else:
            require("descriptor_digest" not in attrs, "descriptor digest without descriptor")
    elif node.kind == "receipt":
        Receipt(id=node.id, **attrs)


def _provenances(graph: GraphData):
    for collection in (graph.nodes, graph.edges, graph.claims, graph.coverage):
        for item in collection:
            yield item.provenance


def _validate_supersession(edges: tuple[Edge, ...]) -> None:
    """Accepted precedence is acyclic; ordinary dependency relations may cycle."""
    successors = {}
    for edge in edges:
        if edge.relation == "supersedes":
            successors.setdefault(edge.source, []).append(edge.target)
    state = {}
    for origin in successors:
        if state.get(origin) == 2:
            continue
        stack = [(origin, False)]
        while stack:
            identity, exiting = stack.pop()
            if exiting:
                state[identity] = 2
                continue
            require(state.get(identity) != 1, "supersession cycle")
            if state.get(identity) == 2:
                continue
            state[identity] = 1
            stack.append((identity, True))
            stack.extend((target, False) for target in successors.get(identity, ()))


def validate_graph(graph: GraphData) -> None:
    nodes = {node.id: node for node in graph.nodes}
    require(len(nodes) == len(graph.nodes), "duplicate node id")
    anchors = {anchor.id: anchor for anchor in graph.anchors}
    require(len(anchors) == len(graph.anchors), "duplicate anchor id")
    for provenance in _provenances(graph):
        if provenance.origin.startswith("plurx:"):
            require(provenance.origin in nodes or provenance.origin in anchors, "unknown provenance origin")
        if provenance.method == "reviewed" and provenance.origin in anchors:
            require(provenance.source_digest == anchors[provenance.origin].digest, "reviewed provenance digest must match reviewed anchor digest")
    for field in ("coverage", "conflicts", "receipts"):
        items = getattr(graph, field)
        require(len({item.id for item in items}) == len(items), f"duplicate {field} id")
    for node in graph.nodes:
        require(set(node.anchors) <= anchors.keys(), f"unknown node anchor: {node.id}")
        if node.kind in AUTHORED_KINDS and not (node.kind == "check" and node.provenance.method == "imported"):
            require(node.provenance.method == "reviewed", "authored records require reviewed attribution")
            _authored(node)
        if node.kind == "check" and node.provenance.method == "imported":
            require(node.attributes.get("execution") == "not-run", "imported check is a definition, not execution evidence")
            require(not ({"origin", "validator_identity", "validator_verdict"} & node.attributes.keys()), "check cannot assert receipt authentication")
        if node.provenance.method != "imported" and node.kind in AUTHORED_KINDS:
            attrs = node.attributes
            references = []
            if node.kind == "contract":
                references = [attrs["authoritative_section"], *attrs["check_ids"]]
            elif node.kind == "obligation":
                references = [attrs["authority"]]
            elif node.kind == "decision":
                references = [attrs["source_section"]]
            elif node.kind == "check":
                references = list(attrs["governed_ids"])
            for reference in references:
                require(reference in nodes, f"record reference missing: {reference}")
        if node.provenance.origin.startswith("plurx:"):
            require(node.provenance.origin in nodes or node.provenance.origin in anchors, "unknown provenance origin")
    edge_ids = set()
    for edge in graph.edges:
        require(edge.source in nodes and edge.target in nodes, "edge endpoint missing")
        triple = (edge.source, edge.relation, edge.target)
        require(triple not in edge_ids, "duplicate edge")
        edge_ids.add(triple)
        left, right = nodes[edge.source].kind, nodes[edge.target].kind
        allowed = {
            "implements": (left in {"symbol", "artifact", "route", "point"} and right in {"contract", "obligation", "decision"}),
            "specified-by": (left in {"contract", "obligation", "decision", "check", "route", "point"} and right == "section"),
            "constrained-by": right in {"obligation", "contract", "decision", "section"},
            "delivered-by": left in {"contract", "obligation", "decision"} and right in {"artifact", "section"},
            "assessed-by": left in {"contract", "obligation", "decision", "artifact", "section"} and right in {"finding", "artifact", "section"},
            "creates": left in {"finding", "decision", "section"} and right == "obligation",
            "checked-by": left in {"contract", "obligation", "route", "point"} and right == "check",
            "executed-as": left == "check" and right == "receipt",
            "supersedes": left == right and left in {"contract", "obligation", "decision", "section"},
            "resolved-by": left in {"finding", "obligation"} and right in {"receipt", "artifact", "decision", "section", "symbol"},
            "depends-on": left in {"point", "check", "contract", "obligation", "decision", "artifact", "symbol", "route"} and right in {"point", "check", "contract", "obligation", "decision", "artifact", "symbol", "route"},
            "mentions": True,
        }
        require(allowed[edge.relation], f"invalid {left} --{edge.relation}--> {right}")
        if edge.provenance.origin.startswith("plurx:"):
            require(edge.provenance.origin in nodes or edge.provenance.origin in anchors, "unknown edge provenance origin")
    _validate_supersession(graph.edges)
    for node in graph.nodes:
        if node.kind == "obligation" and node.attributes.get("disposition") == "satisfied":
            supporting = {e.target for e in graph.edges if e.source == node.id and e.relation == "resolved-by"}
            require(any(r.id in supporting and r.origin == "authenticated" for r in graph.receipts), "satisfied obligation requires existing-validator evidence; commit references alone cannot satisfy")
        if node.kind == "finding" and node.attributes.get("disposition") == "resolved":
            require(any(e.source == node.id and e.relation == "resolved-by" for e in graph.edges), "resolved finding needs corrective-change relation")
        if node.kind in {"contract", "obligation", "decision"} and (node.attributes.get("status") == "superseded" or node.attributes.get("disposition") == "superseded"):
            require(any(e.target == node.id and e.relation == "supersedes" for e in graph.edges), "superseded record needs accepted explicit supersession")
    claims = {}
    for claim in graph.claims:
        require(claim.subject in nodes, "claim subject missing")
        claims.setdefault((claim.subject, claim.dimension), set()).add(claim.value)
    for conflict in graph.conflicts:
        require(set(conflict.subjects) <= nodes.keys(), "conflict subject missing")
        require(set(conflict.precedence) <= set(conflict.subjects), "precedence outside conflict")
    for (subject, dimension), values in claims.items():
        if len(values) > 1:
            require(any(subject in c.subjects and dimension == c.dimension for c in graph.conflicts), "unregistered contradictory claims")
    receipt_ids = {r.id for r in graph.receipts}
    require({n.id for n in graph.nodes if n.kind == "receipt"} == receipt_ids, "every receipt node needs normalized evidence metadata")
    for receipt in graph.receipts:
        require(receipt.id in nodes and nodes[receipt.id].kind == "receipt", "receipt node missing")
        require(receipt.check_id in nodes, "receipt check missing")


@dataclass(frozen=True)
class EffectiveLifecycle:
    state: str
    reasons: tuple[str, ...] = ()


_HISTORICAL = frozenset({"superseded", "withdrawn", "waived", "rejected", "resolved"})


def effective_lifecycle(graph: GraphData) -> dict[str, EffectiveLifecycle]:
    """Resolve accepted precedence once; discovery must not manufacture authority.

    Accepted supersession applies to sections too. Source assertions remain
    claims, and unresolved disagreement prevents choosing a governing claim.
    """
    nodes = {n.id: n for n in graph.nodes}
    anchors = {a.id: a for a in graph.anchors}
    reasons = {identity: set() for identity in nodes.keys() | anchors.keys()}
    historical, conflicted, changed = set(), set(), set()
    for node in graph.nodes:
        if node.attributes.get("status") in _HISTORICAL or node.attributes.get("disposition") in _HISTORICAL:
            historical.add(node.id)
            reasons[node.id].add("authored lifecycle is historical")
        if any(anchors[a].integrity != "valid" for a in node.anchors if a in anchors):
            changed.add(node.id)
            reasons[node.id].add("supporting anchor needs review")
    for anchor in graph.anchors:
        if anchor.integrity != "valid":
            changed.add(anchor.id)
            reasons[anchor.id].add("anchor source changed")
    for edge in graph.edges:
        if edge.relation == "supersedes":
            historical.add(edge.target)
            reasons[edge.target].add("explicitly superseded by " + edge.source)
    for claim in graph.claims:
        if claim.dimension in {"lifecycle", "applicability"} and claim.value in _HISTORICAL:
            historical.add(claim.subject)
            reasons[claim.subject].add("reviewed historical source claim")
    for conflict in graph.conflicts:
        if conflict.disposition == "unresolved":
            conflicted.update(conflict.subjects)
            for identity in conflict.subjects:
                reasons[identity].add("unresolved registered conflict")
    # A record sourced from a retired section does not regain authority merely
    # because its own authored status still says active/open.
    while True:
        before = (len(historical), len(conflicted), len(changed))
        for node in graph.nodes:
            origins = [node.provenance.origin]
            origins.extend(node.attributes[key] for key in ("authoritative_section", "source_section")
                           if isinstance(node.attributes.get(key), str))
            for origin in origins:
                if origin == node.id:
                    continue
                if origin in historical:
                    historical.add(node.id)
                    reasons[node.id].add("governing origin is historical: " + origin)
                if origin in conflicted:
                    conflicted.add(node.id)
                    reasons[node.id].add("governing origin is conflicted: " + origin)
                if origin in changed:
                    changed.add(node.id)
                    reasons[node.id].add("governing origin needs review: " + origin)
        if before == (len(historical), len(conflicted), len(changed)):
            break
    return {identity: EffectiveLifecycle("conflicted" if identity in conflicted else
                                        "historical" if identity in historical else
                                        "needs-review" if identity in changed else "current",
                                        tuple(sorted(reasons[identity]))) for identity in reasons}


def _document(root: Path, path: str, collections: set[str]) -> Mapping:
    try:
        raw = tomllib.loads(read_source(root, path).decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as error:
        raise KnowledgeError(f"invalid manifest {path}: {error}") from error
    exact(raw, {"schema_version", "repository"} | collections, set(), path)
    require(raw["schema_version"] == SCHEMA_VERSION and raw["repository"] == "plurx", "unknown manifest schema/repository")
    for name in collections:
        _rows(raw[name], name)
    return raw


def merge_graphs(*graphs: GraphData) -> GraphData:
    """Combine disjoint data, allowing identical canonical imported identities."""
    values = {}
    for field in fields(GraphData):
        seen = {}
        for graph in graphs:
            for item in getattr(graph, field.name):
                key = (item.source, item.relation, item.target) if isinstance(item, Edge) else (item.subject, item.dimension, item.value, content_digest(item.provenance)) if isinstance(item, Claim) else item.id
                require(key not in seen or seen[key] == item, f"inconsistent duplicate {field.name}: {key}")
                seen[key] = item
        values[field.name] = tuple(seen[key] for key in sorted(seen))
    return GraphData(**values)


_validate_revision = revision


def load_graph(root: Path, *, revision: str | None = None, records_path: str = "validation/knowledge/records.toml", coverage_path: str = "validation/knowledge/coverage.toml", conflicts_path: str = "validation/knowledge/conflicts.toml", pilot_path: str = "validation/knowledge/pilot.json") -> GraphData:
    """Load strict authored data plus bounded read-only canonical adapters."""
    root = root.resolve()
    try:
        pilot = json.loads(read_source(root, pilot_path))
    except (ValueError, UnicodeError) as error:
        raise KnowledgeError("invalid public pilot manifest") from error
    if revision is None:
        try:
            revision = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL).strip()
        except (OSError, subprocess.CalledProcessError) as error:
            raise KnowledgeError("repository revision unavailable; frozen corpora require explicit revision") from error
    reviewed_revision = _validate_revision(revision)
    roles = pilot.get("document_roles", {})
    require(isinstance(roles, Mapping), "document roles must be a map")
    raw = _document(root, records_path, {"anchors", "records", "edges", "claims"})
    coverage = _document(root, coverage_path, {"scopes"})
    conflicts = _document(root, conflicts_path, {"conflicts"})
    anchors = tuple(resolve_anchor(root, entry) for entry in raw["anchors"])
    nodes = tuple(Node.from_dict(entry) for entry in raw["records"])
    require(len(nodes) <= 60, "pilot authored-record limit exceeded")
    for node in nodes:
        require(node.kind in AUTHORED_KINDS, "derived nodes cannot be authored records")
        require(node.provenance.method == "reviewed", "authored semantic record requires reviewed provenance")
        _authored(node)
    anchor_nodes = []
    artifact_nodes = {}
    for anchor in anchors:
        prov = Provenance("parsed", "", "", anchor.owner_module, anchor.current_digest, reviewed_revision)
        from .adapters import artifact_id
        for path in anchor.physical_paths:
            aid = artifact_id(path)
            artifact_nodes[aid] = Node(aid, "artifact", path, (path,), (), {"path": path, "role": roles.get(path, "not-recorded"), "digest": digest_bytes(read_source(root, path))}, Provenance("parsed", "", "", path, digest_bytes(read_source(root, path)), prov.revision))
        anchor_nodes.append(Node(anchor.id, anchor.kind, " / ".join(anchor.selector) if isinstance(anchor.selector, tuple) else anchor.selector, anchor.physical_paths, (anchor.id,), {"integrity": anchor.integrity, "applicability": "reviewed" if anchor.integrity == "valid" else "needs-review", "path": anchor.path, "owner_module": anchor.owner_module}, prov))
    from .adapters import _artifact
    for node in nodes:
        for path in node.scope:
            if not any(mark in path for mark in "*?[]{}"):
                artifact = _artifact(root, path, reviewed_revision, roles.get(path, "not-recorded"))
                artifact_nodes[artifact.id] = artifact
    section_edges = tuple(Edge(artifact_id(a.path), "mentions", a.id, Provenance("parsed", "", "", a.path, a.current_digest, reviewed_revision)) for a in anchors)
    # Use keywords: receipt and conflict collections intentionally remain distinct.
    authored = GraphData(nodes=nodes + tuple(anchor_nodes) + tuple(artifact_nodes.values()), edges=tuple(Edge.from_dict(v) for v in raw["edges"]) + section_edges, claims=tuple(Claim.from_dict(v) for v in raw["claims"]), anchors=anchors, coverage=tuple(Coverage.from_dict(v) for v in coverage["scopes"]), receipts=tuple(Receipt(id=n.id, **n.attributes) for n in nodes if n.kind == "receipt"), conflicts=tuple(Conflict(**exact(v, {"id", "subjects", "dimension", "owner", "reason", "revisit_condition", "disposition"}, {"resolution", "precedence"}, "conflict")) for v in conflicts["conflicts"]))
    from .adapters import import_pilot
    graph = merge_graphs(authored, import_pilot(root, pilot_path=pilot_path, reviewed_revision=reviewed_revision))
    validate_graph(graph)
    for provenance in _provenances(graph):
        if provenance.method == "reviewed" and not provenance.origin.startswith("plurx:"):
            require(digest_bytes(read_source(root, provenance.origin)) == provenance.source_digest, "reviewed source-file origin changed; needs-review")
    return graph
