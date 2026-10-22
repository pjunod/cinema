"""Explain source facts independently of execution and trusted origin.

No authentication bridge exists in this pilot. Even an unchanged canonical
proof closure remains reported evidence and cannot authorize adoption.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Mapping

from .model import KnowledgeError, Receipt, canonical_json, content_digest, digest_bytes, require, safe_relative
from .snapshot import Snapshot, _inventory, _matches, git

VERSION = 1
INPUTS = "validation/main-preflight-inputs.json"


def _tree(root: Path, revision: str):
    entries = {}
    for entry in git(root, "ls-tree", "-r", "-z", revision).split(b"\0"):
        if entry:
            header, path = entry.split(b"\t", 1)
            mode, kind, oid = header.decode().split()
            entries[path.decode("utf-8")] = (int(mode, 8), kind, oid)
    return entries


def _witness(raw: bytes, identity: str):
    from validation.main_preflight_adoption import witness
    manifest = json.loads(raw)
    require(manifest.get("version") == 1, "unknown canonical input schema")
    return witness(identity, manifest["families"])


def _blob(root: Path, entry) -> bytes:
    require(entry[1] == "blob" and entry[0] in {0o100644, 0o100755}, "unsupported proof input type")
    return git(root, "cat-file", "blob", entry[2])


def assess(receipt, snapshot: Snapshot, root: Path, *, target_revision=None,
           descriptor_complete=False, dependency_paths=None, target_environment=None) -> dict:
    """Return independent dimensions without upgrading any execution result.

    ``descriptor_complete`` is a necessary review assertion, never sufficient:
    the receipt must retain the exact canonical witness, checked against both
    immutable history and captured current descriptor bytes. Extra dependency
    paths expand this witness; they cannot replace or narrow its selection.
    """
    result = {"anchor_integrity": "unknown", "semantic_applicability": "unknown",
              "origin": "unavailable", "source_compatibility": "unknown",
              "execution": "not-run", "check_kind": "not-recorded", "environment": {},
              "overall_applicability": "unknown", "authenticated_credit": False,
              "reasons": []}
    if not isinstance(receipt, Receipt):
        # Named checks carry no execution. Raw mappings cannot claim origin.
        attributes = getattr(receipt, "attributes", {})
        if isinstance(attributes, Mapping):
            result["check_kind"] = attributes.get("check_kind", "not-recorded")
        result["reasons"].append("named check is not an execution receipt")
        return result
    result.update(origin=receipt.origin, execution=receipt.result,
                  check_kind=receipt.scope_descriptor.get("check_kind", "not-recorded"),
                  environment=json.loads(canonical_json(receipt.environment)))
    root = Path(root).resolve()
    def unknown(reason):
        result["reasons"].append(reason)
        return result
    if receipt.origin == "unavailable":
        return unknown("execution artifact unavailable")
    if target_revision is not None and target_revision != snapshot.head:
        return unknown("requested target is not the captured worktree revision")
    if target_environment is None:
        target_environment = snapshot.environment or None
    if target_environment is None or dict(target_environment) != dict(receipt.environment):
        return unknown("environment identity unavailable or changed")
    if not snapshot.verify(root):
        return unknown("snapshot-changed")
    try:
        historical = _tree(root, receipt.source_revision)
        historical_tree = git(root, "rev-parse", receipt.source_revision + "^{tree}").decode().strip()
        if historical_tree != receipt.source_tree:
            return unknown("receipt revision/tree identity mismatch")
        if receipt.scope_descriptor.get("history"):
            if git(root, "rev-parse", "--is-shallow-repository").strip() != b"false":
                return unknown("complete history unavailable")
            if git(root, "log", "--format=%H:%ct:%s", receipt.source_revision) != git(root, "log", "--format=%H:%ct:%s", snapshot.head):
                result["source_compatibility"] = "stale"
                return unknown("canonical history input changed")
        # Exact-tree describes source identity only, even for a reported run.
        # Global dirtiness includes unrelated tracked edits and untracked files.
        if snapshot.tree == receipt.source_tree and not snapshot.repository_dirty:
            result["source_compatibility"] = "exact-tree"
            result["reasons"].append("globally clean target tree equals reported execution tree")
            return result
        if not descriptor_complete or receipt.descriptor_completeness != "canonical":
            return unknown("reviewed canonical proof completeness unavailable")
        descriptor = receipt.scope_descriptor
        identity = descriptor.get("id_prefix")
        if not isinstance(identity, str) or not identity:
            return unknown("canonical check identity unavailable")
        captured = {item.path: item for item in snapshot.inputs}
        current_manifest = captured.get(INPUTS)
        if current_manifest is None or current_manifest.data is None or INPUTS not in historical:
            return unknown("canonical descriptor history unavailable")
        current = _witness(current_manifest.data, identity)
        prior = _witness(_blob(root, historical[INPUTS]), identity)
        # Preserve the whole canonical row: input patterns, pathsets, history,
        # review reason and check identity. Retrieval scope never substitutes.
        if content_digest(current) != receipt.descriptor_digest or content_digest(prior) != receipt.descriptor_digest:
            result["source_compatibility"] = "stale"
            return unknown("canonical dependency descriptor changed")
        patterns = tuple(current["inputs"])
        pathsets = tuple(current.get("pathsets", ()))
        extra = tuple(dependency_paths or ())
        for pattern in (*patterns, *pathsets, *extra):
            safe_relative(pattern, pattern=True)
        patterns = tuple(dict.fromkeys((*patterns, *extra, INPUTS)))
        tracked, untracked = _inventory(root)
        current_paths = tracked | untracked
        selected_before = {p for p in historical if any(_matches(p, pattern) for pattern in patterns)}
        selected_now = {p for p in current_paths if any(_matches(p, pattern) for pattern in patterns)}
        # Bind physical existence, not only index membership (a deleted tracked
        # input still appears in ls-files).
        if not selected_now <= captured.keys():
            return unknown("canonical proof extends beyond captured pilot inputs")
        current_items = {path: captured[path] for path in selected_now}
        selected_now = {path for path, item in current_items.items() if item.data is not None}
        membership_before = {p for p in historical if any(_matches(p, pattern) for pattern in pathsets)}
        membership_paths = {p for p in current_paths if any(_matches(p, pattern) for pattern in pathsets)}
        if not membership_paths <= captured.keys():
            return unknown("canonical path-set extends beyond captured pilot inputs")
        membership_now = {p for p in membership_paths if captured[p].data is not None}
        if selected_before != selected_now or membership_before != membership_now:
            result["source_compatibility"] = "stale"
            return unknown("dependency addition/removal/rename or path-set membership changed")
        for path in sorted(selected_before):
            mode, kind, _ = historical[path]
            item = current_items[path]
            if kind != "blob" or mode not in {0o100644, 0o100755}:
                return unknown("symlink/submodule proof input unsupported")
            if mode != item.mode or digest_bytes(_blob(root, historical[path])) != item.digest:
                result["source_compatibility"] = "stale"
                return unknown("dependency bytes or file mode changed")
        if current.get("history"):
            if git(root, "rev-parse", "--is-shallow-repository").strip() != b"false":
                return unknown("complete history unavailable")
            old = git(root, "log", "--format=%H:%ct:%s", receipt.source_revision)
            new = git(root, "log", "--format=%H:%ct:%s", snapshot.head)
            if old != new:
                result["source_compatibility"] = "stale"
                return unknown("canonical history input changed")
        if not snapshot.verify(root):
            return unknown("snapshot-changed")
        result["source_compatibility"] = "compatible-within-reviewed-scope"
        result["reasons"].append("canonical proof inputs unchanged; reported origin cannot authorize adoption")
        return result
    except (KnowledgeError, OSError, ValueError, KeyError, TypeError, RuntimeError):
        return unknown("proof input, canonical descriptor or historical Git object unavailable")
