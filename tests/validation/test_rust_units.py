"""The PR-time Rust unit stage: content keys, selection, receipts and shards.

Every selection case builds a real git history and computes keys from its
objects, the same way CI does; nothing here mocks a key or a boolean.
"""

from __future__ import annotations

import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from types import SimpleNamespace
import textwrap
import unittest
from unittest import mock
from xml.sax.saxutils import escape
import zipfile

from validation import rust_units as ru


ROOT = Path(__file__).resolve().parents[2]

def workflow_gate(**outputs):
    """Exercise the actual checked-in gate shell, not a copied predicate."""
    workflow = (ROOT / ".github/workflows/rust-units.yml").read_text()
    gate = workflow.split("\n  gate:\n", 1)[1]
    run = gate.split("        run: |\n", 1)[1]
    script_lines = []
    for line in run.splitlines():
        if line.strip() and not line.startswith("          "):
            break
        script_lines.append(line)
    script = textwrap.dedent("\n".join(script_lines))
    values = dict(SELECT="success", BUILD="true", ARCHIVE="success", SHARDS="[1, 2]",
                  SHARD="success", VERDICT="success", COMPLETE="true", FAILURES="false", BASELINE="success",
                  ABSENT_ONLY="false", ABSENT="false", TRAIN_TESTS="")
    values.update(outputs)
    environment = os.environ.copy()
    for name, value in values.items():
        if value is None:
            environment.pop(name, None)
        else:
            environment[name] = value
    return subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script], env=environment,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def main_promotion_gate(**outputs):
    """Exercise only the actual main promotion prerequisite shell block."""
    workflow = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
    gate = workflow.split("\n  promotion_gate:\n", 1)[1]
    run = gate.split("        run: |\n", 1)[1]
    script_lines = []
    for line in run.splitlines():
        if line.strip() and not line.startswith("          "):
            break
        script_lines.append(line)
    values = dict(PAPERWORK_RESULT="success", SCOPE_RESULT="success", PREFLIGHT_RESULT="success",
                  WORKER_CONTROL_RESULT="success", RUST_UNITS_REQUIRED="true", RUST_UNITS_RESULT="success",
                  RUST_RESULT="skipped", WEB_RESULT="skipped", APPLE_RESULT="skipped", ANDROID_RESULT="skipped")
    values.update(outputs)
    environment = os.environ.copy()
    for name, value in values.items():
        if value is None:
            environment.pop(name, None)
        else:
            environment[name] = value
    return subprocess.run(["bash", "-e", "-o", "pipefail", "-c", textwrap.dedent("\n".join(script_lines))],
                          env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


CONFIG = """
version = 1
excluded_crates = ["harness"]
harness = ["validation/rust-unit-inputs.toml"]
exempt = ["manual/**", "tests/**", "validation/**", "**/*.md"]
"""


class Repo:
    def __init__(self, directory):
        self.root = Path(directory)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")

    def git(self, *arguments):
        return subprocess.run(["git", "-C", str(self.root), *arguments], check=True,
                              stdout=subprocess.PIPE, text=True).stdout.strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message="change"):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")


def crate(name, deps="", dev="", build="", source=""):
    manifest = f'[package]\nname = "{name}"\nversion = "0.1.0"\n'
    if deps:
        manifest += f"\n[dependencies]\n{deps}\n"
    if dev:
        manifest += f"\n[dev-dependencies]\n{dev}\n"
    if build:
        manifest += f"\n[build-dependencies]\n{build}\n"
    return manifest, source or f"pub fn {name.replace('-', '_')}() {{}}\n"


def workspace_repo(directory):
    """core <- app (workspace dep), devonly is core's dev-dependency, leaf reads
    an exempt fixture through a literal, harness is excluded from the lane."""
    repo = Repo(directory)
    repo.write("Cargo.toml", """[workspace]
members = ["crates/core", "crates/app", "crates/leaf", "crates/devonly", "crates/harness"]

[workspace.dependencies]
core = { path = "crates/core" }
""")
    repo.write("Cargo.lock", "# lock v1\n")
    repo.write("validation/rust-unit-inputs.toml", CONFIG)
    repo.write("manual/guide.md", "guide\n")
    repo.write("shared/fixture.json", "{}\n")
    repo.write("tests/data/leaf.json", "[1]\n")
    for name, spec in {
        "core": crate("core", dev='devonly = { path = "../devonly" }'),
        "app": crate("app", deps="core = { workspace = true }"),
        "leaf": crate("leaf", source='pub const DATA: &str = include_str!("../../../tests/data/leaf.json");\n'),
        "devonly": crate("devonly"),
        "harness": crate("harness", deps='core = { path = "../core" }'),
    }.items():
        repo.write(f"crates/{name}/Cargo.toml", spec[0])
        repo.write(f"crates/{name}/src/lib.rs", spec[1])
    return repo


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = workspace_repo(self.temporary.name)
        self.base = self.repo.commit("base")

    def tearDown(self):
        self.temporary.cleanup()

    def due_after(self, files):
        for path, text in files.items():
            self.repo.write(path, text)
        head = self.repo.commit()
        doc = ru.selection(head, self.base, repository=self.repo.root)
        return sorted(name for name, entry in doc["crates"].items() if entry["due"])

    def test_a_source_change_selects_the_crate_and_every_dependent(self):
        self.assertEqual(self.due_after({"crates/core/src/lib.rs": "pub fn core() { /* new */ }\n"}),
                         ["app", "core"])

    def test_a_dependents_feature_manifest_rekeys_every_workspace_crate(self):
        core = self.repo.root / "crates/core/Cargo.toml"
        self.repo.write("crates/core/Cargo.toml", core.read_text() + "\n[features]\nobserved = []\n")
        self.base = self.repo.commit("declare dependency feature")
        app = (self.repo.root / "crates/app/Cargo.toml").read_text()
        self.assertEqual(
            self.due_after({"crates/app/Cargo.toml": app.replace(
                "core = { workspace = true }", 'core = { workspace = true, features = ["observed"] }'
            )}),
            ["app", "core", "devonly", "leaf"],
        )

    def test_an_unrelated_crate_change_selects_only_that_crate(self):
        self.assertEqual(self.due_after({"crates/app/src/lib.rs": "pub fn app() { /* new */ }\n"}), ["app"])

    def test_an_exempt_tree_selects_nothing(self):
        self.assertEqual(self.due_after({"manual/guide.md": "changed\n"}), [])

    def test_the_lock_file_selects_everything(self):
        self.assertEqual(self.due_after({"Cargo.lock": "# lock v2\n"}), ["app", "core", "devonly", "leaf"])

    def test_an_unexempted_shared_fixture_selects_everything(self):
        self.assertEqual(self.due_after({"shared/fixture.json": '{"a": 1}\n'}),
                         ["app", "core", "devonly", "leaf"])

    def test_the_harness_selects_everything(self):
        self.assertEqual(self.due_after({"validation/rust-unit-inputs.toml": CONFIG + "# touched\n"}),
                         ["app", "core", "devonly", "leaf"])

    def test_an_exempt_file_named_by_a_literal_selects_its_reader(self):
        self.assertEqual(self.due_after({"tests/data/leaf.json": "[2]\n"}), ["leaf"])

    def test_a_build_script_selects_the_crate_and_its_dependents(self):
        self.assertEqual(self.due_after({"crates/core/build.rs": "fn main() {}\n"}), ["app", "core"])

    def test_dev_dependencies_do_not_propagate_to_dependents(self):
        # app's tests link core's library, not core's dev-dependencies.
        self.assertEqual(self.due_after({"crates/devonly/src/lib.rs": "pub fn devonly() { /* x */ }\n"}),
                         ["core", "devonly"])

    def test_the_excluded_crate_is_never_selected(self):
        self.assertEqual(self.due_after({"crates/harness/src/lib.rs": "pub fn harness() { /* x */ }\n"}), [])

    def test_the_key_is_identical_for_identical_inputs_across_commits(self):
        first = ru.Keys(ru.Tree(self.base, self.repo.root), ru.Config.load(CONFIG)).key("app")
        self.repo.write("manual/guide.md", "other\n")
        second_commit = self.repo.commit()
        second = ru.Keys(ru.Tree(second_commit, self.repo.root), ru.Config.load(CONFIG)).key("app")
        self.assertEqual(first, second)

    def test_a_new_crate_is_due(self):
        self.repo.write("Cargo.toml", self.repo.root.joinpath("Cargo.toml").read_text().replace(
            '"crates/harness"]', '"crates/harness", "crates/fresh"]'))
        manifest, source = crate("fresh")
        self.assertIn("fresh", self.due_after({"crates/fresh/Cargo.toml": manifest,
                                              "crates/fresh/src/lib.rs": source}))

    def test_a_run_time_workspace_walk_must_be_declared(self):
        walker = ('pub fn walk() -> std::path::PathBuf {\n'
                  '    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))\n'
                  '        .parent()\n        .unwrap()\n        .join("manual")\n}\n')
        self.repo.write("crates/leaf/src/walk.rs", walker)
        head = self.repo.commit()
        keys = ru.Keys(ru.Tree(head, self.repo.root), ru.Config.load(CONFIG))
        self.assertEqual(ru.dynamic_site_drift(keys), ([("leaf", "crates/leaf/src/walk.rs", "walk")], []))
        declared = CONFIG + ('\n[[dynamic]]\ncrate = "leaf"\nfile = "crates/leaf/src/walk.rs"\n'
                             'function = "walk"\nreads = ["manual/**"]\ntests = ["walk::reads_the_manual"]\n'
                             'reason = "walks the manual"\n')
        self.repo.write("validation/rust-unit-inputs.toml", declared)
        self.base = self.repo.commit()
        keys = ru.Keys(ru.Tree(self.base, self.repo.root), ru.Config.load(declared))
        self.assertEqual(ru.dynamic_site_drift(keys), ([], []))
        # Once declared, the read paths are inputs: the manual now re-keys the
        # reader, and since nothing else of it changed only its reading test is due.
        self.assertEqual(self.due_after({"manual/guide.md": "read by leaf\n"}), ["leaf"])
        head = self.repo.git("rev-parse", "HEAD")
        doc = ru.selection(head, self.base, repository=self.repo.root)
        self.assertEqual(doc["crates"]["leaf"]["only"], ["walk::reads_the_manual"])
        # A change to the reader's own sources makes the whole crate due again.
        self.repo.write("crates/leaf/src/lib.rs", "pub const X: u8 = 1;\n")
        doc = ru.selection(self.repo.commit(), self.base, repository=self.repo.root)
        self.assertIsNone(doc["crates"]["leaf"]["only"])


class ReceiptAuthenticationTests(unittest.TestCase):
    """A historical receipt is believed only as far as the forge confirms it."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = workspace_repo(self.temporary.name)
        self.commit = self.repo.commit("base")
        self.config = ru.Config.load(CONFIG)
        self.key = ru.Keys(ru.Tree(self.commit, self.repo.root), self.config).key("app")
        self.passes = ["app aa::first", "app aa::second"]

    def tearDown(self):
        self.temporary.cleanup()

    def forge(self, receipt=None, shard_log=None, publisher_log=None, run_commit=None,
              workflow="rust-units.yml", shard_job="shard", event="workflow_dispatch"):
        name = ru.receipt_name("app", self.key)
        receipt = receipt or {
            "schema": 1, "producer": "validation.rust_units", "crate": "app", "key": self.key,
            "commit": self.commit, "run": 41, "publisher": ru.JOB_VERDICT,
            "evidence_jobs": [ru.JOB_SHARD.format(index=1)], "listing": {"count": 2, "sha256": "0" * 64},
            "passed": self.passes, "failed": []}
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr("receipt.json", json.dumps(receipt))
        if shard_log is None:
            shard_log = evidence_log(dict.fromkeys(self.passes, "PASS"), 0)
        publisher_log = publisher_log if publisher_log is not None else (
            f"Artifact {name} has been successfully uploaded! Final size is 300 bytes.\n")
        responses = {
            "/actions/artifacts": [{"id": 7, "name": name, "run_id": 41, "expired": False,
                                    "size_in_bytes": len(buffer.getvalue())}],
            "/actions/runs/41": {"id": 41, "commit_sha": run_commit or self.commit, "workflow_id": workflow},
            "/actions/runs/41/jobs": [
                {"id": 501, "name": ru.JOB_VERDICT, "status": "success"},
                {"id": 502, "name": ru.JOB_SHARD.format(index=1) + " (1)", "status": "success"}],
            "/actions/artifacts/7/zip": buffer.getvalue(),
            "/actions/jobs/501/logs": (self.task("verdict", event) + publisher_log).encode(),
            "/actions/jobs/502/logs": (self.task(shard_job, event) + shard_log).encode(),
        }

        class Forge:
            budget = 100

            def bytes(self, path, query=None, limit=0):
                value = responses[path]
                return value if isinstance(value, bytes) else json.dumps(value).encode()

            def get(self, path, query=None):
                return json.loads(self.bytes(path, query))

        return Forge()

    @staticmethod
    def task(job, event="workflow_dispatch"):
        return (f"2026-11-12T10:00:00.0Z Runner 1f (version:v13.2.0) received task 77 of job {job}, "
                f"triggered by event: {event}\n")

    def adopted(self, forge):
        authenticator = ru.Authenticator(forge, current_run=99, config=self.config,
                                         repository=str(self.repo.root))
        return ru.history(forge, authenticator, "app", self.key)["passed"]

    def test_an_authenticated_receipt_is_adopted(self):
        self.assertEqual(self.adopted(self.forge()), set(self.passes))

    def test_receipts_from_the_main_lane_reusable_stage_are_adopted(self):
        for event in ("workflow_call", "pull_request"):
            with self.subTest(event=event):
                self.assertEqual(
                    self.adopted(self.forge(workflow="main-fast-lane.yml", event=event)),
                    set(self.passes),
                )

    def test_dispatch_cannot_impersonate_the_main_lane_stage(self):
        self.assertEqual(self.adopted(self.forge(workflow="main-fast-lane.yml")), set())

    def test_malformed_artifact_lookup_json_runs_tests_instead_of_adopting(self):
        forge = ru.Forge("https://forge.invalid/api/v1", "owner/repo", "fixture-token")
        authenticator = mock.Mock()
        with mock.patch.object(forge, "bytes", return_value=b"{malformed"):
            result = ru.history(forge, authenticator, "app", self.key)
        self.assertEqual(result["passed"], set())
        self.assertEqual(result["runs"], [])
        authenticator.authenticate.assert_not_called()

    def test_malformed_artifact_lookup_shapes_run_tests_instead_of_adopting(self):
        for value in (None, 7, "bad", {}, {"artifacts": None}, {"artifacts": {}}, {"artifacts": "bad"}):
            with self.subTest(value=value):
                forge, authenticator = mock.Mock(), mock.Mock()
                forge.get.return_value = value
                result = ru.history(forge, authenticator, "app", self.key)
                self.assertEqual(result["passed"], set())
                self.assertEqual(result["runs"], [])
                authenticator.authenticate.assert_not_called()

    def test_malformed_artifact_run_ids_do_not_hide_valid_history(self):
        forge = self.forge()
        original_get = forge.get
        artifacts = original_get("/actions/artifacts")
        name = ru.receipt_name("app", self.key)
        malformed = [{"name": name, "run_id": value} for value in (None, "41", {}, True, 0, -1)]
        def get(path, query=None):
            return artifacts + malformed if path == "/actions/artifacts" else original_get(path, query)
        with mock.patch.object(forge, "get", side_effect=get):
            self.assertEqual(self.adopted(forge), set(self.passes))

    def test_a_timed_out_history_commit_fetch_runs_tests_instead_of_adopting(self):
        forge = self.forge()
        with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "fixture-token"}), mock.patch.object(
            ru.subprocess, "run", side_effect=[
                mock.Mock(returncode=1), subprocess.TimeoutExpired(["git", "fetch"], 120)
            ]
        ) as run:
            self.assertEqual(self.adopted(forge), set())
            self.assertEqual(run.call_count, 2)

    def test_a_pass_missing_from_the_shard_log_is_not_adopted(self):
        log = evidence_log({self.passes[0]: "PASS"}, 0)
        self.assertEqual(self.adopted(self.forge(shard_log=log)), {"app aa::first"})

    def test_a_logged_failure_cannot_be_claimed_as_a_pass(self):
        log = evidence_log({self.passes[0]: "PASS", self.passes[1]: "FAIL"}, 100)
        self.assertEqual(self.adopted(self.forge(shard_log=log)), {"app aa::first"})

    def test_raw_pass_and_json_in_shard_diagnostics_cannot_authenticate_a_receipt(self):
        forged = (
            "2026-11-12T10:00:00.1Z         PASS [   0.013s] app aa::second\n"
            + "nextest: " + json.dumps(evidence_log({self.passes[1]: "PASS"}, 0)) + "\n"
        )
        proper = evidence_log({self.passes[0]: "PASS"}, 0)
        self.assertEqual(self.adopted(self.forge(shard_log=forged)), set())
        self.assertEqual(self.adopted(self.forge(shard_log=proper + forged)), {self.passes[0]})

    def test_partial_and_conflicting_evidence_cannot_authenticate(self):
        first_pass = evidence_log({self.passes[0]: "PASS"}, 0)
        for log in (first_pass.splitlines(keepends=True)[0],
                    evidence_log({self.passes[0]: "PASS"}, 100),
                    first_pass + evidence_log({self.passes[0]: "FAIL"}, 100)):
            with self.subTest(log=log):
                self.assertEqual(self.adopted(self.forge(shard_log=log)), set())


    def test_a_receipt_whose_commit_has_another_key_is_not_adopted(self):
        # The producer claims this key for a commit whose inputs differ: the
        # key recomputed from that commit by this code disagrees.
        self.repo.write("crates/core/src/lib.rs", "pub fn core() { /* different */ }\n")
        other = self.repo.commit()
        receipt = {"schema": 1, "producer": "validation.rust_units", "crate": "app", "key": self.key,
                   "commit": other, "run": 41, "publisher": ru.JOB_VERDICT,
                   "evidence_jobs": [ru.JOB_SHARD.format(index=1)], "listing": None,
                   "passed": self.passes, "failed": []}
        self.assertEqual(self.adopted(self.forge(receipt=receipt, run_commit=other)), set())

    def test_a_receipt_the_publisher_never_uploaded_is_not_adopted(self):
        self.assertEqual(self.adopted(self.forge(publisher_log="nothing uploaded\n")), set())

    def test_a_receipt_for_a_different_run_commit_is_not_adopted(self):
        self.assertEqual(self.adopted(self.forge(run_commit="f" * 40)), set())

    def test_a_run_of_another_workflow_is_not_adopted(self):
        self.assertEqual(self.adopted(self.forge(workflow="something-else.yml")), set())

    def test_evidence_from_a_job_that_is_not_the_stage_shard_is_not_adopted(self):
        self.assertEqual(self.adopted(self.forge(shard_job="impostor")), set())

    def test_a_complete_listing_lets_the_whole_crate_be_skipped(self):
        listing = {"count": 2, "sha256": ru.digest_lines(sorted(self.passes))}
        self.assertTrue(ru.fully_passed([listing], set(self.passes)))
        self.assertFalse(ru.fully_passed([listing], {self.passes[0]}))
        self.assertFalse(ru.fully_passed([], set(self.passes)))

    def test_authenticated_same_key_passes_are_not_replayed_by_the_strict_gate(self):
        adopted = self.adopted(self.forge())
        self.assertEqual(adopted, set(self.passes))
        current = listing(**{"app": [("aa::first", False), ("aa::second", False)]})
        plan = ru.make_plan({"head": self.commit, "base": self.commit},
                            {"crates": {"app": {"key": self.key, "passed": sorted(adopted)}}},
                            current, self.config, run=42)
        self.assertEqual(plan["crates"]["app"]["adopted"], sorted(adopted))
        self.assertEqual(plan["crates"]["app"]["remaining"], [])
        self.assertEqual(plan["crates"]["app"]["serial"], [])
        self.assertEqual(plan["shards"], 1)  # Planner shape is fixed; the producer emits no runnable shards.
        with mock.patch.object(ru, "read_json", side_effect=[
                {"head": self.commit, "base": self.commit},
                {"crates": {"app": {"key": self.key, "passed": sorted(adopted)}}}, current]), \
                mock.patch.object(ru.Config, "load", return_value=self.config), \
                mock.patch.object(ru, "write_json"), mock.patch.object(ru, "summarize"), \
                mock.patch.object(ru, "emit") as emitted:
            self.assertEqual(ru.command_plan(SimpleNamespace(selection="selection", pending="pending",
                              listing="listing", output="plan")), 0)
        produced = dict(call.args for call in emitted.call_args_list)
        self.assertEqual(produced["shards"], [])
        self.assertEqual(produced["tests"], "0")
        result = workflow_gate(SHARDS=json.dumps(produced["shards"]), SHARD="skipped", VERDICT="skipped", COMPLETE="", FAILURES="", ABSENT="")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_valid_empty_selection_produces_explicit_false_without_a_workflow_default(self):
        real_tree = ru.Tree
        with mock.patch.object(ru, "read_json", return_value={"head": self.commit, "base": self.commit, "crates": {}}), \
                mock.patch.object(ru, "Tree", side_effect=lambda ref: real_tree(ref, self.repo.root)), \
                mock.patch.object(ru, "write_json"), mock.patch.object(ru, "emit") as emitted, \
                mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(ru.command_adopt(SimpleNamespace(selection="selection", output="pending")), 0)
        produced = dict(call.args for call in emitted.call_args_list)
        self.assertEqual(produced["build"], "false")
        self.assertEqual(produced["crates"], [])


def evidence_log(outcomes, returncode):
    """The structured records rendered by a shard, with Forgejo timestamps."""
    lines = [ru.EVIDENCE_CASE + json.dumps([test, status]) for test, status in outcomes.items()]
    lines.append(ru.EVIDENCE_END + json.dumps([returncode, len(outcomes), ru.evidence_digest(outcomes)]))
    return "".join("2026-11-12T10:00:00.1Z " + line + "\n" for line in lines)


def listing(**suites):
    return {"rust-suites": {binary: {"testcases": {name: {"ignored": ignored} for name, ignored in cases}}
                            for binary, cases in suites.items()}}


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.config = ru.Config.load(CONFIG + '\n[[serial_ignored]]\ncrate = "app"\ntests = ["slow::one"]\n')
        self.selection = {"head": "a" * 40, "base": "b" * 40}
        self.listing = listing(**{"app": [("t::a", False), ("t::b", False), ("slow::one", True),
                                          ("other::ignored", True)],
                                  "app::bin/app": [("m::c", False)],
                                  "core": [("c::d", False)]})

    def test_adopted_passes_are_not_planned_and_serial_ignored_tests_are(self):
        pending = {"crates": {"app": {"key": "1" * 64, "passed": ["app t::a"]}}}
        plan = ru.make_plan(self.selection, pending, self.listing, self.config, run=5)
        app = plan["crates"]["app"]
        self.assertEqual(app["remaining"], ["app t::b", "app::bin/app m::c"])
        self.assertEqual(app["serial"], ["app slow::one"])
        self.assertEqual(app["listing"]["count"], 4)
        self.assertEqual(plan["shards"], 1)
        self.assertNotIn("core", plan["crates"])

    def test_ignored_cases_outside_required_ids_do_not_create_gate_failures(self):
        plan = ru.make_plan(self.selection, {"crates": {"app": {"key": "1" * 64, "passed": []}}},
                            self.listing, self.config, run=5)
        required = plan["crates"]["app"]["remaining"] + plan["crates"]["app"]["serial"]
        self.assertNotIn("app other::ignored", required)
        self.assertIn("app slow::one", required)
        receipt = {"schema": 1, "run": 5, "head": plan["head"], "shards": 1, "index": 1,
                   "results": {"app": {"passed": required, "failed": [], "skipped": []}}}
        outcome, problems = ru.collect(plan, [receipt])
        self.assertEqual(problems, [])
        self.assertTrue(all(outcome[test] == "passed" for test in required))
        result = workflow_gate(SHARDS="[1]")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_malformed_listing_and_missing_pending_crate_cannot_become_zero_work(self):
        pending = {"crates": {"app": {"key": "1" * 64, "passed": []}}}
        for document in ({}, {"rust-suites": None}, {"rust-suites": []}, {"rust-suites": {}},
                         listing(other=[]), {"rust-suites": {"app": {}}},
                         {"rust-suites": {"app": {"testcases": []}}},
                         {"rust-suites": {"app": {"testcases": {"case": {}}}}},
                         {"rust-suites": {"app": {"testcases": {"case": {"ignored": "false"}}}}}):
            with self.subTest(document=document), self.assertRaises(ru.UnitsError):
                ru.make_plan(self.selection, pending, document, self.config, run=5)

    def test_explicit_zero_test_and_filtered_suites_remain_valid_zero_work(self):
        config = ru.Config.load(CONFIG)
        pending = {"crates": {"app": {"key": "1" * 64, "passed": []}}}
        for document in (listing(app=[]), {"rust-suites": {"app": {"testcases": {
                "excluded": {"ignored": False, "filter-match": {"status": "mismatch", "reason": "default-filter"}}
        }}}}):
            with self.subTest(document=document):
                plan = ru.make_plan(self.selection, pending, document, config, run=5)
                self.assertEqual(plan["crates"]["app"]["listing"]["count"], 0)
                self.assertEqual(plan["crates"]["app"]["remaining"], [])
                self.assertEqual(plan["crates"]["app"]["serial"], [])

    def test_filters_select_exactly_the_remaining_tests(self):
        pending = {"crates": {"app": {"key": "1" * 64, "passed": ["app t::a"]}}}
        plan = ru.make_plan(self.selection, pending, self.listing, self.config, run=5)
        self.assertEqual(plan["filters"],
                         ["binary_id(=app) & (test(=t::b)) | binary_id(=app::bin/app)"])

    def test_large_selections_split_into_bounded_filter_arguments(self):
        names = [f"module::case_{i:05}_" + "x" * 60 for i in range(4000)]
        suites = {"plurxd::bin/plurxd": {name: False for name in names + ["extra"]}}
        expressions = ru.filter_expressions([f"plurxd::bin/plurxd {n}" for n in names], suites)
        self.assertGreater(len(expressions), 1)
        self.assertTrue(all(len(e) <= ru.MAX_FILTER_BYTES for e in expressions))
        mentioned = re.findall(r"test\(=([^)]*)\)", " ".join(expressions))
        self.assertEqual(sorted(mentioned), sorted(names))

    def test_tests_the_default_filter_excludes_are_not_planned(self):
        listing = self.listing
        listing["rust-suites"]["app"]["testcases"]["child::body"] = {
            "ignored": False, "filter-match": {"status": "mismatch", "reason": "default-filter"}}
        pending = {"crates": {"app": {"key": "1" * 64, "passed": []}}}
        plan = ru.make_plan(self.selection, pending, listing, self.config, run=5)
        self.assertNotIn("app child::body", plan["crates"]["app"]["remaining"])
        self.assertEqual(plan["crates"]["app"]["listing"]["count"], 4)

    def test_a_reads_only_change_plans_just_the_declared_readers(self):
        config = ru.Config.load(CONFIG + '\n[[dynamic]]\ncrate = "app"\nfile = "x.rs"\nfunction = "f"\n'
                                'reads = ["manual/**"]\ntests = ["t::b"]\nreason = "r"\n')
        pending = {"crates": {"app": {"key": "1" * 64, "passed": [], "only": ["t::b"]}}}
        plan = ru.make_plan(self.selection, pending, self.listing, config, run=5)
        self.assertEqual(plan["crates"]["app"]["remaining"], ["app t::b"])
        self.assertEqual(plan["crates"]["app"]["serial"], [])

    def test_a_declared_reader_that_does_not_exist_fails_the_plan(self):
        config = ru.Config.load(CONFIG + '\n[[dynamic]]\ncrate = "app"\nfile = "x.rs"\nfunction = "f"\n'
                                'reads = ["manual/**"]\ntests = ["t::gone"]\nreason = "r"\n')
        pending = {"crates": {"app": {"key": "1" * 64, "passed": [], "only": ["t::gone"]}}}
        with self.assertRaises(ru.UnitsError):
            ru.make_plan(self.selection, pending, self.listing, config, run=5)

    def test_shard_count_follows_the_selection(self):
        self.assertEqual(ru.shard_count(1), 1)
        self.assertEqual(ru.shard_count(ru.TESTS_PER_SHARD), 1)
        self.assertEqual(ru.shard_count(ru.TESTS_PER_SHARD + 1), 2)
        self.assertEqual(ru.shard_count(10 ** 6), ru.MAX_SHARDS)


class NamedTestsTests(unittest.TestCase):
    """A merge-train probe's Train-Tests: exactly those tests, run now."""
    A, B, C = "app t::a", "app::bin/app m::c", "core c::d"

    def doc(self):
        entry = lambda due: {"key": "1" * 64, "base_key": "1" * 64, "due": due, "only": None, "reason": []}
        return {"head": "a" * 40, "base": "b" * 40, "crates": {"app": entry(False), "core": entry(True)}}

    def test_ids_parse_as_nextest_prints_them(self):
        self.assertEqual(ru.parse_test_ids(f"{self.B}, {self.A},{self.A}"), [self.A, self.B])
        self.assertEqual(ru.parse_test_ids(""), [])
        for bad in ("app", "app t::a extra", "app t::a;rm"):
            with self.assertRaises(ru.UnitsError):
                ru.parse_test_ids(bad)

    def test_named_tests_make_exactly_their_crates_due_whatever_the_keys_say(self):
        doc = ru.restrict_to_tests(self.doc(), [self.A, self.B])
        self.assertTrue(doc["crates"]["app"]["due"])
        self.assertFalse(doc["crates"]["core"]["due"])  # content-due, but not named
        self.assertEqual(doc["crates"]["app"]["exact"], [self.A, self.B])
        self.assertEqual(doc["crates"]["app"]["only"], ["m::c", "t::a"])

    def test_an_unknown_crate_is_carried_as_absent_not_a_select_failure(self):
        doc = ru.restrict_to_tests(self.doc(), ["ghost t::a", self.A])
        self.assertEqual(doc["absent"], ["ghost t::a"])
        self.assertTrue(doc["crates"]["app"]["due"])
        with self.assertRaises(ru.UnitsError):
            ru.parse_test_ids("ghost")  # malformed still fails closed

    def test_the_plan_runs_exactly_the_named_ids_and_adopts_nothing(self):
        config = ru.Config.load(CONFIG)
        listing_ = listing(**{"app": [("t::a", False), ("t::b", False), ("m::c", False)],
                              "app::bin/app": [("m::c", False)]})
        pending = {"crates": {"app": {"key": "1" * 64, "passed": [], "only": ["m::c", "t::a"],
                                      "exact": ["app::bin/app m::c", "app t::a"]}}}
        plan = ru.make_plan({"head": "a" * 40, "base": "b" * 40}, pending, listing_, config, run=5)
        self.assertEqual(plan["crates"]["app"]["remaining"], ["app t::a", "app::bin/app m::c"])  # not `app m::c`
        self.assertEqual(plan["crates"]["app"]["adopted"], [])

    def test_a_named_id_the_listing_lacks_is_absent_and_the_others_still_run(self):
        config = ru.Config.load(CONFIG)
        pending = {"crates": {"app": {"key": "1" * 64, "passed": [], "only": ["t::a", "t::gone"],
                                      "exact": ["app t::a", "app t::gone"]}}}
        plan = ru.make_plan({"head": "a" * 40, "base": "b" * 40}, pending, listing(app=[("t::a", False)]), config, run=5)
        self.assertEqual(plan["crates"]["app"]["remaining"], ["app t::a"])
        self.assertEqual(plan["crates"]["app"]["absent"], ["app t::gone"])

    def test_adopt_never_takes_a_pass_from_a_receipt_for_a_named_test(self):
        with tempfile.TemporaryDirectory() as tmp:
            sel, out = Path(tmp) / "sel.json", Path(tmp) / "pending.json"
            doc = ru.restrict_to_tests(self.doc(), [self.A])
            ru.write_json(sel, doc)
            with mock.patch.object(ru, "Tree") as tree, mock.patch.object(ru, "history") as history, \
                    mock.patch.dict(os.environ, {"GITHUB_TOKEN": "x", "GITHUB_RUN_ID": "1"}), \
                    mock.patch.object(ru, "forge_from_environment", return_value=object()), \
                    mock.patch.object(ru, "Authenticator"):
                tree.return_value.text.return_value = CONFIG
                ru.command_adopt(mock.Mock(selection=str(sel), output=str(out)))
            history.assert_not_called()
            pending = json.loads(out.read_text())
            self.assertEqual(pending["crates"]["app"]["passed"], [])
            self.assertEqual(pending["crates"]["app"]["exact"], [self.A])


class NamedVerdictTests(unittest.TestCase):
    def run_verdict(self, passed=(), failed=(), absent=()):
        planned = [t for t in ("app t::a", "app t::b") if t not in absent]
        plan = {"run": 9, "shards": 1, "head": "a" * 40, "base": "b" * 40, "crates": {
            "app": {"key": "1" * 64, "listing": {"count": 2, "sha256": "2" * 64}, "adopted": [],
                    "remaining": planned, "serial": [], "exact": ["app t::a", "app t::b"],
                    "absent": list(absent)}}}
        receipt = {"schema": 1, "run": 9, "shards": 1, "head": "a" * 40, "index": 1,
                   "results": {"app": {"passed": list(passed), "failed": list(failed), "skipped": []}}}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ru.write_json(root / "plan.json", plan)
            ru.write_json(root / "shards" / "1" / "receipt.json", receipt)
            captured = []
            with mock.patch.object(ru, "STATE", root / "state"), mock.patch.object(ru, "emit") as emit, \
                    mock.patch.object(ru, "summarize", side_effect=lambda lines: captured.append("\n".join(lines))):
                ru.command_verdict(mock.Mock(plan=str(root / "plan.json"), receipts=str(root / "shards"),
                                             output=str(root / "receipts")))
            self.written = {"named": json.loads((root / "state" / "named.json").read_text()),
                            "receipt": json.loads((root / "receipts" / "app" / "receipt.json").read_text())}
            return {c.args[0]: c.args[1] for c in emit.call_args_list}, captured[0]

    def test_every_named_test_reports_its_actual_outcome(self):
        outputs, text = self.run_verdict(passed=["app t::a"], failed=["app t::b"])
        self.assertIn("named `app t::a`: PASS", text)
        self.assertIn("named `app t::b`: FAIL", text)
        self.assertEqual(outputs["failures"], "true")
        self.assertEqual(outputs["complete"], "true")

    def test_an_absent_named_test_is_reported_distinctly_and_is_never_a_pass(self):
        outputs, text = self.run_verdict(passed=["app t::a"], absent=["app t::b"])
        self.assertIn("\nABSENT app t::b", "\n" + text)
        self.assertIn("named `app t::a`: PASS", text)
        # The verdict job itself succeeds; only the gate fails on absent.
        self.assertEqual(outputs["complete"], "true")
        self.assertEqual(outputs["absent"], "true")

    def test_a_mixed_probe_reports_absent_and_a_failing_present_test(self):
        outputs, text = self.run_verdict(failed=["app t::a"], absent=["app t::b"])
        self.assertIn("named `app t::a`: FAIL", text)
        self.assertIn("\nABSENT app t::b", "\n" + text)
        self.assertEqual((outputs["complete"], outputs["failures"], outputs["absent"]), ("true", "true", "true"))

    def test_an_absent_id_of_a_missing_crate_is_reported_by_the_verdict(self):
        plan = {"run": 9, "shards": 1, "head": "a" * 40, "base": "b" * 40, "crates": {},
                "absent": ["ghost t::a"]}
        receipt = {"schema": 1, "run": 9, "shards": 1, "head": "a" * 40, "index": 1, "results": {}}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ru.write_json(root / "plan.json", plan)
            ru.write_json(root / "shards" / "1" / "receipt.json", receipt)
            captured = []
            with mock.patch.object(ru, "STATE", root / "state"), mock.patch.object(ru, "emit") as emit, \
                    mock.patch.object(ru, "summarize", side_effect=lambda lines: captured.append("\n".join(lines))):
                ru.command_verdict(mock.Mock(plan=str(root / "plan.json"), receipts=str(root / "shards"),
                                             output=str(root / "receipts")))
            outputs = {c.args[0]: c.args[1] for c in emit.call_args_list}
        self.assertIn("\nABSENT ghost t::a", "\n" + captured[0])
        self.assertEqual((outputs["complete"], outputs["absent"]), ("true", "true"))

    def test_all_absent_builds_nothing_and_the_verdict_runs_from_the_select_plan(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            doc = {"head": "a" * 40, "base": "b" * 40, "crates": {}, "tests": ["ghost t::a"], "absent": ["ghost t::a"]}
            ru.write_json(root / "sel.json", doc)
            emitted = {}
            with mock.patch.object(ru, "Tree") as tree, mock.patch.object(ru, "STATE", root / "state"), \
                    mock.patch.object(ru, "emit", side_effect=lambda k, v: emitted.update({k: v})), \
                    mock.patch.dict(os.environ, {"GITHUB_RUN_ID": "7"}, clear=False):
                os.environ.pop("GITHUB_TOKEN", None)
                tree.return_value.text.return_value = CONFIG
                ru.command_adopt(mock.Mock(selection=str(root / "sel.json"), output=str(root / "pending.json")))
                self.assertEqual((emitted["build"], emitted["absent_only"]), ("false", "true"))
                plan = json.loads((root / "state" / "plan.json").read_text())
                self.assertEqual((plan["shards"], plan["crates"], plan["absent"]), (0, {}, ["ghost t::a"]))
                captured = []
                with mock.patch.object(ru, "summarize", side_effect=lambda lines: captured.append("\n".join(lines))):
                    (root / "shards").mkdir()
                    ru.command_verdict(mock.Mock(plan=str(root / "state" / "plan.json"),
                                                 receipts=str(root / "shards"), output=str(root / "receipts")))
            self.assertIn("Rust units verdict: 0 passed, 0 failed, 0 adopted.", captured[0])
            self.assertIn("\nABSENT ghost t::a", "\n" + captured[0])
            self.assertEqual((emitted["complete"], emitted["absent"]), ("true", "true"))

    def test_the_all_absent_wiring_skips_archive_and_shards_and_the_gate_fails(self):
        stage = (ROOT / ".github/workflows/rust-units.yml").read_text()
        # Raw producer output: a missing `absent_only` must not default to false.
        self.assertIn("absent_only: ${{ steps.adopt.outputs.absent_only }}\n", stage)
        self.assertIn("needs.select.outputs.absent_only == 'true'", stage)
        # NamedProbeGateTests exercises the actual shell's absent-only refusal.
        self.assertNotIn("none()", (ROOT / "validation/rust_units.py").read_text())

    def test_the_gate_fails_on_absent_but_the_verdict_job_does_not(self):
        stage = (ROOT / ".github/workflows/rust-units.yml").read_text()
        self.assertIn("absent: ${{ steps.verdict.outputs.absent }}", stage)
        self.assertIn('[ "$ABSENT" = false ]', stage)

    def test_the_absent_status_reaches_the_json_and_the_receipt(self):
        self.run_verdict(passed=["app t::a"], absent=["app t::b"])
        self.assertEqual(self.written["named"]["tests"], {"app t::a": "passed", "app t::b": "absent"})
        self.assertEqual(self.written["receipt"]["absent"], ["app t::b"])

    def test_a_named_test_that_did_not_run_is_incomplete_and_never_a_pass(self):
        outputs, text = self.run_verdict(passed=["app t::a"])
        self.assertIn("named `app t::b`: NOT RUN", text)
        self.assertEqual(outputs["complete"], "false")


class VerdictTests(unittest.TestCase):
    plan = {"run": 9, "shards": 2, "head": "a" * 40, "base": "b" * 40, "crates": {
        "app": {"key": "1" * 64, "listing": {"count": 3, "sha256": "2" * 64}, "adopted": [],
                "remaining": ["app t::a", "app t::b"], "serial": ["app s::c"]}}}

    def receipt(self, index, passed=(), failed=()):
        return {"schema": 1, "run": 9, "shards": 2, "head": "a" * 40, "index": index,
                "results": {"app": {"passed": list(passed), "failed": list(failed), "skipped": []}}}

    def test_complete_coverage_has_no_problems(self):
        outcome, problems = ru.collect(self.plan, [self.receipt(1, ["app t::a", "app s::c"]),
                                                   self.receipt(2, failed=["app t::b"])])
        self.assertEqual(problems, [])
        self.assertEqual(outcome["app t::b"], "failed")

    def test_all_actual_planned_passes_allow_the_strict_gate(self):
        outcome, problems = ru.collect(self.plan, [self.receipt(1, ["app t::a", "app s::c"]),
                                                   self.receipt(2, ["app t::b"])])
        self.assertEqual(problems, [])
        self.assertEqual(set(outcome.values()), {"passed"})
        result = workflow_gate(COMPLETE="true", FAILURES="false")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_all_skipped_and_mixed_pass_skipped_cases_cannot_authorize(self):
        for first_passes in ([], ["app t::a"]):
            with self.subTest(first_passes=first_passes):
                first = self.receipt(1, first_passes)
                first["results"]["app"]["skipped"] = sorted({"app t::a", "app s::c"} - set(first_passes))
                second = self.receipt(2)
                second["results"]["app"]["skipped"] = ["app t::b"]
                outcome, problems = ru.collect(self.plan, [first, second])
                self.assertTrue(problems)
                self.assertEqual(outcome["app t::b"], "skipped")
                # There are no FAILs, but the required positive coverage is incomplete.
                self.assertNotEqual(workflow_gate(COMPLETE="false", FAILURES="false").returncode, 0)

    def test_a_missing_shard_is_reported_and_its_tests_are_missing(self):
        _outcome, problems = ru.collect(self.plan, [self.receipt(1, ["app t::a"])])
        self.assertEqual(len(problems), 2)

    def test_a_test_run_twice_is_an_integrity_failure(self):
        with self.assertRaises(ru.UnitsError):
            ru.collect(self.plan, [self.receipt(1, ["app t::a"]), self.receipt(2, ["app t::a"])])

    def test_a_test_outside_the_plan_is_an_integrity_failure(self):
        with self.assertRaises(ru.UnitsError):
            ru.collect(self.plan, [self.receipt(1, ["app t::zzz"]), self.receipt(2)])

    def test_classification_blocks_only_on_failures_that_pass_at_the_merge_base(self):
        with tempfile.TemporaryDirectory() as directory:
            failures = Path(directory, "failures.json")
            failures.write_text(json.dumps({"head": "a" * 40, "base": "b" * 40, "failures": {
                "app": ["app t::old", "app t::new", "app t::added"]}}))
            output = Path(directory, "merge-base.log")
            output.write_text("        FAIL [   0.010s] app t::old\n        PASS [   0.010s] app t::new\n")
            previous = os.getcwd()
            os.chdir(directory)
            try:
                status = ru.main(["classify", "--failures", str(failures), "--log", str(output)])
                result = json.loads(Path("target/rust-units/classification.json").read_text())
                self.assertEqual(status, 1)
                self.assertEqual(result["pre_existing"], {"app": ["app t::old"]})
                self.assertEqual(result["new"], {"app": ["app t::new", "app t::added"]})
                output.write_text("    SIGABRT [   0.010s] app t::old\n")
                failures.write_text(json.dumps({"head": "a" * 40, "base": "b" * 40,
                                                "failures": {"app": ["app t::old"]}}))
                self.assertEqual(ru.main(["classify", "--failures", str(failures), "--log", str(output)]), 0)
            finally:
                os.chdir(previous)

    def test_log_status_lines_parse_with_timestamps_and_counters(self):
        log = ("2026-11-12T10:00:00.1234567Z         PASS [   0.012s] plurxd::bin/plurxd a::b\n"
               "2026-11-12T10:00:00.1234567Z         FAIL [   1.500s] (3/9) plurx-core c::d\n"
               "2026-11-12T10:00:00.1234567Z         LEAK [   0.100s] plurx-core c::e\n"
               "2026-11-12T10:00:00.1234567Z     SIGSEGV [   0.100s] plurx-core c::f\n")
        self.assertEqual(ru.baseline_outcomes(log), {"plurxd::bin/plurxd a::b": "PASS",
                                                   "plurx-core c::d": "FAIL",
                                                   "plurx-core c::e": "PASS",
                                                   "plurx-core c::f": "FAIL"})


class ShardEvidenceTests(unittest.TestCase):
    @staticmethod
    def report(cases, tests, failures=0, errors=0):
        totals = f'tests="{tests}" failures="{failures}" errors="{errors}"'
        binary = 'app::bin/app'
        return f'<testsuites {totals}><testsuite name="{binary}" {totals}>{cases}</testsuite></testsuites>'

    @staticmethod
    def case(name, body=""):
        return f'<testcase classname="app::bin/app" name="{name}">{body}</testcase>'

    def run_report(self, report, exit_code, diagnostics=""):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory, "rust-units.xml")
            path.write_text(self.report(self.case("stale"), 1))
            process = mock.MagicMock()
            process.__enter__.return_value = process
            process.stdout = io.StringIO(diagnostics)
            process.wait.return_value = exit_code

            def start(*_args, **_kwargs):
                self.assertFalse(path.exists(), "the previous invocation's report must be removed")
                if report is not None:
                    path.write_text(report)
                return process

            with mock.patch.object(ru, "JUNIT_REPORT", path), \
                    mock.patch.object(ru.subprocess, "Popen", side_effect=start), \
                    mock.patch("sys.stdout", io.StringIO()) as out:
                result = ru.run_nextest(["cargo-nextest", "nextest", "run"])
                return result, out.getvalue()

    def test_only_the_report_can_supply_current_and_reusable_passes(self):
        forged = evidence_log({"app::bin/app t::failed": "PASS", "app::bin/app t::missing": "PASS"}, 0)
        diagnostic = "PASS [0.010s] app::bin/app t::failed\r" + forged
        cases = self.case("t::passed") + self.case(
            "t::failed", f"<failure/><system-out>{escape(forged)}</system-out>")
        checked, log = self.run_report(self.report(cases, 2, failures=1), 100, diagnostic)
        expected = {"app::bin/app t::passed": "PASS", "app::bin/app t::failed": "FAIL"}
        self.assertEqual(checked, (100, expected))
        self.assertEqual(ru.logged_outcomes(log), expected)
        self.assertNotIn("\r", log)

    def test_success_and_empty_partition_log_separate_complete_invocations(self):
        (exit_code, passed), serial_log = self.run_report(self.report(self.case("t::serial"), 1), 0)
        self.assertEqual((exit_code, passed), (0, {"app::bin/app t::serial": "PASS"}))
        checked, empty_log = self.run_report(None, 4)
        self.assertEqual(checked, (4, {}))
        self.assertEqual(ru.logged_outcomes(empty_log + serial_log), passed)

    def test_skips_do_not_become_passes_and_reported_errors_are_failures(self):
        cases = self.case("t::skipped", "<skipped/>") + self.case("t::timeout", "<error/>")
        checked, log = self.run_report(self.report(cases, 2, errors=1), 100)
        expected = {"app::bin/app t::skipped": "SKIP", "app::bin/app t::timeout": "FAIL"}
        self.assertEqual(checked, (100, expected))
        self.assertEqual(ru.logged_outcomes(log), expected)

    def test_missing_malformed_and_inconsistent_reports_are_rejected(self):
        success = self.report(self.case("t::passed"), 1)
        for report, exit_code in (
                (None, 0), (None, 100), ("<testsuites", 0), (success, 100), (success, 4),
                (self.report(self.case("t::failed", "<failure/>"), 1), 100),
                (self.report(self.case("t::passed"), 1).replace('tests="1"', 'tests="2"', 1), 0),
                (self.report(self.case("t::one") + self.case("t::one", "<failure/>"), 2, failures=1), 100),
                (self.report(self.case("t::one", "<skipped/><failure/>"), 1, failures=1), 100)):
            with self.subTest(report=report, exit_code=exit_code), self.assertRaises(ru.UnitsError):
                self.run_report(report, exit_code)


class RepositoryContractTests(unittest.TestCase):
    """The committed configuration agrees with the tree it describes."""

    @classmethod
    def setUpClass(cls):
        cls.config = ru.Config.load((ROOT / ru.CONFIG).read_text())
        cls.keys = ru.Keys(ru.Tree("HEAD", ROOT), cls.config)
        cls.workflow = (ROOT / ".github/workflows/rust-units.yml").read_text()

    def test_every_run_time_workspace_walk_is_declared(self):
        undeclared, stale = ru.dynamic_site_drift(self.keys)
        self.assertEqual(undeclared, [], "declare what these read in validation/rust-unit-inputs.toml")
        self.assertEqual(stale, [], "remove these declarations: the sites are gone")

    def test_the_workflow_publishes_a_receipt_for_every_unit_crate(self):
        published = re.findall(r"name: rust-units-([a-z0-9-]+)-\$\{\{ steps\.verdict\.outputs\.key_", self.workflow)
        self.assertEqual(sorted(published), self.keys.units)

    def test_serial_ignored_tests_are_the_ones_make_unit_runs(self):
        makefile = (ROOT / "Makefile").read_text()
        recipe = makefile.split("vodencode-restart-check: ##", 1)[1].split("\n\n", 1)[0]
        self.assertEqual(sorted(self.config.serial_ignored["plurxd"]),
                         sorted(re.findall(r"(vodserve::tests::\w+)", recipe)))

    def test_one_pinned_nextest_version(self):
        self.assertIn(f'NEXTEST_VERSION: "{ru.NEXTEST_VERSION}"', self.workflow)
        self.assertIn(f'nextest-version = {{ required = "{ru.NEXTEST_VERSION}" }}',
                      (ROOT / ".config/nextest.toml").read_text())

    def test_job_names_match_the_receipt_authenticator(self):
        for name in (ru.JOB_SELECT, ru.JOB_ARCHIVE, ru.JOB_VERDICT, ru.JOB_BASELINE):
            self.assertIn(f'name: "{name}"', self.workflow)
        self.assertIn('name: "' + ru.JOB_SHARD.format(index="${{ matrix.shard }}") + '"', self.workflow)

    def test_the_main_lane_invokes_units_and_requires_their_result(self):
        lane = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
        self.assertIn("uses: ./.github/workflows/rust-units.yml", lane)
        self.assertIn("RUST_UNITS_RESULT: ${{ needs.rust_units.result }}", lane)
        self.assertIn('"$RUST_UNITS_RESULT"', lane)
        self.assertIn(".github/workflows/main-fast-lane.yml", self.config.harness)

    def test_archive_listing_uses_the_same_ci_profile_as_shards(self):
        listing = self.workflow.split("      - name: Plan the shards\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("--profile ci", listing)
        self.assertIn("--run-ignored all", listing)

    def test_unit_selection_is_independent_of_the_compile_surface_scope(self):
        lane = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
        unit_job = lane.split("\n  rust_units:\n", 1)[1].split("\n  web_compile:", 1)[0]
        self.assertIn("needs.scope.outputs.rust_units == 'true'", unit_job)
        self.assertNotIn("needs.scope.outputs.rust ==", unit_job)
        self.assertIn("rust_units: ${{ steps.scope.outputs.rust_units || 'true' }}", lane)

    def test_the_default_filter_covers_exactly_the_argv_detected_child_fixtures(self):
        children = set()
        for path in ROOT.joinpath("crates").rglob("*.rs"):
            children.update(re.findall(r'invoked_as_child\("([^"]+)"\)', path.read_text(errors="replace")))
        config = (ROOT / ".config/nextest.toml").read_text()
        prefix = re.search(r'default-filter = "not test\(/\^([^/]+)/\)"', config).group(1)
        self.assertTrue(children)
        self.assertTrue(all(child.startswith(prefix) for child in children), children)
        # ...and the filter excludes nothing else: every test it can match is one.
        probe = (ROOT / "crates/plurx-core/src/scan/probe.rs").read_text()
        named = {"scan::probe::tests::" + name for name in re.findall(r"fn (fixture_child_\w+)\(", probe)}
        self.assertEqual(named, {child for child in children if child.startswith(prefix)})

    def test_the_excluded_crates_match_make_unit(self):
        makefile = (ROOT / "Makefile").read_text()
        unit = makefile.split("\nunit: ", 1)[1].split("\n\n", 1)[0]
        self.assertEqual(list(self.config.excluded), re.findall(r"--exclude (\S+)", unit))


class StrictGateTests(unittest.TestCase):
    def test_actual_workflow_output_bindings_preserve_missing_producer_outputs(self):
        workflow = (ROOT / ".github/workflows/rust-units.yml").read_text()
        select = workflow.split("\n  select:\n", 1)[1].split("\n  rust_units_archive:\n", 1)[0]
        archive = workflow.split("\n  rust_units_archive:\n", 1)[1].split("\n  shard:\n", 1)[0]
        build_binding = re.search(r"^      build: (.+)$", select, re.MULTILINE).group(1)
        shards_binding = re.search(r"^      shards: (.+)$", archive, re.MULTILINE).group(1)
        self.assertEqual(build_binding, "${{ steps.adopt.outputs.build }}")
        self.assertEqual(shards_binding, "${{ steps.plan.outputs.shards }}")
        adopt = select.split("        id: adopt\n", 1)[1].split("      - name:", 1)[0]
        self.assertNotRegex(adopt, r"(?m)^\s+if:")
        # Direct bindings propagate absent output as empty; neither may turn
        # a successful job with a missing producer result into an exemption.
        missing_build = workflow_gate(BUILD="", ARCHIVE="skipped", SHARDS="", SHARD="skipped",
                                      VERDICT="skipped", COMPLETE="", FAILURES="")
        self.assertNotEqual(missing_build.returncode, 0)
        missing_plan = workflow_gate(SHARDS="", SHARD="skipped", VERDICT="skipped", COMPLETE="", FAILURES="", ABSENT="")
        self.assertNotEqual(missing_plan.returncode, 0)

    def test_baseline_failure_never_substitutes_for_a_positive_pass(self):
        for baseline in ("success", "failure", "skipped", ""):
            with self.subTest(baseline=baseline):
                result = workflow_gate(FAILURES="true", BASELINE=baseline)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_malformed_and_unknown_required_outputs_fail_closed(self):
        for name in ("SELECT", "BUILD", "ABSENT_ONLY", "ARCHIVE", "SHARD", "VERDICT", "COMPLETE", "FAILURES", "ABSENT"):
            for value in (None, "", "unknown", "TRUE"):
                with self.subTest(name=name, value=value):
                    result = workflow_gate(**{name: value})
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        for value in (None, "", "null", "{}", "broken", "[0]", "[2]", "[1, true]", "[1, 1]", "[1,2,3,4,5,6,7]"):
            with self.subTest(shards=value):
                result = workflow_gate(SHARDS=value)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_partition_and_incomplete_verdict_cannot_authorize(self):
        for fields in ({"SHARD": "failure"}, {"VERDICT": "failure"}, {"COMPLETE": "false"}):
            with self.subTest(fields=fields):
                self.assertNotEqual(workflow_gate(**fields).returncode, 0)

    def test_no_due_cases_require_literal_false_and_skipped_downstream_jobs(self):
        fields = dict(BUILD="false", ARCHIVE="skipped", SHARDS="", SHARD="skipped", VERDICT="skipped",
                      COMPLETE="", FAILURES="", ABSENT="")
        result = workflow_gate(**fields)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        fields["ARCHIVE"] = "failure"
        self.assertNotEqual(workflow_gate(**fields).returncode, 0)

    def test_all_adopted_cases_require_a_valid_empty_plan_and_no_downstream_results(self):
        # Parse the JSON instead of comparing its raw spelling to "[]".
        fields = dict(SHARDS="[ ]", SHARD="skipped", VERDICT="skipped", COMPLETE="", FAILURES="", ABSENT="")
        result = workflow_gate(**fields)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for unexpected in ({"TRAIN_TESTS": "app t::a"}, {"SHARD": "success"}, {"VERDICT": "success"},
                           {"COMPLETE": "true"}, {"FAILURES": "false"}, {"ABSENT": "false"}):
            with self.subTest(unexpected=unexpected):
                self.assertNotEqual(workflow_gate(**{**fields, **unexpected}).returncode, 0)

    def test_no_build_selection_rejects_named_tests_and_unexpected_outputs(self):
        fields = dict(BUILD="false", ARCHIVE="skipped", SHARDS="", SHARD="skipped", VERDICT="skipped",
                      COMPLETE="", FAILURES="", ABSENT="")
        for unexpected in ({"TRAIN_TESTS": "app t::a"}, {"ABSENT_ONLY": ""}, {"ABSENT_ONLY": "true"},
                           {"SHARDS": "[]"}, {"COMPLETE": "true"}, {"FAILURES": "false"}, {"ABSENT": "false"}):
            with self.subTest(unexpected=unexpected):
                self.assertNotEqual(workflow_gate(**{**fields, **unexpected}).returncode, 0)


class MainPromotionGateTests(unittest.TestCase):
    def test_successful_selected_rust_units_allow_promotion(self):
        workflow = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
        gate = workflow.split("\n  promotion_gate:\n", 1)[1]
        self.assertIn("RUST_UNITS_REQUIRED: ${{ needs.scope.outputs.rust_units }}", gate)
        result = main_promotion_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_selected_rust_units_must_succeed(self):
        for result in ("skipped", "failure", "cancelled", "", None):
            with self.subTest(result=result):
                self.assertNotEqual(main_promotion_gate(RUST_UNITS_RESULT=result).returncode, 0)

    def test_explicitly_unaffected_rust_units_may_be_skipped(self):
        result = main_promotion_gate(RUST_UNITS_REQUIRED="false", RUST_UNITS_RESULT="skipped")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotEqual(main_promotion_gate(RUST_UNITS_REQUIRED="false", RUST_UNITS_RESULT="failure").returncode, 0)

    def test_missing_or_unknown_rust_unit_scope_refuses_promotion(self):
        for required in (None, "", "unknown", "TRUE"):
            with self.subTest(required=required):
                self.assertNotEqual(main_promotion_gate(RUST_UNITS_REQUIRED=required).returncode, 0)

    def test_scope_and_preflight_must_succeed(self):
        for stage in ("SCOPE_RESULT", "PREFLIGHT_RESULT"):
            for result in ("skipped", "failure", "cancelled", "", None):
                with self.subTest(stage=stage, result=result):
                    self.assertNotEqual(main_promotion_gate(**{stage: result}).returncode, 0)


class NamedProbeGateTests(unittest.TestCase):
    """The strict gate on a Train-Tests probe: ABSENT is a result, never a pass."""

    NAMED = dict(TRAIN_TESTS="app t::a,app t::b", SHARDS="[1]")

    def gate(self, **fields):
        return workflow_gate(**{**self.NAMED, **fields})

    def test_every_named_test_passing_is_the_only_green(self):
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Every named test ran and passed.", result.stdout)

    def test_an_absent_named_test_is_never_a_positive_pass(self):
        result = self.gate(ABSENT="true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("absent is never a pass", result.stdout)

    def test_a_missing_or_malformed_named_output_fails_closed(self):
        for name in ("SELECT", "BUILD", "ABSENT_ONLY", "ARCHIVE", "SHARD", "VERDICT", "COMPLETE", "FAILURES", "ABSENT"):
            for value in (None, "", "unknown", "TRUE"):
                with self.subTest(name=name, value=value):
                    self.assertNotEqual(self.gate(**{name: value}).returncode, 0)
        for value in (None, "", "null", "true", "{}", "broken", "[0]", "[2]", "[1, true]",
                      "[1, 1]", "[1, 3]", "[1,2,3,4,5,6,7]"):
            with self.subTest(shards=value):
                self.assertNotEqual(self.gate(SHARDS=value).returncode, 0)
        for fields in ({"COMPLETE": "false"}, {"FAILURES": "true"}, {"SHARDS": "[]"}, {"BUILD": "false"},
                       {"SHARD": "skipped"}, {"VERDICT": "failure"}, {"ARCHIVE": "skipped"}):
            with self.subTest(fields=fields):
                self.assertNotEqual(self.gate(**fields).returncode, 0)

    def test_a_baseline_never_excuses_a_named_failure(self):
        for baseline in ("success", "failure", "skipped", ""):
            with self.subTest(baseline=baseline):
                self.assertNotEqual(self.gate(FAILURES="true", BASELINE=baseline).returncode, 0)

    def test_an_all_absent_probe_reports_absent_from_a_successful_verdict(self):
        # The archive and shards are skipped; the verdict job ran and succeeded
        # (ABSENT is its result), and the gate alone says absent is not a pass.
        fields = dict(BUILD="false", ABSENT_ONLY="true", ARCHIVE="skipped", SHARDS="", SHARD="skipped",
                      VERDICT="success", COMPLETE="true", FAILURES="false", ABSENT="true")
        result = self.gate(**fields)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("every named test is absent", result.stdout)
        self.assertNotIn("unexpected", result.stdout)
        # A verdict that did not run is a broken probe, reported as such.
        broken = self.gate(**{**fields, "VERDICT": "skipped"})
        self.assertNotEqual(broken.returncode, 0)
        self.assertIn("the verdict ended with skipped", broken.stdout)

    def test_absent_outputs_outside_a_named_probe_fail_closed(self):
        for fields in ({"ABSENT_ONLY": "true"}, {"ABSENT": "true"}):
            with self.subTest(fields=fields):
                self.assertNotEqual(workflow_gate(**fields).returncode, 0)


class TestDiskTests(unittest.TestCase):
    """A shard refuses a runner whose temp filesystem is under the playback
    cache's floor instead of reporting the cache's refusals as test failures."""

    @staticmethod
    def filesystem(total, free, fragment=4096):
        return lambda _directory: os.statvfs_result(
            (fragment, fragment, total // fragment, free // fragment, free // fragment,
             0, 0, 0, 0, 255))

    def test_the_floor_is_the_playback_caches_own(self):
        source = (ROOT / "crates/plurxd/src/playback_cache.rs").read_text()
        self.assertIn("const GIB: u64 = 1024 * 1024 * 1024;", source)
        self.assertIn("GIB.max(self.total / 50)", source)
        self.assertEqual(ru.CACHE_FLOOR_MIN_BYTES, 1024 ** 3)
        self.assertEqual(ru.CACHE_FLOOR_FRACTION, 50)
        self.assertEqual(ru.cache_floor(10 * 2 ** 30), 2 ** 30)
        self.assertEqual(ru.cache_floor(1000 * 2 ** 30), 20 * 2 ** 30)

    def test_a_disk_under_the_cache_floor_stops_the_shard_and_names_the_runner(self):
        # 1 TiB with 2% free: every cache reservation would be refused.
        full = self.filesystem(1024 * 2 ** 30, 1024 * 2 ** 30 // 50)
        with mock.patch.dict(os.environ, {"RUNNER_NAME": "lab5-general-2"}):
            with self.assertRaisesRegex(ru.UnitsError, "lab5-general-2 cannot run unit tests"):
                ru.require_test_disk("/tmp", statvfs=full)
        # A small temp filesystem is held to the 1 GiB minimum, not to 2%.
        small = self.filesystem(900 * 2 ** 20, 890 * 2 ** 20)
        with self.assertRaises(ru.UnitsError):
            ru.require_test_disk("/tmp", statvfs=small)

    def test_a_disk_with_the_floor_and_headroom_free_runs(self):
        total = 1000 * 2 ** 30
        enough = self.filesystem(total, ru.cache_floor(total) + ru.TEST_DISK_HEADROOM_BYTES)
        with mock.patch("sys.stdout", io.StringIO()) as out:
            ru.require_test_disk("/tmp", statvfs=enough)
        self.assertIn("test disk:", out.getvalue())

    def test_the_shard_checks_the_disk_before_it_runs_a_test(self):
        with tempfile.TemporaryDirectory() as directory:
            plan = Path(directory) / "plan.json"
            plan.write_text(json.dumps({"shards": 1, "filters": ["all()"], "serial_filters": []}))
            with mock.patch.object(ru, "require_test_disk",
                                   side_effect=ru.UnitsError("full")) as check, \
                    mock.patch.object(ru, "run_nextest") as nextest:
                with self.assertRaises(ru.UnitsError):
                    ru.run_shard(plan, "archive.tar.zst", 1, 2)
            check.assert_called_once_with()
            nextest.assert_not_called()


if __name__ == "__main__":
    unittest.main()
