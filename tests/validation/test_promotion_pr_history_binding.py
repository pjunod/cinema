"""Promotion history uses live main, never an unpromoted effort head."""
from copy import deepcopy
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from validation.history import HistoryError, authenticated_history_baseline
from validation.qualification import (
    NoRedirect, QualificationError, resolve_promotion_pr_history_binding,
)


class PromotionPrHistoryBindingCase(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.repo = Path(directory.name)
        self.git("init", "-q")
        tree = self.git("mktree", content="")

        def commit(name, *parents):
            args = [flag for parent in parents for flag in ("-p", parent)]
            return self.git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                            "commit-tree", tree, *args, "-m", name)

        self.commit = commit
        root = commit("root")
        self.main = commit("actual main", root)
        self.effort = commit("unpromoted corrective effort work", root)
        self.head = commit("incorporate actual main into effort", self.effort, self.main)
        self.next_main = commit("later main", self.main)
        self.foreign = commit("unrelated root")
        self.git("update-ref", "HEAD", self.head)
        self.env = dict(GITHUB_EVENT_NAME="pull_request", GITHUB_RUN_ATTEMPT="1",
                        GITHUB_REF="refs/pull/1397/head", GITHUB_SHA=self.head,
                        GITHUB_HEAD_REF="effort/cinema", GITHUB_BASE_REF="main",
                        GITHUB_REPOSITORY="example/project", PR_NUMBER="1397",
                        GITHUB_API_URL="https://forge.example/api/v1",
                        GITHUB_SERVER_URL="https://forge.example", GITHUB_TOKEN="test-only")
        repo = dict(id=7, full_name="example/project")
        self.documents = {
            "": repo,
            "/pulls/1397": dict(number=1397, state="open", merged=False, merge_base=self.main,
                                head=dict(ref="effort/cinema", sha=self.head, repo=repo.copy()),
                                base=dict(ref="main", sha=self.main, repo=repo.copy())),
            "/branches/effort%2Fcinema": {"commit": {"id": self.head}},
            "/branches/main": {"commit": {"id": self.main}},
        }

    def git(self, *args, content=None):
        return subprocess.run(["git", *args], cwd=self.repo, input=content, text=True,
                              capture_output=True, check=True).stdout.strip()

    def resolve(self, environment=None, documents=None):
        with mock.patch("validation.qualification.api_document",
                        side_effect=lambda env, path: (documents or self.documents)[path]):
            return resolve_promotion_pr_history_binding(environment or self.env, self.repo)

    def test_live_main_not_unpromoted_effort_is_the_landed_baseline(self):
        result = self.resolve()
        self.assertEqual(result, dict(base_sha=self.main, head_sha=self.head, pull_request=1397))
        self.assertNotIn(self.effort, self.git("rev-list", result["base_sha"]).split())
        self.assertIn(self.effort, self.git("rev-list", self.head).split())

    def test_closed_merged_foreign_or_wrong_pr_route_refuses(self):
        edits = [(("number",), 2), (("state",), "closed"), (("merged",), True),
                 (("merged",), None), (("head", "repo", "id"), 8),
                 (("base", "repo", "full_name"), "foreign/project"),
                 (("head", "ref"), "codex/ordinary"), (("base", "ref"), "effort/other")]
        for keys, value in edits:
            documents = deepcopy(self.documents)
            row = documents["/pulls/1397"]
            for key in keys[:-1]:
                row = row[key]
            row[keys[-1]] = value
            with self.subTest(keys=keys, value=value), self.assertRaises(QualificationError):
                self.resolve(documents=documents)

    def test_stale_head_base_live_tips_or_api_merge_base_refuses(self):
        for path, keys, value in [
            ("/pulls/1397", ("head", "sha"), self.effort),
            ("/pulls/1397", ("base", "sha"), self.next_main),
            ("/branches/effort%2Fcinema", ("commit", "id"), self.effort),
            ("/branches/main", ("commit", "id"), self.next_main),
            ("/branches/main", ("commit", "id"), "not-a-sha"),
            ("/pulls/1397", ("merge_base",), self.effort),
        ]:
            documents = deepcopy(self.documents)
            row = documents[path]
            for key in keys[:-1]:
                row = row[key]
            row[keys[-1]] = value
            with self.subTest(path=path, keys=keys), self.assertRaises(QualificationError):
                self.resolve(documents=documents)

    def test_unincorporated_main_and_ambiguous_ancestry_refuse(self):
        documents = deepcopy(self.documents)
        documents["/branches/main"]["commit"]["id"] = self.next_main
        documents["/pulls/1397"]["base"]["sha"] = self.next_main
        documents["/pulls/1397"]["merge_base"] = self.main
        with self.assertRaises(QualificationError):
            self.resolve(documents=documents)
        # The two independent merges have two equally valid merge bases.
        # Neither input nor the API's claimed fork may choose one of them.
        other_merge = self.commit("opposite independent merge", self.main, self.effort)
        documents["/branches/main"]["commit"]["id"] = other_merge
        documents["/pulls/1397"]["base"]["sha"] = other_merge
        self.assertEqual(len(self.git("merge-base", "--all", self.head, other_merge).split()), 2)
        with self.assertRaises(QualificationError):
            self.resolve(documents=documents)

    def test_forged_event_checkout_refs_number_or_attempt_refuses(self):
        for key, value in [("GITHUB_EVENT_NAME", "push"), ("GITHUB_RUN_ATTEMPT", "2"),
                           ("GITHUB_SHA", self.main), ("GITHUB_HEAD_REF", "effort/foreign"),
                           ("GITHUB_BASE_REF", "effort/foreign"), ("PR_NUMBER", "-1"),
                           ("PR_NUMBER", "01397"), ("GITHUB_REF", "refs/pull/2/head"),
                           ("GITHUB_REF", "refs/pull/1397/merge"),
                           ("GITHUB_REF", "refs/heads/effort/cinema")]:
            with self.subTest(key=key), self.assertRaises(QualificationError):
                self.resolve(environment={**self.env, key: value})

    def test_foreign_api_origin_and_redirect_refuse_without_baseline(self):
        with mock.patch("validation.qualification.urllib.request.build_opener") as opener:
            with self.assertRaises(QualificationError):
                resolve_promotion_pr_history_binding(
                    {**self.env, "GITHUB_API_URL": "https://foreign.example/api/v1"}, self.repo)
            opener.assert_not_called()
        with self.assertRaises(QualificationError):
            NoRedirect().redirect_request(None, None, 302, "redirect", {}, "https://foreign.example")
        with mock.patch("validation.qualification.api_document",
                        side_effect=QualificationError("qualification API redirect refused")):
            with self.assertRaises(QualificationError):
                resolve_promotion_pr_history_binding(self.env, self.repo)

    def test_history_authenticates_only_explicit_unmixed_promotion_pr_context(self):
        environment = {**self.env, "PLURX_HISTORY_CONTEXT": "promotion-pr"}
        with mock.patch("validation.qualification.resolve_promotion_pr_history_binding",
                        return_value={"base_sha": self.main}) as resolve:
            self.assertEqual(authenticated_history_baseline(environment, self.repo), self.main)
            resolve.assert_called_once_with(environment, self.repo)
        for extra in [{"PLURX_PROMOTION_PR": "1397"},
                      {"PLURX_PROMOTION_HEAD_SHA": self.head},
                      {"PLURX_PROMOTION_BASE_SHA": self.head}]:
            with self.subTest(extra=extra), self.assertRaises(HistoryError):
                authenticated_history_baseline({**environment, **extra}, self.repo)
        with mock.patch("validation.qualification.resolve_promotion_pr_history_binding") as resolve:
            self.assertIsNone(authenticated_history_baseline(self.env, self.repo))
            resolve.assert_not_called()
        with self.assertRaises(HistoryError):
            authenticated_history_baseline({"PLURX_HISTORY_CONTEXT": "forged"}, self.repo)

    def test_history_never_falls_back_after_authentication_refusal(self):
        with mock.patch("validation.qualification.resolve_promotion_pr_history_binding",
                        side_effect=QualificationError("refused")):
            with self.assertRaises(HistoryError):
                authenticated_history_baseline({"PLURX_HISTORY_CONTEXT": "promotion-pr"}, self.repo)

    def test_workflow_selects_context_in_shell_without_string_expression_coercion(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/main-fast-lane.yml").read_text()
        step = workflow.split("      - name: Audit corrective-history evidence\n", 1)[1]
        step = step.split("      # docs/ci/LEDGER", 1)[0]
        self.assertIn("PR_HEAD_REF: ${{ github.head_ref }}", step)
        self.assertNotIn("&& 'promotion-pr'", step)
        body = step.split("        run: |\n", 1)[1]
        script = "\n".join(line[10:] for line in body.splitlines())
        make = 'make() { test "$*" = history-check; printf "%s" "${PLURX_HISTORY_CONTEXT-unset}"; };\n'
        for branch, expected in [("effort/cinema", "promotion-pr"),
                                 ("integration/cinema-into-main", "promotion-pr"),
                                 ("codex/ordinary", "unset"), ("integration/incomplete", "unset"),
                                 ("effort/", "unset"), ("effort/$(false)", "unset")]:
            with self.subTest(branch=branch):
                result = subprocess.run(["bash", "-c", make + script], cwd=root,
                                        env={**os.environ, "PATH": str(Path(sys.executable).parent)
                                             + os.pathsep + os.environ.get("PATH", ""),
                                             "PR_HEAD_REF": branch,
                                             "PLURX_HISTORY_CONTEXT": "untrusted-input"},
                                        text=True, capture_output=True, check=True)
                self.assertEqual(result.stdout, expected)
        # A failed interpreter or predicate import must stop before make,
        # rather than silently choosing the ordinary, unbound history path.
        failure = subprocess.run(["bash", "-c", make + "python3() { return 17; };\n" + script],
                                 cwd=root, env={**os.environ, "PR_HEAD_REF": "effort/cinema"},
                                 text=True, capture_output=True)
        self.assertEqual(failure.returncode, 17)
        self.assertEqual(failure.stdout, "", "make/history must not run after predicate failure")


if __name__ == "__main__":
    unittest.main()
