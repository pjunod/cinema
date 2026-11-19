import hashlib
import io
import json
import unittest
from unittest.mock import Mock, patch
import zipfile

from validation import publish_qualified_main as publisher
from validation import python_unit_receipts as receipts
from validation import qualification


class PublishQualifiedMainCase(unittest.TestCase):
    def setUp(self):
        self.landing, self.head, self.base, self.tree = [value * 40 for value in 'abcd']
        self.branch = 'effort/queue'
        self.slug = 'example/cinema'
        self.env = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_REF': 'refs/heads/main',
                    'GITHUB_SHA': self.landing, 'GITHUB_REPOSITORY': self.slug,
                    'QUALIFICATION_PR': '19', 'QUALIFICATION_RUN': '32'}
        self.refs = {'HEAD': self.landing, 'HEAD^1': self.base, 'HEAD^2': self.head,
                     'HEAD^{tree}': self.tree, 'HEAD^2^{tree}': self.tree}
        self.patch = patch.object(qualification, 'git_object', side_effect=lambda _, ref: self.refs[ref])
        self.patch.start()
        self.addCleanup(self.patch.stop)
        binding = {'promotion_pr': '19', 'promotion_head_sha': self.head,
                   'promotion_base_sha': self.base}
        self.pr = {'number': 19, 'state': 'closed', 'merged': True, 'merge_commit_sha': self.landing,
                   'base': {'ref': 'main', 'repo': {'id': 8}},
                   'head': {'ref': self.branch, 'sha': self.head, 'repo': {'id': 8}}}
        self.run = {'id': 32, 'status': 'success', 'workflow_id': 'ci.yml',
                    'trigger_event': 'workflow_dispatch', 'repository': {'id': 8},
                    'commit_sha': self.head, 'prettyref': self.branch,
                    'event_payload': json.dumps({'ref': 'refs/heads/' + self.branch, 'inputs': binding})}
        self.gate = {'name': 'Main promotion gate', 'status': 'success', 'attempt': 1, 'repo_id': 8, 'run_id': 32}
        self.api = Mock(root='https://forge.lan/api/v1/repos/' + self.slug)
        self.api.get.side_effect = {'': {'id': 8, 'full_name': self.slug},
                                    '/branches/main': {'commit': {'id': self.landing}},
                                    '/pulls/19': self.pr, '/actions/runs/32': self.run}.__getitem__
        self.record = {'schema': 1, 'kind': 'effort-qualification', 'repository': self.slug,
                       'pull_request': 19, 'head_ref': self.branch, 'base_ref': 'main',
                       'head_sha': self.head, 'base_sha': self.base, 'tested_sha': self.head,
                       'tested_tree': self.tree, 'workflow_ref': self.slug + '/.github/workflows/ci.yml@refs/heads/' + self.branch,
                       'run_id': '32', 'run_attempt': '1', 'jobs': dict.fromkeys(qualification.REQUIRED_JOBS, 'success'),
                       'retained_unit_evidence': None, 'binding_mode': 'authenticated-manual-promotion'}
        self.archive()

    def archive(self):
        raw = json.dumps(self.record).encode()
        self.env['QUALIFICATION_RECEIPT_SHA256'] = hashlib.sha256(raw).hexdigest()
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr('qualification-receipt.json', raw)
        content = data.getvalue()
        artifact = {'name': 'effort-qualification-19', 'id': 45, 'run_id': 32, 'expired': False,
                    'archive_download_url': self.api.root + '/actions/artifacts/45/zip',
                    'size_in_bytes': len(content), 'digest': 'sha256:' + hashlib.sha256(content).hexdigest()}
        self.api.pages.side_effect = {'/actions/runs/32/jobs': [self.gate], '/actions/runs/32/artifacts': [artifact]}.__getitem__
        self.api.bytes.side_effect = {'/actions/artifacts/45/zip': content}.__getitem__

    def test_publishes_only_the_tree_from_the_native_qualification(self):
        self.assertEqual(publisher.verify(self.api, self.env), self.landing)

    def test_refuses_a_different_merge_tree(self):
        self.refs['HEAD^{tree}'] = '0' * 40
        with self.assertRaisesRegex(receipts.ReceiptError, 'tree differs'):
            publisher.verify(self.api, self.env)

    def test_refuses_a_receipt_for_another_pull_request(self):
        self.record['pull_request'] = 20
        self.archive()
        with self.assertRaisesRegex(receipts.ReceiptError, 'does not describe'):
            publisher.verify(self.api, self.env)

    def test_refuses_an_unsuccessful_run_or_gate(self):
        for row in [self.run, self.gate]:
            with self.subTest(row=row):
                row['status'] = 'failure'
                with self.assertRaises(receipts.ReceiptError):
                    publisher.verify(self.api, self.env)
                row['status'] = 'success'

    def test_refuses_a_receipt_with_a_failed_or_missing_required_job(self):
        self.record['jobs']['cluster_daemon'] = 'failure'
        self.archive()
        with self.assertRaisesRegex(qualification.QualificationError, 'cluster_daemon=failure'):
            publisher.verify(self.api, self.env)
        del self.record['jobs']['cluster_daemon']
        self.archive()
        with self.assertRaisesRegex(qualification.QualificationError, 'missing required jobs'):
            publisher.verify(self.api, self.env)

    def test_refuses_another_reviewed_receipt_digest(self):
        self.env['QUALIFICATION_RECEIPT_SHA256'] = '0' * 64
        with self.assertRaisesRegex(receipts.ReceiptError, 'reviewed promotion digest'):
            publisher.verify(self.api, self.env)
