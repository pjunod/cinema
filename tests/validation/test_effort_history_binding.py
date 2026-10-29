"""Exact authenticated effort task history; no credential or network access."""
from copy import deepcopy
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from validation.history import HistoryError, authenticated_history_baseline
from validation.qualification import QualificationError, resolve_effort_history_binding


class EffortHistoryBindingCase(unittest.TestCase):
    def setUp(self):
        self.head, self.base, self.main = 'a' * 40, 'b' * 40, 'c' * 40
        self.included = 'd' * 40
        self.env = dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_ATTEMPT='1',
                        GITHUB_REF_NAME='codex/history', GITHUB_REF='refs/heads/codex/history',
                        GITHUB_SHA=self.head, GITHUB_REPOSITORY='example/project',
                        GITHUB_API_URL='https://forge.example/api/v1',
                        GITHUB_SERVER_URL='https://forge.example', GITHUB_TOKEN='test-only')
        self.pull = dict(number=931, state='open', merged=False,
                         head=dict(ref='codex/history', sha=self.head, repo=dict(id=7)),
                         base=dict(ref='effort/cinema', sha=self.base, repo=dict(id=7)))

    def resolve(self, *, environment=None, pull=None, live_head=None, live_base=None,
                main=None, ancestry=(0, 0, 0), checkout=None, candidates=None,
                merge_bases=None, merge_exit=0):
        pull = deepcopy(self.pull if pull is None else pull)
        api = SimpleNamespace(get=lambda path: {'id': 7},
                              pages=lambda path, query: [pull] if candidates is None else candidates)
        documents = {'/pulls/931': pull,
                     '/branches/codex%2Fhistory': {'commit': {'id': live_head or self.head}},
                     '/branches/effort%2Fcinema': {'commit': {'id': live_base or self.base}},
                     '/branches/main': {'commit': {'id': main or self.main}}}
        statuses = [*ancestry, 0, 0, 0]
        bases = [self.included] if merge_bases is None else merge_bases
        replies = [SimpleNamespace(returncode=statuses[0]),
                   SimpleNamespace(returncode=merge_exit, stdout="\n".join(bases)),
                   SimpleNamespace(returncode=statuses[1]),
                   SimpleNamespace(returncode=statuses[2])]
        with mock.patch('validation.python_unit_receipts.API', return_value=api), \
             mock.patch('validation.qualification.api_document', side_effect=lambda env, path: documents[path]), \
             mock.patch('validation.qualification.git_object', return_value=checkout or self.head), \
             mock.patch('validation.qualification.subprocess.run', side_effect=replies) as run:
            result = resolve_effort_history_binding(self.env if environment is None else environment, Path('/source'))
            self.assertEqual([call.args[0][-2:] for call in run.call_args_list],
                             [[self.base, 'HEAD'], [self.main, self.base],
                              [self.included, self.base], [self.included, self.main]])
            return result

    def test_exact_authenticated_task_binds_live_main_through_current_effort(self):
        self.assertEqual(self.resolve(), dict(base_sha=self.included, main_sha=self.main,
                                             effort_sha=self.base, head_sha=self.head,
                                             pull_request=931))

    def test_missing_ambiguous_malformed_or_unproven_common_main_refuses(self):
        for kwargs in (dict(merge_bases=[]), dict(merge_bases=[self.included, self.main]),
                       dict(merge_bases=['not-an-immutable-sha']), dict(merge_exit=1),
                       dict(ancestry=(0, 1, 0)), dict(ancestry=(0, 0, 1))):
            with self.subTest(kwargs=kwargs), self.assertRaises(QualificationError):
                self.resolve(**kwargs)

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
                       dict(checkout='d'*40), dict(ancestry=(1,)),
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


class ActualEffortHistoryAncestryCase(unittest.TestCase):
    """Real Git topology; only authenticated API documents are fixture inputs."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="effort-history-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "-q", "-b", "main")
        self.included = self.commit("landed main")
        self.git("checkout", "-q", "-b", "effort/cinema")
        self.base = self.commit("effort work")
        self.git("checkout", "-q", "-b", "codex/history")
        self.head = self.commit("task work")
        self.git("checkout", "-q", "main")
        self.main = self.commit("unrelated new main work")
        self.git("checkout", "-q", "codex/history")

    def git(self, *args):
        # Ephemeral test history has no project checkout or hook ownership.
        return subprocess.run(
            ["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
             "-c", "user.name=History Fixture",
             "-c", "user.email=history@example.invalid", *args], cwd=self.root,
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        ).stdout.strip()

    def commit(self, title):
        self.git("commit", "--allow-empty", "-qm", title)
        return self.git("rev-parse", "HEAD")

    def resolve(self):
        env = dict(GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_RUN_ATTEMPT="1",
                   GITHUB_REF_NAME="codex/history", GITHUB_REF="refs/heads/codex/history",
                   GITHUB_SHA=self.head, GITHUB_REPOSITORY="example/project",
                   GITHUB_API_URL="https://forge.example/api/v1",
                   GITHUB_SERVER_URL="https://forge.example", GITHUB_TOKEN="test-only")
        pull = dict(number=931, state="open", merged=False,
                    head=dict(ref="codex/history", sha=self.head, repo=dict(id=7)),
                    base=dict(ref="effort/cinema", sha=self.base, repo=dict(id=7)))
        api = SimpleNamespace(get=lambda path: {"id": 7}, pages=lambda path, query: [pull])
        documents = {"/pulls/931": pull,
                     "/branches/codex%2Fhistory": {"commit": {"id": self.head}},
                     "/branches/effort%2Fcinema": {"commit": {"id": self.base}},
                     "/branches/main": {"commit": {"id": self.main}}}
        with mock.patch("validation.python_unit_receipts.API", return_value=api), \
             mock.patch("validation.qualification.api_document",
                        side_effect=lambda _env, path: documents[path]):
            return resolve_effort_history_binding(env, self.root)

    def test_task_stays_bound_to_included_main_when_live_main_advances(self):
        binding = self.resolve()
        self.assertEqual(binding["base_sha"], self.included)
        self.assertEqual(binding["main_sha"], self.main)
        self.assertEqual(binding["effort_sha"], self.base)
        self.assertEqual(binding["head_sha"], self.head)
        self.assertNotEqual(binding["base_sha"], binding["main_sha"])

    def test_later_effort_integration_moves_baseline_to_actual_included_main(self):
        self.git("checkout", "-q", "effort/cinema")
        self.git("merge", "-q", "--no-edit", "main")
        self.base = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "codex/history")
        self.git("merge", "-q", "--no-edit", "effort/cinema")
        self.head = self.git("rev-parse", "HEAD")
        binding = self.resolve()
        self.assertEqual(binding["base_sha"], self.main)
        self.assertEqual(binding["main_sha"], self.main)
        self.assertEqual(binding["effort_sha"], self.base)

    def test_missing_authenticated_main_object_never_falls_back_to_effort_tip(self):
        self.main = "f" * 40
        with self.assertRaises(QualificationError):
            self.resolve()


if __name__ == '__main__':
    unittest.main()
