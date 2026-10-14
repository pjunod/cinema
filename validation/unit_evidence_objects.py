"""Expand bounded embedded evidence objects without changing their authority."""
from __future__ import annotations
import base64
import hashlib
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
        self.cache[key] = (dict(blob), value)
        return value
