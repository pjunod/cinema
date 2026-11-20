"""Main-lane journals retain their own typed source identity."""
import unittest
from unittest.mock import Mock
from validation import main_python_continuation as source
from validation import python_unit_receipts as r
class MainPythonContinuationTests(unittest.TestCase):
    def test_unknown_provider_or_wrong_candidate_never_reaches_original_journal(self):
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), {'version': 'native-python-continuation-v1'}, {'schema': 2}, 'a' * 40, 1)
        record = dict(version=source.POLICY, repository=1, pr=42, branch='codex/task',
                      base_sha='b' * 40, source_commit='c' * 40, prior='d' * 40,
                      run=10, journal_artifact=20, changes={})
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), record, {'schema': 2}, 'a' * 40, 1)
