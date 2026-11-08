import unittest
from unittest.mock import Mock, patch

from validation import landed_python_completion as completion
from validation import python_unit_receipts as receipts


class CinemaPythonPublicationCase(unittest.TestCase):
    def test_other_selection_and_job_attempt_cannot_supply_completion(self):
        api = Mock()
        binding = {'head_sha': completion.EFFORT}
        scope = {'source_commit': completion.LANDING, 'postmerge_binding': binding, 'evidence_sha256': '0' * 64}
        with patch.object(completion.postmerge, 'landing_binding', return_value=binding):
            with self.assertRaisesRegex(receipts.ReceiptError, 'Unexpected Cinema Python selection'):
                completion.verify_python_completion(api, completion.LANDING, scope)
        api.get.return_value = {'id': 7, 'repository': {'id': 1}, 'workflow_id': 'checks.yml',
                                'trigger_event': 'workflow_dispatch', 'commit_sha': completion.EFFORT}
        api.pages.return_value = [{'id': 8, 'run_id': 7, 'repo_id': 1, 'attempt': 2, 'status': 'success'}]
        with self.assertRaisesRegex(receipts.ReceiptError, 'job identity'):
            completion.native_job(api, 7, 8, 'checks.yml', 'workflow_dispatch',
                                  completion.EFFORT, 'success', '0' * 64)
        api.bytes.assert_not_called()
