from contextlib import chdir
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from validation import postmerge_unit_retention as post
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts


class LandedUnitSelectionCase(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.repo = Path(directory.name)
        self.enterContext(patch.dict(os.environ, {'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull}))
        self.git('init', '-q', '-b', 'main')
        self.git('config', 'user.email', 'fixture@invalid.example')
        self.git('config', 'user.name', 'Fixture')
        self.git('commit', '-q', '--allow-empty', '-m', 'base')
        self.base = self.git('rev-parse', 'HEAD')
        self.git('switch', '-qc', 'effort/jobs')
        (self.repo / 'source').write_text('qualified bytes\n')
        self.git('add', 'source')
        self.git('commit', '-qm', 'head')
        self.head = self.git('rev-parse', 'HEAD')
        self.git('switch', '-q', 'main')
        self.git('merge', '-q', '--no-ff', 'effort/jobs', '-m', 'merge')
        self.landing = self.git('rev-parse', 'HEAD')
        self.pull = {'number': 7, 'state': 'closed', 'merged': True, 'merge_commit_sha': self.landing,
                     'merge_base': self.base, 'base': {'ref': 'main', 'sha': self.landing, 'repo': {'id': 1}},
                     'head': {'ref': 'effort/jobs', 'sha': self.head, 'repo': {'id': 1}}}
        self.documents = {'': {'id': 1, 'full_name': 'example/cinema'}, '/pulls/7': self.pull,
                          '/branches/main': {'commit': {'id': self.landing}},
                          '/collaborators/writer/permission': {'user': {'id': 3}, 'permission': 'write'}}
        self.api = Mock()
        self.api.get.side_effect = self.documents.__getitem__

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.repo, text=True).strip()

    def test_closed_merge_accepts_only_the_original_writer_attested_evidence(self):
        with chdir(self.repo):
            binding = post.landing_binding(self.api, 7, self.landing)
            self.assertEqual((binding['base_sha'], binding['head_sha']), (self.base, self.head))
            document = {'repository': 1, 'pr': 7, 'branch': 'effort/jobs', 'base_sha': self.base}
            raw = json.dumps(document).encode()
            digest = retention.digest(raw)
            claim = dict(repository=1, pr=7, sha256=digest, attachment_id=4, attachment_uuid='fixture')
            self.api.pages.return_value = [{'id': 9, 'user': {'login': 'writer', 'id': 3},
                'body': 'Promotion-Unit-Evidence: ' + json.dumps(claim)}]
            args = self.api, raw, digest, binding, self.head, 4, 'fixture'
            self.assertEqual(retention.verify_identity(*args)[3], [9])
            self.api.pages.return_value = []
            with self.assertRaisesRegex(receipts.ReceiptError, 'writer'):
                retention.verify_identity(*args)
            del binding['landing_sha']
            with self.assertRaisesRegex(receipts.ReceiptError, 'live promotion'):
                retention.verify_identity(*args)

    def test_execution_selector_binds_actual_checkout_and_keeps_evidence_head(self):
        with chdir(self.repo), patch.object(post.workspace_rust_continuation, 'authenticate') as verify:
            descriptor = {'sha256': 'a' * 64, 'attachment_id': 4, 'attachment_uuid': 'fixture'}
            verify.return_value = {'candidate': self.head, 'must_pass': {'lib:fixture': ['prior_failure']}}
            plan = post.select(self.api, 'rust', descriptor, 7, self.landing)
            self.assertEqual((plan['candidate'], plan['postmerge_binding']['head_sha']), (self.landing, self.head))
            self.assertEqual(plan['must_pass'], verify.return_value['must_pass'])
            verify.return_value = {'candidate': self.base}
            with self.assertRaisesRegex(receipts.ReceiptError, 'selector source'):
                post.select(self.api, 'rust', descriptor, 7, self.landing)

    def test_main_may_advance_but_cannot_drop_the_tested_merge(self):
        self.git('commit', '-q', '--allow-empty', '-m', 'later main')
        later = self.git('rev-parse', 'HEAD')
        self.git('switch', '-q', '--detach', self.landing)
        self.documents['/branches/main']['commit']['id'] = self.pull['base']['sha'] = later
        self.assertEqual(post.landing_binding(self.api, 7, self.landing, self.repo)['base_sha'], self.base)
        self.documents['/branches/main']['commit']['id'] = self.head
        with self.assertRaisesRegex(receipts.ReceiptError, 'outside current main'):
            post.landing_binding(self.api, 7, self.landing, self.repo)

    def test_refuses_dirty_checkout_wrong_live_identity_parents_and_tree(self):
        for obj, key, bad in ((self.pull, 'merge_base', self.head), (self.pull, 'state', 'open'),
                              (self.pull['head']['repo'], 'id', 2),
                              (self.documents['/branches/main']['commit'], 'id', self.head)):
            with self.subTest(key=key), patch.dict(obj, {key: bad}), self.assertRaises(receipts.ReceiptError):
                post.landing_binding(self.api, 7, self.landing, self.repo)
        (self.repo / 'source').write_text('unreviewed bytes\n')
        with self.assertRaisesRegex(receipts.ReceiptError, 'clean checkout'):
            post.landing_binding(self.api, 7, self.landing, self.repo)
        self.git('commit', '-qam', 'change merge result', '--amend')
        landing = self.git('rev-parse', 'HEAD')
        self.documents['/branches/main']['commit']['id'] = landing
        self.pull['merge_commit_sha'] = self.pull['base']['sha'] = landing
        with self.assertRaisesRegex(receipts.ReceiptError, 'tree differ'):
            post.landing_binding(self.api, 7, landing, self.repo)
