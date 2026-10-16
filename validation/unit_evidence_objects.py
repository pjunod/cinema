"""Expand bounded embedded evidence objects without changing their authority."""
from __future__ import annotations
import base64
import hashlib
import io
import json
import re
import zlib
from validation import python_unit_receipts as receipts

STRUCTURAL_BYTES = 32 * 1024 * 1024
STRUCTURAL_NODES = 524288
OBJECT_NODES = 131072
OBJECT_DEPTH = 64
CI_LOG_BYTES = 64 * 1024 * 1024
DIGEST = re.compile(r'[0-9a-f]{64}')


class LiveLogs:
    """One bounded raw-log cache shared by both CI authentication paths."""
    def __init__(self):
        self.cache = {}
        self.bytes = 0

    def get(self, api, run_id, job_id):
        key = (run_id, job_id)
        if key not in self.cache:
            raw = api.bytes(f'/actions/jobs/{job_id}/logs')
            receipts.require(isinstance(raw, bytes) and self.bytes + len(raw) <= CI_LOG_BYTES,
                             'CI log cache byte budget exhausted')
            self.bytes += len(raw)
            self.cache[key] = raw
        return self.cache[key]


class StructuralBudget:
    """One unchanged traversal budget across independently signed proof tables."""
    def __init__(self):
        self.bytes = 0
        self.nodes = 0


class Objects:
    """One proof-local cache; record logs retain their separate decoding rules."""
    def __init__(self, logs, budget=None):
        receipts.require(isinstance(logs, dict) and 0 < len(logs) <= 1024,
                         'Missing or oversized structural evidence table')
        self.logs = logs
        self.cache = {}
        self.applicability_cache = {}
        self.dictionary_cache = set()
        self.budget = budget if budget is not None else StructuralBudget()

    @property
    def bytes(self):
        return self.budget.bytes

    @bytes.setter
    def bytes(self, value):
        self.budget.bytes = value

    @property
    def nodes(self):
        return self.budget.nodes

    @nodes.setter
    def nodes(self, value):
        self.budget.nodes = value

    def resolve(self, item, enabled):
        receipts.require(isinstance(item, dict), 'Evidence slot is not an object')
        if 'sha256_ref' not in item:
            return item
        receipts.require(enabled and set(item) == {'sha256_ref'},
                         'Unknown structural evidence reference')
        key = item['sha256_ref']
        receipts.require(isinstance(key, str) and DIGEST.fullmatch(key)
                         and key in self.logs and isinstance(self.logs[key], dict)
                         and self.logs[key].get('sha256') == key,
                         'Structural evidence unavailable or aliased')
        if key in self.cache:
            original, value = self.cache[key]
            receipts.require(self.logs[key] == original, 'Structural evidence changed after decoding')
            return value
        blob = self.logs[key]
        receipts.require(set(blob) in ({'base64', 'sha256'}, {'gzip_base64', 'sha256'}),
                         'Unknown structural evidence encoding')
        remaining = STRUCTURAL_BYTES - self.bytes
        receipts.require(remaining > 0, 'Structural evidence byte budget exhausted')
        limit = min(receipts.MAX_BYTES, remaining)
        encoded = blob.get('gzip_base64', blob.get('base64'))
        receipts.require(isinstance(encoded, str) and 0 < len(encoded) <= receipts.MAX_BYTES,
                         'Malformed or oversized structural evidence encoding')
        try:
            packed = base64.b64decode(encoded, validate=True)
            if 'gzip_base64' in blob:
                decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
                raw = decoder.decompress(packed, limit + 1)
                receipts.require(decoder.eof and not decoder.unused_data
                                 and not decoder.unconsumed_tail,
                                 'Structural evidence compressed frame incomplete or oversized')
            else:
                raw = packed
        except (ValueError, zlib.error) as error:
            raise receipts.ReceiptError('Malformed structural evidence encoding') from error
        receipts.require(0 < len(raw) <= limit and hashlib.sha256(raw).hexdigest() == key,
                         'Structural evidence byte budget or digest mismatch')
        self.bytes += len(raw)  # Charge before parsing, never refund or recurse.
        try:
            value = receipts.bounded_json(raw)
        except (ValueError, RecursionError) as error:
            raise receipts.ReceiptError('Malformed structural evidence JSON') from error
        receipts.require(isinstance(value, dict) and 'sha256_ref' not in value,
                         'Structural reference chaining is forbidden')
        self.traverse(value)
        self.cache[key] = (dict(blob), value)
        return value

    def traverse(self, value):
        """Charge logical visits too, including repeated dictionary values."""
        stack = [(iter((value,)), 0)]
        nodes = 0
        while stack:
            children, depth = stack[-1]
            try:
                node = next(children)
            except StopIteration:
                stack.pop()
                continue
            nodes += 1
            self.nodes += 1
            receipts.require(nodes <= OBJECT_NODES and self.nodes <= STRUCTURAL_NODES
                             and depth <= OBJECT_DEPTH, 'Structural evidence traversal limit')
            if isinstance(node, dict):
                stack.append((iter(node.values()), depth + 1))
            elif isinstance(node, list):
                stack.append((iter(node), depth + 1))

    def charge_expansion(self, value):
        # Visit before serializing: a small index stream can repeat a large
        # ID list. Never materialize an unbounded expanded JSON string.
        self.traverse(value)
        size = 0
        for part in json.JSONEncoder(separators=(',', ':'), sort_keys=True).iterencode(value):
            amount = len(part.encode())
            size += amount
            self.bytes += amount
            receipts.require(size <= receipts.MAX_BYTES and self.bytes <= STRUCTURAL_BYTES,
                             'Expanded applicability byte budget exhausted')

    def dictionary(self, reference):
        receipts.require(isinstance(reference, dict) and set(reference) == {'sha256_ref'},
                         'Applicability dictionary must be child-local')
        value = self.resolve(reference, True)
        key = reference['sha256_ref']
        if key in self.dictionary_cache:
            return value
        fields = {'paths', 'hashes', 'affected_ids', 'reasons'}
        receipts.require(set(value) == fields | {'format'}
                         and value['format'] == 'source-applicability-dictionary-v1'
                         and all(isinstance(value[field], list) for field in fields),
                         'Unknown applicability dictionary')
        paths, hashes, identities, reasons = (value[field] for field in
                                              ('paths', 'hashes', 'affected_ids', 'reasons'))
        receipts.require(0 < len(paths) <= receipts.MAX_TESTS
                         and all(isinstance(path, str) and 0 < len(path) <= 4096 for path in paths)
                         and len(paths) == len(set(paths)), 'Invalid applicability dictionary paths')
        receipts.require(hashes and all(item is None or isinstance(item, str)
                         and DIGEST.fullmatch(item) for item in hashes)
                         and len(hashes) == len(set(hashes)), 'Invalid applicability dictionary hashes')
        receipts.require(identities and all(isinstance(items, list)
                         and all(isinstance(item, str) and 0 < len(item) <= 1024 for item in items)
                         and len(items) == len(set(items)) for items in identities)
                         and len(identities) == len({tuple(items) for items in identities}),
                         'Invalid applicability dictionary identities')
        receipts.require(all(isinstance(reason, str) and reason.strip() for reason in reasons)
                         and len(reasons) == len(set(reasons)), 'Invalid applicability dictionary reasons')
        self.dictionary_cache.add(key)
        return value

    def applicability(self, reference, source, candidate):
        """Expand only a source-bound map; existing source checks consume it."""
        receipts.sha(source)
        receipts.sha(candidate)
        value = self.resolve(reference, True)
        receipts.require(value.get('source_commit') == source and value.get('candidate') == candidate,
                         'Shared applicability manifest source or candidate differs')
        if set(value) == {'source_commit', 'candidate', 'changes'}:
            receipts.require(isinstance(value['changes'], dict), 'Invalid applicability changes')
            return value['changes']
        receipts.require(set(value) == {'format', 'source_commit', 'candidate', 'dictionary', 'rows'}
                         and value['format'] == 'source-applicability-columns-v1',
                         'Unknown applicability manifest')
        dictionary = self.dictionary(value['dictionary'])
        key = reference['sha256_ref']
        if key in self.applicability_cache:
            return self.applicability_cache[key]
        rows = value['rows']
        receipts.require(isinstance(rows, str) and len(rows) <= receipts.MAX_BYTES,
                         'Invalid applicability row stream')
        changes = {}
        projected_nodes = 4  # Manifest, two source strings and the changes map.
        for ordinal, line in enumerate(io.StringIO(rows)):
            receipts.require(ordinal < len(dictionary['paths']), 'Oversized applicability row stream')
            receipts.require(re.fullmatch(r'(?:0|[1-9][0-9]{0,5})(?:,(?:0|[1-9][0-9]{0,5})){3,4}\n', line),
                             'Noncanonical applicability row')
            indices = [int(item) for item in line[:-1].split(',')]
            fields = ['paths', 'hashes', 'hashes', 'affected_ids', 'reasons'][:len(indices)]
            receipts.require(all(index < len(dictionary[field]) for index, field in zip(indices, fields)),
                             'Applicability dictionary index unavailable')
            path, before, after, identities, *reason = [dictionary[field][index]
                for index, field in zip(indices, fields)]
            receipts.require(path not in changes, 'Repeated applicability path')
            # Include every repeated ID visit before constructing its row.
            projected_nodes += 4 + len(identities) + bool(reason)
            receipts.require(projected_nodes <= OBJECT_NODES
                             and self.nodes + projected_nodes <= STRUCTURAL_NODES,
                             'Expanded applicability traversal limit')
            changes[path] = {'before': before, 'after': after, 'affected_ids': identities}
            if reason:
                changes[path]['reason'] = reason[0]
        self.charge_expansion({'source_commit': source, 'candidate': candidate, 'changes': changes})
        self.applicability_cache[key] = changes
        return changes

    def expand_records(self, item, candidate):
        """Copy structural records so signed inputs and cached slots stay intact."""
        records = item.get('records')
        if not isinstance(records, list):
            return item
        expanded = []
        for record in records:
            receipts.require(isinstance(record, dict), 'Evidence record is not an object')
            changes = record.get('changes')
            if isinstance(changes, dict) and 'sha256_ref' in changes:
                changes = self.applicability(changes, record.get('source_commit'), candidate)
                record = {**record, 'changes': changes}
            expanded.append(record)
        return {**item, 'records': expanded}
