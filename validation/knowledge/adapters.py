"""Read-only projections of canonical routing, functionality and receipt inputs.

Adapters preserve original row fields and identities. Catalog selection is impact
scope, never a proof that a behavioral execution used every required input.
"""
from __future__ import annotations

from collections.abc import Mapping
from dataclasses import fields
import hashlib
import json
from pathlib import Path
import re
import tomllib
from typing import Any

from .model import (Anchor, Edge, GraphData, KnowledgeError, Node, Provenance,
                    Receipt, content_digest, digest_bytes, exact, identifier,
                    merge_graphs, read_source, require, resolve_anchor, revision,
                    safe_relative, strings, text)

ROUTING_FIELDS = {"id", "source", "source_anchor", "test", "test_anchor"}
ROUTING_MANIFEST = "tests/playback/routing-decisions.toml"
CATALOG = "validation/points.toml"
INPUTS = "validation/main-preflight-inputs.json"


def route_id(original: str) -> str:
    return identifier("plurx:route:" + text(original, "route id"), "route")


def route_check_id(original: str) -> str:
    return identifier("plurx:check:routing." + text(original, "route id"), "check")


def artifact_id(path: str) -> str:
    return "plurx:artifact:" + hashlib.sha256(safe_relative(path).encode()).hexdigest()[:20]


def symbol_id(owner: str, needle: str) -> str:
    return "plurx:symbol:" + hashlib.sha256((safe_relative(owner) + "\n" + text(needle, "symbol")).encode()).hexdigest()[:20]


def point_id(original: str) -> str:
    return identifier("plurx:point:" + original, "point")


def check_id(original: str) -> str:
    return identifier("plurx:check:" + original, "check")


def _provenance(path: str, raw: Any, reviewed_revision: str) -> Provenance:
    return Provenance("imported", "", "", path, content_digest(raw), revision(reviewed_revision))


def _artifact(root: Path, path: str, reviewed_revision: str, role: str = "not-recorded") -> Node:
    raw = read_source(root, path)
    return Node(artifact_id(path), "artifact", path, (path,), (),
                {"path": path, "role": role, "digest": digest_bytes(raw)},
                Provenance("parsed", "", "", path, digest_bytes(raw), revision(reviewed_revision)))


def import_routing(root: Path, ids: tuple[str, ...] | list[str], reviewed_revision: str,
                   *, manifest: str = ROUTING_MANIFEST) -> GraphData:
    """Import selected unchanged five-field rows; refuse lost/ambiguous anchors."""
    raw = tomllib.loads(read_source(root, manifest).decode("utf-8"))
    exact(raw, {"version", "decisions"}, set(), "routing inventory")
    require(raw["version"] == 1 and isinstance(raw["decisions"], list), "unknown routing schema")
    rows = {}
    for row in raw["decisions"]:
        exact(row, ROUTING_FIELDS, set(), "routing row")
        original = text(row["id"], "routing id")
        require(original not in rows, "duplicate routing id")
        rows[original] = row
    ids = strings(ids, "selected routing ids")
    require(len(set(ids)) == len(ids) and bool(ids), "routing selection must be nonempty and unique")
    require(set(ids) <= rows.keys(), "selected routing row missing")
    nodes, edges, anchors = [], [], []
    artifacts = {}
    for original in ids:
        row = rows[original]
        prov = _provenance(manifest, row, reviewed_revision)
        rid, cid = route_id(original), route_check_id(original)
        selected = []
        for path_key, needle_key in (("source", "source_anchor"), ("test", "test_anchor")):
            owner = safe_relative(row[path_key])
            sid = symbol_id(owner, row[needle_key])
            anchor = resolve_anchor(root, {"id": sid, "path": owner, "kind": "symbol", "selector": row[needle_key], "digest": "sha256:" + "0" * 64})
            # Imported anchors bind the current canonical row/source snapshot;
            # authored semantic claims retain their separately reviewed digests.
            anchor = Anchor(**{**anchor.__dict__, "digest": anchor.current_digest, "integrity": "valid"})
            anchors.append(anchor)
            selected.append(sid)
            symbol_provenance = Provenance("parsed", "", "", owner, anchor.current_digest, revision(reviewed_revision))
            nodes.append(Node(sid, "symbol", row[needle_key], anchor.physical_paths, (sid,),
                              {"path": anchor.path, "owner_module": owner, "selector": row[needle_key], "physical_paths": anchor.physical_paths, "line": anchor.line}, symbol_provenance))
            for physical in anchor.physical_paths:
                artifacts[physical] = _artifact(root, physical, reviewed_revision)
        descriptor = {"source_manifest": manifest, "row_id": original,
                      "source_paths": sorted(set(p for a in anchors[-2:] for p in a.physical_paths)),
                      "row": row, "completeness": "impact-only"}
        nodes.append(Node(rid, "route", original, tuple(descriptor["source_paths"]), tuple(selected),
                          {"canonical_id": original, "manifest": manifest, "row": row,
                           "row_digest": content_digest(row), "descriptor": descriptor,
                           "descriptor_digest": content_digest(descriptor), "descriptor_completeness": "impact-only"}, prov))
        nodes.append(Node(cid, "check", row["test_anchor"], tuple(descriptor["source_paths"]), (selected[1],),
                          {"canonical_id": original, "check_kind": "behavioral", "selector": row["test_anchor"],
                           "test": row["test"], "source_manifest": manifest, "row": row,
                           "descriptor": descriptor, "descriptor_digest": content_digest(descriptor),
                           "descriptor_completeness": "impact-only", "execution": "not-run"}, prov))
        edges.extend((Edge(rid, "checked-by", cid, prov), Edge(rid, "mentions", selected[0], prov), Edge(cid, "mentions", selected[1], prov)))
    nodes.extend(artifacts.values())
    return merge_graphs(GraphData(nodes=tuple(nodes), edges=tuple(edges), anchors=tuple(anchors)))


def _matches(path: str, pattern: str) -> bool:
    """Component-bounded catalog glob semantics, including zero-component **."""
    from validation.runner import matches
    return matches(path, (pattern,))


def import_catalog(root: Path, point_ids: tuple[str, ...] | list[str] | None,
                   reviewed_revision: str, *, seed_paths: tuple[str, ...] = (),
                   catalog_path: str = CATALOG) -> GraphData:
    """Import selected points and transitive providers, retaining all six fields."""
    from validation.runner import Check, Point, load_catalog
    raw = tomllib.loads(read_source(root, catalog_path).decode("utf-8"))
    exact(raw, {"version", "settings", "checks", "points"}, set(), "functionality catalog")
    exact(raw["settings"], {"profiles"}, {"artifact_dir", "always_checks", "audit_paths", "audit_exclude", "allow_unmatched_globs"}, "catalog settings")
    for row in raw["points"]:
        exact(row, {"id", "title", "contract", "paths", "checks"}, {"depends_on"}, "Point")
    check_fields = {f.name for f in fields(Check)}
    for row in raw["checks"]:
        exact(row, {"id", "title", "command", "profiles"}, check_fields - {"id", "title", "command", "profiles"}, "Check")
    catalog = load_catalog(root / catalog_path)
    points = {p.id: p for p in catalog.points}
    require(len(points) == len(catalog.points), "duplicate catalog point")
    checks = catalog.check_map
    require(len(checks) == len(catalog.checks), "duplicate catalog check")
    if point_ids is None:
        selected = {p.id for p in points.values() if any(_matches(path, pattern) for path in seed_paths for pattern in p.paths)}
    else:
        selected = set(strings(point_ids, "point ids"))
    require(selected <= points.keys(), "unknown selected point")
    visiting, visited = set(), set()
    def providers(identity):
        require(identity in points, "unknown provider")
        require(identity not in visiting, "catalog provider cycle")
        if identity in visited:
            return
        visiting.add(identity)
        for provider in points[identity].depends_on:
            providers(provider)
        visiting.remove(identity)
        visited.add(identity)
    for identity in sorted(selected):
        providers(identity)
    nodes, edges = [], []
    for identity in sorted(visited):
        point = points[identity]
        original = {f.name: getattr(point, f.name) for f in fields(Point)}
        prov = _provenance(catalog_path, original, reviewed_revision)
        closure = set()
        def gather(pid):
            if pid in closure:
                return
            closure.add(pid)
            for provider in points[pid].depends_on:
                gather(provider)
        gather(identity)
        descriptor = {"source_manifest": catalog_path, "point": identity,
                      "providers": sorted(closure - {identity}),
                      "paths": sorted({path for pid in closure for path in points[pid].paths}),
                      "point_rows": [{f.name: getattr(points[pid], f.name) for f in fields(Point)} for pid in sorted(closure)],
                      "completeness": "impact-only"}
        for pattern in descriptor["paths"]:
            safe_relative(pattern, pattern=True)
        nodes.append(Node(point_id(identity), "point", point.title, point.paths, (),
                          {"canonical_id": identity, "row": original, "contract": point.contract,
                           "descriptor": descriptor, "descriptor_digest": content_digest(descriptor),
                           "descriptor_completeness": "impact-only"}, prov))
        for provider in point.depends_on:
            edges.append(Edge(point_id(identity), "depends-on", point_id(provider), prov))
        for cid in point.checks:
            require(cid in checks, "unknown point check")
            edges.append(Edge(point_id(identity), "checked-by", check_id(cid), prov))
    selected_checks = {cid for pid in visited for cid in points[pid].checks}
    for cid in sorted(selected_checks):
        check = checks[cid]
        original = {f.name: getattr(check, f.name) for f in fields(Check)}
        prov = _provenance(catalog_path, original, reviewed_revision)
        consumers = {pid for pid in visited if cid in points[pid].checks}
        descriptor = {"source_manifest": catalog_path, "check": original,
                      "points": sorted(consumers),
                      "paths": sorted({pattern for node in nodes if node.kind == "point" and node.attributes["canonical_id"] in consumers for pattern in node.attributes["descriptor"]["paths"]}),
                      "completeness": "impact-only"}
        nodes.append(Node(check_id(cid), "check", check.title, tuple(descriptor["paths"]), (),
                          {"canonical_id": cid, "row": original, "check_kind": "catalog-defined",
                           "descriptor": descriptor, "descriptor_digest": content_digest(descriptor),
                           "descriptor_completeness": "impact-only", "execution": "not-run"}, prov))
    return GraphData(nodes=tuple(nodes), edges=tuple(edges))


def input_descriptor(root: Path, identity: str, *, manifest: str = INPUTS) -> dict[str, Any]:
    """Preserve canonical witness selection, including node script and pathsets."""
    from validation.main_preflight_adoption import witness
    raw = json.loads(read_source(root, manifest))
    exact(raw, {"version", "families"}, set(), "input descriptor manifest")
    require(raw["version"] == 1 and isinstance(raw["families"], list), "unknown input descriptor schema")
    for row in raw["families"]:
        exact(row, {"id_prefix", "inputs", "reason"}, {"pathsets", "history"}, "input descriptor row")
        text(row["id_prefix"], "input identity prefix")
        require(bool(strings(row["inputs"], "inputs")), "empty canonical input descriptor")
        text(row["reason"], "input descriptor review reason")
        for pattern in (*row["inputs"], *row.get("pathsets", [])):
            safe_relative(pattern, pattern=True)
        require("history" not in row or type(row["history"]) is bool, "history must be boolean")
    try:
        selected = witness(text(identity, "check identity"), raw["families"])
    except (RuntimeError, KeyError, TypeError, ValueError) as error:
        raise KnowledgeError("canonical input witness refused: " + str(error)) from error
    for pattern in (*selected["inputs"], *selected.get("pathsets", [])):
        safe_relative(pattern, pattern=True)
    # Copy all canonical fields. Missing history/pathsets remain absent rather
    # than silently manufacturing a stronger reviewed descriptor.
    return json.loads(json.dumps(selected))


def import_preflight_inputs(root: Path, reviewed_revision: str,
                            identities: tuple[str, ...] = ()) -> GraphData:
    nodes = []
    for identity in identities:
        descriptor = input_descriptor(root, identity)
        local = "preflight." + re.sub(r"[^A-Za-z0-9._/-]", ".", identity)
        prov = _provenance(INPUTS, descriptor, reviewed_revision)
        nodes.append(Node(check_id(local), "check", identity, tuple(descriptor["inputs"]), (),
                          {"canonical_id": identity, "check_kind": "canonical-preflight",
                           "descriptor": descriptor, "descriptor_digest": content_digest(descriptor),
                           "descriptor_completeness": "canonical", "execution": "not-run",
                           "compatibility": "unknown"}, prov))
    return GraphData(nodes=tuple(nodes))


def normalize_receipt(raw: Mapping[str, Any], *, validator_verdict=None) -> Receipt:
    """Normalize local metadata without authenticating a run or reusing a pass.

    No existing-validator bridge is available in M1: missing API/artifact
    prerequisites are explicit unavailable evidence. Arbitrary typed objects,
    dictionaries, CLI flags and serialized caches cannot create trust here.
    """
    require(validator_verdict is None, "existing-validator bridge unavailable; supplied verdict cannot authenticate")
    required = {"id", "execution_id", "check_id", "source_revision", "source_tree", "environment", "result", "scope_descriptor", "descriptor_digest", "origin"}
    exact(raw, required, {"artifact_ref", "artifact_digest"}, "receipt metadata")
    require(raw["origin"] in {"reported", "unavailable"}, "raw receipt cannot assert authenticated origin")
    return Receipt(**raw)


def import_pilot(root: Path, *, pilot_path: str = "validation/knowledge/pilot.json", reviewed_revision: str | None = None) -> GraphData:
    """Use only the public pilot's selected routes and receipt dependency roots."""
    pilot = json.loads(read_source(root, pilot_path))
    require(pilot.get("schema_version") == 1 and pilot.get("repository") == "plurx", "unknown pilot schema/repository")
    reviewed_revision = revision(reviewed_revision or pilot["source_commit"])
    cohort = pilot["cohorts"]["routing"]
    routes = import_routing(root, cohort["ids"], reviewed_revision, manifest=cohort["manifest"])
    receipt = pilot["cohorts"]["receipt"]
    paths = tuple(sorted({p for n in routes.nodes if n.kind == "route" for p in n.scope} | set(receipt["sources"])))
    catalog = import_catalog(root, None, reviewed_revision, seed_paths=paths)
    roles = pilot.get("document_roles", {})
    require(isinstance(roles, Mapping), "document roles must be a map")
    artifacts = []
    for path in dict.fromkeys([*receipt["sources"], receipt["policy"], *roles]):
        role = roles.get(path, "not-recorded")
        require(role in {"reference", "plan", "build-ledger", "review", "design", "not-recorded"}, "unknown document role")
        if path not in {n.attributes.get("path") for n in routes.nodes if n.kind == "artifact"}:
            artifacts.append(_artifact(root, path, reviewed_revision, role))
    # Families are imported only for this receipt cohort, not all unit methods.
    families = json.loads(read_source(root, INPUTS))["families"]
    prefixes = ("validation:test_python_unit_receipts.", "validation:test_main_preflight_adoption.", "validation:test_main_unit_receipts.")
    identities = tuple(row["id_prefix"] for row in families if any(row["id_prefix"].startswith(prefix) or prefix.startswith(row["id_prefix"]) for prefix in prefixes))
    inputs = import_preflight_inputs(root, reviewed_revision, identities)
    return merge_graphs(routes, catalog, GraphData(nodes=tuple(artifacts)), inputs)
