"""Failed-job retries preserve published run artifacts and use workflow job IDs."""

import contextlib
import importlib.machinery
import importlib.util
import io
import sys
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("ci_failures", str(ROOT / "scripts/ci-failures"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
CI = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(CI)
CONFIG = {"forge_url": "https://forge.example/", "repo": "owner/repo", "token": "test-token"}
FAILED = {"id": 700, "task_id": 300, "run_id": 70, "name": "compile", "status": "failure"}
RUN = {"id": 70, "status": "failure"}


class RetryJobTests(unittest.TestCase):
    def test_retry_posts_selected_job_id_and_accepts_empty_204(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = 204
        response.read.return_value = b""
        with mock.patch.object(CI.urllib.request, "urlopen", return_value=response) as send:
            self.assertIsNone(CI.api(CONFIG, "actions/jobs/700/rerun", method="POST"))
        request = send.call_args.args[0]
        self.assertEqual(request.full_url, "https://forge.example/api/v1/repos/owner/repo/actions/jobs/700/rerun")
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.get_header("Authorization"), "token test-token")
        response.read.assert_not_called()  # HTTP 204 has no JSON body.

    def test_failed_job_is_checked_then_retried_without_using_its_task_id(self):
        with mock.patch.object(CI, "api", side_effect=[FAILED, RUN, [], None]) as api, contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(CI.retry_job(CONFIG, 700, False), 0)
        self.assertEqual(api.call_args_list, [
            mock.call(CONFIG, "actions/jobs/700"),
            mock.call(CONFIG, "actions/runs/70"),
            mock.call(CONFIG, "actions/runs/70/artifacts?limit=50&page=1"),
            mock.call(CONFIG, "actions/jobs/700/rerun", method="POST"),
        ])
        self.assertIn("Retry requested", out.getvalue())
        self.assertIn("dependent jobs also rerun", out.getvalue())

    def test_preview_checks_the_job_but_never_posts(self):
        with mock.patch.object(CI, "api", side_effect=[FAILED, RUN, []]) as api, contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(CI.retry_job(CONFIG, 700, True), 0)
        self.assertEqual(api.call_args_list, [
            mock.call(CONFIG, "actions/jobs/700"), mock.call(CONFIG, "actions/runs/70"),
            mock.call(CONFIG, "actions/runs/70/artifacts?limit=50&page=1"),
        ])
        self.assertIn("Would retry", out.getvalue())
        self.assertNotIn("Retry requested", out.getvalue())

    def test_nonfailed_jobs_are_never_retried(self):
        for status in ["success", "running", "waiting", "blocked", "cancelled", "skipped", None]:
            with self.subTest(status=status), mock.patch.object(CI, "api", return_value={**FAILED, "status": status}) as api, contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(CI.retry_job(CONFIG, 700, False), 1)
                api.assert_called_once_with(CONFIG, "actions/jobs/700")
                self.assertIn("refusing", err.getvalue())

    def test_api_errors_do_not_report_success_or_echo_server_credentials(self):
        for status in [400, 403, 404, 409, 500]:
            error = urllib.error.HTTPError("https://forge.example", status, "private credential", {}, io.BytesIO(b"private credential"))
            with self.subTest(status=status), mock.patch.object(CI, "api", side_effect=[FAILED, RUN, [], error]) as api, contextlib.redirect_stderr(io.StringIO()) as err, contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(CI.retry_job(CONFIG, 700, False), 1)
                self.assertEqual(api.call_count, 4)
                self.assertIn(f"HTTP {status}", err.getvalue())
                self.assertNotIn("private credential", err.getvalue())
                self.assertNotIn("Retry requested", out.getvalue())

    def test_network_failure_is_not_automatically_retried(self):
        with mock.patch.object(CI, "api", side_effect=[FAILED, RUN, [], urllib.error.URLError("private detail")]) as api, contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(CI.retry_job(CONFIG, 700, False), 1)
        self.assertEqual(api.call_count, 4)
        self.assertIn("check its status", err.getvalue())
        self.assertNotIn("private detail", err.getvalue())

    def test_unexpected_success_status_is_not_accepted_as_a_retry(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        with mock.patch.object(CI.urllib.request, "urlopen", return_value=response):
            with self.assertRaisesRegex(RuntimeError, "expected HTTP 204"):
                CI.api(CONFIG, "actions/jobs/700/rerun", method="POST")


class ArtifactPreservationTests(unittest.TestCase):
    def invoke(self, responses, dry_run=False):
        with mock.patch.object(CI, "api", side_effect=responses) as api, contextlib.redirect_stderr(io.StringIO()) as err, contextlib.redirect_stdout(io.StringIO()) as out:
            result = CI.retry_job(CONFIG, 700, dry_run)
        return result, api.call_args_list, err.getvalue(), out.getvalue()

    def assert_refused(self, responses, dry_run=False):
        result, calls, error, output = self.invoke(responses, dry_run)
        self.assertEqual(result, 1)
        self.assertTrue(all(call.kwargs.get("method", "GET") != "POST" for call in calls), calls)
        self.assertIn("refusing", error)
        self.assertIn("fresh lane", error)
        self.assertNotIn("Retry requested", output)
        return calls, error

    def test_unexpired_proofs_and_ordinary_data_block_real_retry_and_preview(self):
        for name in ("rust-unit-proof", "ordinary-build-log"):
            for dry_run in (False, True):
                with self.subTest(name=name, dry_run=dry_run):
                    artifact = {"id": 1, "run_id": 70, "expired": False, "name": name}
                    self.assert_refused([FAILED, RUN, [artifact]], dry_run)

    def test_expired_artifacts_allow_failed_setup_retry_after_complete_inventory(self):
        result, calls, error, _ = self.invoke([FAILED, RUN, [{"id": 1, "run_id": 70, "expired": True}], [], None])
        self.assertEqual(result, 0, error)
        self.assertEqual(calls[-1], mock.call(CONFIG, "actions/jobs/700/rerun", method="POST"))

    def test_unknown_job_or_active_mismatched_run_metadata_never_reaches_post(self):
        for job in (None, {**FAILED, "id": 701}, {**FAILED, "run_id": None}, {**FAILED, "run_id": True}):
            with self.subTest(job=job):
                self.assert_refused([job])
        for run in (None, {}, {**RUN, "id": 71}, {**RUN, "status": "running"}, {**RUN, "status": None}):
            with self.subTest(run=run):
                self.assert_refused([FAILED, run])

    def test_unknown_artifact_schema_or_expiry_is_not_treated_as_an_empty_inventory(self):
        for artifacts in (None, {}, {"artifacts": []}, [None], [{"id": 1, "run_id": 70}],
                          [{"id": 1, "run_id": 71, "expired": True}],
                          [{"id": 1, "run_id": 70, "expired": "true"}]):
            with self.subTest(artifacts=artifacts):
                self.assert_refused([FAILED, RUN, artifacts])

    def test_listing_failures_are_closed_and_do_not_echo_private_response_details(self):
        for failure in (urllib.error.HTTPError("https://forge.example", 403, "private body", {}, io.BytesIO(b"private body")),
                        urllib.error.URLError("private body"), TimeoutError("private body"), ValueError("private body")):
            with self.subTest(failure=type(failure).__name__):
                calls, error = self.assert_refused([FAILED, RUN, failure])
                self.assertEqual(len(calls), 3)
                self.assertNotIn("private body", error)

    def test_artifact_on_later_page_blocks_and_incomplete_pagination_never_allows_post(self):
        expired = [{"id": i, "run_id": 70, "expired": True} for i in range(1, 51)]
        for first_page in (expired, expired[:1]):
            with self.subTest(first_page_size=len(first_page)):
                calls, _ = self.assert_refused([FAILED, RUN, first_page, [{"id": 51, "run_id": 70, "expired": False}]])
                self.assertEqual(calls[-1], mock.call(CONFIG, "actions/runs/70/artifacts?limit=50&page=2"))
        with mock.patch.object(CI, "ARTIFACT_PAGES", 1):
            self.assert_refused([FAILED, RUN, expired])


class CurrentJobReportTests(unittest.TestCase):
    def test_commit_listing_uses_run_ids_and_current_jobs_not_task_history(self):
        runs = [{"id": 70, "run_number": 999, "commit_sha": "abc123"}, {"id": 71, "commit_sha": "def456"}]
        current = {**FAILED, "status": "success", "task_id": 301}
        with mock.patch.object(CI, "api", side_effect=[{"workflow_runs": runs}, [current]]) as api:
            self.assertEqual(CI.jobs_for(CONFIG, "abc", 6), [current])
        self.assertEqual(api.call_args_list, [
            mock.call(CONFIG, "actions/runs?limit=50&page=1"),
            mock.call(CONFIG, "actions/runs/70/jobs"),
        ])

    def test_run_search_continues_to_next_page_and_respects_page_budget(self):
        runs = [{"id": i, "commit_sha": "def456"} for i in range(50)]
        with mock.patch.object(CI, "api", side_effect=[{"workflow_runs": runs}, {"workflow_runs": [{"id": 70, "commit_sha": "abc123"}]}, [FAILED]]) as api:
            self.assertEqual(CI.jobs_for(CONFIG, "abc", 2), [FAILED])
        self.assertEqual(api.call_args_list[1], mock.call(CONFIG, "actions/runs?limit=50&page=2"))
        with mock.patch.object(CI, "api", return_value={"workflow_runs": runs}) as api:
            self.assertEqual(CI.jobs_for(CONFIG, "abc", 1), [])
        self.assertEqual(api.call_count, 1)

    def test_empty_or_null_api_collections_are_a_report_with_no_jobs(self):
        for runs in [[], None]:
            with self.subTest(runs=runs), mock.patch.object(CI, "api", return_value={"workflow_runs": runs}):
                self.assertEqual(CI.jobs_for(CONFIG, "abc", 2), [])
        with mock.patch.object(CI, "api", side_effect=[{"workflow_runs": [{"id": 70, "commit_sha": "abc"}]}, None]):
            self.assertEqual(CI.jobs_for(CONFIG, "abc", 1), [])

    def test_report_prints_job_id_but_reads_log_by_task_id(self):
        with mock.patch.object(sys, "argv", ["ci-failures", "abc"]), mock.patch.object(CI, "load_config", return_value=CONFIG), mock.patch.object(CI.subprocess, "run", return_value=mock.Mock(stdout="abc123\n")), mock.patch.object(CI, "jobs_for", return_value=[FAILED]), mock.patch.object(CI, "read_log", return_value="ERROR: failed") as read_log, contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(CI.main(), 1)
        self.assertRegex(out.getvalue(), r"job 700\s+task 300")
        self.assertIn("run 70", out.getvalue())
        read_log.assert_called_once_with(CONFIG, 300)

    def test_successful_current_attempt_does_not_leave_an_old_failure(self):
        with mock.patch.object(sys, "argv", ["ci-failures", "abc"]), mock.patch.object(CI, "load_config", return_value=CONFIG), mock.patch.object(CI.subprocess, "run", return_value=mock.Mock(stdout="abc123\n")), mock.patch.object(CI, "jobs_for", return_value=[{**FAILED, "status": "success"}]), mock.patch.object(CI, "read_log") as read_log, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(CI.main(), 0)
        read_log.assert_not_called()

    def test_wait_timeout_with_pending_jobs_is_not_success(self):
        with mock.patch.object(sys, "argv", ["ci-failures", "abc", "--wait", "1"]), mock.patch.object(CI, "load_config", return_value=CONFIG), mock.patch.object(CI.subprocess, "run", return_value=mock.Mock(stdout="abc123\n")), mock.patch.object(CI, "jobs_for", return_value=[{**FAILED, "status": "running"}]), mock.patch.object(CI.time, "monotonic", side_effect=[0, 2]), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(CI.main(), 2)

    def test_invalid_retry_selectors_are_rejected_before_loading_credentials(self):
        for args in [["--retry-job", "0"], ["--retry-job", "-1"], ["--retry-job", "700", "--pr", "1"], ["--retry-job", "700", "abc"], ["--retry-job", "700", "--wait", "1"], ["--dry-run"]]:
            with self.subTest(args=args), mock.patch.object(sys, "argv", ["ci-failures", *args]), mock.patch.object(CI, "load_config") as config, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as raised:
                    CI.main()
                self.assertEqual(raised.exception.code, 2)
                config.assert_not_called()

    def test_retry_cli_routes_only_to_the_selected_job(self):
        with mock.patch.object(sys, "argv", ["ci-failures", "--retry-job", "700", "--dry-run"]), mock.patch.object(CI, "load_config", return_value=CONFIG), mock.patch.object(CI, "retry_job", return_value=0) as retry, mock.patch.object(CI, "jobs_for") as report:
            self.assertEqual(CI.main(), 0)
        retry.assert_called_once_with(CONFIG, 700, True)
        report.assert_not_called()


if __name__ == "__main__":
    unittest.main()
