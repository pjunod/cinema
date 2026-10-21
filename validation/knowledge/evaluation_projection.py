"""Mechanical visibility projection for sealed evaluations, independent of gold.

A QueryProjection is a filtered retrieval view of a validated graph, not a new
canonical GraphData interchange document. References to excluded definitions
remain identifiers only. No hidden source text, edge or provenance is served.
"""
from __future__ import annotations

from dataclasses import dataclass, replace
import fnmatch
import hashlib
from typing import Mapping

from .model import GraphData, KnowledgeError, canonical_json, content_digest, digest_bytes, safe_source_relative


@dataclass(frozen=True)
class QueryProjection:
    nodes: tuple
    edges: tuple
    claims: tuple
    anchors: tuple
    coverage: tuple
    receipts: tuple
    conflicts: tuple
    visibility_digest: str
    excluded_nodes: int


def project(graph: GraphData, files: Mapping, visibility: Mapping) -> QueryProjection:
    """Filter solely by visible bytes/ranges; never consult gold or task answers.

    An authored summary is eligible only when all its reviewed anchors and its
    provenance origin are visible. This conservative rule may reduce utility;
    it cannot buy apparent utility by disclosing an inaccessible authority.
    """
    if not isinstance(files, Mapping) or set(files) != set(visibility):
        raise KnowledgeError('visible files and range declarations must match')
    bodies = {}
    ranges = {}
    for raw_path, row in files.items():
        path = safe_source_relative(raw_path)
        if not isinstance(row, Mapping) or set(row) != {'content', 'mode'} or not isinstance(row['content'], str) or type(row['mode']) is not int or row['mode'] not in {0o644, 0o755, 0o100644, 0o100755}:
            raise KnowledgeError('invalid visible file')
        raw = row['content'].encode('utf-8')
        declaration = visibility[path]
        if set(declaration) != {'line_ranges', 'content_sha256'} or declaration['content_sha256'].removeprefix('sha256:') != hashlib.sha256(raw).hexdigest():
            raise KnowledgeError('visible content digest mismatch')
        lines = row['content'].splitlines(keepends=True)
        selected = set()
        prior = 0
        for pair in declaration['line_ranges']:
            if not isinstance(pair, (list, tuple)) or len(pair) != 2 or any(type(v) is not int for v in pair):
                raise KnowledgeError('invalid visible line range')
            start, end = pair
            if not prior < start <= end <= len(lines):
                raise KnowledgeError('unordered or out-of-bounds visible ranges')
            selected.update(range(start, end + 1))
            prior = end
        if any(line.strip() for number, line in enumerate(lines, 1) if number not in selected):
            raise KnowledgeError('hidden lines must contain no source text')
        bodies[path], ranges[path] = lines, selected
    whole = {p for p, lines in bodies.items() if ranges[p] == set(range(1, len(lines) + 1))}
    visible_anchors = {}
    for anchor in graph.anchors:
        if anchor.path not in bodies or not set(range(anchor.line, anchor.end_line + 1)) <= ranges[anchor.path]:
            continue
        passage = ''.join(bodies[anchor.path][anchor.line - 1:anchor.end_line])
        if anchor.excerpt and anchor.excerpt not in passage:
            continue
        if anchor.kind == 'symbol' and str(anchor.selector) not in passage:
            continue
        if anchor.kind == 'section':
            current = digest_bytes(passage.encode())
        elif set(anchor.physical_paths) <= whole:
            current = content_digest({p: digest_bytes(''.join(bodies[p]).encode()) for p in anchor.physical_paths})
        else:
            # Partial visibility cannot attest the whole physical-source digest.
            current = content_digest({'partial': anchor.path, 'visible': digest_bytes(passage.encode())})
        visible_anchors[anchor.id] = replace(anchor, current_digest=current,
                                             integrity='valid' if current == anchor.digest else 'changed')
    candidates = {node.id: node for node in graph.nodes
                  if node.anchors and set(node.anchors) <= visible_anchors.keys()
                  or not node.anchors and node.scope and all(any(fnmatch.fnmatchcase(p, scope) for p in whole) for scope in node.scope)}

    def origin_visible(provenance, kept):
        origin = provenance.origin
        return origin in visible_anchors or origin in kept if origin.startswith('plurx:') else origin in whole

    # Every surviving imported/derived origin must itself survive the same filter.
    while True:
        retained = {key: node for key, node in candidates.items() if origin_visible(node.provenance, candidates)}
        if retained.keys() == candidates.keys():
            break
        candidates = retained
    nodes = tuple(node for node in graph.nodes if node.id in candidates)
    edges = tuple(edge for edge in graph.edges if edge.source in candidates and edge.target in candidates
                  and origin_visible(edge.provenance, candidates))
    claims = tuple(claim for claim in graph.claims if claim.subject in candidates
                   and origin_visible(claim.provenance, candidates))
    coverage = tuple(replace(c, scope=tuple(p for p in bodies if any(fnmatch.fnmatchcase(p, s) for s in c.scope)),
                             boundaries=(*c.boundaries, 'Only sealed visible passages are available in this evaluation.'))
                     for c in graph.coverage if origin_visible(c.provenance, candidates)
                     and any(fnmatch.fnmatchcase(p, s) for p in bodies for s in c.scope))
    conflicts = tuple(c for c in graph.conflicts if set(c.subjects) <= candidates.keys())
    receipts = tuple(r for r in graph.receipts if r.id in candidates and r.check_id in candidates)
    anchors = tuple(a for a in visible_anchors.values() if any(a.id in n.anchors for n in nodes))
    digest = hashlib.sha256(canonical_json({'files': files, 'visibility': visibility}).encode()).hexdigest()
    return QueryProjection(nodes, edges, claims, anchors, coverage, receipts, conflicts,
                           digest, len(graph.nodes) - len(nodes))
