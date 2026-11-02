"""Deterministic schema integrity plus advisory, bounded PR acknowledgment."""
from __future__ import annotations

import fnmatch
import os
from pathlib import Path
import re

from . import capture, index
from .model import KnowledgeError, identifier

FIELD = re.compile(r'^Knowledge-(Obligation|Decision|Impact):[ \t]*(.*)$')


def inspect_body(graph, body: str | None, paths=()) -> dict:
    nodes = {n.id: n for n in graph.nodes}
    matched = sorted(n.id for n in graph.nodes if n.kind == 'obligation'
                     and n.attributes.get('disposition') in {'open', 'implemented-awaiting-evidence'}
                     and any(fnmatch.fnmatchcase(p, scope) for p in paths for scope in n.scope))
    result = {'state': 'unchecked' if body is None else 'advisory', 'errors': [], 'warnings': [],
              'acknowledged': [], 'matched_open_obligations': matched,
              'acknowledgment_proves_satisfaction': False}
    if body is None:
        result['warnings'].append('PR capture unchecked: no canonical body supplied.')
        return result
    if not isinstance(body, str) or len(body.encode('utf-8')) > 1024 * 1024:
        raise KnowledgeError('PR body must be bounded UTF-8 text')
    supplied = False
    for line in body.splitlines():
        field = FIELD.fullmatch(line)
        if not field:
            continue
        supplied = True
        kind, value = field.groups()
        value = value.strip()
        if kind == 'Impact':
            prefix, separator, reason = value.partition(';')
            if prefix.strip() != 'reviewed-no-change' or not separator or not reason.strip() or reason.strip() in {'<reason>', 'TODO'}:
                result['errors'].append('Knowledge-Impact requires reviewed-no-change; and a concrete reason.')
            else:
                result['acknowledged'].append({'kind': 'impact', 'reason': reason.strip()})
            continue
        try:
            value = identifier(value, kind.lower())
        except KnowledgeError:
            result['errors'].append('Invalid supplied Knowledge-' + kind + ' ID.')
            continue
        if value not in nodes or nodes[value].kind != kind.lower():
            result['errors'].append('Unknown supplied Knowledge-' + kind + ' ID: ' + value)
        else:
            result['acknowledged'].append({'kind': kind.lower(), 'id': value})
    if matched and not supplied:
        result['warnings'].append('Curated scope changed without Knowledge acknowledgment; advisory during observation window.')
    if matched:
        result['warnings'].append('Acknowledgment and no-change reasons do not erase matched obligations or qualify execution evidence.')
    return result


def run(root: Path, *, body: str | None = None, base: str | None = None, staged=False) -> dict:
    if base is None and body is not None:
        base = os.environ.get("KNOWLEDGE_BASE") or None
    indexed = index.load(root)
    paths = capture.changed_paths(root, indexed, base=base, staged=staged,
                                  worktree=not staged) if base is not None or staged or body is not None else ()
    capture_result = inspect_body(indexed.graph, body, paths)
    return {'status': 'error' if capture_result['errors'] else 'ok',
            'snapshot': indexed.snapshot.metadata(), 'errors': capture_result['errors'],
            'warnings': capture_result['warnings'], 'capture': capture_result,
            'changed_path_count': len(paths), 'mode': 'advisory',
            'schema_and_references': 'valid', 'observation_window': 'not-started-before-main-landing'}
