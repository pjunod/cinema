"""N2b: `validation_receipt` is a declared, closed transported record, not just a ContentRef kind.

The pinned transport schema must declare it with `properties.kind.const` (the specification validator
refuses an accounting kind no schema declares as a record), and the session must build and authenticate
exactly those bytes. These fixtures run the real schema and the real `check_activation_validations`.
"""
import json
import sqlite3
import unittest
from pathlib import Path

from tests.operations.test_async_refinement_provenance import SESSION, SOURCE, checkpoint
from validation import async_refinement_provenance as p
from validation import async_refinement_session as session
from validation import async_snapshot_transport as snapshots

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / p.TRANSPORT_SCHEMA


def declared_record_kinds():
    schema = json.loads(SCHEMA.read_text())
    return {name: definition['properties']['kind']['const'] for name, definition in schema['$defs'].items()
            if 'const' in definition.get('properties', {}).get('kind', {})}


def verifier_receipt():
    return {'schema_version': 1, 'operation': 'validate_restored_verifier', 'source': SOURCE, 'session_id': SESSION,
            'unit_ordinal': 0, 'checkpoint_sha256': 'a' * 64, 'resource_plan_sha256': 'c' * 64,
            'initial_image_sha256': 'b' * 64, 'verifier_schema_sha256': 'd' * 64, 'verifier_commit': 1,
            'verifier_logical_digest': 'e' * 64, 'work_db_sha256': 'f' * 64, 'work_db_bytes': 4096,
            'progress_sha256': '1' * 64, 'result_chain_root_sha256': '2' * 64, 'complete': True, 'acceptance': False}


def catalog_receipts(input_preparation=False):
    envelope = checkpoint()
    root = {'catalog_sha256': '3' * 64, 'catalog_bytes': 8192}
    private, semantic = session.expected_validations(envelope, root, input_preparation)
    return envelope, root, private, semantic


class ValidationReceiptSchemaTests(unittest.TestCase):
    def test_the_pinned_schema_declares_validation_receipt_as_a_record_kind(self):
        # Every accounting kind that is a transported record is declared by some record definition.
        declared = set(declared_record_kinds().values())
        self.assertIn('validation_receipt', declared)
        self.assertIn('validation_receipt', p.AUXILIARY_ACCOUNTING_KINDS)

    def test_every_real_validator_output_is_a_valid_record(self):
        for partial in (False, True):
            _, _, private, semantic = catalog_receipts(partial)
            for validator, raw in (('catalog_private', private), ('catalog_semantic', semantic)):
                with self.subTest(validator=validator, partial=partial):
                    record = session.validation_receipt(validator, raw, ROOT)
                    self.assertEqual(session.unwrap_validation(json.loads(json.dumps(record)), validator, ROOT), raw)
        raw = verifier_receipt()
        self.assertEqual(session.validation_receipt('verifier', raw, ROOT)['receipt'], raw)

    def test_the_semantic_scope_enumerates_the_exact_constants_the_session_uses(self):
        schema = json.loads(SCHEMA.read_text())
        scopes = schema['$defs']['CatalogSemanticValidation']['properties']['scope']['enum']
        self.assertEqual(sorted(scopes), sorted(['input_preparation', session.FINAL_INPUT_SCOPE]))

    def test_the_record_is_closed_and_bound_to_its_validator(self):
        _, _, private, semantic = catalog_receipts()
        # a raw (unwrapped) validator output is not a transport record
        for raw in (private, semantic, verifier_receipt()):
            with self.assertRaises(p.receipts.ReceiptError):
                p.auxiliary(raw, ROOT)
        good = session.validation_receipt('catalog_semantic', semantic, ROOT)
        with self.assertRaises(p.receipts.ReceiptError):
            p.auxiliary({**good, 'extra': 1}, ROOT)
        with self.assertRaises(p.receipts.ReceiptError):
            p.auxiliary({**good, 'receipt': {**semantic, 'extra': 1}}, ROOT)
        # a catalog_semantic body labelled as another validator
        with self.assertRaises(p.receipts.ReceiptError):
            p.auxiliary({**good, 'validator': 'catalog_private'}, ROOT)
        with self.assertRaises(p.receipts.ReceiptError):
            p.auxiliary({**good, 'validator': 'verifier'}, ROOT)
        with self.assertRaises(p.receipts.ReceiptError):
            session.unwrap_validation(good, 'catalog_private', ROOT)

    def test_acceptance_is_never_a_true_validation(self):
        _, _, _, semantic = catalog_receipts()
        with self.assertRaises(p.receipts.ReceiptError):
            session.validation_receipt('catalog_semantic', {**semantic, 'acceptance': True}, ROOT)
        with self.assertRaises(p.receipts.ReceiptError):
            session.validation_receipt('verifier', {**verifier_receipt(), 'complete': False}, ROOT)


class ActivationValidationTests(unittest.TestCase):
    """check_activation_validations reads the bytes prepare_activation builds."""
    def setUp(self):
        self.envelope, self.root_record, private, semantic = catalog_receipts()
        self.bytes = {}
        self.ledger = type('L', (), {})()
        self.ledger.db = sqlite3.connect(':memory:')
        self.addCleanup(self.ledger.db.close)
        self.ledger.db.execute('CREATE TABLE coverage(kind TEXT, sha256 TEXT, bytes INTEGER)')
        self.catalog_ref = self.store('catalog_backup', p.canonical(self.root_record), covered=False)
        self.private = private
        self.semantic = semantic

    def store(self, kind, data, covered=True):
        ref = snapshots.content_ref(kind, data)
        self.bytes[ref['sha256']] = data
        if covered:
            self.ledger.db.execute('INSERT INTO coverage VALUES(?,?,?)', (ref['kind'], ref['sha256'], ref['bytes']))
        return ref

    def resolver(self, ref):
        return self.bytes[ref['sha256']]

    def validations(self, private=None, semantic=None, validators=('catalog_private', 'catalog_semantic')):
        refs = [self.store('validation_receipt',
                           p.canonical(session.validation_receipt(name, raw, ROOT)))
                for name, raw in zip(validators, (private or self.private, semantic or self.semantic))]
        return {'catalog_private': refs[0], 'catalog_semantic': refs[1], 'verifier': None, 'final_input': refs[1]}

    def check(self, validations):
        session.check_activation_validations(
            {'checkpoint': self.envelope}, {'catalog_root': self.catalog_ref, 'verifier_root': None}, validations,
            self.resolver, self.ledger, None, ROOT)

    def test_the_records_prepare_activation_builds_are_the_records_the_checker_authenticates(self):
        self.check(self.validations())

    def test_a_raw_unwrapped_receipt_is_no_longer_accepted(self):
        validations = self.validations()
        raw = self.store('validation_receipt', p.canonical(self.private))
        with self.assertRaises(p.receipts.ReceiptError):
            self.check({**validations, 'catalog_private': raw})

    def test_receipts_swapped_between_validators_are_refused(self):
        swapped = self.validations(private=self.semantic, semantic=self.private)
        with self.assertRaises(p.receipts.ReceiptError):
            self.check(swapped)

    def test_a_receipt_that_differs_from_the_authenticated_root_is_refused(self):
        wrong = {**self.private, 'catalog_bytes': self.private['catalog_bytes'] + 1}
        with self.assertRaises(p.receipts.ReceiptError):
            self.check(self.validations(private=wrong))


if __name__ == '__main__':
    unittest.main()
