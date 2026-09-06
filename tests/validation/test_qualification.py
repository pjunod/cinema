from __future__ import annotations

import unittest

from validation.qualification import (
    REQUIRED_JOBS,
    QualificationError,
    build_receipt,
)


class QualificationReceiptCase(unittest.TestCase):
    def environment(self) -> dict[str, str]:
        return {
            "GITHUB_REPOSITORY": "pjunod/plurx",
            "GITHUB_HEAD_REF": "effort/playback-control",
            "GITHUB_BASE_REF": "main",
            "GITHUB_SHA": "a" * 40,
            "GITHUB_WORKFLOW_REF": "pjunod/plurx/.github/workflows/ci.yml@refs/pull/1/merge",
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "2",
            "PLURX_PULL_REQUEST": "42",
            "PLURX_HEAD_SHA": "b" * 40,
            "PLURX_BASE_SHA": "c" * 40,
        }

    def results(self) -> dict[str, str]:
        return {job: "success" for job in REQUIRED_JOBS}

    def test_receipt_names_the_exact_tested_tree_and_job_results(self):
        receipt = build_receipt(
            self.environment(), self.results(), "a" * 40, "d" * 40
        )

        self.assertEqual(receipt["kind"], "effort-qualification")
        self.assertEqual(receipt["pull_request"], 42)
        self.assertEqual(receipt["tested_sha"], "a" * 40)
        self.assertEqual(receipt["tested_tree"], "d" * 40)
        self.assertEqual(receipt["jobs"], dict(sorted(self.results().items())))

    def test_receipt_accepts_conflict_resolution_integration_head(self):
        environment = self.environment()
        environment["GITHUB_HEAD_REF"] = "integration/playback-control-into-main"

        receipt = build_receipt(
            environment, self.results(), "a" * 40, "d" * 40
        )

        self.assertEqual(
            receipt["head_ref"], "integration/playback-control-into-main"
        )

    def test_receipt_refuses_non_qualification_or_non_main_refs(self):
        for field, value in (
            ("GITHUB_HEAD_REF", "codex/task"),
            ("GITHUB_HEAD_REF", "integration/-into-main"),
            ("GITHUB_HEAD_REF", "integration/playback-control"),
            ("GITHUB_BASE_REF", "effort/playback-control"),
        ):
            with self.subTest(field=field):
                environment = self.environment()
                environment[field] = value
                with self.assertRaises(QualificationError):
                    build_receipt(
                        environment, self.results(), "a" * 40, "d" * 40
                    )

    def test_receipt_refuses_skipped_failed_or_cancelled_fan_out(self):
        for result in ("skipped", "failure", "cancelled"):
            with self.subTest(result=result):
                results = self.results()
                results["cluster_wal"] = result
                with self.assertRaisesRegex(
                    QualificationError, f"cluster_wal={result}"
                ):
                    build_receipt(
                        self.environment(), results, "a" * 40, "d" * 40
                    )

    def test_receipt_refuses_a_missing_fan_out_job(self):
        results = self.results()
        del results["android_device"]

        with self.assertRaisesRegex(
            QualificationError, "missing required jobs: android_device"
        ):
            build_receipt(
                self.environment(), results, "a" * 40, "d" * 40
            )

    def test_receipt_refuses_missing_run_metadata(self):
        environment = self.environment()
        del environment["GITHUB_WORKFLOW_REF"]

        with self.assertRaisesRegex(
            QualificationError,
            "qualification metadata is missing: GITHUB_WORKFLOW_REF",
        ):
            build_receipt(
                environment, self.results(), "a" * 40, "d" * 40
            )


if __name__ == "__main__":
    unittest.main()
