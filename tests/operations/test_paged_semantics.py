"""Disk adapter regressions for demand-before-outcome and raw arithmetic."""
from __future__ import annotations

import copy
from pathlib import Path
import sqlite3
import sys
import unittest
from types import SimpleNamespace

PACKAGE = Path(__file__).resolve().parents[2] / 'verification' / 'async-driver-refinement'
sys.path.insert(0, str(PACKAGE))
import checker
import paged_checker


def state():
    return {'now': 0, 'jobs': [{'state': 'queued', 'owner': None, 'expiry': None, 'request_done': False, 'not_before_ms': 0} for _ in range(2)],
        'captured_tokens': [[], []], 'captured_leases': [], 'domain': None,
        'budgets': {'claims': [0, 0], 'renewed_claims': [], 'domain_acquired': [False] * 3, 'removed': [False] * 2}}


def product():
    return {'id': 1, 'context': 'stable', 'sql': 0, 'w': [copy.deepcopy(checker.IDLE), copy.deepcopy(checker.IDLE)],
        'available': [True, True], 'completed': [], 'mono_now_ms': 0, 'wall_anchors': [], 'mono_anchors': [], 'claim_loss_keys': []}


class PagedSemanticFixtures(unittest.TestCase):
    def test_missing_cross_page_capacity_outcome_never_makes_premise_false(self):
        p, s = product(), state()
        s['captured_tokens'][0] = [dict(job_id='J0', node_id='node-a', boot_id='B', claim_id='C', fence=1, revision=1, lease_expires_ms=10)]
        p['w'][0].update(stage='driver', call='sent', job=0, token=0, ctrl=0, retire='first', original=0, sample=0, inner='ready')
        driver = {'states': [{'pending': 'RetireQueue'}]}
        with sqlite3.connect(':memory:') as db:
            db.execute('CREATE TABLE v_oracle(id INTEGER PRIMARY KEY,src INTEGER,action TEXT)')
            oracle = paged_checker.Oracle(SimpleNamespace(db=db))
            demands = checker.expected_product_demands(p, driver, {'states': [s]})
            self.assertEqual([d['role'] for d in demands], ['capacity'])
            with self.assertRaisesRegex(checker.Invalid, 'MissingProductOracleEdge'):
                checker.complete_product_demands(p, driver, {'states': [s], 'edges': []}, oracle)

    def test_disabled_candidate_arithmetic_is_checked_globally(self):
        p, s = product(), state()
        p['wall_anchors'] = [checker.TLC_INT_MAX]
        with self.assertRaisesRegex(checker.Invalid, 'UnsupportedTlcNumericDomain'):
            checker.certify_tlc_arithmetic(p, {'states': [s], 'edges': []})
        p['wall_anchors'] = []
        p['w'][0]['adapter_reply'] = {'now_ms': checker.TLC_INT_MAX, 'retry_delay_ms': 1}
        with self.assertRaisesRegex(checker.Invalid, 'UnsupportedTlcNumericDomain'):
            checker.certify_tlc_arithmetic(p, {'states': [s], 'edges': []})

    def test_huge_global_transport_handles_preserve_raw_numeric_checks(self):
        huge=checker.TLC_INT_MAX+100
        p=product()
        p.update(id=huge,sql=huge)
        paged_checker.certify_semantic_numbers(p,'Product')
        import tempfile
        import test_paged_temporal as fixture
        base=PACKAGE.parents[1]/'.async-proof-sessions'/'paged-fixtures'
        base.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=base) as directory:
            naming=paged_checker.LocalIds([1,huge],fixture.admitted(directory))
            self.assertEqual(dict(naming),{1:0,huge:1})
            self.assertEqual(len(set(naming.values())),len(naming))
            checker.certify_tlc_numbers(list(naming.values()))
        with self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain'):
            checker.certify_tlc_numbers(p)  # The legacy default is unchanged.
        p['w'][0]['claim_request_now_ms']=huge
        with self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain'):
            paged_checker.certify_semantic_numbers(p,'Product')
        for field in ('fence','revision','lease_expires_ms'):
            s=state()
            token={'job_id':'raw','node_id':'node-a','boot_id':'boot-a','claim_id':'raw-claim',
                   'fence':1,'revision':1,'lease_expires_ms':10}
            token[field]=huge
            s['captured_tokens'][0]=[token]
            with self.subTest(raw_operand=field),self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain'):
                paged_checker.certify_semantic_numbers(s,'Store')

    def test_pair_lookup_compares_full_bytes_even_under_forced_digest_collision(self):
        from unittest.mock import patch
        with sqlite3.connect(':memory:') as db:
            db.execute('CREATE TABLE v_products(id INTEGER,digest TEXT,canonical TEXT)')
            db.executemany('INSERT INTO v_products VALUES(?,?,?)', [(1, 'same', 'first'), (2, 'same', 'second')])
            lookup = paged_checker.Payloads(SimpleNamespace(db=db))
            with patch.object(paged_checker, 'sha_bytes', return_value='same'):
                self.assertEqual(lookup.get('first'), 1)
                self.assertEqual(lookup.get('second'), 2)
                self.assertIsNone(lookup.get('absent'))



if __name__ == '__main__':
    unittest.main()
