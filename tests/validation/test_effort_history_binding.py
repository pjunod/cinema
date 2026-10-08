"""Exact authenticated effort task history; no credential or network access."""
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

from validation.history import HistoryError, authenticated_history_baseline
from validation.qualification import QualificationError, resolve_effort_history_binding


class EffortHistoryBindingCase(unittest.TestCase):
    def setUp(self):
        self.head, self.base, self.main = 'a' * 40, 'b' * 40, 'c' * 40
        self.env = dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1',
                        GITHUB_REF_NAME='codex/history', GITHUB_REF='refs/heads/codex/history',
                        GITHUB_SHA=self.head, GITHUB_REPOSITORY='example/project',
                        GITHUB_API_URL='https://forge.example/api/v1',
                        GITHUB_SERVER_URL='https://forge.example', GITHUB_TOKEN='test-only')
        self.pull = dict(number=931, state='open', merged=False,
                         head=dict(ref='codex/history', sha=self.head, repo=dict(id=7)),
                         base=dict(ref='effort/cinema', sha=self.base, repo=dict(id=7)))

    def resolve(self, *, environment=None, pull=None, live_head=None, live_base=None,
                main=None, ancestry=(0, 0), checkout=None, candidates=None):
        pull = deepcopy(self.pull if pull is None else pull)
        api = SimpleNamespace(get=lambda path: {'id': 7},
                              pages=lambda path, query: [pull] if candidates is None else candidates)
        documents = {'/pulls/931': pull,
                     '/branches/codex%2Fhistory': {'commit': {'id': live_head or self.head}},
                     '/branches/effort%2Fcinema': {'commit': {'id': live_base or self.base}},
                     '/branches/main': {'commit': {'id': main or self.main}}}
        with mock.patch('validation.python_unit_receipts.API', return_value=api), \
             mock.patch('validation.qualification.api_document', side_effect=lambda env, path: documents[path]), \
             mock.patch('validation.qualification.git_object', return_value=checkout or self.head), \
             mock.patch('validation.qualification.subprocess.run', side_effect=[SimpleNamespace(returncode=x) for x in ancestry]) as run:
            result = resolve_effort_history_binding(self.env if environment is None else environment, Path('/source'))
            self.assertEqual([call.args[0][-2:] for call in run.call_args_list],
                             [[self.base, 'HEAD'], [self.main, self.base]])
            return result

    def test_exact_authenticated_task_binds_live_main_through_current_effort(self):
        self.assertEqual(self.resolve(), dict(base_sha=self.main, effort_sha=self.base,
                                             head_sha=self.head, pull_request=931))

    def test_unknown_closed_foreign_or_ambiguous_task_pr_refuses(self):
        for edit in ('closed', 'foreign', 'merged', 'missing-merged'):
            pull = deepcopy(self.pull)
            if edit == 'closed': pull['state'] = 'closed'
            if edit == 'foreign': pull['head']['repo']['id'] = 8
            if edit == 'merged': pull['merged'] = True
            if edit == 'missing-merged': pull.pop('merged')
            with self.subTest(edit=edit), self.assertRaises(QualificationError):
                self.resolve(pull=pull)
        for candidates in ([], [self.pull, self.pull]):
            with self.subTest(candidates=len(candidates)), self.assertRaises(QualificationError):
                self.resolve(candidates=candidates)

    def test_stale_head_base_checkout_or_main_ancestry_refuses(self):
        for kwargs in (dict(live_head='d'*40), dict(live_base='d'*40),
                       dict(checkout='d'*40), dict(ancestry=(1,)), dict(ancestry=(0, 1)),
                       dict(main='not-an-immutable-sha')):
            with self.subTest(kwargs=kwargs), self.assertRaises(QualificationError):
                self.resolve(**kwargs)

    def test_other_event_attempt_or_foreign_api_origin_refuses(self):
        for key, value in (('GITHUB_EVENT_NAME','push'), ('GITHUB_RUN_ATTEMPT','2'),
                           ('GITHUB_REF','refs/heads/foreign'),
                           ('GITHUB_API_URL','https://foreign.example/api/v1')):
            with self.subTest(key=key), self.assertRaises(QualificationError):
                self.resolve(environment={**self.env, key:value})

    def test_history_uses_effort_authentication_and_never_mixed_context(self):
        env = {**self.env, 'PLURX_HISTORY_CONTEXT':'effort-task'}
        with mock.patch('validation.qualification.resolve_effort_history_binding', return_value={'base_sha':self.main}) as resolve:
            self.assertEqual(authenticated_history_baseline(env, Path('/source')), self.main)
            resolve.assert_called_once_with(env, Path('/source'))
        with mock.patch('validation.qualification.resolve_effort_history_binding') as resolve:
            with self.assertRaises(HistoryError):
                authenticated_history_baseline({**env,'PLURX_PROMOTION_PR':'931'}, Path('/source'))
            resolve.assert_not_called()

    def test_history_refuses_failed_effort_authentication_without_fallback(self):
        with mock.patch('validation.qualification.resolve_effort_history_binding', side_effect=QualificationError('refused')):
            with self.assertRaises(HistoryError):
                authenticated_history_baseline({'PLURX_HISTORY_CONTEXT':'effort-task'}, Path('/source'))


if __name__ == '__main__':
    unittest.main()
