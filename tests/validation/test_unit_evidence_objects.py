"""Refusal controls for bounded embedded qualification objects."""
import base64
import copy
import gzip
import hashlib
import json
import unittest
from unittest.mock import patch
from validation import python_unit_receipts as receipts
from validation import unit_evidence_objects as objects


def evidence(raw):
    key = hashlib.sha256(raw).hexdigest()
    return key, {'sha256': key, 'gzip_base64': base64.b64encode(gzip.compress(raw, mtime=0)).decode()}


def columns(rows='0,0,1,0,0\n', dictionary=None):
    dictionary = dictionary or {'format': 'source-applicability-dictionary-v1',
        'paths': ['input.rs'], 'hashes': ['d' * 64, 'e' * 64],
        'affected_ids': [['a']], 'reasons': ['reviewed changed function']}
    key, blob = evidence(json.dumps(dictionary).encode())
    manifest = {'format': 'source-applicability-columns-v1', 'source_commit': 'a' * 40,
                'candidate': 'b' * 40, 'dictionary': {'sha256_ref': key}, 'rows': rows}
    digest, capture = evidence(json.dumps(manifest).encode())
    return manifest, {key: blob, digest: capture}, {'sha256_ref': digest}


class EmbeddedObjectsCase(unittest.TestCase):
    def test_references_preserve_exact_json_digest_and_refuse_aliases_or_mutation(self):
        raw = b'{"name":"core","ids":["a"]}'
        key, blob = evidence(raw)
        decoder = objects.Objects({key: blob})
        self.assertEqual(decoder.resolve({'sha256_ref': key}, True), json.loads(raw))
        for item, enabled in [({'sha256_ref': key}, False), ({'sha256_ref': key, 'other': 1}, True),
                              ({'sha256_ref': '0' * 64}, True), ({'sha256_ref': 1}, True)]:
            with self.subTest(item=item), self.assertRaises(receipts.ReceiptError):
                decoder.resolve(item, enabled)
        decoder.logs[key] = {**blob, 'sha256': '0' * 64}
        with self.assertRaises(receipts.ReceiptError):
            decoder.resolve({'sha256_ref': key}, True)
        decoder.logs[key] = {**blob, 'gzip_base64': evidence(b'{"name":"changed"}')[1]['gzip_base64']}
        with self.assertRaises(receipts.ReceiptError):
            decoder.resolve({'sha256_ref': key}, True)

        manifest, logs, reference = columns()
        decoder = objects.Objects(logs)
        expected = {'input.rs': {'before': 'd' * 64, 'after': 'e' * 64,
                    'affected_ids': ['a'], 'reason': 'reviewed changed function'}}
        self.assertEqual(decoder.applicability(reference, 'a' * 40, 'b' * 40), expected)
        for source, candidate in [('c' * 40, 'b' * 40), ('a' * 40, 'c' * 40), (None, 'b' * 40)]:
            with self.subTest(source=source), self.assertRaises(receipts.ReceiptError):
                decoder.applicability(reference, source, candidate)
        for rows in ['0,0,1,0', '00,0,1,0\n', '-1,0,1,0\n', '0,0,1,0\r\n',
                     '0,0,1,0,0,0\n', '0,0,2,0\n', '1,0,1,0\n', '0,0,1,0,1\n',
                     '0,0,1,0\n0,0,1,0\n', 'true,0,1,0\n']:
            _, table, slot = columns(rows)
            with self.subTest(rows=rows), self.assertRaises(receipts.ReceiptError):
                objects.Objects(table).applicability(slot, 'a' * 40, 'b' * 40)
        dictionary = decoder.dictionary(manifest['dictionary'])
        for field, value in [('paths', ['input.rs', 'input.rs']), ('hashes', [True]),
                             ('affected_ids', [['a', 'a']]), ('affected_ids', [['a'], ['a']]),
                             ('reasons', ['']), ('extra', 'unknown')]:
            invalid = {**dictionary, field: value}
            _, table, slot = columns(dictionary=invalid)
            with self.subTest(field=field, value=value), self.assertRaises(receipts.ReceiptError):
                objects.Objects(table).applicability(slot, 'a' * 40, 'b' * 40)
        table = copy.deepcopy(logs)
        table[manifest['dictionary']['sha256_ref']]['sha256'] = '0' * 64
        decoder.logs.update(table)
        with self.assertRaises(receipts.ReceiptError):
            decoder.applicability(reference, 'a' * 40, 'b' * 40)

    def test_cache_charges_unique_structural_bytes_once_and_budget_never_refunds(self):
        one, blob1 = evidence(b'{"name":"one"}')
        two, blob2 = evidence(b'{"name":"two","additional":"payload"}')
        decoder = objects.Objects({one: blob1, two: blob2})
        with patch.object(objects, 'STRUCTURAL_BYTES', len(b'{"name":"one"}') + 1):
            value = decoder.resolve({'sha256_ref': one}, True)
            charged = decoder.bytes
            self.assertIs(value, decoder.resolve({'sha256_ref': one}, True))
            self.assertEqual(decoder.bytes, charged)
            with self.assertRaises(receipts.ReceiptError):
                decoder.resolve({'sha256_ref': two}, True)
            self.assertEqual(decoder.bytes, charged)
            self.assertNotIn(two, decoder.cache)
        other = objects.Objects({one: blob1})
        self.assertEqual(other.bytes, 0)
        other.resolve({'sha256_ref': one}, True)
        self.assertEqual(other.bytes, charged)

        manifest, table, reference = columns()
        budget = objects.StructuralBudget()
        decoder = objects.Objects(table, budget)
        expected = decoder.applicability(reference, 'a' * 40, 'b' * 40)
        measured = (budget.bytes, budget.nodes)
        wire_bytes = sum(len(gzip.decompress(base64.b64decode(blob['gzip_base64'])))
                         for blob in table.values())
        logical = {'source_commit': 'a' * 40, 'candidate': 'b' * 40, 'changes': expected}
        self.assertEqual(measured[0], wire_bytes + len(json.dumps(
            logical, separators=(',', ':'), sort_keys=True).encode()))
        self.assertEqual(decoder.applicability(reference, 'a' * 40, 'b' * 40), expected)
        self.assertEqual((budget.bytes, budget.nodes), measured)
        with patch.object(objects, 'STRUCTURAL_BYTES', budget.bytes + 1), \
                self.assertRaises(receipts.ReceiptError):
            objects.Objects(table, budget).applicability(reference, 'a' * 40, 'b' * 40)
        self.assertGreaterEqual(budget.bytes, measured[0])
        dictionary = {'format': 'source-applicability-dictionary-v1',
            'paths': ['one.rs', 'two.rs', 'three.rs'], 'hashes': [None],
            'affected_ids': [[str(index) for index in range(20)]], 'reasons': []}
        _, table, reference = columns('0,0,0,0\n1,0,0,0\n2,0,0,0\n', dictionary)
        decoder = objects.Objects(table)
        with patch.object(objects, 'OBJECT_NODES', 50), \
                patch.object(decoder, 'charge_expansion') as materialized, \
                self.assertRaises(receipts.ReceiptError):
            try:
                decoder.applicability(reference, 'a' * 40, 'b' * 40)
            finally:
                materialized.assert_not_called()
                self.assertEqual(decoder.applicability_cache, {})
        wire_bytes = sum(len(gzip.decompress(base64.b64decode(blob['gzip_base64'])))
                         for blob in table.values())
        for field, limit in [('STRUCTURAL_BYTES', wire_bytes + 1), ('STRUCTURAL_NODES', 50)]:
            decoder = objects.Objects(table)
            with patch.object(objects, field, limit), self.assertRaises(receipts.ReceiptError):
                decoder.applicability(reference, 'a' * 40, 'b' * 40)

    def test_chaining_duplicate_keys_nonobjects_and_traversal_bombs_refuse(self):
        deep = {}
        node = deep
        for _ in range(65):
            node['child'] = {}
            node = node['child']
        invalid = [b'{"sha256_ref":"' + b'0' * 64 + b'"}', b'{"name":"a","name":"b"}',
                   b'[]', json.dumps(deep).encode(), json.dumps({'nodes': [0] * 131072}).encode()]
        for raw in invalid:
            key, blob = evidence(raw)
            decoder = objects.Objects({key: blob})
            with self.subTest(length=len(raw)), self.assertRaises(receipts.ReceiptError):
                decoder.resolve({'sha256_ref': key}, True)
            self.assertEqual(decoder.bytes, len(raw))
            self.assertNotIn(key, decoder.cache)

    def test_object_limit_encoding_and_cumulative_node_budget_remain_bounded(self):
        key, blob = evidence(json.dumps({'nodes': [0] * 8}).encode())
        class CountingList(list):
            consumed = 0
            def __iter__(self):
                for value in super().__iter__():
                    self.consumed += 1
                    yield value
        children = CountingList([0] * 10000)
        with patch.object(receipts, 'bounded_json', return_value={'nodes': children}), \
                patch.object(objects, 'OBJECT_NODES', 5), self.assertRaises(receipts.ReceiptError):
            objects.Objects({key: blob}).resolve({'sha256_ref': key}, True)
        self.assertEqual(children.consumed, 4)
        with patch.object(objects, 'STRUCTURAL_NODES', 5), self.assertRaises(receipts.ReceiptError):
            objects.Objects({key: blob}).resolve({'sha256_ref': key}, True)
        malformed = [{**blob, 'gzip_base64': 1}, {**blob, 'gzip_base64': '!'},
                     {**blob, 'other': 'unsupported'},
                     {**blob, 'gzip_base64': base64.b64encode(gzip.compress(b'{}') + b'trailing').decode()}]
        for value in malformed:
            with self.subTest(value=value), self.assertRaises(receipts.ReceiptError):
                objects.Objects({key: value}).resolve({'sha256_ref': key}, True)
        oversized = {**blob, 'base64': base64.b64encode(b'x' * (receipts.MAX_BYTES + 1)).decode()}
        oversized.pop('gzip_base64')
        with self.assertRaises(receipts.ReceiptError):
            objects.Objects({key: oversized}).resolve({'sha256_ref': key}, True)

    def test_ci_logs_share_budget_and_preserve_cached_raw_bytes(self):
        class API:
            calls = 0
            def bytes(self, path):
                self.calls += 1
                return b'raw'
        api = API()
        cache = objects.LiveLogs()
        with patch.object(objects, 'CI_LOG_BYTES', 5):
            self.assertEqual(cache.get(api, 1, 2), b'raw')
            self.assertEqual(cache.get(api, 1, 2), b'raw')
            self.assertEqual(api.calls, 1)
            with self.assertRaises(receipts.ReceiptError):
                cache.get(api, 1, 3)
            self.assertEqual(cache.bytes, 3)
            self.assertNotIn((1, 3), cache.cache)
