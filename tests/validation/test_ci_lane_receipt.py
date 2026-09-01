from __future__ import annotations

import unittest

from validation.ci_lane_receipt import LaneReceiptError, build_receipt


class CiLaneReceiptCase(unittest.TestCase):
    def environment(self) -> dict[str, str]:
        return {
            "GITHUB_REPOSITORY": "pjunod/plurx",
            "GITHUB_SHA": "a" * 40,
            "GITHUB_WORKFLOW_REF": (
                "pjunod/plurx/.github/workflows/ci.yml@refs/pull/1/merge"
            ),
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_JOB": "cluster_store",
        }

    def receipt(self, **overrides: object) -> dict[str, object]:
        arguments: dict[str, object] = {
            "environment": self.environment(),
            "lane": "cluster-store",
            "result": "success",
            "commands": ["make cluster-store-check"],
            "log_name": "cluster-store.log",
            "log_digest": "b" * 64,
            "log_bytes": 1234,
            "tested_sha": "a" * 40,
            "tested_tree": "c" * 40,
        }
        arguments.update(overrides)
        return build_receipt(**arguments)  # type: ignore[arg-type]

    def test_receipt_binds_the_result_commands_log_and_exact_tree(self):
        receipt = self.receipt()

        self.assertEqual(receipt["kind"], "ci-lane")
        self.assertEqual(receipt["lane"], "cluster-store")
        self.assertEqual(receipt["result"], "success")
        self.assertEqual(receipt["commands"], ["make cluster-store-check"])
        self.assertEqual(
            receipt["log"],
            {"name": "cluster-store.log", "sha256": "b" * 64, "bytes": 1234},
        )
        self.assertEqual(receipt["tested_sha"], "a" * 40)
        self.assertEqual(receipt["tested_tree"], "c" * 40)

    def test_receipt_accepts_a_failed_lane_for_postmortem_evidence(self):
        receipt = self.receipt(result="failure")

        self.assertEqual(receipt["result"], "failure")

    def test_receipt_accepts_every_store_rollout_lane(self):
        for lane in (
            "cluster-store",
            "cluster-store-legacy",
            "cluster-store-backstop",
        ):
            with self.subTest(lane=lane):
                self.assertEqual(self.receipt(lane=lane)["lane"], lane)

    def test_receipt_refuses_unknown_lanes_results_or_empty_commands(self):
        for overrides in (
            {"lane": "cluster-wal"},
            {"result": "skipped"},
            {"commands": []},
        ):
            with self.subTest(overrides=overrides):
                with self.assertRaises(LaneReceiptError):
                    self.receipt(**overrides)

    def test_receipt_refuses_wrong_tree_metadata_or_malformed_log(self):
        for overrides in (
            {"tested_sha": "d" * 40},
            {"log_name": "target/cluster-store.log"},
            {"log_digest": "B" * 64},
            {"log_bytes": -1},
        ):
            with self.subTest(overrides=overrides):
                with self.assertRaises(LaneReceiptError):
                    self.receipt(**overrides)

    def test_receipt_refuses_missing_workflow_metadata(self):
        environment = self.environment()
        del environment["GITHUB_RUN_ID"]

        with self.assertRaisesRegex(LaneReceiptError, "GITHUB_RUN_ID"):
            self.receipt(environment=environment)


if __name__ == "__main__":
    unittest.main()
