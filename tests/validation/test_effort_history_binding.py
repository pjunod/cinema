"""Exact authenticated effort task history; no credential or network access."""
from copy import deepcopy
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from validation.history import HistoryError, authenticated_history_baseline
from validation.qualification import QualificationError, _ensure_task_history_tips, resolve_effort_history_binding


class EffortHistoryBindingCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repo = Path(temporary.name)
        self.git("init", "-q")
        tree = self.git("mktree", content="")
        def commit(name, *parents):
            args = [flag for parent in parents for flag in ("-p", parent)]
            return self.git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                            "commit-tree", tree, *args, "-m", name)
        self.main = commit("main")
        self.base = commit("effort fork", self.main)
        self.head = commit("task", self.base)
        self.next_main = commit("independent main", self.main)
        self.next_effort = commit("independent effort", self.base)
        self.unrelated = commit("unrelated root")
        self.git("update-ref", "HEAD", self.head)
        self.env = dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1',
                        GITHUB_REF_NAME='codex/history', GITHUB_REF='refs/heads/codex/history',
                        GITHUB_SHA=self.head, GITHUB_REPOSITORY='example/project',
                        GITHUB_API_URL='https://forge.example/api/v1',
                        GITHUB_SERVER_URL='https://forge.example', GITHUB_TOKEN='test-only')
        self.pull = dict(number=931, state='open', merged=False, merge_base=self.base,
                         head=dict(ref='codex/history', sha=self.head, repo=dict(id=7)),
                         base=dict(ref='effort/cinema', sha=self.base, repo=dict(id=7)))

    def git(self, *args, content=None):
        return subprocess.run(["git", *args], cwd=self.repo, input=content, text=True,
                              capture_output=True, check=True).stdout.strip()

    def resolve(self, *, environment=None, pull=None, live_head=None,
                main=None, candidates=None):
        pull = deepcopy(self.pull if pull is None else pull)
        api = SimpleNamespace(get=lambda path: {'id': 7},
                              pages=lambda path, query: [pull] if candidates is None else candidates)
        documents = {'/pulls/931': pull,
                     '/branches/codex%2Fhistory': {'commit': {'id': live_head or self.head}},
                     '/branches/main': {'commit': {'id': main or self.main}}}
        with mock.patch('validation.python_unit_receipts.API', return_value=api), \
             mock.patch('validation.qualification.api_document', side_effect=lambda env, path: documents[path]):
            return resolve_effort_history_binding(self.env if environment is None else environment, self.repo)

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

    def test_unrelated_main_and_effort_advances_preserve_dispatched_history(self):
        pull = {**self.pull, "base": {**self.pull["base"], "sha": self.next_effort}}
        self.assertEqual(self.resolve(pull=pull, main=self.next_main),
                         dict(base_sha=self.main, effort_sha=self.base, head_sha=self.head, pull_request=931))

    def test_tip_missing_after_checkout_fetches_only_authenticated_object(self):
        for status in (0, 1):
            with self.subTest(status=status), mock.patch("validation.qualification.subprocess.run") as run:
                run.side_effect = [SimpleNamespace(returncode=x) for x in (0, 1, status)]
                if status:
                    with self.assertRaises(QualificationError):
                        _ensure_task_history_tips(self.env, self.repo, (self.base, self.next_main))
                else:
                    _ensure_task_history_tips(self.env, self.repo, (self.base, self.next_main))
                argv = run.call_args.args[0]
                self.assertEqual(argv[-2:], ["https://forge.example/example/project.git", self.next_main])
                self.assertIn("http.followRedirects=false", argv)
                self.assertIn("--no-write-fetch-head", argv)
                self.assertNotIn(self.env["GITHUB_TOKEN"], " ".join(argv))

    def test_stale_head_base_checkout_or_main_ancestry_refuses(self):
        for kwargs in (dict(live_head=self.next_effort),
                       dict(pull={**self.pull, "base": {**self.pull["base"], "sha": self.unrelated}}),
                       dict(pull={**self.pull, "base": {**self.pull["base"], "ref": "effort/foreign"}},
                            candidates=[self.pull]),
                       dict(environment={**self.env, "GITHUB_SHA": self.next_main}),
                       dict(pull={**self.pull, "merge_base": self.main}),
                       dict(pull={**self.pull, "merge_base": self.unrelated}),
                       dict(main=self.unrelated), dict(main="not-an-immutable-sha")):
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
