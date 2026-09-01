from __future__ import annotations

import copy
import json
from pathlib import Path
import tempfile
import unittest

from validation.store_shard import (
    ALGORITHM,
    StoreShardError,
    assigned_tests,
    record_preexecution_failure,
    run_shard,
    validate_receipts,
)


class StoreShardCase(unittest.TestCase):
    def environment(self) -> dict[str, str]:
        return {
            "GITHUB_REPOSITORY": "pjunod/plurx",
            "GITHUB_SHA": "a" * 40,
            "GITHUB_WORKFLOW_REF": (
                "pjunod/plurx/.github/workflows/ci.yml@refs/pull/1/merge"
            ),
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "2",
            "GITHUB_JOB": "cluster_store_shard",
        }

    def fixture(self, root: Path) -> tuple[Path, Path, Path, list[str], list[str]]:
        inventory = [f"contract_case_{index}" for index in range(16)]
        ignored = ["contract_case_3", "contract_case_11"]
        counter = root / "run-invocations.jsonl"
        binary = root / "store_contract"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import json\n"
            "from pathlib import Path\n"
            "import sys\n"
            f"INVENTORY = {inventory!r}\n"
            f"IGNORED = {ignored!r}\n"
            f"COUNTER = Path({str(counter)!r})\n"
            "arguments = sys.argv[1:]\n"
            "if '--list' in arguments:\n"
            "    selected = IGNORED if '--ignored' in arguments else INVENTORY\n"
            "    for name in selected:\n"
            "        print(f'{name}: test')\n"
            "    raise SystemExit(0)\n"
            "selected = sorted(name for name in INVENTORY if name in arguments)\n"
            "with COUNTER.open('a', encoding='utf-8') as target:\n"
            "    target.write(json.dumps(arguments) + '\\n')\n"
            "print(f'running {len(selected)} tests')\n"
            "for name in selected:\n"
            "    outcome = 'ignored' if name in IGNORED else 'ok'\n"
            "    print(f'test {name} ... {outcome}', flush=True)\n"
            "print('test result: ok')\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        rustc_vv = root / "rustc-vv.txt"
        rustc_vv.write_text(
            "rustc 1.97.1 (fixture)\nrelease: 1.97.1\nhost: x86_64-unknown-linux-gnu\n",
            encoding="utf-8",
        )
        return binary, rustc_vv, counter, inventory, ignored

    def successful_receipts(self, root: Path) -> tuple[list[dict[str, object]], Path]:
        binary, rustc_vv, counter, inventory, _ignored = self.fixture(root)
        receipts: list[dict[str, object]] = []
        for index in range(2):
            receipt = run_shard(
                binary,
                rustc_vv,
                root / f"shard-{index}-receipt.json",
                root / f"shard-{index}.log",
                environment=self.environment(),
                tested_sha="a" * 40,
                tested_tree="b" * 40,
                shard_count=2,
                shard_index=index,
                require_x86_64=False,
            )
            self.assertEqual(
                receipt["assignment"]["tests"],  # type: ignore[index]
                assigned_tests(inventory, 2, index),
            )
            receipt["binary"]["elf_machine"] = 62  # type: ignore[index]
            receipts.append(receipt)
        return receipts, counter

    def test_two_shards_run_one_exact_invocation_and_validate_dynamic_union(self):
        with tempfile.TemporaryDirectory() as directory:
            receipts, counter = self.successful_receipts(Path(directory))

            aggregate = validate_receipts(receipts)
            invocations = [
                json.loads(line)
                for line in counter.read_text(encoding="utf-8").splitlines()
            ]

            self.assertEqual(aggregate["kind"], "store-shard-aggregate")
            self.assertEqual(aggregate["algorithm"], ALGORITHM)
            self.assertEqual(aggregate["inventory_count"], 16)
            self.assertEqual(aggregate["ignored_count"], 2)
            self.assertEqual(len(invocations), 2)
            for index, arguments in enumerate(invocations):
                self.assertIn("--exact", arguments)
                self.assertIn("--test-threads=1", arguments)
                assigned = receipts[index]["assignment"]["tests"]  # type: ignore[index]
                self.assertEqual(
                    sorted(argument for argument in arguments if argument in assigned),
                    assigned,
                )
                self.assertEqual(receipts[index]["started"], assigned)
                self.assertEqual(
                    [record["name"] for record in receipts[index]["completed"]],  # type: ignore[index]
                    assigned,
                )

    def test_validator_rejects_wrong_tree_binary_overlap_and_missing_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            receipts, _counter = self.successful_receipts(Path(directory))
            cases: list[tuple[str, list[dict[str, object]], str]] = []

            wrong_tree = copy.deepcopy(receipts)
            wrong_tree[1]["tested_tree"] = "c" * 40
            cases.append(("wrong tree", wrong_tree, "disagree on tested_tree"))

            wrong_binary = copy.deepcopy(receipts)
            wrong_binary[1]["binary"]["sha256"] = "d" * 64  # type: ignore[index]
            cases.append(("wrong binary", wrong_binary, "disagree on binary"))

            overlap = copy.deepcopy(receipts)
            stolen = overlap[0]["assignment"]["tests"][0]  # type: ignore[index]
            overlap[1]["assignment"]["tests"].append(stolen)  # type: ignore[index]
            overlap[1]["assignment"]["tests"].sort()  # type: ignore[index]
            cases.append(("overlap", overlap, "does not match sha256-mod-v1"))

            incomplete = copy.deepcopy(receipts)
            incomplete[0]["completed"].pop()  # type: ignore[union-attr]
            incomplete[0]["started"].pop()  # type: ignore[union-attr]
            cases.append(("incomplete", incomplete, "did not complete every"))

            for name, candidate, message in cases:
                with self.subTest(name=name):
                    with self.assertRaisesRegex(StoreShardError, message):
                        validate_receipts(candidate)

    def test_preexecution_failure_retains_tree_and_fails_aggregation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = record_preexecution_failure(
                root / "failure-receipt.json",
                root / "failure.log",
                environment=self.environment(),
                tested_sha="a" * 40,
                tested_tree="b" * 40,
                shard_count=2,
                shard_index=0,
                error="binary build failed",
            )

            self.assertEqual(receipt["tested_tree"], "b" * 40)
            self.assertEqual(receipt["errors"], ["binary build failed"])
            self.assertEqual(root.joinpath("failure.log").read_text(), "binary build failed\n")
            with self.assertRaisesRegex(StoreShardError, "test-binary identity"):
                validate_receipts([receipt, copy.deepcopy(receipt)])

    def test_binary_identity_failure_still_writes_failure_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, rustc_vv, _counter, _inventory, _ignored = self.fixture(root)
            rustc_vv.write_text("release: 1.97.0\n", encoding="utf-8")
            receipt_path = root / "shard-receipt.json"
            log_path = root / "shard.log"

            receipt = run_shard(
                binary,
                rustc_vv,
                receipt_path,
                log_path,
                environment=self.environment(),
                tested_sha="a" * 40,
                tested_tree="b" * 40,
                shard_count=2,
                shard_index=0,
                require_x86_64=False,
            )

            self.assertEqual(receipt["result"], "failure")
            self.assertIsNone(receipt["binary"])
            self.assertIn("pinned Rust 1.97.1", receipt["errors"][0])  # type: ignore[index]
            self.assertEqual(
                json.loads(receipt_path.read_text(encoding="utf-8"))["result"],
                "failure",
            )
            self.assertIn("pinned Rust 1.97.1", log_path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
