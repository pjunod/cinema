from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation.ci_lane_receipt import LaneReceiptError, build_receipt, main


class CiLaneReceiptCase(unittest.TestCase):
    def environment(self) -> dict[str, str]:
        return {
            "GITHUB_REPOSITORY": "pjunod/plurx",
            "GITHUB_SHA": "a" * 40,
            "GITHUB_WORKFLOW_REF": (
                "pjunod/plurx/.github/workflows/ci.yml@refs/pull/1/merge"
            ),
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "1",
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
            "evidence_name": None,
            "evidence_digest": None,
            "evidence_bytes": None,
            "evidence_build_sha": None,
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

    def test_non_recovery_success_still_accepts_a_workflow_rerun(self):
        environment = self.environment()
        environment["GITHUB_RUN_ATTEMPT"] = "2"

        receipt = self.receipt(environment=environment)

        self.assertEqual(receipt["result"], "success")
        self.assertEqual(receipt["run_attempt"], "2")

    def test_receipt_accepts_every_store_rollout_lane(self):
        for lane in (
            "cluster-store",
            "cluster-store-legacy",
            "cluster-store-backstop",
        ):
            with self.subTest(lane=lane):
                self.assertEqual(self.receipt(lane=lane)["lane"], lane)

    def test_receipt_accepts_the_transport_recovery_lane(self):
        environment = self.environment()
        environment["GITHUB_JOB"] = "cluster_transport_recovery"

        receipt = self.receipt(
            environment=environment,
            lane="cluster-transport-recovery",
            commands=["make cluster-transport-recovery-check"],
            log_name="cluster-transport-recovery.log",
            evidence_name="cluster-transport-recovery.json",
            evidence_digest="d" * 64,
            evidence_bytes=5678,
            evidence_build_sha="a" * 40,
        )

        self.assertEqual(receipt["lane"], "cluster-transport-recovery")
        self.assertEqual(
            receipt["commands"], ["make cluster-transport-recovery-check"]
        )
        self.assertEqual(
            receipt["evidence"],
            {
                "name": "cluster-transport-recovery.json",
                "sha256": "d" * 64,
                "bytes": 5678,
                "build_sha": "a" * 40,
            },
        )

    def test_successful_transport_recovery_refuses_a_workflow_rerun(self):
        environment = self.environment()
        environment["GITHUB_JOB"] = "cluster_transport_recovery"
        environment["GITHUB_RUN_ATTEMPT"] = "2"

        with self.assertRaisesRegex(
            LaneReceiptError, "must come from workflow run attempt 1"
        ):
            self.receipt(
                environment=environment,
                lane="cluster-transport-recovery",
                commands=["make cluster-transport-recovery-check"],
                log_name="cluster-transport-recovery.log",
                evidence_name="cluster-transport-recovery.json",
                evidence_digest="d" * 64,
                evidence_bytes=5678,
                evidence_build_sha="a" * 40,
            )

    def test_successful_transport_recovery_requires_exact_tree_evidence(self):
        environment = self.environment()
        environment["GITHUB_JOB"] = "cluster_transport_recovery"
        base = {
            "environment": environment,
            "lane": "cluster-transport-recovery",
            "commands": ["make cluster-transport-recovery-check"],
            "log_name": "cluster-transport-recovery.log",
        }

        with self.assertRaisesRegex(LaneReceiptError, "requires retained evidence"):
            self.receipt(**base)
        with self.assertRaisesRegex(LaneReceiptError, "does not match tested sha"):
            self.receipt(
                **base,
                evidence_name="cluster-transport-recovery.json",
                evidence_digest="d" * 64,
                evidence_bytes=5678,
                evidence_build_sha="e" * 40,
            )

    def test_failed_transport_recovery_accepts_an_absent_evidence_artifact(self):
        environment = self.environment()
        environment["GITHUB_JOB"] = "cluster_transport_recovery"
        environment["GITHUB_RUN_ATTEMPT"] = "2"

        receipt = self.receipt(
            environment=environment,
            lane="cluster-transport-recovery",
            result="failure",
            commands=["make cluster-transport-recovery-check"],
            log_name="cluster-transport-recovery.log",
        )

        self.assertNotIn("evidence", receipt)
        self.assertEqual(receipt["run_attempt"], "2")

    def test_cli_binds_successful_evidence_and_accepts_absent_failure_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "cluster-transport-recovery.log"
            evidence = root / "cluster-transport-recovery.json"
            receipt_path = root / "receipt.json"
            log.write_bytes(b"campaign output\n")
            evidence_contents = json.dumps(
                {"build_sha": "a" * 40, "cycles": []}, sort_keys=True
            ).encode("utf-8")
            evidence.write_bytes(evidence_contents)
            common = [
                "--output",
                str(receipt_path),
                "--log",
                str(log),
                "--evidence",
                str(evidence),
                "--lane",
                "cluster-transport-recovery",
                "--command",
                "make cluster-transport-recovery-check",
            ]

            with (
                patch.dict(os.environ, self.environment(), clear=True),
                patch(
                    "validation.ci_lane_receipt.git_object",
                    side_effect=lambda _repository, revision: (
                        "a" * 40 if revision == "HEAD^{commit}" else "c" * 40
                    ),
                ),
            ):
                self.assertEqual(main([*common, "--result", "success"]), 0)
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
                self.assertEqual(
                    receipt["evidence"],
                    {
                        "name": evidence.name,
                        "sha256": hashlib.sha256(evidence_contents).hexdigest(),
                        "bytes": len(evidence_contents),
                        "build_sha": "a" * 40,
                    },
                )

                evidence.unlink()
                self.assertEqual(main([*common, "--result", "failure"]), 0)
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
                self.assertNotIn("evidence", receipt)

                evidence.write_bytes(b'{"build_sha":')
                self.assertEqual(main([*common, "--result", "failure"]), 0)
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
                self.assertNotIn("evidence", receipt)

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
            {"evidence_name": "cluster-store.json"},
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
