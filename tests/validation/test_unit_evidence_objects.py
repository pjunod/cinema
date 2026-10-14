"""Refusal controls for bounded embedded qualification objects."""
import base64
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
