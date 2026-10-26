"""Bounded, deterministic graph answers. Prose and selectors remain inert data."""
from __future__ import annotations

import base64
from collections import defaultdict, deque
from collections.abc import Mapping
import fnmatch
import hashlib
import json
import re
from typing import Any

from .model import GraphData, KnowledgeError, canonical_json, effective_lifecycle, identifier, safe_relative

MAX_DEPTH = 8
MAX_RESPONSE = 1024 * 1024
PRIORITY = {'obligation': 0, 'finding': 1, 'contract': 2, 'receipt': 3,
            'check': 4, 'decision': 5, 'route': 6, 'point': 7,
            'symbol': 8, 'section': 9, 'artifact': 10, 'source-snapshot': 11}
ATTRIBUTES = {
    'contract': ('behavior', 'authoritative_section', 'check_ids', 'status'),
    'obligation': ('action', 'authority', 'disposition', 'responsible_role', 'resolution',
                   'remaining_evidence', 'waiver_reason', 'waiver_owner', 'waiver_scope'),
    'finding': ('review', 'finding_id', 'severity', 'disposition', 'resolution', 'remaining_evidence'),
    'decision': ('rationale', 'status', 'source_section'),
    'check': ('check_kind', 'selector', 'reference', 'execution', 'descriptor_digest', 'descriptor_completeness'),
    'receipt': ('execution_id', 'check_id', 'source_revision', 'source_tree', 'environment',
                'result', 'origin', 'descriptor_digest', 'artifact_ref', 'artifact_digest'),
    'route': ('canonical_id', 'manifest', 'row_digest', 'descriptor_digest', 'descriptor_completeness'),
    'point': ('canonical_id', 'contract', 'checks', 'depends_on', 'descriptor_digest', 'descriptor_completeness'),
    'artifact': ('path', 'role', 'digest'),
    'symbol': ('path', 'owner_module', 'selector', 'physical_paths', 'line'),
    'section': ('path', 'owner_module', 'integrity', 'applicability'),
}


def plain(value: Any) -> Any:
    return json.loads(canonical_json(value))


def encoded(value: Any) -> bytes:
    return canonical_json(value).encode('utf-8')


def _matches(path: str, scope: str) -> bool:
    return path == scope or fnmatch.fnmatchcase(path, scope)


def _scope_paths(node) -> tuple[str, ...]:
    return tuple(node.scope)


def _direct(graph: GraphData, paths: tuple[str, ...], symbol: str | None):
    anchors = {a.id: a for a in graph.anchors}
    symbol_ids = set()
    if symbol:
        if not isinstance(symbol, str) or not symbol.strip() or len(symbol) > 256:
            raise KnowledgeError('invalid symbol query')
        pattern = re.compile(r'(?<![A-Za-z0-9_])' + re.escape(symbol) + r'(?![A-Za-z0-9_])')
        symbol_ids = {a.id for a in graph.anchors if a.kind == 'symbol'
                      and any(p in a.physical_paths or p == a.owner_module for p in paths)
                      and pattern.search(str(a.selector))}
    starts = set()
    for node in graph.nodes:
        matched = any(_matches(path, scope) for path in paths for scope in _scope_paths(node))
        matched = matched or any(a in anchors and any(p in anchors[a].physical_paths for p in paths)
                                 for a in node.anchors)
        if matched and (not symbol or bool(set(node.anchors) & symbol_ids)):
            starts.add(node.id)
    return starts, ('not-requested' if not symbol else 'resolved' if symbol_ids else 'unknown')


def _walk(graph: GraphData, starts: set[str], depth: int, lifecycle):
    """Bounded undirected discovery retains actual edge direction and authority."""
    adjacency = defaultdict(list)
    for edge in graph.edges:
        adjacency[edge.source].append((edge.target, edge, 'outgoing'))
        adjacency[edge.target].append((edge.source, edge, 'incoming'))
    paths = {key: [] for key in starts}
    governing = {key: lifecycle[key].state == "current" for key in starts}
    queue = deque((key, governing[key], []) for key in sorted(starts))
    visited = {(key, governing[key]) for key in starts}
    while queue:
        current, current_authority, prefix = queue.popleft()
        if len(prefix) >= depth:
            continue
        for target, edge, direction in sorted(adjacency[current], key=lambda v: (v[1].relation, v[0], v[2])):
            origin_state = lifecycle.get(edge.provenance.origin)
            authority = (current_authority and edge.relation != 'mentions'
                         and lifecycle[target].state == 'current'
                         and (origin_state is None or origin_state.state == 'current'))
            state = (target, authority)
            if state in visited:
                continue
            visited.add(state)
            step = {'source': edge.source, 'relation': edge.relation, 'target': edge.target,
                    'direction': direction, 'provenance': plain(edge.provenance)}
            candidate = prefix + [step]
            if target not in paths or (authority and not governing[target]):
                paths[target] = candidate
                governing[target] = authority
            queue.append((target, authority, candidate))
    return paths, governing


def _record(node, anchors, conflicts, governing, claims, lifecycle):
    linked = [anchors[a] for a in node.anchors if a in anchors]
    integrity = 'changed' if any(a.integrity != 'valid' for a in linked) else 'valid' if linked else 'unknown'
    conflicted = any(node.id in c.subjects and c.disposition == 'unresolved' for c in conflicts)
    applicability = ('conflicted' if conflicted or lifecycle.state == 'conflicted' else 'needs-review' if integrity == 'changed' or lifecycle.state == 'needs-review'
                     else 'reviewed' if node.provenance.method == 'reviewed' else 'unknown')
    attributes = {key: plain(node.attributes[key]) for key in ATTRIBUTES.get(node.kind, ()) if key in node.attributes}
    return {'id': node.id, 'kind': node.kind, 'title': node.title, 'scope': list(node.scope),
            'attributes': attributes, 'metadata_omitted_fields': len(node.attributes) - len(attributes),
            'governing_path': governing and lifecycle.state == 'current', 'anchor_integrity': integrity,
            'effective_lifecycle': plain(lifecycle),
            'claims': [plain(c) for c in claims if c.subject == node.id],
            'semantic_applicability': applicability, 'provenance': plain(node.provenance),
            'evidence': {'origin': attributes.get('origin', 'unavailable'),
                         'source_compatibility': 'unknown', 'execution': attributes.get('result', 'not-run'),
                         'authenticated_credit': False},
            'anchors': [{'id': a.id, 'path': a.path, 'owner_module': a.owner_module,
                         'physical_paths': list(a.physical_paths), 'selector': plain(a.selector),
                         'reviewed_digest': a.digest, 'current_digest': a.current_digest,
                         'integrity': a.integrity, 'line': a.line, 'end_line': a.end_line,
                         'excerpt': a.excerpt} for a in linked]}


def _coverage(graph, paths, reached):
    selected = []
    for coverage in graph.coverage:
        if any(_matches(path, scope) for path in paths for scope in coverage.scope):
            selected.append(plain(coverage))
    if not selected:
        return {'level': 'none', 'declarations': [], 'limit': 'No reviewed coverage declaration matches this query; an empty result proves no absence of obligations.'}
    levels = {c['level'] for c in selected}
    return {'level': next(iter(levels)) if len(levels) == 1 else 'overlapping',
            'declarations': selected,
            'limit': 'Only the combined stated boundaries are reviewed; unresolved overlaps are not resolved by selecting the broadest scope.'}


def _continuation(key: str, query_hash: str, offset: int) -> str:
    raw = encoded({'snapshot': key, 'query': query_hash, 'offset': offset})
    return base64.urlsafe_b64encode(raw).decode().rstrip('=')


def _offset(handle, key, query_hash, maximum):
    if handle is None:
        return 0
    if not isinstance(handle, str) or len(handle) > 512:
        raise KnowledgeError('invalid continuation')
    try:
        raw = json.loads(base64.b64decode(handle + '=' * (-len(handle) % 4), altchars=b'-_', validate=True))
    except (ValueError, UnicodeError) as error:
        raise KnowledgeError('invalid continuation') from error
    if (not isinstance(raw, dict) or set(raw) != {'snapshot', 'query', 'offset'}
            or raw['snapshot'] != key or raw['query'] != query_hash
            or type(raw['offset']) is not int or not 0 <= raw['offset'] <= maximum):
        raise KnowledgeError('continuation does not match this snapshot/query')
    return raw['offset']


def answer(graph: GraphData, snapshot: Mapping[str, Any], *, command: str,
           paths=(), symbol: str | None = None, identity: str | None = None,
           cohort: str | None = None, max_depth: int = 6, max_bytes: int = 8192,
           continuation: str | None = None, evidence_assessment: Mapping | None = None) -> dict:
    """B/C share all formatting, metadata and order; only traversal depth differs."""
    if command not in {'context', 'obligations', 'why', 'evidence', 'conflicts'}:
        raise KnowledgeError('unknown query command')
    if type(max_depth) is not int or not 0 <= max_depth <= MAX_DEPTH:
        raise KnowledgeError('max-depth must be between 0 and 8')
    if type(max_bytes) is not int or not 0 < max_bytes <= MAX_RESPONSE:
        raise KnowledgeError('max-bytes must be between 1 and 1048576')
    if any(not isinstance(snapshot.get(k), str) or not snapshot[k] for k in ('key', 'head', 'tree', 'worktree_id')):
        raise KnowledgeError('snapshot identity is required')
    paths = tuple(sorted({safe_relative(p) for p in paths}))
    nodes = {n.id: n for n in graph.nodes}
    symbol_resolution = 'not-requested'
    if identity is not None:
        identity = identifier(identity)
        if identity not in nodes:
            raise KnowledgeError('unknown graph identity')
        starts = {identity}
        if not paths:
            paths = tuple(p for p in nodes[identity].scope if not any(c in p for c in '*?[]'))
    else:
        starts, symbol_resolution = _direct(graph, paths, symbol)
    if command == 'conflicts':
        if cohort is not None and cohort not in {c.cohort for c in graph.coverage}:
            raise KnowledgeError('unknown cohort')
        permitted = {c.id for c in graph.coverage if not cohort or c.cohort == cohort}
        scope = tuple(p for c in graph.coverage if c.id in permitted for p in c.scope)
        starts = {n.id for n in graph.nodes if any(_matches(p, s) for p in n.scope for s in scope)}
        paths = scope
    lifecycle = effective_lifecycle(graph)
    walk, governing = _walk(graph, starts, max_depth, lifecycle)
    if command == 'obligations':
        walk = {k: v for k, v in walk.items() if nodes[k].kind in {'obligation', 'finding', 'decision', 'contract'} and governing[k]}
    relevant_conflicts = tuple(c for c in graph.conflicts if set(c.subjects) & set(walk))
    anchors = {a.id: a for a in graph.anchors}
    ordered = sorted(walk, key=lambda key: (lifecycle[key].state == 'historical',
                       PRIORITY.get(nodes[key].kind, 99), key))
    mandatory_snapshot = {k: plain(snapshot[k]) for k in ('key', 'head', 'tree', 'worktree_id', 'repository_dirty', 'input_count', 'knowledge_view', 'change_view') if k in snapshot}
    key = str(snapshot.get('key', 'unknown'))
    query = {'command': command, 'paths': paths, 'symbol': symbol, 'identity': identity,
             'cohort': cohort, 'max_depth': max_depth}
    query_hash = hashlib.sha256(encoded(query)).hexdigest()
    offset = _offset(continuation, key, query_hash, len(ordered))
    result = {'status': 'ok', 'snapshot': mandatory_snapshot, 'query': plain(query),
              'coverage': _coverage(graph, paths, walk), 'symbol_resolution': symbol_resolution,
              'included_scopes': list(paths),
              'excluded_scopes': ['Uncurated behavior outside declared boundaries.', 'Private CI artifacts, credentials and hardware evidence are not indexed.'],
              'conflicts': [plain(c) for c in relevant_conflicts],
              'records': [], 'relationship_paths': [], 'omitted_records': len(ordered) - offset,
              'continuation': None, 'empty_meaning': 'No known applicable records; never proof that no obligation exists.'}
    if evidence_assessment is not None:
        result['evidence_assessment'] = plain(evidence_assessment)
    # The mandatory envelope may not fit; return an explicit bounded refusal.
    reserve = _continuation(key, query_hash, offset)
    result['continuation'] = reserve if ordered[offset:] else None
    if len(encoded(result)) + 1 > max_bytes:
        refusal = {'status': 'budget-too-small', 'required_bytes_at_least': len(encoded(result)) + 1}
        if len(encoded(refusal)) + 1 > max_bytes:
            raise KnowledgeError('budget-too-small: minimum error envelope cannot fit')
        return refusal
    for index, node_id in enumerate(ordered[offset:], offset):
        item = _record(nodes[node_id], anchors, relevant_conflicts, governing[node_id], graph.claims, lifecycle[node_id])
        result['records'].append(item)
        if walk[node_id]:
            result['relationship_paths'].append({'to': node_id, 'steps': walk[node_id]})
        result['omitted_records'] = len(ordered) - index - 1
        result['continuation'] = _continuation(key, query_hash, index + 1) if result['omitted_records'] else None
        if len(encoded(result)) + 1 > max_bytes:
            result['records'].pop()
            if walk[node_id]:
                result['relationship_paths'].pop()
            result['omitted_records'] = len(ordered) - index
            result['continuation'] = _continuation(key, query_hash, index)
            result['status'] = 'partial'
            if not result['records']:
                return {'status': 'budget-too-small', 'required_bytes_at_least': max_bytes + 1}
            break
    if result['omitted_records']:
        result['status'] = 'partial'
    if len(encoded(result)) + 1 > max_bytes:
        raise KnowledgeError('budget-too-small: continuation metadata cannot fit')
    return result


def render(result: dict, *, format: str = 'json', max_bytes: int = 8192) -> str:
    """Text uses the identical information envelope; canonical JSON is authoritative."""
    if format not in {'json', 'text'}:
        raise KnowledgeError('format must be json or text')
    # Text deliberately retains the canonical envelope, avoiding an arm-specific summary.
    output = canonical_json(result) + '\n'
    if len(output.encode('utf-8')) > max_bytes:
        raise KnowledgeError('budget-too-small: rendered envelope exceeds byte limit')
    return output
