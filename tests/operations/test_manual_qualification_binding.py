"""Manual promotion qualification proves API identity, not invented PR context."""
from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation import qualification as q

ROOT = Path(__file__).resolve().parents[2]


class ManualQualificationBindingCase(unittest.TestCase):
    def test_authenticated_manual_promotion_binding_refuses_stale_or_partial_evidence(self):
        head, base = "a" * 40, "b" * 40
        environment = dict(
            GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REPOSITORY="noirr/plurx",
            GITHUB_SHA=head, GITHUB_REF="refs/heads/integration/project-into-main",
            GITHUB_RUN_ID="900", GITHUB_RUN_ATTEMPT="1",
            GITHUB_WORKFLOW_REF="noirr/plurx/.github/workflows/ci.yml@refs/heads/integration/project-into-main",
            PLURX_PROMOTION_PR="42", PLURX_PROMOTION_HEAD_SHA=head,
            PLURX_PROMOTION_BASE_SHA=base,
        )
        pr = dict(number=42, state="open", merged=False,
                  head=dict(ref="integration/project-into-main", sha=head, repo=dict(full_name="noirr/plurx")),
                  base=dict(ref="main", sha=base, repo=dict(full_name="noirr/plurx")))
        binding = q.manual_binding(environment, pr, head, base, head, True)
        environment["PLURX_PROMOTION_BINDING"] = json.dumps(binding)
        results = {name: "success" for name in q.REQUIRED_JOBS}
        receipt = q.build_receipt(environment, results, head, "c" * 40)
        self.assertEqual(receipt["binding_mode"], "authenticated-manual-promotion")
        self.assertEqual(receipt["pull_request"], 42)
        self.assertEqual(receipt["head_ref"], "integration/project-into-main")
        self.assertIn("windows_compile", receipt["jobs"])
        # Production resolver reads authenticated PR and BOTH branch tips each time.
        def fetch(_environment, path):
            if path == "/pulls/42":
                return deepcopy(pr)
            if path.startswith("/branches/integration%2F"):
                return dict(commit=dict(id=head))
            if path == "/branches/main":
                return dict(commit=dict(id=base))
            self.fail("unexpected API route: " + path)
        with patch.object(q, "api_document", side_effect=fetch) as api, \
             patch.object(q, "git_object", return_value=head), \
             patch.object(q.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
            self.assertEqual(q.resolve_manual_binding(environment, ROOT), binding)
            self.assertEqual(api.call_count, 3)
        changes = [("state", "closed"), ("merged", True)]
        for field, value in changes:
            altered = deepcopy(pr); altered[field] = value
            with self.subTest(field=field), self.assertRaises(q.QualificationError):
                q.manual_binding(environment, altered, head, base, head, True)
        for side, field, value in [
            ("head", "repo", dict(full_name="outsider/plurx")),
            ("base", "repo", dict(full_name="outsider/plurx")),
            ("head", "ref", "codex/task"), ("head", "ref", "integration/-into-main"),
            ("base", "ref", "effort/project"), ("head", "sha", "d" * 40),
            ("base", "sha", "d" * 40),
        ]:
            altered = deepcopy(pr); altered[side][field] = value
            with self.subTest(side=side, field=field), self.assertRaises(q.QualificationError):
                q.manual_binding(environment, altered, head, base, head, True)
        for actual_head, actual_base, checkout, ancestry in [
            ("d" * 40, base, head, True), (head, "d" * 40, head, True),
            (head, base, "d" * 40, True), (head, base, head, False),
        ]:
            with self.subTest(checkout=checkout, ancestry=ancestry), self.assertRaises(q.QualificationError):
                q.manual_binding(environment, pr, actual_head, actual_base, checkout, ancestry)
        for field, value in [("GITHUB_REF", "refs/heads/main"), ("PLURX_PROMOTION_PR", "0"),
                             ("PLURX_PROMOTION_HEAD_SHA", "branch-name"),
                             ("GITHUB_RUN_ATTEMPT", ""), ("GITHUB_RUN_ATTEMPT", "2")]:
            altered = dict(environment); altered[field] = value
            with self.subTest(field=field), self.assertRaises(q.QualificationError):
                q.manual_binding(altered, pr, head, base, head, True)
        for outcome in ("skipped", "failure", "cancelled"):
            altered = dict(results); altered["windows_compile"] = outcome
            with self.subTest(outcome=outcome), self.assertRaises(q.QualificationError):
                q.build_receipt(environment, altered, head, "c" * 40)
        altered = dict(results); del altered["windows_compile"]
        with self.assertRaises(q.QualificationError):
            q.build_receipt(environment, altered, head, "c" * 40)
        # Final CLI refuses API drift even when all supplied job results are green.
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "receipt.json"
            env = dict(environment, PLURX_QUALIFICATION_RESULTS=json.dumps(results))
            live = dict(binding, base_sha="d" * 40)
            with patch.dict(os.environ, env, clear=True), \
                 patch.object(q, "resolve_manual_binding", return_value=live), \
                 redirect_stderr(io.StringIO()):
                self.assertEqual(q.main(["--output", str(output)]), 1)
            self.assertFalse(output.exists())
            with patch.dict(os.environ, env, clear=True), \
                 patch.object(q, "resolve_manual_binding", return_value=binding), \
                 patch.object(q, "git_object", side_effect=[head, "c" * 40]), \
                 redirect_stdout(io.StringIO()):
                self.assertEqual(q.main(["--output", str(output)]), 0)
            self.assertEqual(json.loads(output.read_text())["base_sha"], base)
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("--bind-manual", workflow)
        self.assertIn("scope_event=effort_qualification", workflow)
        self.assertIn('VALIDATION_BASE="$BOUND_BASE_SHA"', workflow)
        self.assertIn('test "$(git rev-parse "origin/$BASE_REF")" = "$BOUND_BASE_SHA"', workflow)
        self.assertIn('"windows_compile":"${{ needs.windows_compile.result }}"', workflow)
        publish = workflow.split("  publish:\n", 1)[1].split("  publish_main:\n", 1)[0]
        self.assertIn("startsWith(github.ref, 'refs/tags/v')", publish)
        for field in ("promotion_pr", "promotion_head_sha", "promotion_base_sha"):
            self.assertIn(f"inputs.{field} == ''", publish)
        self.assertIn("needs.scope.outputs.qualification != 'true'", publish)
        publish_main = workflow.split("  publish_main:\n", 1)[1].split("  pr_gate:\n", 1)[0]
        self.assertIn("inputs.promotion_pr == ''", publish_main)
        self.assertIn("needs.scope.outputs.qualification != 'true'", publish_main)
        self.assertNotIn("GITHUB_HEAD_REF:", workflow)


if __name__ == "__main__":
    unittest.main()
