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
import unittest
import zipfile

from validation import rust_units as ru


ROOT = Path(__file__).resolve().parents[2]

CONFIG = """
version = 1
excluded_crates = ["harness"]
harness = ["validation/rust-unit-inputs.toml"]
exempt = ["docs/**", "tests/**", "validation/**", "**/*.md"]
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
    repo.write("docs/guide.md", "guide\n")
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

    def test_an_unrelated_crate_change_selects_only_that_crate(self):
        self.assertEqual(self.due_after({"crates/app/src/lib.rs": "pub fn app() { /* new */ }\n"}), ["app"])

    def test_documentation_selects_nothing(self):
        self.assertEqual(self.due_after({"docs/guide.md": "changed\n"}), [])

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
        self.repo.write("docs/guide.md", "other\n")
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
                  '        .parent()\n        .unwrap()\n        .join("docs")\n}\n')
        self.repo.write("crates/leaf/src/walk.rs", walker)
        head = self.repo.commit()
        keys = ru.Keys(ru.Tree(head, self.repo.root), ru.Config.load(CONFIG))
        self.assertEqual(ru.dynamic_site_drift(keys), ([("leaf", "crates/leaf/src/walk.rs", "walk")], []))
        declared = CONFIG + ('\n[[dynamic]]\ncrate = "leaf"\nfile = "crates/leaf/src/walk.rs"\n'
                             'function = "walk"\nreads = ["docs/**"]\nreason = "walks docs"\n')
        self.repo.write("validation/rust-unit-inputs.toml", declared)
        self.base = self.repo.commit()
        keys = ru.Keys(ru.Tree(self.base, self.repo.root), ru.Config.load(declared))
        self.assertEqual(ru.dynamic_site_drift(keys), ([], []))
        # Once declared, the read paths are inputs: docs now re-key the reader.
        self.assertEqual(self.due_after({"docs/guide.md": "read by leaf\n"}), ["leaf"])


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

    def forge(self, receipt=None, shard_log=None, publisher_log=None, run_commit=None):
        name = ru.receipt_name("app", self.key)
        receipt = receipt or {
            "schema": 1, "producer": "validation.rust_units", "crate": "app", "key": self.key,
            "commit": self.commit, "run": 41, "publisher": ru.JOB_VERDICT,
            "evidence_jobs": [ru.JOB_SHARD.format(index=1)], "listing": {"count": 2, "sha256": "0" * 64},
            "passed": self.passes, "failed": []}
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr("receipt.json", json.dumps(receipt))
        shard_log = shard_log if shard_log is not None else (
            "2026-11-12T10:00:00.1Z         PASS [   0.012s] app aa::first\n"
            "2026-11-12T10:00:00.2Z         PASS [   0.013s] (2/2) app aa::second\n")
        publisher_log = publisher_log if publisher_log is not None else (
            f"Artifact {name} has been successfully uploaded! Final size is 300 bytes.\n")
        responses = {
            "/actions/artifacts": [{"id": 7, "name": name, "run_id": 41, "expired": False,
                                    "size_in_bytes": len(buffer.getvalue())}],
            "/actions/runs/41": {"id": 41, "commit_sha": run_commit or self.commit},
            "/actions/runs/41/jobs": [
                {"id": 501, "name": ru.JOB_VERDICT, "status": "success"},
                {"id": 502, "name": ru.JOB_SHARD.format(index=1), "status": "success"}],
            "/actions/artifacts/7/zip": buffer.getvalue(),
            "/actions/jobs/501/logs": publisher_log.encode(),
            "/actions/jobs/502/logs": shard_log.encode(),
        }

        class Forge:
            budget = 100

            def bytes(self, path, query=None, limit=0):
                value = responses[path]
                return value if isinstance(value, bytes) else json.dumps(value).encode()

            def get(self, path, query=None):
                return json.loads(self.bytes(path, query))

        return Forge()

    def adopted(self, forge):
        authenticator = ru.Authenticator(forge, current_run=99, config=self.config,
                                         repository=str(self.repo.root))
        return ru.history(forge, authenticator, "app", self.key)["passed"]

    def test_an_authenticated_receipt_is_adopted(self):
        self.assertEqual(self.adopted(self.forge()), set(self.passes))

    def test_a_pass_missing_from_the_shard_log_is_not_adopted(self):
        log = "2026-11-12T10:00:00.1Z         PASS [   0.012s] app aa::first\n"
        self.assertEqual(self.adopted(self.forge(shard_log=log)), {"app aa::first"})

    def test_a_logged_failure_cannot_be_claimed_as_a_pass(self):
        log = ("2026-11-12T10:00:00.1Z         PASS [   0.012s] app aa::first\n"
               "2026-11-12T10:00:00.2Z         FAIL [   0.013s] app aa::second\n")
        self.assertEqual(self.adopted(self.forge(shard_log=log)), {"app aa::first"})

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

    def test_a_complete_listing_lets_the_whole_crate_be_skipped(self):
        listing = {"count": 2, "sha256": ru.digest_lines(sorted(self.passes))}
        self.assertTrue(ru.fully_passed([listing], set(self.passes)))
        self.assertFalse(ru.fully_passed([listing], {self.passes[0]}))
        self.assertFalse(ru.fully_passed([], set(self.passes)))


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

    def test_shard_count_follows_the_selection(self):
        self.assertEqual(ru.shard_count(1), 1)
        self.assertEqual(ru.shard_count(ru.TESTS_PER_SHARD), 1)
        self.assertEqual(ru.shard_count(ru.TESTS_PER_SHARD + 1), 2)
        self.assertEqual(ru.shard_count(10 ** 6), ru.MAX_SHARDS)


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
            junit = Path(directory, "junit.xml")
            junit.write_text('<testsuites><testsuite name="app">'
                             '<testcase name="t::old"><failure/></testcase>'
                             '<testcase name="t::new"/></testsuite></testsuites>')
            previous = os.getcwd()
            os.chdir(directory)
            try:
                status = ru.main(["classify", "--failures", str(failures), "--junit", str(junit)])
                result = json.loads(Path("target/rust-units/classification.json").read_text())
                self.assertEqual(status, 1)
                self.assertEqual(result["pre_existing"], {"app": ["app t::old"]})
                self.assertEqual(result["new"], {"app": ["app t::new", "app t::added"]})
                junit.write_text('<testsuites><testsuite name="app">'
                                 '<testcase name="t::old"><failure/></testcase></testsuite></testsuites>')
                failures.write_text(json.dumps({"head": "a" * 40, "base": "b" * 40,
                                                "failures": {"app": ["app t::old"]}}))
                self.assertEqual(ru.main(["classify", "--failures", str(failures), "--junit", str(junit)]), 0)
            finally:
                os.chdir(previous)

    def test_log_status_lines_parse_with_timestamps_and_counters(self):
        log = ("2026-11-12T10:00:00.1234567Z         PASS [   0.012s] plurxd::bin/plurxd a::b\n"
               "2026-11-12T10:00:00.1234567Z         FAIL [   1.500s] (3/9) plurx-core c::d\n"
               "2026-11-12T10:00:00.1234567Z         LEAK [   0.100s] plurx-core c::e\n"
               "2026-11-12T10:00:00.1234567Z     SIGSEGV [   0.100s] plurx-core c::f\n")
        self.assertEqual(ru.logged_outcomes(log), {"plurxd::bin/plurxd a::b": "PASS",
                                                   "plurx-core c::d": "FAIL",
                                                   "plurx-core c::e": "PASS",
                                                   "plurx-core c::f": "FAIL"})


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

    def test_the_excluded_crates_match_make_unit(self):
        makefile = (ROOT / "Makefile").read_text()
        unit = makefile.split("\nunit: ", 1)[1].split("\n\n", 1)[0]
        self.assertEqual(list(self.config.excluded), re.findall(r"--exclude (\S+)", unit))


if __name__ == "__main__":
    unittest.main()
