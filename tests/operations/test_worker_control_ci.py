"""Contracts for production worker controller proof selection and promotion."""

import io
import json
import hashlib
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import zipfile
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/main-fast-lane.yml"


def job_block(source: str, name: str) -> str:
    start = source.index(f"\n  {name}:\n") + 1
    following = re.search(r"\n  [a-zA-Z0-9_-]+:\n", source[start:])
    end = start + following.start() if following else len(source)
    return source[start:end]


def step_block(source: str, name: str) -> str:
    start = source.index(f"      - name: {name}\n")
    following = re.search(r"\n      - name:", source[start + 1 :])
    end = start + 1 + following.start() if following else len(source)
    return source[start:end]


class WorkerControlCiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (ROOT / WORKFLOW).read_text(encoding="utf-8")
        self.proof = job_block(self.workflow, "worker_control_proof")
        self.gate = job_block(self.workflow, "promotion_gate")

    def test_proof_job_selects_only_relevant_source_changes(self) -> None:
        self.assertIn("needs: scope", self.proof)
        self.assertIn("if: github.event.pull_request.draft == false", self.proof)
        selection = step_block(self.proof, "Select affected worker controller proof inputs")
        self.assertIn('git diff --name-only "$PR_BASE_SHA...$PR_HEAD_SHA"', selection)
        for path in (
            "crates/plurx-worker-control/*",
            "crates/plurxd/src/library_work.rs",
            "crates/plurxd/Cargo.toml",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "scripts/check-worker-control",
            "validation/worker_control_resume.py",
            "verification/worker-control/*",
            ".github/workflows/main-fast-lane.yml",
        ):
            with self.subTest(path=path):
                self.assertIn(path, selection)

    def test_selected_inputs_run_pinned_full_proof_and_preserve_attempt(self) -> None:
        python = step_block(self.proof, "Install the pinned Python 3.12 runtime")
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", python)
        self.assertIn("target/worker-control-python-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT", python)
        self.assertIn("731af898886c5f821890dc901eca3c651cca8e51fa7308c159d12a1194aeac91", python)
        self.assertIn("sys.version_info[:3] == (3, 12, 15)", python)

        resume = step_block(self.proof, "Prepare same-PR worker proof resumption")
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", resume)
        self.assertIn("GITHUB_TOKEN: ${{ github.token }}", resume)
        self.assertIn("python3 -m validation.worker_control_resume", resume)

        run = step_block(self.proof, "Run the pinned production worker controller proof")
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", run)
        self.assertIn("scripts/check-worker-control full", run)
        self.assertIn("--cache-dir target/worker-control-tools", run)
        self.assertIn("target/verification/worker-control-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT", run)
        self.assertIn("--provision", run)
        self.assertIn("--resume-from", run)
        self.assertIn("steps.worker-control-resume.outputs.resume_dir", run)
        self.assertNotIn("continue-on-error", run)

        artifact = step_block(self.proof, "Preserve worker controller proof receipts and logs")
        self.assertIn("always()", artifact)
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", artifact)
        self.assertIn("target/verification/worker-control-${{ github.run_id }}-${{ github.run_attempt }}/", artifact)
        self.assertIn("if-no-files-found: error", artifact)
        self.assertRegex(artifact, r"retention-days:\s*90\b")

        provenance = step_block(self.proof, "Attach worker proof resume provenance")
        self.assertIn("always()", provenance)
        self.assertIn('test -s "$output_dir/receipt.json"', provenance)
        self.assertIn('cp -- "$origin" "$output_dir/resume-origin.json"', provenance)

    def test_promotion_gate_requires_worker_proof_success(self) -> None:
        self.assertIn("- worker_control_proof", self.gate)
        self.assertIn("WORKER_CONTROL_RESULT: ${{ needs.worker_control_proof.result }}", self.gate)
        self.assertIn('if [ "$WORKER_CONTROL_RESULT" != success ]; then', self.gate)
        self.assertIn('echo "The required production worker controller proof ended with:', self.gate)


class WorkerControlResumeFixtures(unittest.TestCase):
    """Exercise the Forgejo run/job shape and selective proof evidence hydration."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.fixture_scratch = ROOT / "target/worker-control-resume-fixtures"
        cls.fixture_scratch.mkdir(parents=True, exist_ok=True)

    def setUp(self) -> None:
        from validation import worker_control_resume as resume
        trust = patch.object(resume, "invocation_trust", return_value={"decision": "fixture_only"})
        trust.start()
        self.addCleanup(trust.stop)
        self.scope = {
            "repository": 1,
            "pr": 1260,
            "branch": "codex/admission-rust-refinement",
            "base": "main",
            "workflow": "main-fast-lane.yml",
        }
        self.run = {
            "id": 5811,
            "workflow_id": "main-fast-lane.yml",
            "status": "running",
            "event": "pull_request",
            "prettyref": "#1260",
            "commit_sha": "6fb7996f2b44c98ed0c33a2eb0695a42c023d766",
            "repository": {"id": 1},
            "event_payload": json.dumps({
                "number": 1260,
                "repository": {"id": 1},
                "pull_request": {
                    "number": 1260,
                    "head": {"ref": "codex/admission-rust-refinement",
                             "sha": "6fb7996f2b44c98ed0c33a2eb0695a42c023d766",
                             "repo": {"id": 1}},
                    "base": {"ref": "main", "repo": {"id": 1}},
                },
            }),
        }
        self.job = {
            "id": 56598,
            "name": "production worker controller proof",
            "status": "failure",
            "attempt": 1,
            "task_id": 22458,
            "repo_id": 1,
            "run_id": 5811,
        }
        self.artifact = {
            "id": 7811,
            "name": "worker-control-proof-5811-1",
            "run_id": 5811,
            "size_in_bytes": 15_143_261,
            "expired": False,
        }

    def api(self, run=None, job=None, artifact=None, upload_sha=None):
        run = self.run if run is None else run
        job = self.job if job is None else job
        artifact = self.artifact if artifact is None else artifact
        commit = run["commit_sha"]
        upload_sha = "a" * 64 if upload_sha is None else upload_sha
        return _WorkerProofApi(run, job, artifact,
                               f"{commit}:refs/remotes/pull/1260/head\n"
                               f"SHA256 hash of uploaded artifact zip is {upload_sha}\n"
                               f"Artifact {artifact['name']} has been successfully uploaded! "
                               f"Final size is {artifact['size_in_bytes']} bytes. "
                               f"Artifact ID is {artifact['id']}\n")

    def test_failed_and_successful_forgejo_jobs_are_eligible(self) -> None:
        from validation import worker_control_resume as resume

        for status in ("failure", "success"):
            with self.subTest(status=status):
                job = dict(self.job, status=status)
                candidate = resume.artifact_for_run(self.api(job=job), self.scope, self.run, 5900, self.run["commit_sha"], ROOT)
                self.assertIsNotNone(candidate)
                self.assertEqual(candidate[1]["status"], status)

    def test_run_selection_records_forgejo_job_status(self) -> None:
        from validation import worker_control_resume as resume

        archive = b"archive bytes"
        artifact = dict(self.artifact, size_in_bytes=len(archive))
        api = self.api(artifact=artifact, upload_sha=hashlib.sha256(archive).hexdigest())
        with patch.object(resume, "archive_bytes", return_value=archive):
            candidate = resume.find_prior(api, self.scope, 5900, self.run["commit_sha"], ROOT)
        self.assertEqual(candidate["job_result"], "failure")
        self.assertEqual(candidate["attempt"], 1)
        self.assertEqual(candidate["run_id"], 5811)

    def test_minimal_main_context_resolves_canonical_pr_before_selection(self) -> None:
        from validation import worker_control_resume as resume

        archive = b"context fixture archive"
        artifact = dict(self.artifact, size_in_bytes=len(archive))
        api = self.api(artifact=artifact, upload_sha=hashlib.sha256(archive).hexdigest())
        api.repository = {"id": 1}
        api.pull_number = 1260
        api.pull = {
            "state": "open",
            "head": {"ref": self.scope["branch"], "sha": self.run["commit_sha"],
                     "repo": {"id": 1}},
            "base": {"ref": "main", "repo": {"id": 1}},
        }
        minimal_scope = {"repository": 1, "pr": 1260}
        with patch.object(resume.adoption, "api_context",
                          return_value=(api, minimal_scope, self.run["commit_sha"], 5900)):
            resolved_api, scope, commit, current_run = resume.current_context()
        self.assertEqual(scope, {
            "repository": 1, "pr": 1260, "branch": "codex/admission-rust-refinement",
            "base": "main", "workflow": "main-fast-lane.yml",
        })
        self.assertEqual((commit, current_run), (self.run["commit_sha"], 5900))
        with patch.object(resume, "archive_bytes", return_value=archive):
            candidate = resume.find_prior(resolved_api, scope, current_run, commit, ROOT)
        self.assertEqual(candidate["artifact_id"], 7811)
        self.assertEqual(candidate["job_result"], "failure")

    def test_current_run_wrong_pr_and_unassigned_jobs_are_rejected(self) -> None:
        from validation import python_unit_receipts as receipts
        from validation import worker_control_resume as resume

        api = self.api()
        self.assertIsNone(resume.artifact_for_run(api, self.scope, self.run, 5811, self.run["commit_sha"], ROOT))
        self.assertEqual(api.run_reads, 0)

        wrong_pr = dict(self.scope, pr=1261)
        with self.assertRaises(receipts.ReceiptError):
            resume.artifact_for_run(self.api(), wrong_pr, self.run, 5900, self.run["commit_sha"], ROOT)

        unassigned = dict(self.job, task_id=0)
        self.assertIsNone(resume.artifact_for_run(self.api(job=unassigned), self.scope, self.run, 5900, self.run["commit_sha"], ROOT))

    def test_list_prefilter_avoids_detail_reads_for_wrong_pr_or_event(self) -> None:
        from validation import worker_control_resume as resume

        for changed in ({"prettyref": "#999"}, {"event": "push"}):
            api = self.api(run=dict(self.run, **changed))
            with patch.object(resume, "archive_bytes") as download:
                self.assertIsNone(resume.find_prior(api, self.scope, 5900, self.run["commit_sha"], ROOT))
            self.assertEqual(api.run_reads, 0)
            download.assert_not_called()

    def test_unreviewed_invocation_rejects_before_artifact_download(self) -> None:
        from validation import python_unit_receipts as receipts
        from validation import worker_control_resume as resume

        api = self.api()
        with patch.object(resume, "invocation_trust", side_effect=receipts.ReceiptError("unreviewed")), \
                patch.object(resume, "archive_bytes") as download:
            self.assertIsNone(resume.find_prior(api, self.scope, 5900, self.run["commit_sha"], ROOT))
        download.assert_not_called()

    def test_safe_archive_extracts_only_receipts_logs_and_source(self) -> None:
        from validation import worker_control_resume as resume

        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("t1_example.json", b"{}")
            archive.writestr("t1_example.log", b"VERIFICATION: SUCCESSFUL\n")
            archive.writestr("source/Cargo.toml", b"[package]\n")
            archive.writestr("source/src/lib.rs", b"pub fn safe() {}\n")
            archive.writestr("build/ignored.bin", b"not needed")
        with tempfile.TemporaryDirectory(dir=self.fixture_scratch) as scratch:
            destination = Path(scratch) / "prior"
            destination.mkdir()
            count, byte_count = resume.extract_evidence(buffer.getvalue(), destination)
            self.assertEqual(count, 4)
            self.assertEqual(byte_count, sum(len(value) for value in
                                             (b"{}", b"VERIFICATION: SUCCESSFUL\n",
                                              b"[package]\n", b"pub fn safe() {}\n")))
            self.assertTrue((destination / "source/src/lib.rs").is_file())
            self.assertFalse((destination / "build/ignored.bin").exists())

    def test_unsafe_archive_member_is_rejected(self) -> None:
        from validation import python_unit_receipts as receipts
        from validation import worker_control_resume as resume

        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr("../escape.log", b"bad")
        with tempfile.TemporaryDirectory(dir=self.fixture_scratch) as scratch:
            with self.assertRaises(receipts.ReceiptError):
                resume.extract_evidence(buffer.getvalue(), Path(scratch))


class WorkerControlInvocationTrustTests(unittest.TestCase):
    """Real Git blob identities, independent of receipt self-reports."""

    def setUp(self) -> None:
        from validation import worker_control_resume as resume

        self.resume = resume
        scratch = ROOT / "target/worker-control-invocation-fixtures"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.workspace = Path(temporary.name)
        self.git("init", "--quiet")
        for path in resume.INVOCATION_PATHS:
            target = self.workspace / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(("original " + path + "\n\n").encode())
        self.prior = self.commit()
        self.prior_digests = resume.invocation_digests(self.prior, self.workspace)
        (self.workspace / resume.INVOCATION_PATHS[0]).write_bytes(b"changed honest runner\n")
        self.current = self.commit()
        self.policy = {"schema_version": 1, "entries": []}
        self.write_policy()

    def git(self, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=self.workspace,
                                       stderr=subprocess.DEVNULL).decode().strip()

    def commit(self):
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "--quiet", "-m", "fixture invocation")
        return self.git("rev-parse", "HEAD")

    def write_policy(self):
        (self.workspace / self.resume.REVIEWED_POLICY).write_text(json.dumps(self.policy) + "\n")

    def entry(self):
        return {"source_commit": self.prior, "digests": self.prior_digests,
                "review_basis": {"original_review": "fixture original review",
                                 "delta_review": "fixture delta review"},
                "coordinator_attestation": "fixture exact historical compatibility decision"}

    def test_changed_actual_runner_rejects_without_reviewed_exception(self):
        from validation.python_unit_receipts import ReceiptError

        with self.assertRaises(ReceiptError):
            self.resume.invocation_trust(self.prior, self.current, self.workspace)

    def test_identical_actual_tuple_is_strict_default(self):
        result = self.resume.invocation_trust(self.current, self.current, self.workspace)
        self.assertEqual(result["decision"], "identical_invocation")
        self.assertIsNone(result["reviewed_compatibility"])
        self.assertEqual(result["prior_digests"], result["current_digests"])

    def test_exact_reviewed_tuple_records_actual_git_bytes_and_basis(self):
        self.policy["entries"] = [self.entry()]
        self.write_policy()
        result = self.resume.invocation_trust(self.prior, self.current, self.workspace)
        self.assertEqual(result["decision"], "explicit_reviewed_compatibility")
        self.assertEqual(result["reviewed_compatibility"], self.entry())
        self.assertEqual(result["prior_digests"], self.prior_digests)
        self.assertEqual(result["current_digests"], self.resume.invocation_digests(self.current, self.workspace))
        self.assertEqual(result["policy_sha256"], hashlib.sha256(
            (self.workspace / self.resume.REVIEWED_POLICY).read_bytes()).hexdigest())
        path = self.resume.INVOCATION_PATHS[0]
        original = ("original " + path + "\n\n").encode()
        self.assertEqual(result["prior_digests"][path], hashlib.sha256(original).hexdigest())
        self.assertNotEqual(result["prior_digests"][path], hashlib.sha256(original.rstrip()).hexdigest())

    def test_reviewed_runner_does_not_authorize_changed_workflow_or_manifest(self):
        from validation.python_unit_receipts import ReceiptError

        for path in self.resume.INVOCATION_PATHS[1:]:
            for original in self.resume.INVOCATION_PATHS:
                (self.workspace / original).write_bytes(("original " + original + "\n\n").encode())
            (self.workspace / path).write_bytes(b"unreviewed historical invocation\n")
            changed = self.commit()
            self.policy["entries"] = [dict(self.entry(), source_commit=changed)]
            self.write_policy()
            with self.subTest(path=path), self.assertRaises(ReceiptError):
                self.resume.invocation_trust(changed, self.current, self.workspace)

    def test_policy_schema_duplicates_and_bad_digests_fail_closed(self):
        from validation.python_unit_receipts import ReceiptError

        entry = self.entry()
        bad_entry = dict(entry, digests=dict(entry["digests"], **{self.resume.INVOCATION_PATHS[0]: "bad"}))
        for policy in ({"schema_version": 2, "entries": []},
                       {"schema_version": True, "entries": []},
                       {"schema_version": 1, "entries": [], "unknown": 1},
                       {"schema_version": 1, "entries": [entry, entry]},
                       {"schema_version": 1, "entries": [bad_entry]},
                       {"schema_version": 1, "entries": [dict(entry, unknown=True)]}):
            self.policy = policy
            self.write_policy()
            with self.subTest(policy=policy), self.assertRaises(ReceiptError):
                self.resume.invocation_trust(self.current, self.current, self.workspace)
        (self.workspace / self.resume.REVIEWED_POLICY).write_text(
            '{"schema_version":1,"schema_version":1,"entries":[]}')
        with self.assertRaises(ReceiptError):
            self.resume.invocation_trust(self.current, self.current, self.workspace)

    def test_unavailable_or_invalid_commit_and_oversized_blob_fail_closed(self):
        from validation.python_unit_receipts import ReceiptError

        for commit in ("HEAD", "0" * 40, "a" * 39, "a" * 40 + ":path"):
            with self.subTest(commit=commit), self.assertRaises(ReceiptError):
                self.resume.invocation_digests(commit, self.workspace)
        (self.workspace / self.resume.INVOCATION_PATHS[0]).write_bytes(
            b"x" * (self.resume.MAX_INVOCATION_BYTES + 1))
        oversized = self.commit()
        with self.assertRaises(ReceiptError):
            self.resume.invocation_digests(oversized, self.workspace)


class _WorkerProofApi:
    def __init__(self, run, job, artifact, log):
        self.run = run
        self.job = job
        self.artifact = artifact
        self.log = log.encode()
        self.run_reads = 0

    def get(self, path):
        self.run_reads += 1
        if path == "":
            return getattr(self, "repository", self.run["repository"])
        if path == f"/pulls/{getattr(self, 'pull_number', 1260)}":
            return self.pull
        self.assert_path(path, f"/actions/runs/{self.run['id']}")
        return self.run

    def pages(self, path, query=None, field=None):
        if path == "/actions/runs":
            return [self.run]
        if path.endswith("/jobs"):
            return [self.job]
        if path == "/actions/artifacts":
            return [self.artifact] if self.artifact["name"] == query["name"] else []
        raise AssertionError(f"unexpected API path: {path}")

    def bytes(self, path):
        self.assert_path(path, f"/actions/jobs/{self.job['id']}/logs")
        return self.log

    @staticmethod
    def assert_path(actual, expected):
        if actual != expected:
            raise AssertionError(f"unexpected API path: {actual}")


if __name__ == "__main__":
    unittest.main()
