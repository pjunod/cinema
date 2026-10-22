"""A successful Cargo exit alone does not prove the two writer bodies ran."""
import unittest

from validation.rust_writer_evidence import CASES, assess


class WriterEvidenceCase(unittest.TestCase):
    def listing(self):
        return '\n'.join(name + ': test' for name in CASES + ['other']) + '\n3 tests, 0 benchmarks\n'

    def native(self):
        return ('running 2 tests\n' + '\n'.join('test ' + name + ' ... ok' for name in CASES)
                + '\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.01s\n')

    def test_exact_unignored_bodies_are_required(self):
        self.assertEqual(assess(self.listing(), self.native())['declared_tests'], 2)
        for log in ['', self.native().replace(CASES[0], 'unknown'),
                    self.native().replace(' ... ok', ' ... ignored', 1),
                    self.native().replace('1 filtered out', '0 filtered out'),
                    self.native().replace('running 2 tests', 'running 0 tests')]:
            with self.subTest(log=log), self.assertRaises(ValueError):
                assess(self.listing(), log)

    def test_failure_and_bad_inventory_cannot_qualify(self):
        failed = self.native().replace(' ... ok', ' ... FAILED', 1).replace(
            'result: ok. 2 passed; 0 failed', 'result: FAILED. 1 passed; 1 failed')
        with self.assertRaises(ValueError):
            assess(self.listing(), failed)
        for listing in ['', self.listing() + CASES[0] + ': test\n',
                        self.listing().replace(CASES[0], 'unknown'), self.listing() + 'unexpected\n']:
            with self.subTest(listing=listing), self.assertRaises(ValueError):
                assess(listing, self.native())
