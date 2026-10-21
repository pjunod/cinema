"""Fail-closed correspondence contracts for the two-worker formal package."""

from __future__ import annotations

import copy
import hashlib
import importlib.machinery
import importlib.util
import json
import tempfile
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("check_formal_worker", str(ROOT / "scripts/check-formal"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None and SPEC.loader is not None
formal = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(formal)


BOUNDARIES = (
    "select.queue_loss", "select.domain_loss", "select.admission_pressure", "select.work_returned",
    "lock.acquire.queue_token_mutex", "lock.release.queue_token_mutex",
    "lock.acquire.domain_fence", "lock.release.domain_fence",
    "heartbeat.queue.deadline", "heartbeat.domain.deadline",
    "heartbeat.queue.stop_join", "heartbeat.domain.stop_join",
    "release.attempt", "release.retry", "release.ambiguous_fallback",
    "retire.claim", "drop.admission", "sql.complete_library_work.trigger",
)


class WorkerCorrespondenceTests(unittest.TestCase):
    def test_package_registry_is_closed_and_bounded(self) -> None:
        package = formal.package_spec("worker-refinement")
        self.assertEqual(package["model_file"], "verification/worker-refinement/WorkerRefinement.tla")
        self.assertEqual(len(package["config_order"]), 14)
        self.assertIn("control-no-useful-progress.cfg", package["config_order"])
        self.assertEqual(package["smoke_configs"], ("witness-both-complete.cfg",))
        self.assertEqual(package["smoke_wall_budget_seconds"], 300)
        self.assertEqual(package["config_timeout_cap_seconds"], 1800)
        self.assertEqual(
            package["smoke_config_timeouts"],
            {"witness-both-complete.cfg": 300},
        )

    def test_final_liveness_checking_is_limited_to_worker_refinement_positives(self) -> None:
        self.assertEqual(
            formal.liveness_check_args("worker-refinement", {"verdict": "pass"}),
            ["-lncheck", "final"],
        )
        for verdict in ("violation", "witness"):
            self.assertEqual(formal.liveness_check_args("worker-refinement", {"verdict": verdict}), [])
        self.assertEqual(formal.liveness_check_args("durable-jobs", {"verdict": "pass"}), [])
        self.assertEqual(formal.liveness_check_args("operation-progress", {"verdict": "pass"}), [])

    def test_next_action_names_include_quantified_parameterized_operators(self) -> None:
        model = """\
Claim(w) == TRUE
Renew(w) == TRUE
Next == \\E w \\in Workers:
    Claim(w)
    \\/ \\E other \\in Workers: Renew(other)
Spec == Next
"""
        self.assertEqual(formal.next_action_names(model), {"Claim", "Renew"})
        with self.assertRaises(formal.RunnerError):
            formal.next_action_names("Spec == TRUE\n")

    def test_correspondence_is_hash_bound_and_covers_every_next_and_boundary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package_dir = root / "verification/worker-refinement"
            package_dir.mkdir(parents=True)
            model_file = "verification/worker-refinement/WorkerRefinement.tla"
            source_file = "crates/plurxd/src/library_work.rs"
            test_id = source_file + "::tests::worker_schedule"
            model = """\
Claim(w) == TRUE
Renew(w) == TRUE
Next == \\E w \\in Workers: Claim(w) \\/ Renew(w)
Spec == Next
"""
            source = "// source pin\n#[test]\nfn worker_schedule() {}\n"
            (root / model_file).write_text(model, encoding="utf-8")
            (root / source_file).parent.mkdir(parents=True)
            (root / source_file).write_text(source, encoding="utf-8")
            (root / "verification/durable-jobs").mkdir(parents=True)
            (root / "verification/durable-jobs/toolchain.json").write_text("{}\n", encoding="utf-8")
            (root / "scripts").mkdir()
            (root / "scripts/bootstrap-formal-toolchain").write_text("bootstrap\n", encoding="utf-8")

            file_hash = lambda path: hashlib.sha256((root / path).read_bytes()).hexdigest()
            rows = []
            for kind, names in (("action", ("Claim", "Renew")), ("boundary", BOUNDARIES)):
                for name in names:
                    rows.append({
                        "kind": kind,
                        "action": name,
                        "worker": "both" if kind == "action" else "shared",
                        "source": {
                            "file": source_file,
                            "symbol": "worker_schedule",
                            "anchor": "// source pin",
                            "sha256": file_hash(source_file),
                        },
                        "held_across_await": [],
                        "completed_by": "worker task",
                        "trust": "code-fact",
                        "schedules": [test_id],
                    })
            correspondence = {
                "schema_version": 1,
                "status": "reviewed",
                "model_file": model_file,
                "model_sha256": file_hash(model_file),
                "tests": [{"id": test_id, "file": source_file, "name": "worker_schedule"}],
                "actions": rows,
            }
            correspondence_path = "verification/worker-refinement/correspondence.json"
            (root / correspondence_path).write_text(json.dumps(correspondence), encoding="utf-8")
            mapping = {
                "model_file": model_file,
                "model_sha256": file_hash(model_file),
                "correspondence": {"path": correspondence_path, "sha256": file_hash(correspondence_path)},
            }

            with mock.patch.object(formal, "ROOT", root):
                def validate(candidate: dict[str, object]) -> list[str]:
                    (root / correspondence_path).write_text(json.dumps(candidate), encoding="utf-8")
                    mapping["correspondence"]["sha256"] = file_hash(correspondence_path)
                    return formal.worker_correspondence_inputs(mapping, {})

                hashes: dict[str, str] = {}
                self.assertEqual(formal.worker_correspondence_inputs(mapping, hashes), [])
                worker_package = formal.package_spec("worker-refinement")
                self.assertEqual(mapping["model_file"], worker_package["model_file"])
                self.assertEqual(mapping["model_sha256"], file_hash(worker_package["model_file"]))
                self.assertEqual(hashes[source_file], file_hash(source_file))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = [row for row in rows if row["action"] != "Claim"]
                drift = validate(candidate)
                self.assertTrue(any("Next action coverage differs" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = rows + [dict(rows[0], action="Invented")]
                drift = validate(candidate)
                self.assertTrue(any("unknown Next action" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = [row for row in rows if row["action"] != BOUNDARIES[0]]
                drift = validate(candidate)
                self.assertTrue(any("lacks required Rust boundary" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = rows + [dict(rows[-1], action="boundary.unknown")]
                drift = validate(candidate)
                self.assertTrue(any("unknown worker correspondence boundary" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = rows + [copy.deepcopy(rows[0])]
                drift = validate(candidate)
                self.assertTrue(any("duplicate correspondence row" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["unrecognized"] = True
                drift = validate(candidate)
                self.assertTrue(any("missing or unknown schema fields" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = [dict(row, schedules=["unknown-test-id"]) for row in rows]
                drift = validate(candidate)
                self.assertTrue(any("unknown test ID" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["tests"] = candidate["tests"] + candidate["tests"]
                drift = validate(candidate)
                self.assertTrue(any("duplicate conformance test ID" in item for item in drift))

                extra_test = "crates/extra.rs"
                (root / extra_test).parent.mkdir(parents=True, exist_ok=True)
                (root / extra_test).write_text("fn extra_schedule() {}\n", encoding="utf-8")
                candidate = copy.deepcopy(correspondence)
                candidate["tests"] = [{"id": extra_test + "::tests::extra_schedule", "file": extra_test, "name": "extra_schedule"}]
                candidate["actions"] = [dict(row, schedules=[candidate["tests"][0]["id"]]) for row in rows]
                drift = validate(candidate)
                self.assertTrue(any("lack reviewed source hash pins" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"][0]["schedules"] = "unchecked"
                drift = validate(candidate)
                self.assertTrue(any("unchecked correspondence needs an explicit gap" in item for item in drift))

                candidate = copy.deepcopy(correspondence)
                candidate["actions"] = rows
                (root / source_file).write_text(source + "// drift\n", encoding="utf-8")
                drift = validate(candidate)
                self.assertTrue(any("source drift requires review" in item for item in drift))


if __name__ == "__main__":
    unittest.main()
