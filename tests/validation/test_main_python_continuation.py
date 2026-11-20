"""Main-lane journals retain their own typed source identity."""
import unittest
from unittest.mock import Mock, patch
from validation import main_python_continuation as source
from validation import python_unit_receipts as r
class MainPythonContinuationTests(unittest.TestCase):
    def test_unknown_provider_or_wrong_candidate_never_reaches_original_journal(self):
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), {'version': 'native-python-continuation-v1'}, {'schema': 2}, 'a' * 40, 1)
        record = dict(version=source.POLICY, repository=1, pr=42, branch='codex/task',
                      base_sha='b' * 40, source_commit='c' * 40, prior='d' * 40,
                      run=10, journal_artifact=20, changes={}, source_impacts={})
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), record, {'schema': 2}, 'a' * 40, 1)

    def test_missing_or_forged_inherited_origin_never_becomes_a_direct_pass(self):
        scope = {'repository': 1, 'pr': 42, 'branch': 'codex/task', 'base': 'main', 'workflow': 'main-fast-lane.yml'}
        key = 'validation:test_fixture.Case.test_body'
        source_record = {'run': 10, 'commit': 'a' * 40}
        enclosing = {'run': 11, 'commit': 'b' * 40, 'passes': {key: source_record}}
        api = Mock()
        api.pages.return_value = [{'name': source.main.key(scope), 'run_id': 10}]
        with self.assertRaises(r.ReceiptError):
            source.authenticate_origins(api, scope, enclosing, [], 'c' * 40)
        origin = {'run': 10, 'commit': 'a' * 40, 'passes': {key: source_record}}
        with patch.object(source.main, 'journal_from_artifacts', return_value=origin), \
                patch.object(source, 'require_producer'):
            result = source.authenticate_origins(api, scope, enclosing, [{'id': 10, 'commit_sha': 'a' * 40}], 'c' * 40)
            self.assertEqual(result, {10: 'a' * 40, 11: 'b' * 40})
        forged = {'run': 10, 'commit': 'a' * 40, 'passes': {key: {'run': 9, 'commit': 'd' * 40}}}
        with patch.object(source.main, 'journal_from_artifacts', return_value=forged), \
                patch.object(source, 'require_producer'), self.assertRaises(r.ReceiptError):
            source.authenticate_origins(api, scope, enclosing, [{'id': 10, 'commit_sha': 'a' * 40}], 'c' * 40)

    def test_imported_change_before_enclosing_prior_requires_actual_origin_impact(self):
        key = 'validation:test_fixture.Case.test_unchanged_method'
        path = 'validation/imported_implementation.py'
        witness = {path: {'before': source.retention.digest(b'old'), 'after': source.retention.digest(b'new'),
                         'affected': [key], 'reason': 'Imported behavior changed before the enclosing journal'}}
        with patch.object(source.retention, 'changed', return_value={path}), \
                patch.object(source.retention, 'git_bytes', side_effect=lambda rev, _: b'old' if rev == 'a' * 40 else b'new'):
            self.assertEqual(source.origin_impacts('a' * 40, 'c' * 40, witness, {key}), {key})
            with self.assertRaises(r.ReceiptError):
                source.origin_impacts('a' * 40, 'c' * 40, {}, {key})
            wrong = {path: {**witness[path], 'before': 'f' * 64}}
            with self.assertRaises(r.ReceiptError):
                source.origin_impacts('a' * 40, 'c' * 40, wrong, {key})
