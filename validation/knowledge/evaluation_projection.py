"""Mechanical sealed-view projection, independent of tasks and hidden answers.

A projection is a retrieval view, not a new canonical GraphData document.
Original semantic payloads require unchanged visible governing support. Visible
structure is rebuilt from view bytes, rather than copied from a pristine graph.
"""
from __future__ import annotations

from dataclasses import dataclass, replace
import fnmatch
import hashlib
import json
import tomllib
from typing import Mapping

from .model import (GraphData, KnowledgeError, Provenance, _sections,
                    canonical_json, content_digest, digest_bytes,
                    normalize_child_source, safe_source_relative)


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
    """Project only independently visible sources; never infer missing semantics."""
    if not isinstance(files, Mapping) or set(files) != set(visibility):
        raise KnowledgeError('visible files and range declarations must match')
    bodies, ranges = {}, {}
    for raw_path, row in files.items():
        path = safe_source_relative(raw_path)
        if not isinstance(row, Mapping) or set(row) != {'content', 'mode'} or not isinstance(row['content'], str) or type(row['mode']) is not int or row['mode'] not in {0o644, 0o755, 0o100644, 0o100755}:
            raise KnowledgeError('invalid visible file')
        raw = row['content'].encode('utf-8')
        declaration = visibility[path]
        if not isinstance(declaration, Mapping) or set(declaration) != {'line_ranges', 'content_sha256'} or not isinstance(declaration['content_sha256'], str) or declaration['content_sha256'].removeprefix('sha256:') != hashlib.sha256(raw).hexdigest():
            raise KnowledgeError('visible content digest mismatch')
        lines = row['content'].splitlines(keepends=True)
        selected, prior = set(), 0
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
    whole = {path for path, lines in bodies.items() if ranges[path] == set(range(1, len(lines) + 1))}
    raw_files = {path: ''.join(lines).encode('utf-8') for path, lines in bodies.items()}
    file_digests = {path: digest_bytes(raw) for path, raw in raw_files.items()}
    visible_anchors = {}
    for anchor in graph.anchors:
        if anchor.path not in bodies or not set(range(anchor.line, anchor.end_line + 1)) <= ranges[anchor.path]:
            continue
        passage = ''.join(bodies[anchor.path][anchor.line - 1:anchor.end_line])
        selector = anchor.selector
        if anchor.kind == 'section':
            headings = [row for row in _sections(raw_files[anchor.path]) if row[2] == anchor.line]
            if len(headings) != 1:
                continue
            selector = headings[0][0]
            current = digest_bytes(passage.encode())
        else:
            normalized = normalize_child_source(passage.encode(), anchor.owner_module) if anchor.path != anchor.owner_module else passage
            if str(selector) not in normalized:
                continue
            current = (content_digest({path: file_digests[path] for path in anchor.physical_paths})
                       if set(anchor.physical_paths) <= whole else
                       content_digest({'partial': anchor.path, 'visible': digest_bytes(passage.encode())}))
        visible_anchors[anchor.id] = replace(
            anchor, selector=selector, current_digest=current,
            physical_paths=tuple(path for path in anchor.physical_paths if path in bodies),
            excerpt=anchor.excerpt if anchor.excerpt in passage else '',
            integrity='valid' if current == anchor.digest else 'changed')

    # Canonical adapters bind rows rather than whole manifests. Recognize only
    # visible row encodings (including canonical Point/Check defaults), never a
    # pristine payload merely because its origin pathname is visible.
    hashes = {path: {file_digests[path]} for path in whole}
    documents = {}
    for path in whole:
        try:
            document = json.loads(raw_files[path]) if path.endswith('.json') else tomllib.loads(raw_files[path].decode()) if path.endswith('.toml') else None
        except (ValueError, UnicodeError):
            document = None
        if not isinstance(document, Mapping):
            continue
        documents[path] = document
        hashes[path].add(content_digest(document))
        for collection in ('decisions', 'points', 'checks', 'families'):
            rows = document.get(collection, ())
            if not isinstance(rows, list):
                continue
            for row in rows:
                if not isinstance(row, Mapping):
                    continue
                hashes[path].add(content_digest(row))
                if collection == 'points':
                    hashes[path].add(content_digest({key: row.get(key, []) for key in ('id', 'title', 'contract', 'paths', 'checks', 'depends_on')}))
                elif collection == 'checks':
                    defaults = {'platforms': ['darwin', 'linux', 'windows'], 'requires': [], 'requires_files': [], 'preflight': None, 'missing': 'fail', 'timeout_seconds': 900, 'skip_if_env': [], 'expect_at_least': None}
                    hashes[path].add(content_digest({**defaults, **row}))
    for anchor in visible_anchors.values():
        if anchor.integrity == 'valid':
            hashes.setdefault(anchor.owner_module, set()).add(anchor.current_digest)
            hashes.setdefault(anchor.path, set()).add(anchor.current_digest)

    original_nodes = {node.id: node for node in graph.nodes}
    supported = set()
    def unchanged(provenance):
        origin = provenance.origin
        if origin in visible_anchors:
            anchor = visible_anchors[origin]
            return anchor.integrity == 'valid' and provenance.source_digest == anchor.digest
        if origin.startswith('plurx:'):
            source = original_nodes.get(origin)
            return (origin in supported and source is not None
                    and provenance.source_digest in {source.provenance.source_digest, content_digest(source)})
        return provenance.source_digest in hashes.get(origin, ())

    def governing_anchors(node):
        identities = {identity for identity in node.anchors
                      if any(a.id == identity and a.kind == 'section' for a in graph.anchors)}
        identities.update(node.attributes[key] for key in ('authoritative_section', 'source_section')
                          if isinstance(node.attributes.get(key), str))
        return identities

    # Semantic support is computed separately from structural visibility. A
    # changed implementation cannot retract a separately unchanged policy.
    while True:
        retained = {node.id for node in graph.nodes if unchanged(node.provenance)
                    and all(identity in visible_anchors and visible_anchors[identity].integrity == 'valid'
                            for identity in governing_anchors(node))}
        if retained <= supported:
            break
        supported.update(retained)

    roles = {}
    for path, document in documents.items():
        if path.endswith('/knowledge/pilot.json') and isinstance(document.get('document_roles'), Mapping):
            roles.update(document['document_roles'])
    projected = {}
    for node in graph.nodes:
        if node.kind in {'section', 'symbol'}:
            linked = [visible_anchors[identity] for identity in node.anchors if identity in visible_anchors]
            if not linked:
                continue
            anchor = linked[0]
            attributes = {'path': anchor.path, 'owner_module': anchor.owner_module}
            if node.kind == 'section':
                attributes.update(integrity=anchor.integrity, applicability='reviewed' if anchor.integrity == 'valid' else 'needs-review')
                title = ' / '.join(anchor.selector)
            else:
                attributes.update(selector=anchor.selector, physical_paths=anchor.physical_paths, line=anchor.line)
                title = str(anchor.selector)
            provenance = Provenance('parsed', '', '', anchor.path, anchor.current_digest, node.provenance.revision)
            projected[node.id] = replace(node, title=title, scope=anchor.physical_paths,
                                         anchors=tuple(a.id for a in linked), attributes=attributes, provenance=provenance)
        elif node.kind == 'artifact':
            path = node.attributes.get('path')
            if path not in bodies:
                continue
            attributes = {'path': path, 'role': roles.get(path, 'not-recorded'), 'digest': file_digests[path],
                          'visibility': 'whole' if path in whole else 'partial'}
            provenance = Provenance('parsed', '', '', path, file_digests[path], node.provenance.revision)
            projected[node.id] = replace(node, title=path, scope=(path,), anchors=(), attributes=attributes, provenance=provenance)
        elif node.kind == 'source-snapshot':
            selected = {path: file_digests[path] for path in bodies
                        if any(fnmatch.fnmatchcase(path, scope) for scope in node.scope)}
            if not selected:
                continue
            origin = sorted(selected)[0]
            attributes = {'visible_inputs': selected, 'digest': content_digest(selected),
                          'scope_completeness': 'visible-only'}
            provenance = Provenance('parsed', '', '', origin, file_digests[origin], node.provenance.revision)
            projected[node.id] = replace(node, title='Visible source snapshot', scope=tuple(sorted(selected)),
                                         anchors=(), attributes=attributes, provenance=provenance)
        elif node.id in supported:
            # An authored record's structural implementation anchors do not
            # independently support its prose. Keep its visible governing ones.
            identities = governing_anchors(node)
            linked = tuple(identity for identity in node.anchors if identity in visible_anchors
                           and (identity in identities or visible_anchors[identity].integrity == 'valid'))
            attributes = dict(node.attributes)
            if node.provenance.method != 'reviewed':
                descriptor = {'visible_origin': node.provenance.origin,
                              'origin_digest': content_digest(sorted(hashes.get(node.provenance.origin, ()))),
                              'visible_inputs': {path: file_digests[path] for path in bodies
                                                 if any(fnmatch.fnmatchcase(path, scope) for scope in node.scope)},
                              'completeness': 'visible-only'}
                for key in ('digest', 'row_digest', 'descriptor_digest'):
                    attributes.pop(key, None)
                if 'row' in attributes:
                    attributes['row_digest'] = content_digest(attributes['row'])
                attributes.update(descriptor=descriptor, descriptor_digest=content_digest(descriptor),
                                  descriptor_completeness='unknown')
            projected[node.id] = replace(node, anchors=linked, attributes=attributes)

    # Receipt metadata needs its own visible execution artifact, not policy
    # prose. Nothing here authenticates a reported execution.
    receipts = tuple(receipt for receipt in graph.receipts if receipt.id in projected and receipt.check_id in projected
                     and receipt.artifact_ref in whole and file_digests[receipt.artifact_ref] == receipt.artifact_digest)
    receipt_ids = {receipt.id for receipt in receipts}
    projected = {identity: node for identity, node in projected.items()
                 if node.kind != 'receipt' or identity in receipt_ids}
    nodes = tuple(projected[node.id] for node in graph.nodes if node.id in projected)
    edges = tuple(edge for edge in graph.edges if edge.source in projected and edge.target in projected and unchanged(edge.provenance))
    claims = tuple(claim for claim in graph.claims if claim.subject in projected and unchanged(claim.provenance))
    coverage = tuple(replace(item, scope=tuple(path for path in bodies if any(fnmatch.fnmatchcase(path, scope) for scope in item.scope)),
                             boundaries=(*item.boundaries, 'Only sealed visible passages are available in this evaluation.'))
                     for item in graph.coverage if unchanged(item.provenance)
                     and any(fnmatch.fnmatchcase(path, scope) for path in bodies for scope in item.scope))
    conflicts = tuple(item for item in graph.conflicts if set(item.subjects) <= projected.keys() and set(item.subjects) <= supported)
    digest = hashlib.sha256(canonical_json({'files': files, 'visibility': visibility}).encode()).hexdigest()
    # Visible anchor metadata is safe independently of whether pristine prose
    # could be retained; this keeps changed structure available for inspection.
    return QueryProjection(nodes, edges, claims, tuple(visible_anchors.values()), coverage, receipts, conflicts,
                           digest, len(graph.nodes) - len(nodes))
